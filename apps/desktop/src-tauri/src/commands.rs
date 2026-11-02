use crate::{
    state::AppState,
    types::{
        ApiError, AppEvent, AppSnapshot, ApproveFingerprintInput, BridgeMessage,
        ClientEventPayload, ConnectInput, ConnectResult, ConnectionStatus, CreateChannelInput,
        HistoryRequest, MicrophoneModePayload, SendMessageInput, ServerSummary,
        SpeakerStatsPayload, VoiceDevicePayload, VoiceStatsPayload,
    },
};
use ss_client_core::{
    Client, ClientEvent, ConnectionState, Credentials, Error as ClientError, ServerAddr,
    VoiceDeviceState, VoiceStats,
};
use ss_net::{
    Error as NetworkError,
    pins::{PinStore, ServerId},
    tls::Fingerprint,
};
use std::{
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, State};

pub(crate) fn api_error(error: ClientError) -> ApiError {
    match error {
        ClientError::Network(NetworkError::Untrusted { fingerprint }) => ApiError {
            code: "untrusted_certificate".into(),
            message: "Approve this server certificate before connecting.".into(),
            detail: None,
            fingerprint: Some(fingerprint.to_string()),
        },
        ClientError::Network(NetworkError::FingerprintChanged { expected, received }) => ApiError {
            code: "fingerprint_changed".into(),
            message: "The server certificate changed. Verify it before replacing the pin.".into(),
            detail: Some(format!("expected {expected}; received {received}")),
            fingerprint: Some(received.to_string()),
        },
        ClientError::Disconnected => ApiError::new("disconnected", error.to_string()),
        ClientError::Timeout => ApiError::new("timeout", error.to_string()),
        ClientError::Server { code, message } => ApiError::new(format!("server_{code}"), message),
        _ => ApiError::new("client", error.to_string()),
    }
}

fn status(state: &ConnectionState) -> ConnectionStatus {
    match state {
        ConnectionState::Disconnected => ConnectionStatus::Disconnected,
        ConnectionState::Connecting => ConnectionStatus::Connecting,
        ConnectionState::Connected => ConnectionStatus::Connected,
        ConnectionState::Reconnecting { attempt, delay } => ConnectionStatus::Reconnecting {
            attempt: *attempt,
            delay_ms: delay.as_millis().try_into().unwrap_or(u64::MAX),
        },
        ConnectionState::Failed(message) => ConnectionStatus::Failed {
            message: message.clone(),
        },
    }
}

fn event(event: ClientEvent) -> ClientEventPayload {
    match event {
        ClientEvent::Resynced => ClientEventPayload::Resynced,
        ClientEvent::ChannelsChanged => ClientEventPayload::ChannelsChanged,
        ClientEvent::MessagesChanged { channel } => ClientEventPayload::MessagesChanged {
            channel: channel.to_string(),
        },
        ClientEvent::VoiceChanged => ClientEventPayload::VoiceChanged,
        ClientEvent::VoiceStats(stats) => ClientEventPayload::VoiceStats {
            stats: voice_stats(stats),
        },
        ClientEvent::VoiceDevice(state) => ClientEventPayload::VoiceDevice {
            state: voice_device(state),
        },
        ClientEvent::MembersChanged => ClientEventPayload::MembersChanged,
        ClientEvent::ReadStatesChanged => ClientEventPayload::ReadStatesChanged,
        ClientEvent::InvitesChanged => ClientEventPayload::InvitesChanged,
        ClientEvent::Typing { channel, user } => ClientEventPayload::Typing {
            channel: channel.to_string(),
            user: user.to_string(),
        },
        ClientEvent::Connection(value) => ClientEventPayload::Connection {
            status: status(&value),
        },
        ClientEvent::Notice(message) => ClientEventPayload::Notice { message },
        ClientEvent::Removed { banned, reason } => ClientEventPayload::Removed { banned, reason },
        ClientEvent::KeyframeRequested { track } => ClientEventPayload::KeyframeRequested { track },
        ClientEvent::SubscriberHint {
            track,
            max_height,
            max_fps,
            viewer_count,
        } => ClientEventPayload::SubscriberHint {
            track,
            max_height,
            max_fps,
            viewer_count,
        },
        ClientEvent::ServerError(error) => ClientEventPayload::ServerError {
            code: error.code,
            message: error.message,
        },
    }
}

fn voice_stats(stats: VoiceStats) -> VoiceStatsPayload {
    VoiceStatsPayload {
        speakers: stats
            .speakers
            .into_iter()
            .map(|speaker| SpeakerStatsPayload {
                track: speaker.track,
                user: speaker.user.to_string(),
                jitter_delay_ms: duration_ms(speaker.jitter_delay),
                lost: speaker.lost,
                plc: speaker.plc,
                speaking: speaker.speaking,
            })
            .collect(),
        encoder_bitrate: stats.encoder_bitrate,
        rtt_ms: duration_ms(stats.rtt),
        underruns: stats.underruns,
        overruns: stats.overruns,
        processing_latency_ms: duration_ms(stats.processing_latency),
        processing_over_budget: stats.processing_over_budget,
        output_rms: stats.output_rms,
    }
}

