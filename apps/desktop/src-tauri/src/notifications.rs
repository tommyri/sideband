//! Local notification policy. No message contents or activation links are persisted.
use crate::{
    commands::{api_error, parse_server_id},
    state::AppState,
    types::ApiError,
};
use serde::{Deserialize, Serialize};
use ss_client_core::{ClientEvent, ConnectionState, StateStore, v1, voice::Cue};
use ss_core::{Id, Permissions};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

#[derive(Clone, Copy, Default, Deserialize, Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    All,
    #[default]
    Mentions,
    None,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    pub desktop: bool,
    pub previews: bool,
    pub dnd: bool,
    pub muted_servers: BTreeSet<String>,
    pub channels: BTreeMap<String, Level>,
}
impl Preferences {
    fn allows(&self, server: &str, channel: &str, mentioned: bool, dm: bool) -> bool {
        if self.dnd || self.muted_servers.contains(server) {
            return false;
        }
        match self
            .channels
            .get(&format!("{server}/{channel}"))
            .copied()
            .unwrap_or_default()
        {
            Level::All => true,
            Level::Mentions => mentioned || dm,
            Level::None => false,
        }
    }
    pub fn sounds(&self, server: &str) -> bool {
        !self.dnd && !self.muted_servers.contains(server)
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Target {
    server: String,
    channel: String,
    message: String,
}
#[derive(Default)]
struct Runtime {
    idle_playing: Arc<AtomicBool>,
    next: i32,
    targets: BTreeMap<i32, Target>,
    counts: BTreeMap<String, i64>,
    voices: BTreeMap<String, (Id, BTreeSet<Id>)>,
    seen: BTreeSet<(String, Id)>,
}

pub fn init(app: &AppHandle) {
    app.manage(Mutex::new(Runtime::default()));
    let handle = app.clone();
    let _ = app.notification().on_action(move |action| {
        let target = action.notification().and_then(|n| {
            handle
                .state::<Mutex<Runtime>>()
                .lock()
                .unwrap()
                .targets
                .get(&n.id())
                .cloned()
        });
        let Some(target) = target else {
            return;
        };
        let app = handle.clone();
        tauri::async_runtime::spawn(async move {
            let state = app.state::<AppState>();
            let Ok(server) = parse_server_id(&target.server) else {
                return;
            };
            let Ok(channel) = target.channel.parse::<Id>() else {
                return;
            };
            let clients = state.clients.lock().await;
            let Some(client) = clients.get(&server) else {
                return;
            };
            let store = client.state();
            if !accessible(&store, channel) {
                return;
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            let _ = app.emit("ss://notification-activate", target);
        });
    });
}
fn accessible(store: &StateStore, channel: Id) -> bool {
    let view = store.read();
    view.connection == ConnectionState::Connected
        && view
            .permissions(channel)
            .is_some_and(|p| p.contains(Permissions::VIEW_CHANNEL))
}
#[tauri::command]
pub fn notification_preferences(state: State<'_, AppState>) -> Preferences {
    state.storage.notifications()
}
#[tauri::command]
pub async fn save_notification_preferences(
    app: AppHandle,
    state: State<'_, AppState>,
    preferences: Preferences,
) -> Result<(), ApiError> {
    let _operation = state.operations.lock().await;
    if preferences.channels.len() > 10000
        || preferences.muted_servers.len() > 1000
        || preferences.channels.keys().any(|k| k.len() > 512)
    {
        return Err(ApiError::new(
            "invalid",
            "Too many notification preferences.",
        ));
    }
    state
        .storage
        .save_notifications(preferences.clone())
        .map_err(api_error)?;
    if let Some(active) = state.voice.lock().await.as_ref() {
        active.session.set_cues_enabled(!preferences.dnd);
        active
            .session
            .set_control_cues_enabled(preferences.sounds(active.server.as_str()));
    }
    if preferences.dnd {
        let _ = app.notification().remove_all_active();
        app.state::<Mutex<Runtime>>()
            .lock()
            .unwrap()
            .targets
            .clear();
    }
    {
        for (_, client) in state.clients.lock().await.iter() {
            if client.state().read().connection == ConnectionState::Connected {
                client
                    .set_presence(presence(&client.state(), preferences.dnd))
                    .await
                    .map_err(api_error)?;
            }
        }
    }
    Ok(())
}
#[tauri::command]
pub fn test_notification(app: AppHandle, state: State<'_, AppState>) -> Result<(), ApiError> {
    let prefs = state.storage.notifications();
    if !prefs.desktop || prefs.dnd {
        return Err(ApiError::new(
            "disabled",
            "Enable desktop notifications and turn off Do not disturb first.",
        ));
    }
    // Deliberate user action only. Desktop OS settings remain authoritative.
    app.notification()
        .request_permission()
        .map_err(|e| ApiError::new("notification", e.to_string()))?;
    app.notification()
        .builder()
        .title("Sideband")
        .body("Test notification — your messages stay private by default.")
        .show()
        .map_err(|e| ApiError::new("notification", e.to_string()))
}
pub async fn observe(app: &AppHandle, server: &str, store: &StateStore, event: &ClientEvent) {
    let state = app.state::<AppState>();
    let prefs = state.storage.notifications();
    let connected = store.read().connection == ConnectionState::Connected;
    let count = {
        let view = store.read();
        if connected {
            view.read_states
                .iter()
                .filter(|(channel, _)| {
                    view.permissions(**channel)
                        .is_some_and(|p| p.contains(Permissions::VIEW_CHANNEL))
                })
                .map(|(_, r)| i64::from(r.mention_count))
                .sum()
        } else {
            0
        }
    };
    {
        let runtime = app.state::<Mutex<Runtime>>();
        let mut runtime = runtime.lock().unwrap();
        runtime.counts.insert(server.to_owned(), count);
        runtime.targets.retain(|_, t| {
            t.server != server
                || t.channel
                    .parse()
                    .is_ok_and(|channel| accessible(store, channel))
        });
        if let Some(window) = app.get_webview_window("main") {
            let total = runtime.counts.values().sum::<i64>();
            let _ = window.set_badge_count((total > 0).then_some(total));
        }
    }
    if matches!(event, ClientEvent::Resynced)
        && prefs.dnd
        && let Ok(id) = parse_server_id(server)
        && let Some(client) = state.clients.lock().await.get(&id)
    {
        let _ = client
            .set_presence(v1::SetPresence {
                status: v1::PresenceStatus::Dnd as i32,
                activity: String::new(),
            })
            .await;
    }
    if let Some(active) = state
        .voice
        .lock()
        .await
        .as_ref()
        .filter(|v| v.server.as_str() == server)
    {
        let members = {
            let view = store.read();
            view.voice_states
                .values()
                .filter(|v| {
                    v.channel_id.as_deref().and_then(|id| Id::try_from(id).ok())
                        == Some(active.session.channel())
                })
                .filter_map(|v| Id::try_from(v.user_id.as_ref()).ok())
                .collect::<BTreeSet<_>>()
        };
        let previous = app
            .state::<Mutex<Runtime>>()
            .lock()
            .unwrap()
            .voices
            .insert(server.into(), (active.session.channel(), members.clone()));
        if prefs.sounds(server) {
            if matches!(event, ClientEvent::VoiceChanged)
                && let Some((channel, previous)) =
                    previous.filter(|(channel, _)| *channel == active.session.channel())
            {
                let _ = channel;
                if members.difference(&previous).next().is_some() {
                    active.session.play_cue(Cue::Join);
                } else if previous.difference(&members).next().is_some() {
                    active.session.play_cue(Cue::Leave);
                }
            }
            if matches!(
                event,
                ClientEvent::Connection(ConnectionState::Reconnecting { attempt: 1, .. })
            ) {
                active.session.play_cue(Cue::Lost);
            }
            if matches!(event, ClientEvent::Resynced) {
                active.session.play_cue(Cue::Restored);
            }
        }
    } else {
        app.state::<Mutex<Runtime>>()
            .lock()
            .unwrap()
            .voices
            .remove(server);
    }
    let ClientEvent::MessageReceived {
        channel,
        message: message_id,
    } = event
    else {
        return;
    };
    let (channel, message_id) = (*channel, *message_id);
    if !accessible(store, channel) {
        return;
    }
    {
        let runtime = app.state::<Mutex<Runtime>>();
        let mut runtime = runtime.lock().unwrap();
        if !runtime.seen.insert((server.into(), message_id)) {
            return;
        }
        if runtime.seen.len() > 512 {
            let first = runtime.seen.first().cloned().unwrap();
            runtime.seen.remove(&first);
        }
    }
    let active = state.voice.lock().await;
    if !accessible(store, channel) {
        return;
    }
    let prefs = state.storage.notifications();
    // A queued event may lag an edit, deletion, or visibility change. Project
    // the current cache rather than exposing its earlier body.
    let Some(message) = store.read().messages.get(&channel).and_then(|cache| {
        cache
            .ordered()
            .into_iter()
            .find(|m| Id::try_from(m.id.as_ref()).ok() == Some(message_id))
            .cloned()
    }) else {
        return;
    };
    let mentioned = store.read().mentions_me(&message);
    let dm = {
        let view = store.read();
        if message.author.as_ref().is_some_and(|author| {
            view.own_user
                .as_ref()
                .is_some_and(|own| own.id == author.id)
        }) {
            return;
        }
        view.direct_messages.contains_key(&channel)
    };
    if !prefs.allows(server, &channel.to_string(), mentioned, dm) {
        return;
    }
    let cue = if dm { Cue::DirectMessage } else { Cue::Mention };
    if let Some(active) = active.as_ref() {
        active.session.play_alert(cue);
    } else {
        play_idle(app, server, cue);
    }
    drop(active);
    if !prefs.desktop
        || app
            .get_webview_window("main")
            .is_some_and(|w| w.is_focused().unwrap_or(true))
    {
        return;
    }
    let id = {
        let runtime = app.state::<Mutex<Runtime>>();
        let mut runtime = runtime.lock().unwrap();
        runtime.next = runtime.next.checked_add(1).unwrap_or(1);
        let id = runtime.next;
        if runtime.targets.len() >= 128
            && let Some(old) = runtime.targets.keys().next().copied()
        {
            runtime.targets.remove(&old);
        }
        runtime.targets.insert(
            id,
            Target {
                server: server.into(),
                channel: channel.to_string(),
                message: message_id.to_string(),
            },
        );
        id
    };
    let body: String = if prefs.previews {
        message
            .content
            .chars()
            .filter(|c| !c.is_control())
            .take(180)
            .collect()
    } else if dm {
        "You received a direct message.".into()
    } else {
        "You have a new channel message.".into()
    };
    let _ = app
        .notification()
        .builder()
        .id(id)
        .title("Sideband")
        .body(body)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_survive_restart_without_message_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let storage = crate::persistence::DesktopStorage::open(&path).unwrap();
        assert!(!storage.notifications().desktop && !storage.notifications().previews);
        let mut prefs = Preferences {
            dnd: true,
            ..Default::default()
        };
        prefs.channels.insert("s/c".into(), Level::None);
        storage.save_notifications(prefs).unwrap();
        let restored = crate::persistence::DesktopStorage::open(&path)
            .unwrap()
            .notifications();
        assert!(restored.dnd);
        assert_eq!(restored.channels["s/c"], Level::None);
    }
    #[test]
    fn policy_honors_defaults_overrides_and_suppression() {
        let mut p = Preferences::default();
        assert!(!p.allows("s", "c", false, false));
        assert!(p.allows("s", "c", true, false));
        assert!(p.allows("s", "c", false, true));
        p.channels.insert("s/c".into(), Level::None);
        assert!(!p.allows("s", "c", true, true));
        p.channels.insert("s/c".into(), Level::All);
        assert!(p.allows("s", "c", false, false));
        p.muted_servers.insert("s".into());
        assert!(!p.allows("s", "c", true, true));
        p.muted_servers.clear();
        p.dnd = true;
        assert!(!p.allows("s", "c", true, true));
    }
}

// Output-only playback uses the same self-made cue PCM as the voice graph.
// Device opening latency is platform-dependent and remains an installed-app check.
fn play_idle(app: &AppHandle, server: &str, cue: Cue) {
    let playing = app
        .state::<Mutex<Runtime>>()
        .lock()
        .unwrap()
        .idle_playing
        .clone();
    if playing.swap(true, Ordering::AcqRel) {
        return;
    }
    struct Reset(Arc<AtomicBool>);
    impl Drop for Reset {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let app = app.clone();
    let server = server.to_owned();
    tauri::async_runtime::spawn(async move {
        let _reset = Reset(playing);
        let state = app.state::<AppState>();
        let Ok(preferences) = crate::audio::load(&app) else {
            return;
        };
        if preferences.cues == 0
            || preferences.master == 0
            || !state.storage.notifications().sounds(&server)
        {
            return;
        }
        // Serialize output-only cues with device tests and call setup. Never open capture.
        let _operation = state.operations.lock().await;
        if state.voice.lock().await.is_some() || state.audio_test.lock().await.is_some() {
            return;
        }
        let target = app.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            use ss_audio::device::{DeviceManager, GraphConfig};
            let devices = DeviceManager::available_devices();
            let output = match preferences.output.as_deref() {
                Some(id) => match devices.iter().find(|d| d.output && d.id.as_str() == id) {
                    Some(d) => Some(d.id.clone()),
                    None => return,
                },
                None => None,
            };
            let Ok((_stream, mut producer)) =
                DeviceManager::new().open_playback(output.as_ref(), GraphConfig::STEREO)
            else {
                return;
            };
            let gain = f32::from(preferences.master) * f32::from(preferences.cues) / 10000.0;
            for frame in cue.frames() {
                if !target
                    .state::<AppState>()
                    .storage
                    .notifications()
                    .sounds(&server)
                {
                    return;
                }
                for sample in frame {
                    let _ = producer.push((sample * gain).clamp(-1.0, 1.0));
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        })
        .await;
    });
}

pub fn presence(store: &StateStore, dnd: bool) -> v1::SetPresence {
    let view = store.read();
    let activity = view
        .own_user
        .as_ref()
        .and_then(|own| Id::try_from(own.id.as_ref()).ok())
        .and_then(|id| view.members.get(&id))
        .and_then(|m| m.presence.as_ref())
        .map(|p| p.activity.clone())
        .unwrap_or_default();
    v1::SetPresence {
        status: if dnd {
            v1::PresenceStatus::Dnd
        } else {
            v1::PresenceStatus::Online
        } as i32,
        activity,
    }
}
