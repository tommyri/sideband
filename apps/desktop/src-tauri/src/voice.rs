//! One desktop-owned audio session. Browsing servers never changes the call.
use crate::{
    commands::api_error,
    state::AppState,
    types::ApiError,
    voice_view::{self, ActiveVoiceView, MemberAction, ServerView, SpeakingView},
};
use ss_client_core::{AudioConfig, Client, ConnectionState, VoiceSession, v1};
use ss_core::Id;
use ss_net::pins::ServerId;
use std::{str::FromStr, sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter, Manager, State};

pub struct ActiveVoice {
    pub server: ServerId,
    pub session: Arc<VoiceSession>,
    pub connection: Id,
    pub muted: bool,
    pub deafened: bool,
    pub local_ptt: bool,
    pub global_ptt: bool,
}
impl ActiveVoice {
    pub fn set_local_ptt(&mut self, pressed: bool) {
        self.local_ptt = pressed;
        self.session.set_ptt(self.local_ptt || self.global_ptt);
    }
    pub fn set_global_ptt(&mut self, pressed: bool) {
        self.global_ptt = pressed;
        self.session.set_ptt(self.local_ptt || self.global_ptt);
    }
    pub fn release_ptt(&mut self) {
        self.local_ptt = false;
        self.global_ptt = false;
        self.session.release_ptt();
    }
    fn view(&self) -> ActiveVoiceView {
        ActiveVoiceView {
            server_id: self.server.as_str().into(),
            channel_id: self.session.channel().to_string(),
            muted: self.muted,
            deafened: self.deafened,
        }
    }
}
fn id(value: &str) -> Result<Id, ApiError> {
    Id::from_str(value).map_err(|_| ApiError::new("invalid_id", "Invalid identifier."))
}
fn server_id(value: &str) -> Result<ServerId, ApiError> {
    super::commands::parse_server_id(value)
}
fn disconnected() -> ApiError {
    ApiError::new("disconnected", "Connect to the server first.")
}

fn own_connection(client: &Client) -> Result<Id, ApiError> {
    let state = client.state();
    let state = state.read();
    let user = state.own_user.as_ref().ok_or_else(disconnected)?;
    let track = state
        .tracks
        .values()
        .filter(|t| t.user_id == user.id && t.kind == v1::TrackKind::Audio as i32)
        .max_by_key(|t| t.track_id)
        .ok_or_else(disconnected)?;
    Id::try_from(track.conn_id.as_ref()).map_err(|_| disconnected())
}

/// Called under operations. Release devices before removing/replacing clients.
pub async fn stop(state: &AppState, only_server: Option<&str>) -> Result<(), ApiError> {
    let mut clients = state.clients.lock().await;
    let mut voice = state.voice.lock().await;
    if voice
        .as_ref()
        .is_none_or(|v| only_server.is_some_and(|s| s != v.server.as_str()))
    {
        return Ok(());
    }
    let active = voice.take().unwrap();
    drop(voice);
    active.session.set_ptt(false);
    let connected = clients
        .get(&active.server)
        .is_some_and(|c| c.state().read().connection == ConnectionState::Connected);
    if !connected {
        // Closing the client cancels desired membership during retry, so an
        // explicit leave cannot silently rejoin on the next reconnect.
        if let Some(client) = clients.remove(&active.server) {
            client.close().await;
        }
    }
    let result = match Arc::try_unwrap(active.session) {
        Ok(session) => session.leave().await,
        Err(session) => {
            drop(session);
            match clients.get(&active.server) {
                Some(client) => client.leave_voice(v1::LeaveVoice {}).await,
                None => Ok(()),
            }
        }
    };
    if connected {
        if result.is_err()
            && let Some(client) = clients.remove(&active.server)
        {
            // A failed leave has an uncertain server outcome. Cancel the core
            // reconnect worker as well as local audio instead of rejoining.
            client.close().await;
        }
        result.map_err(api_error)
    } else {
        Ok(())
    }
}

#[tauri::command]
pub async fn get_server_view(
    state: State<'_, AppState>,
    server_id: String,
) -> Result<ServerView, ApiError> {
    let server = self::server_id(&server_id)?;
    let clients = state.clients.lock().await;
    let client = clients.get(&server).ok_or_else(disconnected)?;
    let active = state.voice.lock().await.as_ref().map(ActiveVoice::view);
    voice_view::snapshot(
        &client.state().read(),
        &server,
        state.storage.as_ref(),
        active,
    )
}

