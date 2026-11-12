//! One native listener, bounded semantic diagnostics, and fail-closed PTT delivery.
use crate::{state::AppState, types::ApiError};
use serde::{Deserialize, Serialize};
use ss_client_core::ptt::{PttAction, PttArming};
use ss_input::{Backend, Binding, CapturedBinding, Handle, PttEvent, ShortcutAction};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tauri::{Emitter, Manager};
use tauri_plugin_store::StoreExt;

fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new("input", e.to_string())
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preferences {
    ptt: Option<Binding>,
    mute: Option<Binding>,
    deafen: Option<Binding>,
    portal: bool,
    allow_evdev: bool,
}
impl Preferences {
    fn bindings(&self) -> Vec<Binding> {
        [
            (self.ptt, ShortcutAction::PushToTalk),
            (self.mute, ShortcutAction::ToggleMute),
            (self.deafen, ShortcutAction::ToggleDeafen),
        ]
        .into_iter()
        .filter_map(|(b, a)| b.map(|b| b.with_action(a)))
        .collect()
    }
    fn enabled(&self) -> bool {
        self.portal || self.ptt.is_some() || self.mute.is_some() || self.deafen.is_some()
    }
    fn validate(&self) -> Result<(), ApiError> {
        let bindings = self.bindings();
        for (i, a) in bindings.iter().enumerate() {
            if bindings[..i].iter().any(|b| b.trigger == a.trigger) {
                return Err(error(
                    "Shortcuts sharing a trigger can fire together. Choose different keys or mouse buttons.",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent flags serialized for the webview"
)]
pub struct Snapshot {
    backend: String,
    preferences: Preferences,
    labels: Vec<String>,
    running: bool,
    capturing: bool,
    suspended: bool,
    mouse: bool,
    capture_supported: bool,
    system_binding: bool,
    permission: Option<String>,
    limitations: Vec<String>,
    linux: bool,
    error: String,
    events: Vec<String>,
}
#[derive(Default)]
struct Runtime {
    preferences: Preferences,
    backend: Option<Arc<Backend>>,
    handle: Option<Handle>,
    capturing: bool,
    password: bool,
    suspension_revision: u64,
    generation: u64,
    error: String,
    events: VecDeque<String>,
    /// The listener stopped for want of access: a start that found the
    /// permission missing, or a running tap the backend stopped. Regaining
    /// window focus then rechecks it (T-049, T-108).
    retry_on_focus: bool,
    #[cfg(target_os = "linux")]
    portal_settings: ss_input::linux::portal::Settings,
}
pub struct InputManager {
    runtime: Mutex<Runtime>,
    tx: tokio::sync::mpsc::Sender<(u64, PttEvent)>,
    overflow: AtomicBool,
    inhibited: AtomicBool,
    capture_epoch: AtomicU64,
    pub global_enabled: AtomicBool,
}
impl InputManager {
    fn lock(&self) -> std::sync::MutexGuard<'_, Runtime> {
        crate::state::lock(&self.runtime)
    }
}
fn record(rt: &mut Runtime, text: impl Into<String>) {
    if rt.events.len() == 10 {
        rt.events.pop_front();
    }
    rt.events.push_back(text.into());
}
fn stop(rt: &mut Runtime) {
    rt.generation = rt.generation.wrapping_add(1);
    if let Some(handle) = rt.handle.take() {
        let _ = handle.stop();
    }
}
fn detect(app: &tauri::AppHandle, rt: &mut Runtime) -> Arc<Backend> {
    #[cfg(target_os = "linux")]
    {
        let tx = app.state::<InputManager>().tx.clone();
        let generation = rt.generation;
        let target = app.clone();
        Arc::new(Backend::detect_linux(ss_input::linux::Options {
            allow_evdev: rt.preferences.allow_evdev,
            portal: ss_input::linux::portal::Options {
                push_to_talk: rt.preferences.portal.then(|| "F13".into()),
                toggle_mute: rt.preferences.portal.then(|| "F14".into()),
                toggle_deafen: rt.preferences.portal.then(|| "F15".into()),
                settings: rt.portal_settings.clone(),
                on_action: Some(Arc::new(move |action| {
                    use ss_input::linux::portal::Action;
                    let event = match action {
                        Action::ToggleMute => PttEvent::ToggleMute,
                        Action::ToggleDeafen => PttEvent::ToggleDeafen,
                    };
                    if tx.try_send((generation, event)).is_err() {
                        target
                            .state::<InputManager>()
                            .overflow
                            .store(true, Ordering::Release);
                    }
                })),
            },
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (app, rt);
        Arc::new(Backend::detect())
    }
}
fn start(app: &tauri::AppHandle, rt: &mut Runtime) -> Result<(), ApiError> {
    app.state::<InputManager>()
        .inhibited
        .store(true, Ordering::Release);
    stop(rt);
    rt.error.clear();
    rt.backend = None;
    app.state::<InputManager>()
        .global_enabled
        .store(rt.preferences.enabled(), Ordering::Release);
    if !rt.preferences.enabled() || rt.password || rt.capturing {
        return Ok(());
    }
    let backend = detect(app, rt);
    let caps = backend.capabilities();
    // This attempt prompts once; a grant is picked up on the next focus.
    rt.retry_on_focus = caps.needs_permission.is_some();
    if caps.binding_set_by_system && !rt.preferences.portal {
        rt.backend = Some(backend);
        return Err(error(
            "This compositor owns shortcuts. Enable system-managed shortcuts instead.",
        ));
    }
    if !caps.binding_set_by_system && rt.preferences.portal {
        rt.backend = Some(backend);
        return Err(error(
            "System-managed shortcuts are unavailable in this session.",
        ));
    }
    if !caps.mouse_buttons
        && rt
            .preferences
            .bindings()
            .iter()
            .any(|b| matches!(b.trigger, ss_input::Trigger::Mouse(_)))
    {
        return Err(error("This backend cannot bind mouse buttons"));
    }
    rt.backend = Some(Arc::clone(&backend));
    let tx = app.state::<InputManager>().tx.clone();
    let generation = rt.generation;
    let target = app.clone();
    let handle = backend
        .start(Box::new(move |event| {
            if tx.try_send((generation, event)).is_err() {
                target
                    .state::<InputManager>()
                    .overflow
                    .store(true, Ordering::Release);
            }
        }))
        .map_err(error)?;
    // The audio worker owns the configured tail. Native input only debounces.
    handle.set_release_delay(Duration::ZERO).map_err(error)?;
    if !caps.binding_set_by_system {
        handle
            .update_bindings(&rt.preferences.bindings())
            .map_err(error)?;
    }
    rt.handle = Some(handle);
    rt.retry_on_focus = false;
    app.state::<InputManager>()
        .inhibited
        .store(false, Ordering::Release);
    rt.error.clear();
    record(rt, "Listener started");
    Ok(())
}
/// Whether regaining window focus should start the listener again: it
/// stopped for want of access, and the capability check, which never
/// prompts, now finds the access there. A start while access is still
/// missing would prompt again, so it waits for the next focus.
fn restarts_on_focus(rt: &Runtime, caps: &ss_input::Caps) -> bool {
    rt.retry_on_focus
        && rt.handle.is_none()
        && !rt.password
        && !rt.capturing
        && rt.preferences.enabled()
        && caps.needs_permission.is_none()
}
/// Recheck input access when the main window gains focus, so a grant made
/// in System Settings starts the listener without a restart (T-049).
pub fn recheck(app: &tauri::AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let Some(manager) = app.try_state::<InputManager>() else {
            return;
        };
        let mut rt = manager.lock();
        if !rt.retry_on_focus || rt.handle.is_some() {
            return;
        }
        let backend = rt.backend.clone().unwrap_or_else(|| detect(&app, &mut rt));
        if restarts_on_focus(&rt, &backend.capabilities())
            && let Err(e) = start(&app, &mut rt)
        {
            rt.error = e.message;
        }
    });
}
fn snapshot(app: &tauri::AppHandle, rt: &mut Runtime) -> Snapshot {
    let backend = rt.backend.clone().unwrap_or_else(|| detect(app, rt));
    let caps = backend.capabilities();
    Snapshot {
        backend: backend.name().to_owned(),
        preferences: rt.preferences.clone(),
        labels: [
            rt.preferences.ptt,
            rt.preferences.mute,
            rt.preferences.deafen,
        ]
        .into_iter()
        .map(|b| b.map_or_else(|| "Not bound".into(), |b| b.label()))
        .collect(),
        running: rt.handle.is_some(),
        capturing: rt.capturing,
        suspended: rt.password,
        mouse: caps.mouse_buttons,
        capture_supported: !caps.binding_set_by_system && (caps.passthrough || caps.mouse_buttons),
        system_binding: caps.binding_set_by_system,
        permission: caps.needs_permission.map(|p| p.to_string()),
        limitations: caps.limitations,
        linux: cfg!(target_os = "linux"),
        error: rt.error.clone(),
        events: rt.events.iter().cloned().collect(),
    }
}
async fn release(app: &tauri::AppHandle) {
    if let Some(v) = app.state::<AppState>().voice.lock().await.as_mut() {
        v.release_ptt();
    }
}
pub fn init(app: &tauri::AppHandle) {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    app.manage(InputManager {
        runtime: Mutex::new(Runtime::default()),
        tx,
        overflow: AtomicBool::new(false),
        inhibited: AtomicBool::new(true),
        capture_epoch: AtomicU64::new(0),
        global_enabled: AtomicBool::new(false),
    });
    let target = app.clone();
    tauri::async_runtime::spawn(async move {
        let startup = target.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || {
            let manager = startup.state::<InputManager>();
            let mut rt = manager.lock();
            match startup
                .store("shortcuts.json")
                .map_err(error)
                .and_then(|s| {
                    s.get("preferences")
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(error)
                }) {
                Ok(Some(p)) => {
                    rt.preferences = p;
                    if let Err(e) = rt
                        .preferences
                        .validate()
                        .and_then(|()| start(&startup, &mut rt))
                    {
                        rt.error = e.message;
                    }
                }
                Ok(None) => {}
                Err(e) => rt.error = e.message,
            }
        })
        .await;
        drive(target, rx).await;
    });
}
/// Identifies one call for [`PttArming`].
pub(crate) fn session_identity(voice: &crate::voice::ActiveVoice) -> usize {
    Arc::as_ptr(&voice.session) as usize
}
/// Apply one global PTT key event. Waits only for the voice lock, which no
/// command holds across the network; connection state is read from the
/// call's own store, never through the client map.
pub(crate) async fn apply_ptt(
    state: &AppState,
    arming: &mut PttArming,
    pressed: bool,
    allowed: impl FnOnce() -> bool,
) {
    let mut voice = state.voice.lock().await;
    let Some(active) = voice.as_mut() else {
        return;
    };
    if !allowed() {
        return;
    }
    match arming.key(session_identity(active), active.connected(), pressed) {
        PttAction::Release => active.release_ptt(),
        PttAction::Set(open) => active.set_global_ptt(open),
    }
}
async fn drive(app: tauri::AppHandle, mut rx: tokio::sync::mpsc::Receiver<(u64, PttEvent)>) {
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    let mut arming = PttArming::default();
    let mut tray_state = None;
    loop {
        tokio::select! {
            incoming = rx.recv() => {
                let Some((generation, event)) = incoming else { break };
                if !accept(&app, generation, event) {
                    continue;
                }
                let _ = app.emit("ss://test-shortcut", event.to_string());
                match event {
                    PttEvent::Pressed | PttEvent::Released => {
                        let state = app.state::<AppState>();
                        let inhibited = || app.state::<InputManager>().inhibited.load(Ordering::Acquire);
                        apply_ptt(&state, &mut arming, event == PttEvent::Pressed, || !inhibited()).await;
                    }
                    PttEvent::ToggleMute => toggle_guarded(&app, false, Some(generation)),
                    PttEvent::ToggleDeafen => toggle_guarded(&app, true, Some(generation)),
                }
            }
            _ = tick.tick() => {
                if listener_failed(&app) {
                    release(&app).await;
                    arming.disarm();
                }
                let state = app.state::<AppState>();
                let voice = state.voice.lock().await;
                arming.tick(voice.as_ref().map(session_identity));
                let current = voice.as_ref().map_or((false, false, false), |v| (true, v.muted, v.deafened));
                drop(voice);
                if tray_state != Some(current) {
                    tray_state = Some(current);
                    let (enabled, muted, deaf) = current;
                    if let Some(menu) = app.try_state::<TrayControls>() {
                        let _ = menu.mute.set_enabled(enabled);
                        let _ = menu.deafen.set_enabled(enabled);
                        let _ = menu.mute.set_checked(muted);
                        let _ = menu.deafen.set_checked(deaf);
                    }
                }
            }
        }
    }
}
/// Whether a queued event still belongs to the running, unsuspended listener.
fn accept(app: &tauri::AppHandle, generation: u64, event: PttEvent) -> bool {
    let manager = app.state::<InputManager>();
    let mut rt = manager.lock();
    let allowed = !manager.inhibited.load(Ordering::Acquire)
        && rt.generation == generation
        && rt.handle.is_some()
        && !rt.password
        && !rt.capturing;
    if allowed {
        record(&mut rt, event.to_string());
    }
    allowed
}
/// Stop the listener after a queue overflow or backend error.
fn listener_failed(app: &tauri::AppHandle) -> bool {
    let manager = app.state::<InputManager>();
    let mut rt = manager.lock();
    let overflow = manager.overflow.swap(false, Ordering::AcqRel);
    let failure = overflow
        .then(|| "Input queue overflow; shortcuts stopped".to_owned())
        .or_else(|| {
            rt.handle
                .as_ref()
                .and_then(|h| h.status().err())
                .map(|e| e.to_string())
        });
    if let Some(e) = &failure {
        // The backend stops the tap when access is revoked or secure input
        // starts; regaining focus starts it again once access is back. A
        // portal session is the compositor's, and restarting one can prompt.
        rt.retry_on_focus = !overflow && !rt.preferences.portal;
        manager.inhibited.store(true, Ordering::Release);
        stop(&mut rt);
        rt.error.clone_from(e);
        record(&mut rt, "Listener stopped after backend error");
    }
    failure.is_some()
}
pub struct TrayControls {
    pub mute: tauri::menu::CheckMenuItem<tauri::Wry>,
    pub deafen: tauri::menu::CheckMenuItem<tauri::Wry>,
}
pub fn toggle(app: &tauri::AppHandle, deafen: bool) {
    toggle_guarded(app, deafen, None);
}
/// Toggle mute or deafen from a global shortcut or the tray. It never waits
/// for `operations`, so a connect or join in flight cannot drop the toggle.
fn toggle_guarded(app: &tauri::AppHandle, deafen: bool, generation: Option<u64>) {
    let target = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(generation) = generation {
            let manager = target.state::<InputManager>();
            let rt = manager.lock();
            if manager.inhibited.load(Ordering::Acquire) || rt.generation != generation {
                return;
            }
        }
        let state = target.state::<AppState>();
        if state.voice.lock().await.is_none() {
            return;
        }
        if let Err(e) = crate::voice::set_self_state(&state, deafen, None).await {
            target.state::<InputManager>().lock().error = e.message;
        }
        let _ = target.emit("ss://voice-session", ());
    });
}
#[tauri::command]
pub async fn input_snapshot(app: tauri::AppHandle) -> Result<Snapshot, ApiError> {
    tauri::async_runtime::spawn_blocking(move || {
        let manager = app.state::<InputManager>();
        Ok(snapshot(&app, &mut manager.lock()))
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn save_shortcuts(
    app: tauri::AppHandle,
    preferences: Preferences,
) -> Result<Snapshot, ApiError> {
    preferences.validate()?;
    app.state::<InputManager>()
        .inhibited
        .store(true, Ordering::Release);
    release(&app).await;
    tauri::async_runtime::spawn_blocking(move || {
        let manager = app.state::<InputManager>();
        let mut rt = manager.lock();
        if rt.capturing {
            return Err(error("Finish capture before saving"));
        }
        stop(&mut rt);
        let persist = (|| -> Result<(), ApiError> {
            let store = app
                .store_builder("shortcuts.json")
                .disable_auto_save()
                .build()
                .map_err(error)?;
            let previous = store.get("preferences");
            store.set(
                "preferences",
                serde_json::to_value(&preferences).map_err(error)?,
            );
            if let Err(e) = store.save() {
                if let Some(previous) = previous {
                    store.set("preferences", previous);
                } else {
                    store.delete("preferences");
                }
                return Err(error(e));
            }
            Ok(())
        })();
        if let Err(e) = persist {
            rt.error.clone_from(&e.message);
            return Err(e);
        }
        rt.preferences = preferences;
        if let Err(e) = start(&app, &mut rt) {
            rt.error = e.message;
        }
        Ok(snapshot(&app, &mut rt))
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn suspend_shortcuts(
    app: tauri::AppHandle,
    suspended: bool,
    revision: u64,
) -> Result<(), ApiError> {
    {
        let manager = app.state::<InputManager>();
        let mut rt = manager.lock();
        if revision <= rt.suspension_revision {
            return Ok(());
        }
        rt.suspension_revision = revision;
        if suspended {
            manager.inhibited.store(true, Ordering::Release);
        }
    }
    if suspended {
        release(&app).await;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let manager = app.state::<InputManager>();
        let mut rt = manager.lock();
        if revision != rt.suspension_revision {
            return Ok(());
        }
        if rt.password == suspended {
            return Ok(());
        }
        rt.password = suspended;
        if suspended {
            stop(&mut rt);
            if rt.capturing
                && let Some(b) = &rt.backend
            {
                let _ = b.cancel_capture();
            }
            record(&mut rt, "Suspended for private input");
        } else if let Err(e) = start(&app, &mut rt) {
            rt.error = e.message;
        }
        Ok(())
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn capture_shortcut(app: tauri::AppHandle) -> Result<serde_json::Value, ApiError> {
    let epoch = app
        .state::<InputManager>()
        .capture_epoch
        .fetch_add(1, Ordering::AcqRel)
        + 1;
    app.state::<InputManager>()
        .inhibited
        .store(true, Ordering::Release);
    release(&app).await;
    tauri::async_runtime::spawn_blocking(move || {
        let backend = {
            let manager = app.state::<InputManager>();
            let mut rt = manager.lock();
            if manager.capture_epoch.load(Ordering::Acquire) != epoch {
                manager.inhibited.store(rt.password||rt.capturing||rt.handle.is_none(),Ordering::Release);
                return Err(error("Capture cancelled"));
            }
            if rt.capturing || rt.password {
                return Err(error(
                    "Capture is unavailable while suspended or already capturing",
                ));
            }
            let b = detect(&app, &mut rt);
            let caps = b.capabilities();
            if caps.binding_set_by_system || !(caps.passthrough || caps.mouse_buttons) {
                manager.inhibited.store(rt.password||rt.handle.is_none(),Ordering::Release);
                return Err(error("Binding capture is unavailable on this backend; use system shortcut settings if supported"));
            }
            stop(&mut rt);
            rt.capturing = true;
            rt.backend = Some(Arc::clone(&b));
            b
        };
        let result = backend
            .capture_next(Duration::from_secs(15))
            .map_err(error)
            .and_then(|b| match b {
                CapturedBinding::Binding(b) => {
                    Ok(serde_json::json!({"binding":b,"label":b.label()}))
                }
                CapturedBinding::SetBySystem => {
                    Err(error("Configure shortcuts through the compositor"))
                }
                _ => Err(error("Unsupported capture result")),
            });
        let manager = app.state::<InputManager>();
        let mut rt = manager.lock();
        rt.capturing = false;
        if let Err(e) = start(&app, &mut rt) {
            rt.error = e.message;
        }
        result
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn cancel_shortcut_capture(app: tauri::AppHandle) -> Result<(), ApiError> {
    app.state::<InputManager>()
        .capture_epoch
        .fetch_add(1, Ordering::AcqRel);
    tauri::async_runtime::spawn_blocking(move || {
        let manager = app.state::<InputManager>();
        let rt = manager.lock();
        if rt.capturing
            && let Some(b) = &rt.backend
        {
            b.cancel_capture().map_err(error)?;
        }
        Ok(())
    })
    .await
    .map_err(error)?
}
#[tauri::command]
pub async fn configure_system_shortcuts(app: tauri::AppHandle) -> Result<(), ApiError> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "linux")]
        {
            app.state::<InputManager>()
                .lock()
                .portal_settings
                .configure()
                .map_err(error)
        }
        #[cfg(target_os = "macos")]
        {
            let _ = app;
            std::process::Command::new("/usr/bin/open")
                .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent")
                .spawn()
                .map_err(error)?;
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = app;
            Err(error(
                "No shortcut permission settings are needed on this platform",
            ))
        }
    })
    .await
    .map_err(error)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_do_not_enable_a_listener_and_trigger_conflicts_are_rejected() {
        let mut p = Preferences::default();
        assert!(!p.enabled());
        assert_eq!(p.bindings().len(), 0);
        p.ptt = Some(Binding::key(ss_input::Code::KeyV));
        p.mute = p.ptt;
        assert!(p.validate().is_err());
        p.mute = Some(Binding::key(ss_input::Code::KeyM));
        assert!(p.validate().is_ok());
        assert_eq!(p.bindings()[1].action, ShortcutAction::ToggleMute);
    }
    #[test]
    fn diagnostics_keep_only_ten_semantic_events() {
        let mut rt = Runtime::default();
        for i in 0..15 {
            record(&mut rt, format!("action {i}"));
        }
        assert_eq!(rt.events.len(), 10);
        assert_eq!(rt.events.front().unwrap(), "action 5");
        let generation = rt.generation;
        stop(&mut rt);
        assert_ne!(rt.generation, generation);
    }
    #[test]
    fn regaining_focus_restarts_a_listener_that_waited_for_access_once_it_is_granted() {
        let granted = ss_input::Caps::mock();
        let mut missing = ss_input::Caps::mock();
        missing.needs_permission = Some(ss_input::Permission::InputMonitoring);
        let mut rt = Runtime {
            retry_on_focus: true,
            ..Runtime::default()
        };
        assert!(!restarts_on_focus(&rt, &granted), "nothing is bound");
        rt.preferences.ptt = Some(Binding::key(ss_input::Code::KeyV));
        assert!(
            !restarts_on_focus(&rt, &missing),
            "a start while access is missing would prompt again"
        );
        assert!(
            restarts_on_focus(&rt, &granted),
            "access granted in System Settings starts the listener"
        );
        rt.password = true;
        assert!(
            !restarts_on_focus(&rt, &granted),
            "suspended for a password"
        );
        rt.password = false;
        rt.capturing = true;
        assert!(
            !restarts_on_focus(&rt, &granted),
            "binding capture owns input"
        );
        rt.capturing = false;
        rt.retry_on_focus = false;
        assert!(
            !restarts_on_focus(&rt, &granted),
            "other start failures are not retried on every focus"
        );
    }
}
