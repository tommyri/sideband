//! `Welcome` snapshot and the visibility filter for channel events.
//!
//! [`build`] reports every member offline. The accept loop uses
//! [`build_with`] so live connections show the registry's presence. The voice
//! service fills voice states and tracks separately. The connection owner
//! diffs [`channel_view`] for channel/permission events, including revocation;
//! [`delivers`] filters other fan-out events against current visibility.
//!
//! The snapshot is read in one read transaction. [`build_live`] subscribes to
//! the event bus at that transaction's read point, so a new message is either
//! in the read states' `mention_count` or arrives live, never both.

use std::collections::{HashMap, HashSet};

use ss_core::{ChannelKind, Id, Permissions, UserKind, VoiceProfile};
use ss_proto::{
    v1::{self, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};

use sqlx::Connection as _;
use tokio::sync::broadcast;

use crate::{
    db::{self, EVERYONE_ROLE_ID, NewChannel, channels, messages, read_states, roles, users},
    events::{EventBus, ServerEvent},
    limits,
    perms::PermissionService,
};

/// Builds the snapshot for `user`, seeding the default tree when the guild
/// has never had a channel. Every member is offline.
pub async fn build(perms: &PermissionService, user: Id) -> Result<v1::Snapshot, db::Error> {
    build_with(perms, user, offline_presence).await
}

/// Like [`build`], with `presence` supplying each member's status.
///
/// DMs are the [`limits::WELCOME_DMS`] most recently active; when there are
/// more, `direct_messages_next` is set and `ListDms` loads the rest. Read
/// states cover guild channels and the DMs included.
pub async fn build_with(
    perms: &PermissionService,
    user: Id,
    presence: impl Fn(Id) -> v1::Presence,
) -> Result<v1::Snapshot, db::Error> {
    // Nothing publishes on a bus of its own; its subscription is dropped.
    build_live(perms, &EventBus::new(), user, presence)
        .await
        .map(|(snapshot, _)| snapshot)
}

/// Like [`build_with`], and subscribes to `bus` at the snapshot's read point.
///
/// The receiver gets every message the read states did not count and none
/// they did: it subscribes while no message is between its commit and its
/// publish ([`EventBus::quiet`]), in the same instant the read transaction
/// fixes what it sees. The rest of the snapshot is that transaction and the
/// permission cache, which commands update before they publish, so no other
/// event published before the subscription is missing from it.
pub async fn build_live(
    perms: &PermissionService,
    bus: &EventBus,
    user: Id,
    presence: impl Fn(Id) -> v1::Presence,
) -> Result<(v1::Snapshot, broadcast::Receiver<ServerEvent>), db::Error> {
    ensure_defaults(perms, user).await?;
    perms.reload().await?;
    let mut pooled = perms.pool().read().await?;
    let mut conn = pooled.begin().await?;
    let subscription = {
        let _quiet = bus.quiet().await;
        fix_read_point(&mut conn).await?;
        bus.subscribe()
    };
    let visible = perms.visible_channels(user);
    let mut tree = Vec::new();
    for channel in channels::list_guild(&mut conn).await? {
        if !visible.contains(&channel.id) {
            continue;
        }
        let mut wire = wire_channel(&channel, &[], None)?;
        wire.effective_permissions = perms
            .channel_permissions(user, channel.id)
            .ok()
            .map(|p| p.bits());
        tree.push(wire);
    }
    let page = direct_page(perms, &mut conn, user, None, limits::WELCOME_DMS).await?;
    let role_rows = roles::list(&mut conn).await?;
    let mut position_of: HashMap<Id, i64> = HashMap::new();
    let mut wire_roles = Vec::with_capacity(role_rows.len());
    for role in &role_rows {
        position_of.insert(role.id, role.position);
        wire_roles.push(wire_role(role)?);
    }
    let grants = roles::grants(&mut conn).await?;
    let mut roles_of: HashMap<Id, Vec<Id>> = HashMap::new();
    for (user_id, role_id) in grants {
        roles_of.entry(user_id).or_default().push(role_id);
    }
    let mut members = Vec::new();
    for account in users::list(&mut conn).await? {
        let mut role_ids = roles_of.remove(&account.id).unwrap_or_default();
        if !role_ids.contains(&EVERYONE_ROLE_ID) {
            role_ids.push(EVERYONE_ROLE_ID);
        }
        role_ids.sort_by_key(|id| (position_of.get(id).copied().unwrap_or(0), *id));
        members.push(v1::Member {
            user: Some(wire_user(&account)),
            roles: role_ids.into_iter().map(id_to_bytes).collect(),
            presence: Some(presence(account.id)),
        });
    }
    let guild: HashSet<Id> = tree
        .iter()
        .filter_map(|channel| id_from_bytes(&channel.id).ok())
        .collect();
    let mut reads = read_states_for(&mut conn, user, &guild).await?;
    reads.extend(page.read_states);
    conn.commit().await?;
    let snapshot = v1::Snapshot {
        channels: tree,
        roles: wire_roles,
        members,
        voice_states: Vec::new(),
        tracks: Vec::new(),
        read_states: reads,
        direct_messages: page.channels,
        direct_messages_next: page.next,
    };
    Ok((snapshot, subscription))
}

/// Fixes what the open read transaction sees. A deferred SQLite transaction
/// takes its snapshot at its first read, not at `BEGIN`.
async fn fix_read_point(conn: &mut sqlx::SqliteConnection) -> Result<(), db::Error> {
    sqlx::query("SELECT 1 FROM messages LIMIT 1")
        .fetch_optional(conn)
        .await?;
    Ok(())
}

/// One page of a member's DMs, most recently active first.
pub(crate) struct DirectPage {
    /// Wire channels with members, newest message and permissions.
    pub channels: Vec<v1::Channel>,
    /// The member's read cursors for those channels.
    pub read_states: Vec<v1::ReadState>,
    /// Cursor for the next page, when there is one.
    pub next: Option<bytes::Bytes>,
}

/// Up to `limit` of `user`'s DMs older than the activity key `before`.
/// Members come from the permission snapshot, which DM handlers keep current.
pub(crate) async fn direct_page(
    perms: &PermissionService,
    conn: &mut sqlx::SqliteConnection,
    user: Id,
    before: Option<Id>,
    limit: usize,
) -> Result<DirectPage, db::Error> {
    let fetch = u32::try_from(limit.saturating_add(1)).unwrap_or(u32::MAX);
    let mut rows = channels::direct_page(conn, user, before, fetch).await?;
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next = if more {
        rows.last()
            .map(|(channel, last)| id_to_bytes(channels::activity(channel.id, *last)))
    } else {
        None
    };
    let mut wires = Vec::with_capacity(rows.len());
    let mut ids = HashSet::with_capacity(rows.len());
    for (channel, last) in &rows {
        wires.push(direct_wire(perms, user, channel, *last)?);
        ids.insert(channel.id);
    }
    let read_states = read_states_for(conn, user, &ids).await?;
    Ok(DirectPage {
        channels: wires,
        read_states,
        next,
    })
}

/// Wire form of one DM for `user`, members from the permission snapshot.
pub(crate) fn direct_wire(
    perms: &PermissionService,
    user: Id,
    channel: &db::Channel,
    last_message_id: Option<Id>,
) -> Result<v1::Channel, db::Error> {
    let members = perms.direct_members(channel.id).unwrap_or_default();
    let mut wire = wire_channel(channel, &members, last_message_id)?;
    wire.effective_permissions = perms
        .channel_permissions(user, channel.id)
        .ok()
        .map(|p| p.bits());
    Ok(wire)
}

/// Loads one live DM `user` belongs to, for announcing a conversation the
/// session was never sent. `None` when it is gone or `user` left it.
pub(crate) async fn direct_channel(
    perms: &PermissionService,
    user: Id,
    channel: Id,
) -> Result<Option<v1::Channel>, db::Error> {
    if !perms
        .direct_members(channel)
        .is_some_and(|members| members.contains(&user))
    {
        return Ok(None);
    }
    let mut conn = perms.pool().read().await?;
    let Some(row) = channels::get(&mut conn, channel).await? else {
        return Ok(None);
    };
    if row.deleted_at.is_some() {
        return Ok(None);
    }
    let last = messages::latest_id(&mut conn, channel).await?;
    direct_wire(perms, user, &row, last).map(Some)
}

/// Drops all but the `keep` most recently active DMs from `snapshot`, with
/// their read states, and points `direct_messages_next` past the last one
/// kept. Used when a `Welcome` would not fit in one frame.
pub(crate) fn keep_direct(snapshot: &mut v1::Snapshot, keep: usize) {
    if snapshot.direct_messages.len() <= keep {
        return;
    }
    let dropped: HashSet<bytes::Bytes> = snapshot
        .direct_messages
        .drain(keep..)
        .map(|channel| channel.id)
        .collect();
    snapshot
        .read_states
        .retain(|state| !dropped.contains(&state.channel_id));
    // With nothing kept the cursor is the largest key, which pages from the top.
    snapshot.direct_messages_next = Some(snapshot.direct_messages.last().map_or_else(
        || bytes::Bytes::from_static(&[0xff; 16]),
        |channel| {
            channel
                .last_message_id
                .clone()
                .unwrap_or_else(|| channel.id.clone())
        },
    ));
}

async fn read_states_for(
    conn: &mut sqlx::SqliteConnection,
    user: Id,
    channels: &HashSet<Id>,
) -> Result<Vec<v1::ReadState>, db::Error> {
    let mut reads = Vec::new();
    if channels.is_empty() {
        return Ok(reads);
    }
    for state in read_states::list_for_user(conn, user).await? {
        if !channels.contains(&state.channel_id) {
            continue;
        }
        reads.push(v1::ReadState {
            channel_id: id_to_bytes(state.channel_id),
            last_read_message_id: id_to_bytes(state.last_read_message_id),
            mention_count: messages::unread_mentions(
                conn,
                state.channel_id,
                user,
                state.last_read_message_id,
            )
            .await?,
        });
    }
    Ok(reads)
}

/// Offline presence used when the caller has no connection registry.
#[must_use]
pub fn offline_presence(_user: Id) -> v1::Presence {
    v1::Presence {
        status: i32::from(v1::PresenceStatus::Offline),
        activity: String::new(),
    }
}

/// Whether `user` should be sent `event`.
///
/// Non-channel events pass. A channel event passes when `user` can view that
/// channel. A soft-deleted channel is not viewable, so a delete is not
/// fanned out from here.
#[must_use]
pub fn delivers(perms: &PermissionService, user: Id, event: &server_envelope::Msg) -> bool {
    if matches!(
        event,
        server_envelope::Msg::InviteCreated(_) | server_envelope::Msg::InviteRevoked(_)
    ) {
        return perms.check(user, None, Permissions::MANAGE_SERVER).is_ok();
    }
    let Some(channel_id) = event_channel(event) else {
        return true;
    };
    perms
        .check(user, Some(channel_id), Permissions::VIEW_CHANNEL)
        .is_ok()
}

/// Wire form of a channel. `member_ids` is empty for guild channels.
/// `last_message_id` is the newest visible DM message, when there is one.
pub(crate) fn wire_channel(
    channel: &db::Channel,
    member_ids: &[Id],
    last_message_id: Option<Id>,
) -> Result<v1::Channel, db::Error> {
    Ok(v1::Channel {
        id: id_to_bytes(channel.id),
        kind: i32::from(proto_kind(channel.kind)),
        name: channel.name.clone(),
        topic: channel.topic.clone(),
        parent_id: channel.parent_id.map(id_to_bytes),
        position: i32::try_from(channel.position)
            .map_err(|_| db::Error::Corrupt("channel position"))?,
        voice: wire_voice(channel)?,
        created_by: Some(id_to_bytes(channel.created_by)),
        created_at: channel.created_at.as_millis(),
        member_ids: member_ids.iter().copied().map(id_to_bytes).collect(),
        last_message_id: last_message_id.map(id_to_bytes),
        effective_permissions: None,
    })
}

/// Seeds the default tree once. Checked on a read connection first, so an
/// ordinary `Hello` never waits for the single writer.
async fn ensure_defaults(perms: &PermissionService, creator: Id) -> Result<(), db::Error> {
    {
        let mut conn = perms.pool().read().await?;
        if channels::guild_count(&mut conn).await? > 0 {
            return Ok(());
        }
    }
    let mut guard = perms.pool().write().await;
    if channels::guild_count(&mut guard).await? > 0 {
        return Ok(());
    }
    let category = Id::generate();
    let text = Id::generate();
    let voice = Id::generate();
    channels::insert(
        &mut guard,
        guild_channel(category, ChannelKind::Category, "General", None, 0, creator),
    )
    .await?;
    channels::insert(
        &mut guard,
        guild_channel(
            text,
            ChannelKind::Text,
            "general",
            Some(category),
            0,
            creator,
        ),
    )
    .await?;
    channels::insert(
        &mut guard,
        NewChannel {
            voice_profile: Some(VoiceProfile::Voice),
            ..guild_channel(
                voice,
                ChannelKind::Voice,
                "General",
                Some(category),
                1,
                creator,
            )
        },
    )
    .await?;
    Ok(())
}

fn guild_channel(
    id: Id,
    kind: ChannelKind,
    name: &str,
    parent_id: Option<Id>,
    position: i64,
    created_by: Id,
) -> NewChannel {
    NewChannel {
        id,
        kind,
        name: name.to_owned(),
        topic: None,
        parent_id,
        position,
        voice_profile: None,
        voice_bitrate: None,
        user_limit: None,
        created_by,
    }
}

pub(crate) fn event_channel(event: &server_envelope::Msg) -> Option<Id> {
    let bytes = match event {
        server_envelope::Msg::ChannelCreated(message) => {
            message.channel.as_ref().map(|channel| &channel.id)
        }
        server_envelope::Msg::ChannelUpdated(message) => {
            message.channel.as_ref().map(|channel| &channel.id)
        }
        server_envelope::Msg::ChannelDeleted(message) => {
            message.channel.as_ref().map(|channel| &channel.id)
        }
        server_envelope::Msg::MessageCreated(message) => {
            message.message.as_ref().map(|message| &message.channel_id)
        }
        server_envelope::Msg::MessageUpdated(message) => {
            message.message.as_ref().map(|message| &message.channel_id)
        }
        server_envelope::Msg::MessageDeleted(message) => {
            message.message.as_ref().map(|message| &message.channel_id)
        }
        server_envelope::Msg::TypingStarted(message) => Some(&message.channel_id),
        // Track and voice-state announcements name their channel too, so a
        // member who cannot view a voice channel never learns who is in it or
        // what they publish. `TrackRemoved` carries only an opaque track id and
        // is harmless to a session that never saw the matching `TrackAdded`.
        server_envelope::Msg::TrackAdded(message) => Some(&message.channel_id),
        server_envelope::Msg::VoiceStateUpdated(message) => message.channel_id.as_ref(),
        _ => None,
    }?;
    id_from_bytes(bytes).ok()
}

fn proto_kind(kind: ChannelKind) -> v1::ChannelKind {
    match kind {
        ChannelKind::Category => v1::ChannelKind::Category,
        ChannelKind::Text => v1::ChannelKind::Text,
        ChannelKind::Voice => v1::ChannelKind::Voice,
        ChannelKind::Dm => v1::ChannelKind::Dm,
        ChannelKind::GroupDm => v1::ChannelKind::GroupDm,
        _ => v1::ChannelKind::Unspecified,
    }
}

fn wire_voice(channel: &db::Channel) -> Result<Option<v1::VoiceSettings>, db::Error> {
    if channel.kind != ChannelKind::Voice {
        return Ok(None);
    }
    let profile = channel.voice_profile.unwrap_or(VoiceProfile::Voice);
    Ok(Some(v1::VoiceSettings {
        profile: i32::from(proto_profile(profile)),
        bitrate: optional_u32(channel.voice_bitrate, "voice bitrate")?,
        user_limit: optional_u32(channel.user_limit, "user limit")?,
    }))
}

fn optional_u32(value: Option<i64>, what: &'static str) -> Result<Option<u32>, db::Error> {
    value
        .map(|value| u32::try_from(value).map_err(|_| db::Error::Corrupt(what)))
        .transpose()
}

fn proto_profile(profile: VoiceProfile) -> v1::VoiceProfile {
    match profile {
        VoiceProfile::Voice => v1::VoiceProfile::Voice,
        VoiceProfile::Music => v1::VoiceProfile::Music,
        VoiceProfile::LowLatency => v1::VoiceProfile::LowLatency,
        _ => v1::VoiceProfile::Unspecified,
    }
}

fn wire_role(role: &db::Role) -> Result<v1::Role, db::Error> {
    Ok(v1::Role {
        id: id_to_bytes(role.id),
        name: role.name.clone(),
        color: optional_u32(role.color, "role color")?,
        permissions: role.permissions.bits(),
        position: i32::try_from(role.position).map_err(|_| db::Error::Corrupt("role position"))?,
        is_default: role.is_default,
    })
}

fn wire_user(user: &db::User) -> v1::User {
    let kind = match user.kind {
        UserKind::Bot => v1::UserKind::Bot,
        _ => v1::UserKind::User,
    };
    v1::User {
        id: id_to_bytes(user.id),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        avatar_attachment_id: user.avatar_attachment_id.map(id_to_bytes),
        kind: i32::from(kind),
    }
}

/// Current personalized view of the guild channels. Used after role,
/// override and guild channel changes so connected clients gain newly visible
/// channels and purge revoked ones. DMs are not included: roles never apply
/// to them, and the connection tracks them from DM events.
pub(crate) async fn channel_view(
    perms: &PermissionService,
    user: Id,
) -> Result<std::collections::BTreeMap<Id, v1::Channel>, db::Error> {
    let mut conn = perms.pool().read().await?;
    let listed = channels::list_guild(&mut conn).await?;
    let mut view = std::collections::BTreeMap::new();
    for channel in listed {
        let Ok(permissions) = perms.channel_permissions(user, channel.id) else {
            continue;
        };
        if !permissions.contains(Permissions::VIEW_CHANNEL) {
            continue;
        }
        let mut wire = wire_channel(&channel, &[], None)?;
        wire.effective_permissions = Some(permissions.bits());
        view.insert(channel.id, wire);
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use ss_core::Id;
    use ss_proto::{
        v1::{self, server_envelope},
        validate::{id_from_bytes, id_to_bytes},
    };
    use tokio::sync::broadcast::error::TryRecvError;

    use super::{build_live, offline_presence};
    use crate::{
        db::{self, NewMessage, messages, read_states, testing},
        events::EventBus,
        perms::PermissionService,
    };

    /// Stores message `n` mentioning `owner`. The caller publishes it.
    async fn mention(pool: &db::Pool, n: u8, channel: Id, author: Id, owner: Id) -> Id {
        let mut conn = pool.write().await;
        messages::insert(
            &mut conn,
            NewMessage {
                id: testing::id(n),
                channel_id: channel,
                author_id: Some(author),
                webhook_id: None,
                webhook_name: None,
                content: format!("<@{owner}> {n}"),
                reply_to: None,
                mentions: vec![owner],
                mentions_everyone: false,
            },
        )
        .await
        .unwrap()
        .id
    }

    fn created(id: Id, channel: Id) -> server_envelope::Msg {
        server_envelope::Msg::MessageCreated(v1::MessageCreated {
            message: Some(v1::Message {
                id: id_to_bytes(id),
                channel_id: id_to_bytes(channel),
                ..v1::Message::default()
            }),
            nonce: None,
        })
    }

    fn created_id(msg: &server_envelope::Msg) -> Option<Id> {
        match msg {
            server_envelope::Msg::MessageCreated(event) => event
                .message
                .as_ref()
                .and_then(|message| id_from_bytes(&message.id).ok()),
            _ => None,
        }
    }

    /// A message between its commit and its publish while a session signs
    /// in. Before T-113 the session subscribed first and read later, so this
    /// message was in `mention_count` and delivered live: counted twice.
    #[tokio::test]
    async fn a_mention_committed_during_sign_in_is_counted_or_delivered_not_both() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            db::Pool::open(dir.path().join("snapshot.db"))
                .await
                .unwrap(),
        );
        let (owner, sender, channel) = {
            let mut conn = pool.write().await;
            let owner = testing::user(&mut conn, 1).await.id;
            let sender = testing::user(&mut conn, 2).await.id;
            let channel = testing::text_channel(&mut conn, 10, owner).await.id;
            let read = testing::message(&mut conn, 20, channel, sender).await.id;
            read_states::upsert(&mut conn, owner, channel, read)
                .await
                .unwrap();
            (owner, sender, channel)
        };
        let perms = Arc::new(PermissionService::open(Arc::clone(&pool)).await.unwrap());
        let bus = EventBus::new();

        let before = {
            let _hold = bus.hold().await;
            let id = mention(&pool, 21, channel, sender, owner).await;
            bus.publish(None, created(id, channel));
            id
        };
        // A sender has committed and not yet published.
        let hold = bus.hold().await;
        let during = mention(&pool, 22, channel, sender, owner).await;
        let sign_in = tokio::spawn({
            let perms = Arc::clone(&perms);
            let bus = bus.clone();
            async move { build_live(&perms, &bus, owner, offline_presence).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !sign_in.is_finished(),
            "subscribed inside the sender's hold"
        );
        bus.publish(None, created(during, channel));
        drop(hold);
        let (snapshot, mut subscription) = sign_in.await.unwrap().unwrap();
        let after = {
            let _hold = bus.hold().await;
            let id = mention(&pool, 23, channel, sender, owner).await;
            bus.publish(None, created(id, channel));
            id
        };

        let counted = snapshot
            .read_states
            .iter()
            .find(|state| state.channel_id == id_to_bytes(channel))
            .map(|state| state.mention_count);
        assert_eq!(
            counted,
            Some(2),
            "{before} and {during} are in the snapshot"
        );
        let delivered = created_id(&subscription.try_recv().unwrap().msg);
        assert_eq!(delivered, Some(after));
        assert!(matches!(subscription.try_recv(), Err(TryRecvError::Empty)));
    }
}