#[tauri::command]
pub async fn join_voice(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    channel_id: String,
) -> Result<(), ApiError> {
    join_with_config(
        &state,
        server_id,
        channel_id,
        crate::audio::load(&app)?.config()?,
    )
    .await
}

async fn join_with_config(
    state: &AppState,
    server_id: String,
    channel_id: String,
    config: AudioConfig,
) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    if state.audio_test.lock().await.is_some() {
        return Err(ApiError::new(
            "audio",
            "Stop the local audio test before joining voice",
        ));
    }
    let server = self::server_id(&server_id)?;
    let channel = id(&channel_id)?;
    {
        let clients = state.clients.lock().await;
        let client = clients.get(&server).ok_or_else(disconnected)?;
        {
            let store = client.state();
            let current = store.read();
            if current.connection != ConnectionState::Connected {
                return Err(disconnected());
            }
            let entry = current
                .channels
                .get(&channel)
                .ok_or_else(|| ApiError::new("invalid", "Unknown channel."))?;
            if entry.kind != v1::ChannelKind::Voice as i32
                || !current.permissions(channel).is_some_and(|p| {
                    p.contains(ss_core::Permissions::CONNECT | ss_core::Permissions::VIEW_CHANNEL)
                })
            {
                return Err(ApiError::new("server_2", "You cannot join this channel."));
            }
        }
        if state
            .voice
            .lock()
            .await
            .as_ref()
            .is_some_and(|v| v.server == server && v.session.channel() == channel)
        {
            return Ok(());
        }
    }
    stop(state, None).await?;
    let clients = state.clients.lock().await;
    let client = clients.get(&server).ok_or_else(disconnected)?;
    let session = VoiceSession::start(client, channel, config)
        .await
        .map_err(api_error)?;
    let connection = own_connection(client)?;
    *state.voice.lock().await = Some(ActiveVoice {
        local_ptt: false,
        global_ptt: false,
        server,
        session: Arc::new(session),
        connection,
        muted: false,
        deafened: false,
    });
    Ok(())
}

#[tauri::command]
pub async fn leave_voice(state: State<'_, AppState>) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    stop(&state, None).await
}

