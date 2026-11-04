use crate::types::ApiError;
use serde::{Deserialize, Serialize};
use ss_client_core::{State, storage::Storage};
use ss_core::{Id, Permissions};
use ss_net::pins::ServerId;
use ts_rs::TS;

#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ChannelView {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub kind: i32,
    pub profile: String,
    pub user_limit: Option<u32>,
    pub can_join: bool,
    pub can_speak: bool,
    pub unread: bool,
    pub mentions: u32,
}
#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct MemberView {
    pub id: String,
    pub username: String,
    pub display_name: String,
    pub role_name: String,
    pub role_color: Option<String>,
    pub role_position: i32,
    pub presence: String,
    pub activity: String,
    pub can_kick: bool,
    pub can_ban: bool,
    pub can_mute: bool,
    pub can_move: bool,
    pub volume: u16,
    pub local_muted: bool,
}
#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ParticipantView {
    pub user_id: String,
    pub connection_id: String,
    pub channel_id: String,
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
}
#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ActiveVoiceView {
    pub server_id: String,
    pub channel_id: String,
    pub muted: bool,
    pub deafened: bool,
}
#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ServerView {
    pub can_manage_server: bool,
    pub can_manage_roles: bool,
    pub can_manage_channels: bool,
    pub can_manage_webhooks: bool,
    pub can_view_audit_log: bool,
    pub can_create_invites: bool,
    pub can_manage_invites: bool,
    pub own_user_id: String,
    pub channels: Vec<ChannelView>,
    pub direct_messages: Vec<crate::dm::DmView>,
    pub members: Vec<MemberView>,
    pub participants: Vec<ParticipantView>,
    pub active_voice: Option<ActiveVoiceView>,
}
#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct SpeakingView {
    pub server_id: String,
    pub user_ids: Vec<String>,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct MemberAction {
    pub server_id: String,
    pub user_id: String,
    pub action: String,
    pub channel_id: Option<String>,
    pub enabled: Option<bool>,
    pub volume: Option<u16>,
    pub reason: Option<String>,
}

fn id(bytes: &[u8]) -> Result<Id, ApiError> {
    Id::try_from(bytes)
        .map_err(|_| ApiError::new("invalid_id", "Invalid server entity identifier."))
}
fn roles(state: &State, user: Id) -> Vec<&ss_client_core::v1::Role> {
    state
        .roles
        .values()
        .filter(|r| {
            r.is_default
                || state
                    .members
                    .get(&user)
                    .is_some_and(|m| m.roles.contains(&r.id))
        })
        .collect()
}

