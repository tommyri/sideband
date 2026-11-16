//! The `Welcome` size bound in `docs/security.md` section 7, measured with the
//! real snapshot builder and the protobuf encoder.
//!
//! [`Strings::Longest`] puts every string at its longest allowed value in
//! four-byte characters, gives every member an avatar, and gives the members
//! that can be online at once (one per connection, `MAX_CONNECTIONS`) a full
//! presence activity. [`Strings::Typical`] uses short ASCII names, no topics,
//! no avatars and no activity. [`Topics`] says how the seeded channels carry
//! topics under [`Strings::Longest`].

use std::sync::Arc;

use prost::Message as _;
use ss_core::{ChannelKind, Id, Permissions, UserKind, VoiceProfile};
use ss_proto::{v1, validate::id_to_bytes};
use ss_server::{
    MAX_CONNECTIONS,
    db::{self, NewChannel, NewRole, NewUser, channels, roles, users},
    limits,
    perms::PermissionService,
    snapshot,
};

/// Four UTF-8 bytes, a letter (not hidden, not changed by NFC).
const WIDE: char = '𝕏';

#[derive(Clone, Copy, PartialEq, Eq)]
enum Strings {
    Longest,
    Typical,
}

impl Strings {
    /// `chars` wide characters, or the typical text.
    fn text(self, chars: usize, typical: &str) -> String {
        match self {
            Self::Longest => std::iter::repeat_n(WIDE, chars).collect(),
            Self::Typical => typical.to_owned(),
        }
    }

    fn username(self, n: usize) -> String {
        match self {
            Self::Longest => format!("u{n:0>31}"),
            Self::Typical => format!("member{n}"),
        }
    }
}

/// Topics on the seeded channels. [`Strings::Typical`] channels have none.
#[derive(Clone, Copy)]
enum Topics {
    /// Every channel's topic at the 1,024-character limit.
    Full,
    /// No topic anywhere.
    Absent,
    /// This many topic bytes in all, spread as evenly as the four-byte
    /// characters allow over every channel but the category. Each topic adds
    /// a few bytes of framing, so the most topics is the largest `Welcome`
    /// for a given budget.
    Spread(usize),
}

impl Topics {
    /// Characters in the topic of the `n`th of `channels` channels.
    fn chars(self, n: usize, channels: usize) -> usize {
        match self {
            Self::Full => ss_server::text::MAX_TOPIC,
            Self::Absent => 0,
            Self::Spread(bytes) => {
                let total = bytes / WIDE.len_utf8();
                total / channels + usize::from(n < total % channels)
            }
        }
    }
}

/// The guild a measurement seeds.
#[derive(Clone, Copy)]
struct Guild {
    strings: Strings,
    members: usize,
    /// Roles besides `@everyone` and Admin.
    roles: usize,
    /// Of those roles, how many each member holds.
    held: usize,
    /// Channels besides the one category they sit in.
    channels: usize,
    topics: Topics,
}

struct Seeded {
    viewer: Id,
    perms: PermissionService,
    _dir: tempfile::TempDir,
}

async fn seed(guild: Guild) -> Seeded {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let strings = guild.strings;
    let mut guard = pool.write().await;
    let mut tx = sqlx::Connection::begin(&mut *guard).await.unwrap();
    let mut role_ids = Vec::with_capacity(guild.roles);
    for n in 0..guild.roles {
        let role = NewRole {
            id: Id::generate(),
            name: strings.text(100, "Moderators"),
            color: Some(0xFF_FF_FF),
            permissions: Permissions::all(),
            position: i64::try_from(n).unwrap() + 10,
        };
        role_ids.push(roles::insert(&mut tx, role).await.unwrap().id);
    }
    let mut viewer = None;
    for n in 0..guild.members.max(1) {
        let user = NewUser {
            id: Id::generate(),
            username: strings.username(n),
            display_name: strings.text(64, "Member Name"),
            password_hash: "x".into(),
            kind: UserKind::User,
        };
        let user = users::insert(&mut tx, user).await.unwrap().id;
        viewer.get_or_insert(user);
        for role in role_ids.iter().take(guild.held) {
            roles::assign(&mut tx, user, *role).await.unwrap();
        }
    }
    let creator = viewer.unwrap();
    let category = channels::insert(&mut tx, channel(strings, creator, None, 1, 0))
        .await
        .unwrap()
        .id;
    let mut topic_bytes = 0;
    for n in 0..guild.channels {
        let position = i64::try_from(n).unwrap() + 10;
        let chars = guild.topics.chars(n, guild.channels);
        let row = channel(strings, creator, Some(category), position, chars);
        topic_bytes += row.topic.as_ref().map_or(0, String::len);
        channels::insert(&mut tx, row).await.unwrap();
    }
    if let (Strings::Longest, Topics::Spread(bytes)) = (strings, guild.topics) {
        assert_eq!(topic_bytes, bytes, "the seed spends the whole topic budget");
    }
    tx.commit().await.unwrap();
    drop(guard);
    Seeded {
        viewer: creator,
        perms: PermissionService::open(pool).await.unwrap(),
        _dir: dir,
    }
}