#[tauri::command]
pub async fn voice_control(
    state: State<'_, AppState>,
    action: String,
    enabled: bool,
) -> Result<(), ApiError> {
    // Release PTT without waiting for any network-backed operation.
    if action == "ptt" {
        let mut voice = state.voice.lock().await;
        if let Some(active) = voice.as_mut() {
            active.set_local_ptt(enabled);
        }
        return Ok(());
    }
    let _operation = state.operations.lock().await;
    let session = state
        .voice
        .lock()
        .await
        .as_ref()
        .map(|v| v.session.clone())
        .ok_or_else(disconnected)?;
    match action.as_str() {
        "mute" => session.set_self_mute(enabled).await.map_err(api_error)?,
        "deafen" => session.set_self_deaf(enabled).await.map_err(api_error)?,
        _ => return Err(ApiError::new("invalid", "Unknown voice control.")),
    }
    if let Some(active) = state.voice.lock().await.as_mut() {
        if action == "mute" {
            active.muted = enabled;
        } else {
            active.deafened = enabled;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn member_action(
    state: State<'_, AppState>,
    input: MemberAction,
) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    let server = server_id(&input.server_id)?;
    let user = id(&input.user_id)?;
    if matches!(input.action.as_str(), "volume" | "localMute") {
        let session = state
            .voice
            .lock()
            .await
            .as_ref()
            .filter(|v| v.server == server)
            .map(|v| v.session.clone())
            .ok_or_else(|| {
                ApiError::new(
                    "disconnected",
                    "Join voice on this server to adjust playback.",
                )
            })?;
        return if input.action == "volume" {
            session
                .set_user_volume(
                    user,
                    input
                        .volume
                        .ok_or_else(|| ApiError::new("invalid", "Missing volume."))?,
                )
                .await
                .map_err(api_error)
        } else {
            session
                .set_user_muted(user, input.enabled.unwrap_or(false))
                .await
                .map_err(api_error)
        };
    }
    let clients = state.clients.lock().await;
    let client = clients.get(&server).ok_or_else(disconnected)?;
    let user_id = user.as_bytes().to_vec().into();
    let reason = input.reason.unwrap_or_default();
    // The server rechecks permissions and hierarchy against current state.
    match input.action.as_str() {
        "serverMute" => {
            client
                .server_mute_user(v1::ServerMuteUser {
                    user_id,
                    muted: input.enabled.unwrap_or(false),
                })
                .await
        }
        "move" => {
            client
                .move_user(v1::MoveUser {
                    user_id,
                    channel_id: id(&input
                        .channel_id
                        .ok_or_else(|| ApiError::new("invalid", "Choose a channel."))?)?
                    .as_bytes()
                    .to_vec()
                    .into(),
                })
                .await
        }
        "kick" => client.kick_user(v1::KickUser { user_id, reason }).await,
        "ban" => {
            client
                .ban_user(v1::BanUser {
                    user_id,
                    reason,
                    expires_at: None,
                })
                .await
        }
        _ => return Err(ApiError::new("invalid", "Unknown member action.")),
    }
    .map_err(api_error)
}

/// Speaking uses a small change-only event at 40 ms cadence, not full snapshots.
/// Reconcile removal/moves so stale membership cannot leave capture running.
pub async fn watch(app: AppHandle) {
    let mut tick = tokio::time::interval(Duration::from_millis(40));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut previous = (String::new(), Vec::<String>::new());
    loop {
        tick.tick().await;
        let state = app.state::<AppState>();
        let Ok(_operation) = state.operations.try_lock() else {
            continue;
        };
        let clients = state.clients.lock().await;
        let mut voice = state.voice.lock().await;
        let mut now = (String::new(), Vec::<String>::new());
        if let Some(active) = voice.as_mut() {
            let client = clients.get(&active.server);
            let (connected, retrying, channel) = client.map_or((false, false, None), |client| {
                let store = client.state();
                let current = store.read();
                (
                    current.connection == ConnectionState::Connected,
                    matches!(current.connection, ConnectionState::Reconnecting { .. }),
                    current
                        .voice_states
                        .get(&active.connection)
                        .and_then(|v| v.channel_id.as_ref())
                        .and_then(|c| Id::try_from(c.as_ref()).ok()),
                )
            });
            if retrying {
                active.release_ptt();
            } else if !connected || channel.is_none() {
                voice.take();
                let _ = app.emit("ss://voice-session", ());
            } else if channel != Some(active.session.channel()) {
                // Follow moderator moves muted, adopting the destination profile.
                let old = voice.take().unwrap();
                let server = old.server.clone();
                drop(old);
                if let (Some(client), Some(channel)) = (client, channel) {
                    match VoiceSession::start(
                        client,
                        channel,
                        AudioConfig {
                            start_muted: true,
                            ..crate::audio::load(&app)
                                .and_then(|p| p.config())
                                .unwrap_or_default()
                        },
                    )
                    .await
                    {
                        Ok(session) => {
                            if let Ok(connection) = own_connection(client) {
                                *voice = Some(ActiveVoice {
                                    local_ptt: false,
                                    global_ptt: false,
                                    server,
                                    session: Arc::new(session),
                                    connection,
                                    muted: true,
                                    deafened: false,
                                });
                            }
                        }
                        Err(error) => {
                            let _ = app.emit("ss://voice-error", error.to_string());
                        }
                    }
                }
                let _ = app.emit("ss://voice-session", ());
            } else {
                now = (
                    active.server.as_str().into(),
                    active
                        .session
                        .speaking_users()
                        .into_iter()
                        .map(|id| id.to_string())
                        .collect(),
                );
            }
        }
        if now != previous {
            if !previous.0.is_empty() && previous.0 != now.0 {
                let _ = app.emit(
                    "ss://speaking",
                    SpeakingView {
                        server_id: previous.0.clone(),
                        user_ids: Vec::new(),
                    },
                );
            }
            if !now.0.is_empty() {
                let _ = app.emit(
                    "ss://speaking",
                    SpeakingView {
                        server_id: now.0.clone(),
                        user_ids: now.1.clone(),
                    },
                );
            }
            previous = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_client_core::{AudioSource, Credentials, ServerAddr};
    use ss_net::{pins::PinStore, server::Endpoint, tls};

    #[tokio::test]
    async fn desktop_voice_switches_releases_and_cancels_reconnect_membership() {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            ss_server::db::Pool::open(dir.path().join("server.db"))
                .await
                .unwrap(),
        );
        let invite = ss_server::auth::ensure_owner_invite(&pool)
            .await
            .unwrap()
            .unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let server = Arc::new(ss_server::connection::Server::open(pool).await.unwrap());
        let task = tokio::spawn(server.run(endpoint.clone()));
        let state = AppState::open(&dir.path().join("desktop")).unwrap();
        let address = ServerAddr::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap();
        let key = address.id().unwrap();
        state.pins.set(&key, fingerprint).unwrap();
        let client = Client::connect_with_storage(
            address,
            Credentials::Register(v1::Register {
                invite_code: invite,
                username: "owner".into(),
                password: "test-password".into(),
            }),
            state.pins.clone(),
            state.storage.clone(),
        )
        .await
        .unwrap();
        let first = *client
            .state()
            .read()
            .channels
            .iter()
            .find(|(_, c)| c.kind == v1::ChannelKind::Voice as i32)
            .unwrap()
            .0;
        let second = client
            .create_channel(v1::CreateChannel {
                name: "Second".into(),
                kind: v1::ChannelKind::Voice as i32,
                ..Default::default()
            })
            .await
            .unwrap();
        let own =
            Id::try_from(client.state().read().own_user.as_ref().unwrap().id.as_ref()).unwrap();
        let transport = client.diagnostics().expect("live transport diagnostics");
        assert!(transport.path_mtu_bytes >= 1200);
        assert!(transport.congestion_window_bytes > 0);
        state.clients.lock().await.insert(client).unwrap();
        let config = || AudioConfig {
            source: AudioSource::Test,
            ..AudioConfig::default()
        };
        join_with_config(&state, key.as_str().into(), first.to_string(), config())
            .await
            .unwrap();
        {
            let mut voice = state.voice.lock().await;
            let active = voice.as_mut().unwrap();
            assert!(active.session.speaking_users().is_empty());
            active.set_global_ptt(true);
            active.set_local_ptt(true);
            active.set_local_ptt(false);
            tokio::time::timeout(Duration::from_secs(2), async {
                while !active.session.speaking_users().contains(&own) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            let details = active.session.diagnostics().expect("worker diagnostics");
            assert!(
                details["backend"].is_null(),
                "synthetic input must not claim hardware"
            );
            assert!(details["inputRateHz"].is_null());
            assert!(details["encoderFec"].is_boolean());
            active.release_ptt();
            tokio::time::timeout(Duration::from_millis(100), async {
                while !active.session.speaking_users().is_empty() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            let tuned = AudioConfig {
                gate: ss_client_core::TalkGate::VoiceActivation(ss_audio::vad::Config::default()),
                master_gain: 0.5,
                jitter_min: Duration::from_millis(30),
                ..config()
            };
            active.session.configure(tuned).await.unwrap();
            assert_eq!(active.session.channel(), first);
            assert!(active.session.input_level() > -96.0);
            let invalid = AudioConfig {
                master_gain: f32::NAN,
                ..config()
            };
            assert!(active.session.configure(invalid).await.is_err());
            assert_eq!(active.session.channel(), first);
            active.session.set_self_mute(true).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while !active.session.speaking_users().is_empty() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            active.session.set_user_volume(own, 150).await.unwrap();
            active.session.set_user_muted(own, true).await.unwrap();
        }
        join_with_config(&state, key.as_str().into(), second.to_string(), config())
            .await
            .unwrap();
        assert_eq!(
            state.voice.lock().await.as_ref().unwrap().session.channel(),
            second
        );
        stop(&state, None).await.unwrap();
        assert!(state.voice.lock().await.is_none());
        join_with_config(&state, key.as_str().into(), first.to_string(), config())
            .await
            .unwrap();
        endpoint.close(0, b"test outage");
        task.abort();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if state
                    .clients
                    .lock()
                    .await
                    .get(&key)
                    .unwrap()
                    .state()
                    .read()
                    .connection
                    != ConnectionState::Connected
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        stop(&state, None).await.unwrap();
        assert!(state.voice.lock().await.is_none());
        assert!(
            state.clients.lock().await.get(&key).is_none(),
            "leaving during retry must cancel desired membership"
        );
        use ss_client_core::storage::Storage;
        let reopened =
            crate::persistence::DesktopStorage::open(dir.path().join("desktop/sessions.json"))
                .unwrap();
        assert_eq!(reopened.user_volume(&key, own).unwrap(), Some(1.5));
        assert!(reopened.user_muted(&key, own).unwrap());
    }
}
