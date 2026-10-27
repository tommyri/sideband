//! `Welcome` snapshot and the visibility filter for channel events.
//!
//! Presence is offline until T-024. Voice states and tracks are empty until
//! T-025. A delete event is not visible after the channel is gone, so the
//! actor's own command reply is sent without this filter; other sessions
//! must use [`delivers`].

use std::collections::HashMap;

use ss_core::{ChannelKind, Id, Permissions, UserKind, VoiceProfile};
use ss_proto::{
    v1::{self, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};

use crate::{
    db::{self, EVERYONE_ROLE_ID, NewChannel, channels, read_states, roles, users},
    perms::PermissionService,
};

/// Builds the snapshot for `user`, seeding the default tree when the guild
/// has never had a channel.
pub async fn build(perms: &PermissionService, user: Id) -> Result<v1::Snapshot, db::Error> {
    ensure_defaults(perms, user).await?;
    perms.reload().await?;
    let mut conn = perms.pool().read().await?;
    let listed = channels::list(&mut conn).await?;
    let mut members_of: HashMap<Id, Vec<Id>> = HashMap::new();
    for (channel_id, user_id) in channels::memberships(&mut conn).await? {
        members_of.entry(channel_id).or_default().push(user_id);
    }
    let visible = perms.visible_channels(user);
    let mut tree = Vec::new();
    let mut direct_messages = Vec::new();
    for channel in &listed {
        if !visible.contains(&channel.id) {
            continue;
        }
        let empty: &[Id] = &[];
        let members = members_of.get(&channel.id).map_or(empty, Vec::as_slice);
        let wire = wire_channel(channel, members)?;
        if channel.kind.is_direct() {
            direct_messages.push(wire);
        } else {
            tree.push(wire);
        }
    }
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
            // T-024 owns real presence. Until then everyone is offline.
            presence: Some(v1::Presence {
                status: i32::from(v1::PresenceStatus::Offline),
                activity: String::new(),
            }),
        });
    }
    let mut reads = Vec::new();
    for state in read_states::list_for_user(&mut conn, user).await? {
        if !visible.contains(&state.channel_id) {
            continue;
        }
        reads.push(v1::ReadState {
            channel_id: id_to_bytes(state.channel_id),
            last_read_message_id: id_to_bytes(state.last_read_message_id),
            mention_count: 0,
        });
    }
    Ok(v1::Snapshot {
        channels: tree,
        roles: wire_roles,
        members,
        voice_states: Vec::new(),
        tracks: Vec::new(),
        read_states: reads,
        direct_messages,
    })
}

/// Whether `user` should be sent `event`.
///
/// Non-channel events pass. A channel event passes when `user` can view that
/// channel. A soft-deleted channel is not viewable, so a delete is not
/// fanned out from here.
#[must_use]
pub fn delivers(perms: &PermissionService, user: Id, event: &server_envelope::Msg) -> bool {
    let Some(channel_id) = event_channel(event) else {
        return true;
    };
    perms
        .check(user, Some(channel_id), Permissions::VIEW_CHANNEL)
        .is_ok()
}

/// Wire form of a channel. `member_ids` is empty for guild channels.
pub(crate) fn wire_channel(
    channel: &db::Channel,
    member_ids: &[Id],
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
    })
}

async fn ensure_defaults(perms: &PermissionService, creator: Id) -> Result<(), db::Error> {
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

fn event_channel(event: &server_envelope::Msg) -> Option<Id> {
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
