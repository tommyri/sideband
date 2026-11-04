use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting {
        attempt: u32,
        #[ts(rename = "delayMs", type = "number")]
        delay_ms: u64,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ConnectInput {
    pub host: String,
    pub port: u16,
    pub https_port: u16,
    pub username: String,
    pub password: String,
    pub mode: String,
    pub invite_code: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ConnectResult {
    pub server_id: String,
    pub label: String,
    pub status: ConnectionStatus,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ApproveFingerprintInput {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ServerSummary {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub https_port: u16,
    pub unread: u32,
    pub status: ConnectionStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ProbeInput {
    pub host: String,
    pub port: u16,
    pub https_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ServerProbe {
    pub name: String,
    pub registration_mode: String,
    pub fingerprint: String,
    pub previous_fingerprint: Option<String>,
    pub trusted: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AppSnapshot {
    pub servers: Vec<ServerSummary>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub detail: Option<String>,
    pub fingerprint: Option<String>,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            detail: None,
            fingerprint: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct SpeakerStatsPayload {
    pub track: u32,
    pub user: String,
    #[ts(type = "number")]
    pub jitter_delay_ms: u64,
    #[ts(type = "number")]
    pub lost: u64,
    #[ts(type = "number")]
    pub plc: u64,
    pub speaking: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct VoiceStatsPayload {
    pub speakers: Vec<SpeakerStatsPayload>,
    pub encoder_bitrate: u32,
    #[ts(type = "number")]
    pub rtt_ms: u64,
    #[ts(type = "number")]
    pub underruns: u64,
    #[ts(type = "number")]
    pub overruns: u64,
    #[ts(type = "number")]
    pub processing_latency_ms: u64,
    #[ts(type = "number")]
    pub processing_over_budget: u64,
    pub output_rms: f32,
}

#[derive(Debug, Clone, Copy, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum MicrophoneModePayload {
    Standard,
    WideSpectrum,
    VoiceIsolation,
    Unknown,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum VoiceDevicePayload {
    PermissionDenied,
    SilentInput {
        permission_denied: bool,
    },
    LowQualityInput {
        sample_rate: u32,
    },
    AppleVoiceProcessing {
        microphone_mode: MicrophoneModePayload,
    },
    MutedSpeech {
        active: bool,
    },
    Unavailable {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub enum ClientEventPayload {
    Resynced,
    ChannelsChanged,
    MessagesChanged {
        channel: String,
    },
    VoiceChanged,
    VoiceStats {
        stats: VoiceStatsPayload,
    },
    VoiceDevice {
        state: VoiceDevicePayload,
    },
    MembersChanged,
    ReadStatesChanged,
    InvitesChanged,
    Typing {
        channel: String,
        user: String,
    },
    Connection {
        status: ConnectionStatus,
    },
    Notice {
        message: String,
    },
    Removed {
        banned: bool,
        reason: String,
    },
    KeyframeRequested {
        track: u32,
    },
    SubscriberHint {
        track: u32,
        #[ts(rename = "maxHeight")]
        max_height: u32,
        #[ts(rename = "maxFps")]
        max_fps: u32,
        #[ts(rename = "viewerCount")]
        viewer_count: u32,
    },
    ServerError {
        code: i32,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AppEvent {
    pub server_id: String,
    #[ts(type = "number")]
    pub occurred_at_ms: u64,
    pub event: ClientEventPayload,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct SendMessageInput {
    pub server_id: String,
    pub channel_id: String,
    pub content: String,
    pub reply_to: Option<String>,
    pub attachment_ids: Vec<String>,
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct CreateChannelInput {
    pub server_id: String,
    pub kind: i32,
    pub name: String,
    pub parent_id: Option<String>,
    pub position: i32,
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct HistoryRequest {
    pub server_id: String,
    pub channel_id: String,
    pub before: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct BridgeMessage {
    pub deleted: bool,
    pub id: String,
    pub channel_id: String,
    pub author_name: Option<String>,
    pub author_id: Option<String>,
    pub username: Option<String>,
    pub reply_to: Option<String>,
    pub content: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number | null")]
    pub edited_at: Option<i64>,
    pub attachment_count: u32,
}
