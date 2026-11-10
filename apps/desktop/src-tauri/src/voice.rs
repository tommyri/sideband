//! One desktop-owned audio session. Browsing servers never changes the call.
use crate::{
    commands::api_error,
    state::AppState,
    types::ApiError,
    voice_view::{self, ActiveVoiceView, MemberAction, ServerView, SpeakingView},
};
use ss_client_core::{AudioConfig, Client, ConnectionState, StateStore, VoiceSession, v1};
use ss_core::Id;
use ss_net::pins::ServerId;
use std::{
    str::FromStr,
    sync::{Arc, Weak},
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager, State};

pub struct ActiveVoice {
    pub server: ServerId,
    pub session: Arc<VoiceSession>,
    pub connection: Id,
    /// The call's client state, readable without the client map lock.
    pub store: StateStore,
    pub muted: bool,
    pub deafened: bool,
    ptt: PttInputs,
}
/// The two inputs of the combined push-to-talk gate.
#[derive(Default)]
struct PttInputs {
    local: bool,
    global: bool,
}
impl ActiveVoice {
    fn new(server: ServerId, session: VoiceSession, client: &Client) -> Result<Self, ApiError> {
        Ok(Self {
            connection: own_connection(client)?,
            store: client.state(),
            muted: session.self_muted(),
            deafened: session.self_deafened(),
            server,
            session: Arc::new(session),
            ptt: PttInputs::default(),
        })
    }
    /// Whether the call's server connection is up. Never waits for a lock.
    pub fn connected(&self) -> bool {
        self.store.read().connection == ConnectionState::Connected
    }
    pub fn set_local_ptt(&mut self, pressed: bool) {
        self.ptt.local = pressed;
        self.session.set_ptt(self.ptt.local || self.ptt.global);
    }
    pub fn set_global_ptt(&mut self, pressed: bool) {
        self.ptt.global = pressed;
        self.session.set_ptt(self.ptt.local || self.ptt.global);
    }
    pub fn release_ptt(&mut self) {
        self.ptt = PttInputs::default();
        self.session.release_ptt();
    }
    /// Mirror the gate the session actually applies.
    fn sync_self_state(&mut self) {
        self.muted = self.session.self_muted();
        self.deafened = self.session.self_deafened();
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
    if let Some(conn) = state.own_conn {
        return Ok(conn);
    }
    // Older servers do not name our connection.
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
    let active = {
        let mut voice = state.voice.lock().await;
        if voice
            .as_ref()
            .is_some_and(|v| only_server.is_some_and(|s| s != v.server.as_str()))
        {
            return Ok(());
        }
        let Some(active) = voice.take() else {
            return Ok(());
        };
        active
    };
    active.session.set_ptt(false);
    active.session.play_cue(ss_client_core::voice::Cue::Leave);
    tokio::time::sleep(Duration::from_millis(70)).await;
    let client = state
        .client(&active.server)
        .filter(|c| c.state().read().connection == ConnectionState::Connected);
    let connected = client.is_some();
    if !connected {
        // Closing the client cancels desired membership during retry, so an
        // explicit leave cannot silently rejoin on the next reconnect.
        let removed = state.clients().remove(&active.server);
        if let Some(removed) = removed {
            crate::state::close(removed).await;
        }
    }
    let result = match Arc::try_unwrap(active.session) {
        Ok(session) => session.leave().await,
        Err(session) => {
            drop(session);
            match &client {
                Some(client) => client.leave_voice(v1::LeaveVoice {}).await,
                None => Ok(()),
            }
        }
    };
    drop(client);
    if connected {
        if result.is_err() {
            // A failed leave has an uncertain server outcome. Cancel the core
            // reconnect worker as well as local audio instead of rejoining.
            let removed = state.clients().remove(&active.server);
            if let Some(removed) = removed {
                crate::state::close(removed).await;
            }
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
    let client = state.client(&server).ok_or_else(disconnected)?;
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
        let client = state.client(&server).ok_or_else(disconnected)?;
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
    stop(state, None).await?;
    let client = state.client(&server).ok_or_else(disconnected)?;
    let session = VoiceSession::start(&client, channel, config)
        .await
        .map_err(api_error)?;
    let preferences = state.storage.notifications();
    session.set_cues_enabled(!preferences.dnd);
    session.set_control_cues_enabled(preferences.sounds(server.as_str()));
    session.play_cue(ss_client_core::voice::Cue::Join);
    let active = ActiveVoice::new(server, session, &client)?;
    *state.voice.lock().await = Some(active);
    Ok(())
}

#[tauri::command]
pub async fn leave_voice(state: State<'_, AppState>) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    stop(&state, None).await
}

/// Mute or deafen the call, or toggle it when `enabled` is `None`.
///
/// Never waits for `operations` or the client map: a connect, join or
/// network command in flight cannot delay it. Muting and deafening close the
/// gate before the server is told and count as done when the server cannot
/// be reached (the core corrects the server after reconnecting). Unmuting
/// waits for the server and fails if it cannot be told.
pub async fn set_self_state(
    state: &AppState,
    deafen: bool,
    enabled: Option<bool>,
) -> Result<(), ApiError> {
    let (session, pending, closing) = {
        let mut voice = state.voice.lock().await;
        let active = voice.as_mut().ok_or_else(disconnected)?;
        let current = if deafen {
            active.deafened
        } else {
            active.muted
        };
        let next = enabled.unwrap_or(!current);
        let pending: std::pin::Pin<Box<dyn Future<Output = _> + Send>> = if deafen {
            Box::pin(active.session.set_self_deaf(next))
        } else {
            Box::pin(active.session.set_self_mute(next))
        };
        active.sync_self_state();
        (Arc::downgrade(&active.session), pending, next)
    };
    // Await without holding the voice lock, so PTT is never delayed.
    let result = pending.await;
    if let Some(active) = state.voice.lock().await.as_mut()
        && Weak::ptr_eq(&session, &Arc::downgrade(&active.session))
    {
        active.sync_self_state();
    }
    match result {
        Ok(()) => Ok(()),
        Err(_) if closing => Ok(()),
        Err(error) => Err(api_error(error)),
    }
}

#[tauri::command]
pub async fn voice_control(
    state: State<'_, AppState>,
    action: String,
    enabled: bool,
) -> Result<(), ApiError> {
    match action.as_str() {
        // Release PTT without waiting for any network-backed operation.
        "ptt" => {
            if let Some(active) = state.voice.lock().await.as_mut() {
                active.set_local_ptt(enabled);
            }
            Ok(())
        }
        "mute" => set_self_state(&state, false, Some(enabled)).await,
        "deafen" => set_self_state(&state, true, Some(enabled)).await,
        _ => Err(ApiError::new("invalid", "Unknown voice control.")),
    }
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
    let client = state.client(&server).ok_or_else(disconnected)?;
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
        let now = observe(&app, &state).await;
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

/// One watch tick, called under operations. Returns the speaking users.
async fn observe(app: &AppHandle, state: &AppState) -> (String, Vec<String>) {
    let mut voice = state.voice.lock().await;
    let Some(active) = voice.as_mut() else {
        return (String::new(), Vec::new());
    };
    let client = state.client(&active.server);
    // A reconnect that did not resume the seat has a new connection id.
    let (connected, retrying, channel) = client.as_ref().map_or((false, false, None), |client| {
        let store = client.state();
        let current = store.read();
        if let Some(conn) = current.own_conn {
            active.connection = conn;
        }
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
        return (String::new(), Vec::new());
    }
    let (Some(client), Some(channel)) = (client.filter(|_| connected), channel) else {
        voice.take();
        let _ = app.emit("ss://voice-session", ());
        return (String::new(), Vec::new());
    };
    if channel == active.session.channel() {
        return (
            active.server.as_str().into(),
            active
                .session
                .speaking_users()
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
        );
    }
    // Follow moderator moves muted, adopting the destination profile. The
    // voice lock is released while joining so PTT and mute never wait on it.
    let server = active.server.clone();
    voice.take();
    drop(voice);
    let config = AudioConfig {
        start_muted: true,
        ..crate::audio::load(app)
            .and_then(|p| p.config())
            .unwrap_or_default()
    };
    match VoiceSession::start(&client, channel, config).await {
        Ok(session) => {
            if let Ok(active) = ActiveVoice::new(server, session, &client) {
                *state.voice.lock().await = Some(active);
            }
        }
        Err(error) => {
            let _ = app.emit("ss://voice-error", error.to_string());
        }
    }
    let _ = app.emit("ss://voice-session", ());
    (String::new(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_client_core::{AudioSource, Credentials, ServerAddr};
    use ss_net::{pins::PinStore, server::Endpoint, tls};

    /// An HTTPS port that accepts connections and never answers, so every
    /// history request stalls until its deadline.
    async fn stalled_https() -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        (port, task)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end scenario against a real server"
    )]
    #[tokio::test]
    async fn ptt_and_mute_never_wait_for_slow_requests_or_connect() {
        use crate::input::{apply_ptt, session_identity};
        use ss_client_core::ptt::PttArming;
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
        let (https_port, https) = stalled_https().await;
        let state = AppState::open(&dir.path().join("desktop")).unwrap();
        let mut address =
            ServerAddr::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap();
        address.https_port = https_port;
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
        let channel = |kind: v1::ChannelKind| {
            *client
                .state()
                .read()
                .channels
                .iter()
                .find(|(_, c)| c.kind == kind as i32)
                .unwrap()
                .0
        };
        let (voice, text) = (
            channel(v1::ChannelKind::Voice),
            channel(v1::ChannelKind::Text),
        );
        let own =
            Id::try_from(client.state().read().own_user.as_ref().unwrap().id.as_ref()).unwrap();
        state.clients().insert(client).unwrap();
        let config = AudioConfig {
            source: AudioSource::Test,
            ..AudioConfig::default()
        };
        join_with_config(&state, key.as_str().into(), voice.to_string(), config)
            .await
            .unwrap();
        let session = Arc::clone(&state.voice.lock().await.as_ref().unwrap().session);
        let talking = |expected: bool| {
            let session = Arc::clone(&session);
            async move {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while session.speaking_users().contains(&own) != expected {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap_or_else(|_| panic!("talking never became {expected}"));
            }
        };
        let mut arming = PttArming::default();
        arming.tick(Some(session_identity(
            state.voice.lock().await.as_ref().unwrap(),
        )));

        // A global PTT press while a history request is stalled opens the gate.
        let history = crate::commands::history(
            &state,
            crate::types::HistoryRequest {
                server_id: key.as_str().into(),
                channel_id: text.to_string(),
                before: None,
            },
        );
        tokio::pin!(history);
        tokio::select! {
            biased;
            result = &mut history => panic!("history did not stall: {:?}", result.err()),
            () = async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                apply_ptt(&state, &mut arming, true, || true).await;
                talking(true).await;
                apply_ptt(&state, &mut arming, false, || true).await;
                talking(false).await;
            } => {}
        }

        // Toggles take effect while a connect to another server holds operations.
        let operation = state.operations.lock().await;
        set_self_state(&state, false, None).await.unwrap();
        assert!(session.self_muted());
        assert!(state.voice.lock().await.as_ref().unwrap().muted);
        apply_ptt(&state, &mut arming, true, || true).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !session.speaking_users().contains(&own),
            "muted gate opened"
        );
        set_self_state(&state, false, None).await.unwrap();
        assert!(!state.voice.lock().await.as_ref().unwrap().muted);
        talking(true).await;
        set_self_state(&state, true, Some(true)).await.unwrap();
        assert!(session.self_deafened());
        talking(false).await;
        apply_ptt(&state, &mut arming, false, || true).await;
        set_self_state(&state, true, Some(false)).await.unwrap();
        assert!(!state.voice.lock().await.as_ref().unwrap().deafened);
        drop(operation);

        drop(session);
        stop(&state, None).await.unwrap();
        https.abort();
        task.abort();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end scenario against a real server"
    )]
    #[tokio::test]
    async fn desktop_voice_switches_releases_and_cancels_reconnect_membership() {
        use ss_client_core::storage::Storage;
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
        state.clients().insert(client).unwrap();
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
            assert_eq!(active.session.speaking_users().len(), 0);
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
                if state.clients().get(&key).unwrap().state().read().connection
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
            state.clients().get(&key).is_none(),
            "leaving during retry must cancel desired membership"
        );
        let reopened =
            crate::persistence::DesktopStorage::open(dir.path().join("desktop/sessions.json"))
                .unwrap();
        assert_eq!(reopened.user_volume(&key, own).unwrap(), Some(1.5));
        assert!(reopened.user_muted(&key, own).unwrap());
    }
}
