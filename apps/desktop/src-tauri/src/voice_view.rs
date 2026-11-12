use crate::types::ApiError;
use serde::{Deserialize, Serialize};
use ss_client_core::{
    State,
    authority::{self, Authority},
    storage::Storage,
    unread,
};
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags serialized for the webview"
)]
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
    /// A bot account (`User.kind`), shown with a BOT tag.
    pub bot: bool,
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags serialized for the webview"
)]
pub struct ServerView {
    pub can_moderate_members: bool,
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
    /// The server has older conversations than `direct_messages`
    /// (`Snapshot.direct_messages_next`); `load_dms` loads the next page.
    pub more_direct_messages: bool,
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
/// Playback gain as the 0–200 % volume slider value.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the gain is clamped to 0..=2, so the percentage fits u16"
)]
fn percent(gain: f32) -> u16 {
    (gain.clamp(0.0, 2.0) * 100.0).round() as u16
}

fn channel_view(
    state: &State,
    channel: &ss_client_core::v1::Channel,
) -> Result<ChannelView, ApiError> {
    let key = id(&channel.id)?;
    let perms = state.permissions(key).unwrap_or_else(Permissions::empty);
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
        unread: unread::is_unread(state, key),
        mentions: state.read_states.get(&key).map_or(0, |r| r.mention_count),
    })
}

fn member_view(
    state: &State,
    server: &ServerId,
    storage: &dyn Storage,
    authority: &Authority,
    key: Id,
    member: &ss_client_core::v1::Member,
    user: &ss_client_core::v1::User,
) -> Result<MemberView, ApiError> {
    let roles = authority::roles(state, key);
    let top = roles.iter().max_by_key(|r| r.position);
    let rights = authority.rights_over(state, key);
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
            .map(|c| format!("#{:06x}", c & 0x00ff_ffff)),
        role_position: top.map_or(0, |r| r.position),
        presence: match presence.map(|p| p.status) {
            Some(2) => "online",
            Some(3) => "idle",
            Some(4) => "do not disturb",
            _ => "offline",
        }
        .into(),
        activity: presence.map_or_else(String::new, |p| p.activity.clone()),
        can_kick: rights.kick,
        can_ban: rights.ban,
        can_mute: rights.mute,
        can_move: rights.move_members,
        volume: percent(gain),
        local_muted: storage
            .user_muted(server, key)
            .map_err(super::commands::api_error)?,
        bot: user.kind == i32::from(ss_client_core::v1::UserKind::Bot),
    })
}

pub fn snapshot(
    state: &State,
    server: &ServerId,
    storage: &dyn Storage,
    active_voice: Option<ActiveVoiceView>,
) -> Result<ServerView, ApiError> {
    let authority =
        Authority::own(state).ok_or_else(|| ApiError::new("disconnected", "Not logged in."))?;
    let allowed = |permission| authority.allows(permission);
    let channels = state
        .channel_tree()
        .into_iter()
        .map(|channel| channel_view(state, channel))
        .collect::<Result<Vec<_>, ApiError>>()?;
    let members = state
        .members
        .iter()
        .filter_map(|(key, member)| member.user.as_ref().map(|user| (*key, member, user)))
        .map(|(key, member, user)| {
            member_view(state, server, storage, &authority, key, member, user)
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
        can_moderate_members: allowed(Permissions::KICK_MEMBERS)
            || allowed(Permissions::BAN_MEMBERS),
        can_manage_server: allowed(Permissions::MANAGE_SERVER),
        can_manage_roles: allowed(Permissions::MANAGE_ROLES),
        can_manage_channels: authority.can_manage_channels(),
        can_manage_webhooks: allowed(Permissions::MANAGE_SERVER),
        can_view_audit_log: allowed(Permissions::VIEW_AUDIT_LOG),
        can_create_invites: allowed(Permissions::CREATE_INVITES),
        can_manage_invites: allowed(Permissions::MANAGE_SERVER),
        own_user_id: authority.user().to_string(),
        channels,
        direct_messages: crate::dm::views(state)?,
        more_direct_messages: state.direct_messages_next.is_some(),
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
                color: Some(0x12_34_56),
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
        assert!(!view.more_direct_messages);
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
        state.direct_messages_next = Some(bytes::Bytes::from_static(b"cursor"));
        let view = snapshot(
            &state,
            &ServerId::new("localhost", 5781).unwrap(),
            &MemoryStorage::default(),
            None,
        )
        .unwrap();
        assert!(view.more_direct_messages, "older DMs are offered");
    }
}
