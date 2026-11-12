//! Channel tree, default snapshot, and visibility of channel events.

use ss_core::{ChannelKind, Id, Permissions, Timestamp, UserKind, VoiceProfile};
use ss_proto::{
    v1::{self, ack, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};
use ss_server::{
    db::{
        self, ADMIN_ROLE_ID, EVERYONE_ROLE_ID, NewChannel, NewMessage, NewUser, channels, messages,
        roles, users,
    },
    handlers::{self, Channels},
    limits,
    perms::PermissionService,
    snapshot,
};
use std::sync::Arc;

#[tokio::test]
async fn a_fresh_server_snapshot_has_the_default_tree() {
    let world = world().await;
    let snap = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert_eq!(snap.voice_states, Vec::new());
    assert_eq!(snap.tracks, Vec::new());
    assert_eq!(snap.channels.len(), 3);
    let category = find(&snap, "General", v1::ChannelKind::Category);
    let text = find(&snap, "general", v1::ChannelKind::Text);
    let voice = find(&snap, "General", v1::ChannelKind::Voice);
    assert!(category.parent_id.is_none());
    assert_eq!(text.parent_id.as_ref(), Some(&category.id));
    assert_eq!(voice.parent_id.as_ref(), Some(&category.id));
    assert_eq!(text.position, 0);
    assert_eq!(voice.position, 1);
    assert_eq!(
        voice.voice.as_ref().unwrap().profile,
        i32::from(v1::VoiceProfile::Voice)
    );
    assert!(snap.roles.iter().any(|role| role.is_default));
    assert!(
        snap.members
            .iter()
            .any(|member| member.user.as_ref().unwrap().username == "admin")
    );
    let again = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert_eq!(again.channels.len(), 3);
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "create, move, and delete are one ordered scenario"
)]
async fn commands_validate_order_and_keep_messages() {
    let world = world().await;
    snapshot::build(&world.perms, world.admin).await.unwrap();
    let denied = world
        .channels
        .create(world.member, &text_channel("nope", None, 0))
        .await;
    assert!(matches!(denied, Err(handlers::Error::Forbidden(_))));

    for (name, command, expect) in invalid_creates() {
        let result = world.channels.create(world.admin, &command).await;
        assert_eq!(class(result), expect, "{name}");
    }

    let snap = snapshot::build(&world.perms, world.admin).await.unwrap();
    let category = id_of(find(&snap, "General", v1::ChannelKind::Category));
    let text = id_of(find(&snap, "general", v1::ChannelKind::Text));
    let created = world
        .channels
        .create(
            world.admin,
            &text_channel("announcements", Some(category), 0),
        )
        .await
        .unwrap();
    assert!(matches!(created.ack, ack::Result::ChannelId(_)));
    let Some(server_envelope::Msg::ChannelCreated(event)) = created.events.first() else {
        panic!("expected ChannelCreated");
    };
    assert_eq!(event.channel.as_ref().unwrap().position, 0);
    let mut conn = world.pool.write().await;
    assert_eq!(
        channels::get(&mut conn, text)
            .await
            .unwrap()
            .unwrap()
            .position,
        1
    );
    let message_id = Id::generate();
    messages::insert(
        &mut conn,
        NewMessage {
            id: message_id,
            channel_id: text,
            author_id: Some(world.admin),
            webhook_id: None,
            webhook_name: None,
            content: "stay".into(),
            reply_to: None,
            mentions: Vec::new(),
            mentions_everyone: false,
        },
    )
    .await
    .unwrap();
    drop(conn);

    let moved = world
        .channels
        .relocate(
            world.admin,
            &v1::MoveChannel {
                channel_id: id_to_bytes(text),
                parent_id: Some(id_to_bytes(category)),
                position: 2,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        moved.events.first(),
        Some(server_envelope::Msg::ChannelUpdated(_))
    ));
    let mut conn = world.pool.write().await;
    assert_eq!(
        channels::get(&mut conn, text)
            .await
            .unwrap()
            .unwrap()
            .position,
        2
    );
    drop(conn);

    let deleted = world
        .channels
        .delete(
            world.admin,
            &v1::DeleteChannel {
                channel_id: id_to_bytes(category),
            },
        )
        .await
        .unwrap();
    assert!(deleted.events.len() >= 3);
    let mut conn = world.pool.write().await;
    assert!(
        channels::get(&mut conn, category)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert!(
        channels::get(&mut conn, text)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert_eq!(
        messages::get(&mut conn, message_id)
            .await
            .unwrap()
            .unwrap()
            .content,
        "stay"
    );
    drop(conn);
    let after = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert!(
        after
            .channels
            .iter()
            .all(|channel| channel.name != "general")
    );
    let mut conn = world.pool.write().await;
    assert!(channels::guild_count(&mut conn).await.unwrap() > 0);
}

#[tokio::test]
async fn a_hidden_channel_is_left_out_of_the_snapshot_and_events() {
    let world = world().await;
    snapshot::build(&world.perms, world.admin).await.unwrap();
    let snap = snapshot::build(&world.perms, world.admin).await.unwrap();
    let category = id_of(find(&snap, "General", v1::ChannelKind::Category));
    let created = world
        .channels
        .create(world.admin, &text_channel("secret", Some(category), 2))
        .await
        .unwrap();
    let secret = id_of(created.events.first().unwrap().channel_created());
    let mut conn = world.pool.write().await;
    channels::set_override(
        &mut conn,
        secret,
        EVERYONE_ROLE_ID,
        Permissions::empty(),
        Permissions::VIEW_CHANNEL,
    )
    .await
    .unwrap();
    drop(conn);
    world.perms.reload().await.unwrap();
    let member = snapshot::build(&world.perms, world.member).await.unwrap();
    let admin = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert!(
        member
            .channels
            .iter()
            .all(|channel| channel.name != "secret")
    );
    assert!(
        admin
            .channels
            .iter()
            .any(|channel| channel.name == "secret")
    );
    let event = server_envelope::Msg::ChannelUpdated(v1::ChannelUpdated {
        channel: Some(v1::Channel {
            id: id_to_bytes(secret),
            ..v1::Channel::default()
        }),
    });
    assert!(!snapshot::delivers(&world.perms, world.member, &event));
    assert!(snapshot::delivers(&world.perms, world.admin, &event));
    assert!(snapshot::delivers(
        &world.perms,
        world.member,
        &server_envelope::Msg::RoleCreated(v1::RoleCreated::default())
    ));
    let track = server_envelope::Msg::TrackAdded(v1::TrackAdded {
        channel_id: id_to_bytes(secret),
        ..v1::TrackAdded::default()
    });
    assert!(!snapshot::delivers(&world.perms, world.member, &track));
    assert!(snapshot::delivers(&world.perms, world.admin, &track));
    let voice = server_envelope::Msg::VoiceStateUpdated(v1::VoiceStateUpdated {
        channel_id: Some(id_to_bytes(secret)),
        ..v1::VoiceStateUpdated::default()
    });
    assert!(!snapshot::delivers(&world.perms, world.member, &voice));
    assert!(snapshot::delivers(&world.perms, world.admin, &voice));
    let left = server_envelope::Msg::VoiceStateUpdated(v1::VoiceStateUpdated::default());
    assert!(snapshot::delivers(&world.perms, world.member, &left));
}

const CHANNEL_CAP: &str = "the server already has the maximum number of channels";
const TOPIC_BUDGET: &str = "the server's channel topics would pass their total length limit";

#[tokio::test]
async fn creating_a_channel_past_the_cap_is_refused() {
    let world = world().await;
    let mut conn = world.pool.write().await;
    // Direct messages and deleted channels are not in the guild tree, so they
    // do not count.
    seed(&mut conn, world.admin, ChannelKind::Dm, 3, None).await;
    let deleted = seed(&mut conn, world.admin, ChannelKind::Text, 3, None).await;
    for id in deleted {
        channels::soft_delete(&mut conn, id, Timestamp::now())
            .await
            .unwrap();
    }
    let live = seed(
        &mut conn,
        world.admin,
        ChannelKind::Text,
        limits::MAX_CHANNELS - 1,
        None,
    )
    .await;
    drop(conn);
    world.perms.reload().await.unwrap();
    let mut category = text_channel("last", None, 0);
    category.kind = i32::from(v1::ChannelKind::Category);
    world
        .channels
        .create(world.admin, &category)
        .await
        .expect("the cap itself is allowed");
    // A category is a channel in the `Welcome` too.
    for kind in [v1::ChannelKind::Category, v1::ChannelKind::Voice] {
        let mut command = text_channel("one too many", None, 0);
        command.kind = i32::from(kind);
        assert_cap(&world.channels.create(world.admin, &command).await);
    }
    world
        .channels
        .delete(
            world.admin,
            &v1::DeleteChannel {
                channel_id: id_to_bytes(live[0]),
            },
        )
        .await
        .unwrap();
    world
        .channels
        .create(world.admin, &text_channel("freed", None, 0))
        .await
        .expect("a delete frees a slot");
    assert_cap(
        &world
            .channels
            .create(world.admin, &text_channel("again", None, 0))
            .await,
    );
}

#[tokio::test]
async fn a_server_over_the_channel_cap_keeps_and_edits_its_channels() {
    let world = world().await;
    let mut conn = world.pool.write().await;
    let live = seed(
        &mut conn,
        world.admin,
        ChannelKind::Text,
        limits::MAX_CHANNELS + 5,
        None,
    )
    .await;
    drop(conn);
    world.perms.reload().await.unwrap();
    assert_cap(
        &world
            .channels
            .create(world.admin, &text_channel("more", None, 0))
            .await,
    );
    world
        .channels
        .update(
            world.admin,
            &v1::UpdateChannel {
                channel_id: id_to_bytes(live[0]),
                name: Some("renamed".into()),
                topic: Some("a topic".into()),
                ..Default::default()
            },
        )
        .await
        .expect("existing channels stay editable");
    world
        .channels
        .relocate(
            world.admin,
            &v1::MoveChannel {
                channel_id: id_to_bytes(live[1]),
                parent_id: None,
                position: 0,
            },
        )
        .await
        .expect("existing channels stay movable");
    world
        .channels
        .delete(
            world.admin,
            &v1::DeleteChannel {
                channel_id: id_to_bytes(live[2]),
            },
        )
        .await
        .unwrap();
    assert_cap(
        &world
            .channels
            .create(world.admin, &text_channel("still over", None, 0))
            .await,
    );
    let snap = snapshot::build(&world.perms, world.admin).await.unwrap();
    assert_eq!(snap.channels.len(), limits::MAX_CHANNELS + 4);
}

#[tokio::test]
async fn channel_topics_share_one_byte_budget() {
    let world = world().await;
    let full = "a".repeat(1024);
    let mut conn = world.pool.write().await;
    let seeded = seed(
        &mut conn,
        world.admin,
        ChannelKind::Text,
        limits::CHANNEL_TOPIC_BYTES / full.len(),
        Some(&full),
    )
    .await;
    let rest = "a".repeat(limits::CHANNEL_TOPIC_BYTES % full.len());
    seed(&mut conn, world.admin, ChannelKind::Text, 1, Some(&rest)).await;
    // DM names are not topics and do not count.
    seed(&mut conn, world.admin, ChannelKind::GroupDm, 2, Some(&full)).await;
    drop(conn);
    world.perms.reload().await.unwrap();
    let mut topical = text_channel("topical", None, 0);
    topical.topic = Some("b".into());
    assert_topic_budget(&world.channels.create(world.admin, &topical).await);
    let created = world
        .channels
        .create(world.admin, &text_channel("quiet", None, 0))
        .await
        .expect("a channel without a topic needs no budget");
    let quiet = id_of(created.events.first().unwrap().channel_created());
    let set_topic = |id: Id, topic: String| v1::UpdateChannel {
        channel_id: id_to_bytes(id),
        topic: Some(topic),
        ..Default::default()
    };
    // Bytes, not characters: 300 four-byte characters are 1,200 bytes.
    let wide = |chars: usize| "𝕏".repeat(chars);
    assert_topic_budget(
        &world
            .channels
            .update(world.admin, &set_topic(seeded[0], wide(300)))
            .await,
    );
    assert_topic_budget(
        &world
            .channels
            .update(world.admin, &set_topic(quiet, "c".into()))
            .await,
    );
    world
        .channels
        .update(world.admin, &set_topic(seeded[0], wide(200)))
        .await
        .expect("a shorter topic fits");
    world
        .channels
        .update(world.admin, &set_topic(quiet, "c".repeat(224)))
        .await
        .expect("the 224 bytes freed are reusable");
    assert_topic_budget(
        &world
            .channels
            .update(world.admin, &set_topic(seeded[1], "a".repeat(1023) + "𝕏"))
            .await,
    );
    world
        .channels
        .update(
            world.admin,
            &v1::UpdateChannel {
                channel_id: id_to_bytes(seeded[1]),
                name: Some("renamed".into()),
                topic: Some("d".repeat(1024)),
                ..Default::default()
            },
        )
        .await
        .expect("a rename and a topic of the same length need no room");
}

#[tokio::test]
async fn a_server_over_the_topic_budget_can_shrink_but_not_grow() {
    let world = world().await;
    let full = "a".repeat(1024);
    let mut conn = world.pool.write().await;
    let seeded = seed(
        &mut conn,
        world.admin,
        ChannelKind::Text,
        limits::CHANNEL_TOPIC_BYTES / full.len() + 2,
        Some(&full),
    )
    .await;
    drop(conn);
    world.perms.reload().await.unwrap();
    let shrink = v1::UpdateChannel {
        channel_id: id_to_bytes(seeded[0]),
        topic: Some("a".repeat(1000)),
        ..Default::default()
    };
    world
        .channels
        .update(world.admin, &shrink)
        .await
        .expect("shrinking is allowed over the budget");
    let grow = v1::UpdateChannel {
        topic: Some("a".repeat(1001)),
        ..shrink
    };
    assert_topic_budget(&world.channels.update(world.admin, &grow).await);
    world
        .channels
        .update(
            world.admin,
            &v1::UpdateChannel {
                channel_id: id_to_bytes(seeded[1]),
                topic: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .expect("clearing a topic is allowed");
}

/// Inserts `count` channels of `kind` at the root, each with `topic`.
async fn seed(
    conn: &mut sqlx::SqliteConnection,
    creator: Id,
    kind: ChannelKind,
    count: usize,
    topic: Option<&str>,
) -> Vec<Id> {
    let mut tx = sqlx::Connection::begin(&mut *conn).await.unwrap();
    let mut ids = Vec::with_capacity(count);
    for n in 0..count {
        let channel = NewChannel {
            id: Id::generate(),
            kind,
            name: format!("seeded {n}"),
            topic: topic.map(str::to_owned),
            parent_id: None,
            position: i64::try_from(n).unwrap(),
            voice_profile: None,
            voice_bitrate: None,
            user_limit: None,
            created_by: creator,
        };
        ids.push(channels::insert(&mut tx, channel).await.unwrap().id);
    }
    tx.commit().await.unwrap();
    ids
}

fn assert_cap(result: &Result<handlers::CommandReply, handlers::Error>) {
    let Err(handlers::Error::Forbidden(message)) = result else {
        panic!("expected the channel cap, got {result:?}");
    };
    assert_eq!(*message, CHANNEL_CAP);
}

fn assert_topic_budget(result: &Result<handlers::CommandReply, handlers::Error>) {
    let Err(handlers::Error::Forbidden(message)) = result else {
        panic!("expected the topic budget, got {result:?}");
    };
    assert_eq!(*message, TOPIC_BUDGET);
}

struct World {
    pool: Arc<db::Pool>,
    perms: Arc<PermissionService>,
    channels: Channels,
    admin: Id,
    member: Id,
    _dir: tempfile::TempDir,
}

async fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let (admin, member) = {
        let mut conn = pool.write().await;
        let admin = insert_user(&mut conn, "admin").await;
        let member = insert_user(&mut conn, "member").await;
        roles::assign(&mut conn, admin, ADMIN_ROLE_ID)
            .await
            .unwrap();
        (admin, member)
    };
    let perms = Arc::new(PermissionService::open(Arc::clone(&pool)).await.unwrap());
    let channels = Channels::new(Arc::clone(&perms));
    World {
        pool,
        perms,
        channels,
        admin,
        member,
        _dir: dir,
    }
}

async fn insert_user(conn: &mut sqlx::SqliteConnection, username: &str) -> Id {
    let id = Id::generate();
    users::insert(
        conn,
        NewUser {
            id,
            username: username.to_owned(),
            display_name: username.to_owned(),
            password_hash: "hash".into(),
            kind: UserKind::User,
        },
    )
    .await
    .unwrap();
    id
}

fn text_channel(name: &str, parent: Option<Id>, position: i32) -> v1::CreateChannel {
    v1::CreateChannel {
        kind: i32::from(v1::ChannelKind::Text),
        name: name.to_owned(),
        parent_id: parent.map(id_to_bytes),
        position,
        topic: None,
        voice: None,
    }
}

fn invalid_creates() -> Vec<(&'static str, v1::CreateChannel, &'static str)> {
    let mut nested = text_channel("nested", None, 0);
    nested.kind = i32::from(v1::ChannelKind::Category);
    nested.parent_id = Some(id_to_bytes(Id::generate()));
    let mut voiced = text_channel("talk", None, 0);
    voiced.voice = Some(v1::VoiceSettings {
        profile: i32::from(v1::VoiceProfile::Voice),
        bitrate: None,
        user_limit: None,
    });
    let mut bitrate = text_channel("fast", None, 0);
    bitrate.kind = i32::from(v1::ChannelKind::Voice);
    bitrate.voice = Some(v1::VoiceSettings {
        profile: i32::from(v1::VoiceProfile::Voice),
        bitrate: Some(1),
        user_limit: None,
    });
    let mut limit = text_channel("crowd", None, 0);
    limit.kind = i32::from(v1::ChannelKind::Voice);
    limit.voice = Some(v1::VoiceSettings {
        profile: i32::from(v1::VoiceProfile::LowLatency),
        bitrate: Some(
            VoiceProfile::LowLatency
                .encoder_params()
                .default_bitrate_bps,
        ),
        user_limit: Some(100),
    });
    vec![
        ("empty name", text_channel("", None, 0), "invalid"),
        ("nested category", nested, "invalid"),
        ("voice settings on text", voiced, "invalid"),
        ("bitrate out of range", bitrate, "invalid"),
        ("user limit above 99", limit, "invalid"),
        (
            "direct message kind",
            {
                let mut command = text_channel("dm", None, 0);
                command.kind = i32::from(v1::ChannelKind::Dm);
                command
            },
            "invalid",
        ),
    ]
}

fn class(result: Result<handlers::CommandReply, handlers::Error>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(handlers::Error::Forbidden(_)) => "forbidden",
        Err(handlers::Error::NotFound(_)) => "missing",
        Err(handlers::Error::Invalid(_)) => "invalid",
        Err(error) => panic!("{error}"),
    }
}

fn find<'a>(snap: &'a v1::Snapshot, name: &str, kind: v1::ChannelKind) -> &'a v1::Channel {
    snap.channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(kind))
        .unwrap_or_else(|| panic!("missing {name}"))
}

fn id_of(channel: &v1::Channel) -> Id {
    id_from_bytes(&channel.id).unwrap()
}

trait ChannelEvent {
    fn channel_created(&self) -> &v1::Channel;
}

impl ChannelEvent for server_envelope::Msg {
    fn channel_created(&self) -> &v1::Channel {
        let server_envelope::Msg::ChannelCreated(message) = self else {
            panic!("expected ChannelCreated");
        };
        message.channel.as_ref().unwrap()
    }
}
