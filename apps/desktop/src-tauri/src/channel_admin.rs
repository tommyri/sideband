//! Guild channel administration over the existing authorized control commands.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
    voice_view,
};
use serde::{Deserialize, Serialize};
use ss_client_core::{Client, ConnectionState, v1};
use ss_core::{Id, VoiceProfile};
use tauri::State;
use ts_rs::TS;

#[derive(Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AdminChannel {
    id: String,
    name: String,
    kind: i32,
    topic: String,
    parent_id: Option<String>,
    position: i32,
    profile: i32,
    bitrate: Option<u32>,
    user_limit: Option<u32>,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ChannelProfile {
    id: i32,
    name: String,
    min_bitrate: u32,
    max_bitrate: u32,
    default_bitrate: u32,
}
#[derive(Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ChannelAdminSnapshot {
    channels: Vec<AdminChannel>,
    profiles: Vec<ChannelProfile>,
}
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ChannelFields {
    name: String,
    topic: String,
    profile: i32,
    bitrate: Option<u32>,
    user_limit: Option<u32>,
}
#[derive(Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum ChannelAdminAction {
    Create {
        channel_kind: i32,
        parent_id: Option<String>,
        fields: ChannelFields,
    },
    Edit {
        channel: String,
        fields: ChannelFields,
    },
    Move {
        channel: String,
        parent_id: Option<String>,
        position: i32,
    },
    Delete {
        channel: String,
    },
}
fn invalid(message: &str) -> ApiError {
    ApiError::new("invalid_channel", message)
}
fn parse(value: &str) -> Result<Id, ApiError> {
    value
        .parse()
        .map_err(|_| invalid("Invalid channel identifier"))
}
fn wire(value: &str) -> Result<bytes::Bytes, ApiError> {
    Ok(parse(value)?.as_bytes().to_vec().into())
}
fn display(raw: &[u8]) -> Result<String, ApiError> {
    Id::try_from(raw)
        .map(|id| id.to_string())
        .map_err(|_| invalid("Invalid channel identifier"))
}
fn project(channel: &v1::Channel) -> Result<AdminChannel, ApiError> {
    Ok(AdminChannel {
        id: display(&channel.id)?,
        name: channel.name.clone(),
        kind: channel.kind,
        topic: channel.topic.clone().unwrap_or_default(),
        parent_id: channel.parent_id.as_deref().map(display).transpose()?,
        position: channel.position,
        profile: channel.voice.as_ref().map_or(1, |v| v.profile),
        bitrate: channel.voice.as_ref().and_then(|v| v.bitrate),
        user_limit: channel.voice.as_ref().and_then(|v| v.user_limit),
    })
}
fn profiles() -> Vec<ChannelProfile> {
    [
        (1, VoiceProfile::Voice, "Voice"),
        (2, VoiceProfile::Music, "Music"),
        (3, VoiceProfile::LowLatency, "Low latency"),
    ]
    .into_iter()
    .map(|(id, profile, name)| {
        let params = profile.encoder_params();
        ChannelProfile {
            id,
            name: name.into(),
            min_bitrate: params.min_bitrate_bps,
            max_bitrate: params.max_bitrate_bps,
            default_bitrate: params.default_bitrate_bps,
        }
    })
    .collect()
}
fn authorize(
    client: &Client,
    state: &AppState,
    server: &ss_net::pins::ServerId,
) -> Result<(), ApiError> {
    let store = client.state();
    let snapshot = store.read();
    if snapshot.connection != ConnectionState::Connected {
        return Err(ApiError::new("disconnected", "Server is disconnected"));
    }
    if !voice_view::snapshot(&snapshot, server, state.storage.as_ref(), None)?.can_manage_channels {
        return Err(ApiError::new(
            "forbidden",
            "Manage channels permission is required",
        ));
    }
    Ok(())
}
#[tauri::command]
pub async fn channel_admin_snapshot(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ChannelAdminSnapshot, ApiError> {
    let server = parse_server_id(&server_id)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| invalid("Server is disconnected"))?;
    authorize(client, &state, &server)?;
    let channels = client
        .state()
        .read()
        .channel_tree()
        .into_iter()
        .map(project)
        .collect::<Result<_, _>>()?;
    Ok(ChannelAdminSnapshot {
        channels,
        profiles: profiles(),
    })
}
fn guild(client: &Client, channel: &str) -> Result<v1::Channel, ApiError> {
    client
        .state()
        .read()
        .channels
        .get(&parse(channel)?)
        .filter(|c| matches!(c.kind, 1..=3))
        .cloned()
        .ok_or_else(|| invalid("Channel is unavailable. Refresh the list."))
}
fn fields(
    input: ChannelFields,
    kind: i32,
) -> Result<(String, String, Option<v1::VoiceSettings>), ApiError> {
    let name = input.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(invalid("Name must be 1 to 64 characters"));
    }
    if input.topic.chars().count() > 1024 {
        return Err(invalid("Topic must be at most 1024 characters"));
    }
    let voice = if kind == 3 {
        let profile = profiles()
            .into_iter()
            .find(|p| p.id == input.profile)
            .ok_or_else(|| invalid("Unknown voice profile"))?;
        if input
            .bitrate
            .is_some_and(|rate| rate < profile.min_bitrate || rate > profile.max_bitrate)
        {
            return Err(invalid("Bitrate is outside the profile range"));
        }
        if input.user_limit.is_some_and(|limit| limit > 99) {
            return Err(invalid("User limit must be 0 to 99"));
        }
        Some(v1::VoiceSettings {
            profile: input.profile,
            bitrate: input.bitrate,
            user_limit: input.user_limit,
        })
    } else {
        None
    };
    Ok((name, input.topic, voice))
}
#[tauri::command]
pub async fn channel_admin_action(
    state: State<'_, AppState>,
    server_id: String,
    action: ChannelAdminAction,
) -> Result<(), ApiError> {
    perform(&state, &server_id, action).await
}
async fn perform(
    state: &AppState,
    server: &str,
    action: ChannelAdminAction,
) -> Result<(), ApiError> {
    let server = parse_server_id(server)?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| invalid("Server is disconnected"))?;
    authorize(client, state, &server)?;
    match action {
        ChannelAdminAction::Create {
            channel_kind,
            parent_id,
            fields: input,
        } => {
            if !(1..=3).contains(&channel_kind) {
                return Err(invalid("Choose category, text or voice"));
            }
            if channel_kind == 1 && parent_id.is_some() {
                return Err(invalid("Categories cannot be nested"));
            }
            let (name, topic, voice) = fields(input, channel_kind)?;
            let _ = client
                .create_channel(v1::CreateChannel {
                    kind: channel_kind,
                    name,
                    topic: Some(topic),
                    voice,
                    parent_id: parent_id.as_deref().map(wire).transpose()?,
                    position: i32::MAX,
                })
                .await
                .map_err(api_error)?;
        }
        ChannelAdminAction::Edit {
            channel,
            fields: input,
        } => {
            let current = guild(client, &channel)?;
            let (name, topic, voice) = fields(input, current.kind)?;
            client
                .update_channel(v1::UpdateChannel {
                    channel_id: current.id,
                    name: Some(name),
                    topic: Some(topic),
                    voice,
                    ..Default::default()
                })
                .await
                .map_err(api_error)?;
        }
        ChannelAdminAction::Move {
            channel,
            parent_id,
            position,
        } => {
            let current = guild(client, &channel)?;
            if position < 0 {
                return Err(invalid("Position cannot be negative"));
            }
            client
                .move_channel(v1::MoveChannel {
                    channel_id: current.id,
                    parent_id: parent_id.as_deref().map(wire).transpose()?,
                    position,
                })
                .await
                .map_err(api_error)?;
        }
        ChannelAdminAction::Delete { channel } => {
            let current = guild(client, &channel)?;
            client
                .delete_channel(v1::DeleteChannel {
                    channel_id: current.id,
                })
                .await
                .map_err(api_error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