fn voice_device(state: VoiceDeviceState) -> VoiceDevicePayload {
    match state {
        VoiceDeviceState::PermissionDenied => VoiceDevicePayload::PermissionDenied,
        VoiceDeviceState::SilentInput { permission_denied } => {
            VoiceDevicePayload::SilentInput { permission_denied }
        }
        VoiceDeviceState::LowQualityInput { sample_rate } => {
            VoiceDevicePayload::LowQualityInput { sample_rate }
        }
        VoiceDeviceState::AppleVoiceProcessing { microphone_mode } => {
            VoiceDevicePayload::AppleVoiceProcessing {
                microphone_mode: match microphone_mode {
                    ss_audio::device::MicrophoneMode::Standard => MicrophoneModePayload::Standard,
                    ss_audio::device::MicrophoneMode::WideSpectrum => {
                        MicrophoneModePayload::WideSpectrum
                    }
                    ss_audio::device::MicrophoneMode::VoiceIsolation => {
                        MicrophoneModePayload::VoiceIsolation
                    }
                    ss_audio::device::MicrophoneMode::Unknown => MicrophoneModePayload::Unknown,
                },
            }
        }
        VoiceDeviceState::MutedSpeech { active } => VoiceDevicePayload::MutedSpeech { active },
        VoiceDeviceState::Unavailable(message) => VoiceDevicePayload::Unavailable { message },
    }
}

fn duration_ms(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn parse_server_id(value: &str) -> Result<ServerId, ApiError> {
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| ApiError::new("invalid_server", "Invalid server identity."))?;
        (host, port)
    } else {
        value
            .rsplit_once(':')
            .ok_or_else(|| ApiError::new("invalid_server", "Invalid server identity."))?
    };
    let port = port.parse::<u16>().map_err(|error| {
        ApiError::new("invalid_server", "Invalid server port.").with_detail(error.to_string())
    })?;
    ServerId::new(host, port).map_err(|error| ApiError::new("invalid_server", error.to_string()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn id(value: &str) -> Result<ss_core::Id, ApiError> {
    ss_core::Id::from_str(value).map_err(|error| ApiError::new("invalid_id", error.to_string()))
}

fn id_bytes(value: &str) -> Result<bytes::Bytes, ApiError> {
    Ok(id(value)?.as_bytes().to_vec().into())
}

fn display_id(value: &[u8]) -> Result<String, ApiError> {
    ss_core::Id::try_from(value)
        .map(|id| id.to_string())
        .map_err(|error| ApiError::new("invalid_id", error.to_string()))
}

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    input: ConnectInput,
) -> Result<ConnectResult, ApiError> {
    let _operation = state.operations.lock().await;
    let mut address = ServerAddr::new(&input.host, input.port).map_err(api_error)?;
    address.https_port = input.https_port;
    if input.port == 0 || input.https_port == 0 {
        return Err(ApiError::new("invalid_server", "Invalid port."));
    }
    let credentials = match input.mode.as_str() {
        "saved" => Credentials::Saved,
        "register" => Credentials::Register(ss_client_core::v1::Register {
            invite_code: input.invite_code,
            username: input.username,
            password: input.password,
        }),
        "login" => Credentials::Login(ss_client_core::v1::Login {
            username: input.username,
            password: input.password,
        }),
        _ => return Err(ApiError::new("invalid", "Unknown authentication mode.")),
    };
    let client = Client::connect_with_storage(
        address.clone(),
        credentials,
        state.pins.clone(),
        state.storage.clone(),
    )
    .await
    .map_err(api_error)?;
    let server_id = address.id().map_err(api_error)?.as_str().to_owned();
    let label = client
        .state()
        .read()
        .server
        .as_ref()
        .map_or_else(|| address.host.clone(), |s| s.name.clone());
    state
        .storage
        .label(&server_id, label.clone())
        .map_err(api_error)?;
    let mut receiver = client.subscribe();
    let event_server_id = server_id.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let incoming = match receiver.recv().await {
                Ok(incoming) => incoming,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            let _ = app.emit(
                "ss://event",
                AppEvent {
                    server_id: event_server_id.clone(),
                    occurred_at_ms: now_ms(),
                    event: event(incoming),
                },
            );
        }
    });
    let previous = state
        .clients
        .lock()
        .await
        .insert(client)
        .map_err(api_error)?;
    drop(previous);
    Ok(ConnectResult {
        server_id,
        label,
        status: ConnectionStatus::Connected,
    })
}