/// A category without a parent, else the largest channel `strings` allows: a
/// voice channel with a topic of `topic_chars` (none when zero), every voice
/// setting, and a parent.
fn channel(
    strings: Strings,
    creator: Id,
    parent: Option<Id>,
    position: i64,
    topic_chars: usize,
) -> NewChannel {
    let longest = strings == Strings::Longest && parent.is_some();
    NewChannel {
        id: Id::generate(),
        kind: match (parent, longest) {
            (None, _) => ChannelKind::Category,
            (Some(_), true) => ChannelKind::Voice,
            (Some(_), false) => ChannelKind::Text,
        },
        name: strings.text(64, "general-chat"),
        topic: (longest && topic_chars > 0).then(|| strings.text(topic_chars, "")),
        parent_id: parent,
        position,
        voice_profile: longest.then_some(VoiceProfile::Music),
        voice_bitrate: longest.then_some(510_000),
        user_limit: longest.then_some(99),
        created_by: creator,
    }
}

/// Encoded size of the `Welcome` envelope the seeded viewer would get.
async fn welcome_len(guild: Guild) -> usize {
    let seeded = seed(guild).await;
    let longest = guild.strings == Strings::Longest;
    let activity = guild.strings.text(128, "");
    let mut snapshot = snapshot::build_with(&seeded.perms, seeded.viewer, |_| v1::Presence {
        status: i32::from(v1::PresenceStatus::Online),
        activity: activity.clone(),
    })
    .await
    .unwrap();
    for (n, member) in snapshot.members.iter_mut().enumerate() {
        if longest && let Some(user) = member.user.as_mut() {
            user.avatar_attachment_id = Some(id_to_bytes(Id::generate()));
        }
        if n >= MAX_CONNECTIONS
            && let Some(presence) = member.presence.as_mut()
        {
            presence.status = i32::from(v1::PresenceStatus::Offline);
            presence.activity.clear();
        }
    }
    let user = snapshot.members[0].user.clone();
    v1::ServerEnvelope {
        request_id: u32::MAX,
        msg: Some(v1::server_envelope::Msg::Welcome(v1::Welcome {
            user,
            server_info: Some(v1::ServerInfo {
                name: guild.strings.text(64, "Sideband"),
                version: "0.0.0-measured".into(),
                features: (0..16).map(|n| format!("feature-{n:0>16}")).collect(),
            }),
            resume_token: "t".repeat(64),
            snapshot: Some(snapshot),
            conn_id: id_to_bytes(Id::generate()),
        })),
    }
    .encoded_len()
}

/// Bytes per offline member, role, role held, channel with a full topic, and
/// channel without one.
async fn costs(strings: Strings) -> [usize; 5] {
    let base = Guild {
        strings,
        members: 300,
        roles: 0,
        held: 0,
        channels: 0,
        topics: Topics::Full,
    };
    let empty = welcome_len(base).await;
    let members = welcome_len(Guild {
        members: 1300,
        ..base
    })
    .await;
    let roles = welcome_len(Guild { roles: 100, ..base }).await;
    let held = welcome_len(Guild {
        roles: 10,
        held: 10,
        ..base
    })
    .await;
    let channels = welcome_len(Guild {
        channels: 50,
        ..base
    })
    .await;
    let topicless = welcome_len(Guild {
        channels: 50,
        topics: Topics::Absent,
        ..base
    })
    .await;
    let per_role = (roles - empty) / 100;
    [
        (members - empty) / 1000,
        per_role,
        (held - empty - 10 * per_role) / (10 * 300),
        (channels - empty) / 50,
        (topicless - empty) / 50,
    ]
}