pub fn snapshot(
    state: &State,
    server: &ServerId,
    storage: &dyn Storage,
    active_voice: Option<ActiveVoiceView>,
) -> Result<ServerView, ApiError> {
    let own = state
        .own_user
        .as_ref()
        .ok_or_else(|| ApiError::new("disconnected", "Not logged in."))?;
    let own_id = id(&own.id)?;
    let own_roles = roles(state, own_id);
    let base = own_roles.iter().fold(Permissions::empty(), |p, r| {
        p | Permissions::from_bits_truncate(r.permissions)
    });
    let allowed =
        |permission| base.contains(Permissions::ADMINISTRATOR) || base.contains(permission);
    let highest = own_roles
        .iter()
        .map(|r| r.position)
        .max()
        .unwrap_or(i32::MIN);
    let channels = state
        .channel_tree()
        .into_iter()
        .map(|channel| {
            let key = id(&channel.id)?;
            let perms = state.permissions(key).unwrap_or_else(Permissions::empty);
            let read = state.read_states.get(&key);
            let newest_cached = state
                .messages
                .get(&key)
                .and_then(|m| m.ordered().last().map(|m| m.id.clone()));
            let newest = newest_cached.as_ref().or(channel.last_message_id.as_ref());
            Ok(ChannelView {
                id: key.to_string(),
                parent_id: channel
                    .parent_id
                    .as_ref()
                    .map(|p| id(p).map(|p| p.to_string()))
                    .transpose()?,
                name: channel.name.clone(),
                kind: channel.kind,
                profile: match channel.voice.as_ref().map(|v| v.profile) {
                    Some(2) => "music",
                    Some(3) => "low latency",
                    _ => "voice",
                }
                .into(),
                user_limit: channel.voice.as_ref().and_then(|v| v.user_limit),
                can_join: perms.contains(Permissions::CONNECT | Permissions::VIEW_CHANNEL),
                can_speak: perms.contains(Permissions::SPEAK),
                unread: newest
                    .is_some_and(|last| read.is_none_or(|r| r.last_read_message_id < *last)),
                mentions: read.map_or(0, |r| r.mention_count),
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let members = state
        .members
        .iter()
        .filter_map(|(key, member)| member.user.as_ref().map(|user| (key, member, user)))
        .map(|(&key, member, user)| {
            let roles = roles(state, key);
            let top = roles.iter().max_by_key(|r| r.position);
            let can_act = key != own_id
                && (base.contains(Permissions::ADMINISTRATOR)
                    || highest > top.map_or(i32::MIN, |r| r.position));
            let presence = member.presence.as_ref();
            let gain = storage
                .user_volume(server, key)
                .map_err(super::commands::api_error)?
                .unwrap_or(1.0);
            Ok(MemberView {
                id: key.to_string(),
                username: user.username.clone(),
                display_name: user.display_name.clone(),
                role_name: top.map_or_else(|| "Members".into(), |r| r.name.clone()),
                role_color: top
                    .and_then(|r| r.color)
                    .map(|c| format!("#{:06x}", c & 0xffffff)),
                role_position: top.map_or(0, |r| r.position),
                presence: match presence.map(|p| p.status) {
                    Some(2) => "online",
                    Some(3) => "idle",
                    Some(4) => "do not disturb",
                    _ => "offline",
                }
                .into(),
                activity: presence.map_or_else(String::new, |p| p.activity.clone()),
                can_kick: can_act && allowed(Permissions::KICK_MEMBERS),
                can_ban: can_act && allowed(Permissions::BAN_MEMBERS),
                can_mute: can_act && allowed(Permissions::MUTE_MEMBERS),
                can_move: can_act && allowed(Permissions::MOVE_MEMBERS),
                volume: (gain.clamp(0.0, 2.0) * 100.0).round() as u16,
                local_muted: storage
                    .user_muted(server, key)
                    .map_err(super::commands::api_error)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    let participants = state
        .voice_states
        .values()
        .filter_map(|v| v.channel_id.as_ref().map(|channel| (v, channel)))
        .map(|(v, channel)| {
            Ok(ParticipantView {
                user_id: id(&v.user_id)?.to_string(),
                connection_id: id(&v.conn_id)?.to_string(),
                channel_id: id(channel)?.to_string(),
                muted: v.self_mute,
                deafened: v.self_deaf,
                server_muted: v.server_mute,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(ServerView {
        can_manage_server: allowed(Permissions::MANAGE_SERVER),
        can_manage_roles: allowed(Permissions::MANAGE_ROLES),
        can_manage_channels: allowed(Permissions::MANAGE_CHANNELS),
        can_manage_webhooks: allowed(Permissions::MANAGE_SERVER),
        can_view_audit_log: allowed(Permissions::VIEW_AUDIT_LOG),
        can_create_invites: allowed(Permissions::CREATE_INVITES),
        can_manage_invites: allowed(Permissions::MANAGE_SERVER),
        own_user_id: own_id.to_string(),
        channels,
        direct_messages: crate::dm::views(state)?,
        members,
        participants,
        active_voice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_client_core::{storage::MemoryStorage, v1};

    #[test]
    fn snapshots_preserve_usernames_and_fail_closed_without_channel_permissions() {
        let own = Id::generate();
        let other = Id::generate();
        let role = Id::generate();
        let channel = Id::generate();
        let bytes = |id: Id| bytes::Bytes::copy_from_slice(&id.as_bytes());
        let user = |id: Id, username: &str| v1::User {
            id: bytes(id),
            username: username.into(),
            display_name: "Same nickname".into(),
            ..Default::default()
        };
        let mut state = State {
            own_user: Some(user(own, "moderator")),
            ..Default::default()
        };
        state.members.insert(
            own,
            v1::Member {
                user: Some(user(own, "moderator")),
                roles: vec![bytes(role)],
                ..Default::default()
            },
        );
        state.members.insert(
            other,
            v1::Member {
                user: Some(user(other, "real_account")),
                ..Default::default()
            },
        );
        state.roles.insert(
            role,
            v1::Role {
                id: bytes(role),
                name: "Moderators".into(),
                position: 10,
                permissions: (Permissions::MUTE_MEMBERS | Permissions::MOVE_MEMBERS).bits(),
                color: Some(0x123456),
                ..Default::default()
            },
        );
        state.channels.insert(
            channel,
            v1::Channel {
                id: bytes(channel),
                name: "Voice".into(),
                kind: 3,
                ..Default::default()
            },
        );
        let view = snapshot(
            &state,
            &ServerId::new("localhost", 5781).unwrap(),
            &MemoryStorage::default(),
            None,
        )
        .unwrap();
        assert!(!view.channels[0].can_join);
        assert!(!view.channels[0].can_speak);
        let target = view
            .members
            .iter()
            .find(|m| m.id == other.to_string())
            .unwrap();
        assert_eq!(target.username, "real_account");
        assert_eq!(target.display_name, "Same nickname");
        assert!(target.can_mute && target.can_move);
        assert!(!target.can_kick && !target.can_ban);
        state
            .members
            .get_mut(&other)
            .unwrap()
            .roles
            .push(bytes(role));
        let view = snapshot(
            &state,
            &ServerId::new("localhost", 5781).unwrap(),
            &MemoryStorage::default(),
            None,
        )
        .unwrap();
        assert!(
            !view
                .members
                .iter()
                .find(|m| m.id == other.to_string())
                .unwrap()
                .can_mute
        );
    }
}