#[tauri::command]
pub async fn approve_fingerprint(
    state: State<'_, AppState>,
    input: ApproveFingerprintInput,
) -> Result<(), ApiError> {
    let id = ServerId::new(&input.host, input.port)
        .map_err(|error| ApiError::new("invalid_server", error.to_string()))?;
    let fingerprint = Fingerprint::try_from(input.fingerprint)
        .map_err(|error| ApiError::new("invalid_fingerprint", error))?;
    state
        .pins
        .set(&id, fingerprint)
        .map_err(|error| ApiError::new("pin_store", error.to_string()))
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, server_id: String) -> Result<(), ApiError> {
    let id = parse_server_id(&server_id)?;
    let client = state.clients.lock().await.remove(&id);
    if let Some(client) = client {
        client.close().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_snapshot(state: State<'_, AppState>) -> Result<AppSnapshot, ApiError> {
    let clients = state.clients.lock().await;
    let servers = state
        .storage
        .servers()
        .into_iter()
        .map(|saved| {
            let client = ServerId::new(&saved.host, saved.port)
                .ok()
                .and_then(|id| clients.get(&id));
            ServerSummary {
                id: saved.id,
                label: saved.label,
                host: saved.host,
                port: saved.port,
                https_port: saved.https_port,
                unread: client.map_or(0, |c| {
                    let store = c.state();
                    let current = store.read();
                    current
                        .channels
                        .iter()
                        .chain(current.direct_messages.iter())
                        .filter(|(id, channel)| {
                            channel.last_message_id.as_ref().is_some_and(|last| {
                                current
                                    .read_states
                                    .get(id)
                                    .is_none_or(|read| read.last_read_message_id < *last)
                            })
                        })
                        .count()
                        .try_into()
                        .unwrap_or(u32::MAX)
                }),
                status: client.map_or(ConnectionStatus::Disconnected, |c| {
                    status(&c.state().read().connection)
                }),
            }
        })
        .collect();
    Ok(AppSnapshot { servers })
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    input: SendMessageInput,
) -> Result<String, ApiError> {
    let server = parse_server_id(&input.server_id)?;
    let command = ss_client_core::v1::SendMessage {
        channel_id: id_bytes(&input.channel_id)?,
        content: input.content,
        reply_to: input.reply_to.as_deref().map(id_bytes).transpose()?,
        attachment_ids: input
            .attachment_ids
            .iter()
            .map(|value| id_bytes(value))
            .collect::<Result<_, _>>()?,
        nonce: input.nonce,
    };
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "That server is not connected."))?;
    client
        .send_message(command)
        .await
        .map(|id| id.to_string())
        .map_err(api_error)
}

#[tauri::command]
pub async fn create_channel(
    state: State<'_, AppState>,
    input: CreateChannelInput,
) -> Result<String, ApiError> {
    let server = parse_server_id(&input.server_id)?;
    let command = ss_client_core::v1::CreateChannel {
        kind: input.kind,
        name: input.name,
        parent_id: input.parent_id.as_deref().map(id_bytes).transpose()?,
        position: input.position,
        topic: input.topic,
        voice: None,
    };
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "That server is not connected."))?;
    client
        .create_channel(command)
        .await
        .map(|id| id.to_string())
        .map_err(api_error)
}

#[tauri::command]
pub async fn load_history(
    state: State<'_, AppState>,
    request: HistoryRequest,
) -> Result<Vec<BridgeMessage>, ApiError> {
    let server = parse_server_id(&request.server_id)?;
    let channel = id(&request.channel_id)?;
    let before = request.before.as_deref().map(id).transpose()?;
    let clients = state.clients.lock().await;
    let client = clients
        .get(&server)
        .ok_or_else(|| ApiError::new("disconnected", "That server is not connected."))?;
    client
        .load_older(channel, before)
        .await
        .map_err(api_error)?
        .into_iter()
        .map(|message| {
            let author_name = message
                .author
                .map(|author| author.display_name)
                .or(message.webhook_name);
            Ok(BridgeMessage {
                id: display_id(&message.id)?,
                channel_id: display_id(&message.channel_id)?,
                author_name,
                content: message.content,
                created_at: message.created_at,
                edited_at: message.edited_at,
                attachment_count: message.attachments.len().try_into().unwrap_or(u32::MAX),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_server_id;

    #[test]
    fn parses_stable_ipv4_and_ipv6_server_ids() {
        assert_eq!(
            parse_server_id("[127.0.0.1]:5781").unwrap().as_str(),
            "[127.0.0.1]:5781"
        );
        assert_eq!(
            parse_server_id("[::1]:5781").unwrap().as_str(),
            "[::1]:5781"
        );
    }
}