/// Per-item costs, printed for the numbers in `docs/security.md` section 7.
/// Run with `--nocapture` to see them.
#[tokio::test]
async fn welcome_costs_per_member_role_and_channel() {
    let longest = costs(Strings::Longest).await;
    let typical = costs(Strings::Typical).await;
    let online = welcome_len(Guild {
        strings: Strings::Longest,
        members: MAX_CONNECTIONS,
        roles: 0,
        held: 0,
        channels: 0,
        topics: Topics::Absent,
    })
    .await;
    let alone = welcome_len(Guild {
        strings: Strings::Longest,
        members: 1,
        roles: 0,
        held: 0,
        channels: 0,
        topics: Topics::Absent,
    })
    .await;
    let read_state = read_state_len();
    let seat = seat_len();
    eprintln!(
        "[member, role, role held, channel, channel without topic] longest {longest:?} typical {typical:?}; \
         {MAX_CONNECTIONS} online longest members {online} B; one member alone {alone} B; \
         read state {read_state} B; voice seat {seat} B"
    );
    // The costs the bound in `docs/security.md` is computed from.
    for (cost, stated) in longest.into_iter().zip([360, 440, 18, 4_450, 345]) {
        assert!(cost <= stated, "longest {longest:?}");
    }
    for (cost, stated) in typical.into_iter().zip([70, 45, 18, 90, 90]) {
        assert!(cost <= stated, "typical {typical:?}");
    }
    assert!(online <= 226_000, "{online}");
    assert!(read_state <= 45, "{read_state}");
    assert!(seat <= 140, "{seat}");
}

/// One read state in the snapshot, with its field header, at its largest.
fn read_state_len() -> usize {
    v1::ReadState {
        channel_id: id_to_bytes(Id::generate()),
        last_read_message_id: id_to_bytes(Id::generate()),
        mention_count: u32::MAX,
    }
    .encoded_len()
        + 2
}

/// One voice seat's `VoiceState` and audio `Track` in the snapshot, each with
/// its two-byte field header.
fn seat_len() -> usize {
    let id = || id_to_bytes(Id::generate());
    let state = v1::VoiceState {
        user_id: id(),
        conn_id: id(),
        channel_id: Some(id()),
        self_mute: true,
        self_deaf: true,
        server_mute: true,
    };
    let track = v1::Track {
        track_id: ss_net::media::MAX_TRACK_ID,
        user_id: id(),
        conn_id: id(),
        kind: i32::from(v1::TrackKind::Audio),
        codec: Some(v1::Codec {
            name: "opus".into(),
            sample_rate: 48_000,
            channels: 2,
            description: bytes::Bytes::new(),
        }),
        channel_id: id(),
        renditions: Vec::new(),
    };
    state.encoded_len() + track.encoded_len() + 4
}

/// The typical example stated in `docs/security.md` section 7 fits one frame.
/// The longest one is
/// [`a_welcome_at_the_channel_and_role_caps_leaves_member_headroom`].
#[tokio::test]
async fn the_stated_welcome_examples_fit_one_frame() {
    let typical = welcome_len(Guild {
        strings: Strings::Typical,
        members: 8_000,
        roles: limits::MAX_ROLES - 2,
        held: 2,
        channels: 200,
        topics: Topics::Absent,
    })
    .await;
    eprintln!("typical example {typical} B");
    assert!(typical <= ss_proto::MAX_FRAME_LEN, "{typical} B");
}

/// Longest members, each holding five roles, that `docs/security.md` section
/// 7 promises still fit at the channel and role caps.
const HEADROOM_MEMBERS: usize = 1_000;

/// At the channel cap with the whole topic budget spent and at the role cap,
/// every string at its longest, every connection online with a full activity
/// and a voice seat, and a read state on every channel, a `Welcome` still has
/// room for [`HEADROOM_MEMBERS`].
#[tokio::test]
async fn a_welcome_at_the_channel_and_role_caps_leaves_member_headroom() {
    let guild = Guild {
        strings: Strings::Longest,
        members: HEADROOM_MEMBERS,
        roles: limits::MAX_ROLES - 2,
        held: 5,
        // The seeded category is the last channel the cap allows.
        channels: limits::MAX_CHANNELS - 1,
        topics: Topics::Spread(limits::CHANNEL_TOPIC_BYTES),
    };
    let welcome = welcome_len(guild).await;
    let total = welcome + MAX_CONNECTIONS * seat_len() + limits::MAX_CHANNELS * read_state_len();
    let spare = ss_proto::MAX_FRAME_LEN.saturating_sub(total);
    eprintln!("at the caps: {total} B with {HEADROOM_MEMBERS} longest members, {spare} B spare");
    assert!(total <= ss_proto::MAX_FRAME_LEN, "{total} B");
}
