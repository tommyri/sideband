//! Desktop notifications, the badge and cue playback. What alerts, which
//! cue plays and what the badge counts is decided by
//! [`ss_client_core::notifications`]; this module shows the result. No
//! message contents or activation links are persisted.
use crate::{
    commands::{api_error, parse_server_id},
    state::{AppState, lock},
    types::ApiError,
};
use serde::Serialize;
use ss_client_core::{
    ClientEvent, ConnectionState, StateStore,
    notifications::{self as policy, Preferences, Tracker},
    v1,
    voice::Cue,
};
use ss_core::Id;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

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
    tracker: Tracker,
}

fn runtime(app: &AppHandle) -> std::sync::MutexGuard<'_, Runtime> {
    lock(app.state::<Mutex<Runtime>>().inner())
}

pub fn init(app: &AppHandle) {
    app.manage(Mutex::new(Runtime::default()));
    let handle = app.clone();
    let _ = app.notification().on_action(move |action| {
        let target = action
            .notification()
            .and_then(|n| runtime(&handle).targets.get(&n.id()).cloned());
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
            let Some(store) = state.client(&server).map(|client| client.state()) else {
                return;
            };
            if !policy::can_open(&store.read(), channel) {
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
#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri extracts command arguments by value"
)]
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
    if !preferences.within_limits() {
        return Err(ApiError::new(
            "invalid",
            "Too many notification preferences.",
        ));
    }
    let saved = preferences.clone();
    state
        .storage
        .write(move |storage| storage.save_notifications(saved))
        .await
        .map_err(api_error)?;
    if let Some(active) = state.voice.lock().await.as_ref() {
        active.session.set_cues_enabled(!preferences.dnd);
        active
            .session
            .set_control_cues_enabled(preferences.sounds(active.server.as_str()));
    }
    if preferences.dnd {
        let _ = app.notification().remove_all_active();
        runtime(&app).targets.clear();
    }
    {
        let clients: Vec<_> = state
            .clients()
            .iter()
            .map(|(_, client)| std::sync::Arc::clone(client))
            .collect();
        for client in clients {
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
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri extracts command arguments by value"
)]
pub fn test_notification(app: AppHandle, state: State<'_, AppState>) -> Result<(), ApiError> {
    let prefs = state.storage.notifications();
    if !prefs.desktop_enabled() {
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
    update_badge(app, server, store);
    if let Some(presence) = policy::presence_after(event, &prefs)
        && let Ok(id) = parse_server_id(server)
        && let Some(client) = state.client(&id)
    {
        let _ = client.set_presence(presence).await;
    }
    voice_cues(app, &state, server, store, event, &prefs).await;
    let ClientEvent::MessageReceived {
        channel,
        message: message_id,
    } = event
    else {
        return;
    };
    let (channel, message_id) = (*channel, *message_id);
    notify_message(app, &state, server, store, channel, message_id).await;
}

/// Refresh this server's mention count, drop activation targets the user can
/// no longer open, and show the total on the dock or taskbar badge.
fn update_badge(app: &AppHandle, server: &str, store: &StateStore) {
    // Held across the badge call so concurrent servers set it in order.
    let mut runtime = runtime(app);
    let badge = {
        let view = store.read();
        runtime.targets.retain(|_, t| {
            t.server != server
                || t.channel
                    .parse()
                    .is_ok_and(|channel| policy::can_open(&view, channel))
        });
        runtime.tracker.badge(server, &view)
    };
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_badge_count(badge);
    }
}

/// Play the join, leave, lost or restored cue for the call on this server.
async fn voice_cues(
    app: &AppHandle,
    state: &AppState,
    server: &str,
    store: &StateStore,
    event: &ClientEvent,
    prefs: &Preferences,
) {
    let voice = state.voice.lock().await;
    let active = voice.as_ref().filter(|v| v.server.as_str() == server);
    let call = active.map(|v| v.session.channel());
    let cue = runtime(app)
        .tracker
        .voice_cue(server, call, &store.read(), event, prefs);
    if let Some((active, cue)) = active.zip(cue) {
        active.session.play_cue(cue);
    }
}

/// Play the alert for, and possibly show a desktop notification of, one new
/// message.
async fn notify_message(
    app: &AppHandle,
    state: &AppState,
    server: &str,
    store: &StateStore,
    channel: Id,
    message_id: Id,
) {
    if !runtime(app)
        .tracker
        .first_sighting(&store.read(), server, channel, message_id)
    {
        return;
    }
    let focused = app
        .get_webview_window("main")
        .is_some_and(|w| w.is_focused().unwrap_or(true));
    let active = state.voice.lock().await;
    let prefs = state.storage.notifications();
    let Some(alert) = policy::alert(&store.read(), &prefs, server, channel, message_id, focused)
    else {
        return;
    };
    if let Some(active) = active.as_ref() {
        active.session.play_alert(alert.cue);
    } else {
        play_idle(app, server, alert.cue);
    }
    drop(active);
    let Some(body) = alert.desktop else {
        return;
    };
    let id = {
        let mut runtime = runtime(app);
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
    let _ = app
        .notification()
        .builder()
        .id(id)
        .title("Sideband")
        .body(body)
        .show();
}

// Output-only playback uses the same self-made cue PCM as the voice graph.
// Device opening latency is platform-dependent and remains an installed-app check.
/// Clears the idle-cue flag when the playback task ends.
struct Reset(Arc<AtomicBool>);
impl Drop for Reset {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
fn play_idle(app: &AppHandle, server: &str, cue: Cue) {
    let playing = runtime(app).idle_playing.clone();
    if playing.swap(true, Ordering::AcqRel) {
        return;
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
    policy::presence(&store.read(), dnd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_client_core::notifications::Level;
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
}
