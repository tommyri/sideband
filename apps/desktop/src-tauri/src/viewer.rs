//! Independent native viewer windows; T-073 supplies the channel/source UI.
use crate::{
    commands::parse_server_id,
    state::{AppState, lock},
    types::ApiError,
    viewer_gpu::Gpu,
};
use serde::Serialize;
use ss_client_core::media::{GroupReader, VideoSubscription};
use ss_video::decode::{Playout, Timing, VideoDecoder, delta};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{
    Emitter, Manager, State, WindowEvent,
    menu::{CheckMenuItem, Menu, Submenu},
};

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerStats {
    pub label: String,
    pub server: String,
    pub track: u32,
    pub backend: String,
    pub state: String,
    pub fps: f64,
    pub bitrate: f64,
    pub dropped: u64,
    pub decode_errors: u64,
    /// Estimated video minus mixed-audio time; hardware output latency is excluded.
    pub skew_ms: Option<i32>,
}
#[derive(Default)]
pub struct Viewers(Mutex<BTreeMap<String, Arc<Mutex<ViewerStats>>>>);
impl Viewers {
    fn registry(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Arc<Mutex<ViewerStats>>>> {
        lock(&self.0)
    }
}
// Close the window and remove diagnostics if any setup step fails.
struct SetupGuard(Option<Arc<tauri::Window>>);
impl Drop for SetupGuard {
    fn drop(&mut self) {
        if let Some(window) = self.0.take() {
            window
                .app_handle()
                .state::<Viewers>()
                .registry()
                .remove(window.label());
            let _ = window.close();
        }
    }
}
struct Controls {
    closed: AtomicBool,
    overlay: AtomicBool,
}
fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new("viewer", e.to_string())
}
#[tauri::command]
#[allow(
    clippy::needless_pass_by_value,
    reason = "Tauri extracts command arguments by value"
)]
pub fn stream_viewers(state: State<'_, Viewers>) -> Vec<ViewerStats> {
    state.registry().values().map(|s| lock(s).clone()).collect()
}
/// The viewer window's View menu: fullscreen, always on top, statistics.
fn install_menu(
    app: &tauri::AppHandle,
    window: &Arc<tauri::Window>,
    label: &str,
    controls: &Arc<Controls>,
) -> Result<(), ApiError> {
    let fullscreen = CheckMenuItem::with_id(
        app,
        format!("{label}-fullscreen"),
        "Fullscreen",
        true,
        false,
        Some("F11"),
    )
    .map_err(error)?;
    let top = CheckMenuItem::with_id(
        app,
        format!("{label}-top"),
        "Always on top",
        true,
        false,
        None::<&str>,
    )
    .map_err(error)?;
    let overlay = CheckMenuItem::with_id(
        app,
        format!("{label}-stats"),
        "Show stream statistics",
        true,
        true,
        Some("CmdOrCtrl+I"),
    )
    .map_err(error)?;
    let menu = Menu::with_items(
        app,
        &[
            &Submenu::with_items(app, "View", true, &[&fullscreen, &top, &overlay])
                .map_err(error)?,
        ],
    )
    .map_err(error)?;
    window.set_menu(menu).map_err(error)?;
    let fullscreen_id = fullscreen.id().as_ref().to_owned();
    let top_id = top.id().as_ref().to_owned();
    let stats_id = overlay.id().as_ref().to_owned();
    let weak = Arc::downgrade(window);
    let flags = controls.clone();
    window.on_menu_event(move |_, event| {
        if let Some(w) = weak.upgrade() {
            match event.id.as_ref() {
                id if id == fullscreen_id => {
                    let next = !w.is_fullscreen().unwrap_or(false);
                    let _ = w.set_fullscreen(next);
                    let _ = fullscreen.set_checked(next);
                }
                id if id == top_id => {
                    let next = !top.is_checked().unwrap_or(false);
                    let _ = w.set_always_on_top(next);
                    let _ = top.set_checked(next);
                }
                id if id == stats_id => {
                    let next = !flags.overlay.load(Ordering::Relaxed);
                    flags.overlay.store(next, Ordering::Relaxed);
                    let _ = overlay.set_checked(next);
                }
                _ => {}
            }
        }
    });
    Ok(())
}
#[tauri::command]
pub async fn open_stream_viewer(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    server_id: String,
    track_id: u32,
) -> Result<String, ApiError> {
    let server = parse_server_id(&server_id)?;
    let subscription = {
        let client = state
            .client(&server)
            .ok_or_else(|| error("Server disconnected"))?;
        client.prepare_video(track_id).map_err(error)?
    };
    subscription.subscribe().await.map_err(error)?;
    let label = format!("viewer-{}", ss_core::Id::generate());
    let window = Arc::new(
        tauri::WindowBuilder::new(&app, &label)
            .title("Sideband — screen share")
            .inner_size(960.0, 600.0)
            .min_inner_size(320.0, 200.0)
            .build()
            .map_err(error)?,
    );
    let mut setup = SetupGuard(Some(window.clone()));
    let controls = Arc::new(Controls {
        closed: AtomicBool::new(false),
        overlay: AtomicBool::new(true),
    });
    install_menu(&app, &window, &label, &controls)?;
    let flags = controls.clone();
    window.on_window_event(move |event| {
        if matches!(
            event,
            WindowEvent::Destroyed | WindowEvent::CloseRequested { .. }
        ) {
            flags.closed.store(true, Ordering::Relaxed);
        }
    });
    let shared = Arc::new(Mutex::new(ViewerStats {
        label: label.clone(),
        server: server_id,
        track: track_id,
        state: "reconnecting".into(),
        ..Default::default()
    }));
    app.state::<Viewers>()
        .registry()
        .insert(label.clone(), shared.clone());
    // Surface/layer creation is main-thread bound on macOS. Decode/render resources
    // stay on the dedicated viewer thread for their entire lifetime on every OS.
    let instance = wgpu::Instance::default();
    let create = instance.clone();
    let target = window.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(create.create_surface(target).map_err(|e| e.to_string()));
    })
    .map_err(error)?;
    let surface = rx.await.map_err(error)?.map_err(error)?;
    let (ready, started) = tokio::sync::oneshot::channel();
    let name = label.clone();
    std::thread::Builder::new()
        .name("screen-viewer".into())
        .spawn(move || {
            let mut ready = Some(ready);
            let result = (|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(async {
                    let gpu = Gpu::new(window.clone(), instance, surface).await?;
                    let codec = subscription
                        .track
                        .codec
                        .as_ref()
                        .ok_or("Missing video codec")?;
                    let decoder = VideoDecoder::open(&codec.name, codec.description.clone(), false)
                        .map_err(|e| e.to_string())?;
                    lock(&shared).backend = decoder.backend().into();
                    if let Some(ready) = ready.take() {
                        let _ = ready.send(Ok(()));
                    }
                    run(subscription, gpu, decoder, controls, shared).await
                })
            })();
            if let Some(ready) = ready {
                let _ = ready.send(result.clone());
            }
            if let Err(message) = result {
                let _ = app.emit("ss://viewer-error", (name.clone(), message));
            }
            let _ = window.close();
            app.state::<Viewers>().registry().remove(&name);
        })
        .map_err(error)?;
    started.await.map_err(error)?.map_err(error)?;
    setup.0 = None;
    Ok(label)
}
/// Decode and presentation state for one viewer window.
struct Playback {
    group: Option<GroupReader>,
    picture: Option<ss_video::Frame>,
    queued: Option<ss_video::Frame>,
    timing: Playout,
    last_frame: Instant,
    last_request: Option<Instant>,
    reconnecting: bool,
    bytes: u32,
    frames: u32,
    measure: Instant,
}

/// Position on the sender's wrapping 48 kHz timeline.
fn ticks_48k(micros: u128) -> u32 {
    u32::try_from((micros * 48 / 1000) % (1 << 32)).unwrap_or_default()
}

impl Playback {
    fn new() -> Self {
        Self {
            group: None,
            picture: None,
            queued: None,
            timing: Playout::default(),
            last_frame: Instant::now(),
            last_request: None,
            reconnecting: true,
            bytes: 0,
            frames: 0,
            measure: Instant::now(),
        }
    }

    /// Drop everything shown or queued until the next keyframe arrives.
    fn interrupt(&mut self) {
        self.group = None;
        self.queued = None;
        self.picture = None;
        self.reconnecting = true;
    }

    fn start_group(
        &mut self,
        group: GroupReader,
        decoder: &mut VideoDecoder,
    ) -> Result<(), String> {
        self.group = Some(group);
        self.queued = None;
        decoder.reset().map_err(|e| e.to_string())?;
        // Sender starts each group with an IDR, so stale references never cross resets.
        self.timing.reset();
        Ok(())
    }

    fn receive(
        &mut self,
        frame: Result<Option<ss_client_core::media::Frame>, ss_client_core::media::Error>,
        decoder: &mut VideoDecoder,
        stats: &Mutex<ViewerStats>,
    ) -> Result<(), String> {
        match frame {
            Ok(Some(frame)) => {
                let size = u32::try_from(frame.payload.len()).unwrap_or(u32::MAX);
                self.bytes = self.bytes.saturating_add(size);
                if let Ok(decoded) = decoder.decode(&frame.payload, frame.timestamp, frame.keyframe)
                {
                    for picture in decoded {
                        if self.queued.is_some() {
                            lock(stats).dropped += 1;
                        }
                        self.queued = Some(picture);
                    }
                    self.last_frame = Instant::now();
                } else {
                    lock(stats).decode_errors += 1;
                    self.interrupt();
                    decoder.recover().map_err(|e| e.to_string())?;
                    lock(stats).backend = decoder.backend().into();
                }
            }
            Ok(None) => self.group = None,
            Err(_) => {
                self.interrupt();
                decoder.reset().map_err(|e| e.to_string())?;
                lock(stats).backend = decoder.backend().into();
            }
        }
        Ok(())
    }

    /// Present or drop the queued picture against the audio clock.
    fn schedule(&mut self, subscription: &VideoSubscription, stats: &Mutex<ViewerStats>) {
        if self.last_frame.elapsed() > Duration::from_secs(1) {
            self.reconnecting = true;
            self.picture = None;
            self.queued = None;
        }
        let Some(frame) = self.queued.as_ref() else {
            return;
        };
        let timestamp = ticks_48k(frame.timestamp.as_micros());
        let audio = subscription
            .audio_clock()
            .and_then(ss_client_core::media::AudioClock::now);
        match self.timing.timing(timestamp, Instant::now(), audio) {
            Timing::Present => {
                lock(stats).skew_ms = audio.map(|a| delta(timestamp, a) / 48);
                self.picture = self.queued.take();
                self.frames = self.frames.saturating_add(1);
                self.reconnecting = false;
            }
            Timing::Drop => {
                self.queued = None;
                lock(stats).dropped += 1;
            }
            Timing::Wait(_) => {}
        }
    }

    /// Update the shared statistics and return the overlay text.
    fn overlay(&mut self, stats: &Mutex<ViewerStats>, shown: bool) -> Option<String> {
        let mut s = lock(stats);
        s.state = if self.reconnecting {
            "reconnecting"
        } else {
            "playing"
        }
        .into();
        if self.measure.elapsed() >= Duration::from_secs(1) {
            let seconds = self.measure.elapsed().as_secs_f64();
            s.fps = f64::from(self.frames) / seconds;
            s.bitrate = f64::from(self.bytes) * 8.0 / seconds;
            self.frames = 0;
            self.bytes = 0;
            self.measure = Instant::now();
        }
        if self.reconnecting {
            Some("Reconnecting stream...".to_owned())
        } else if shown {
            Some(format!(
                "{} {:.0} FPS {:.1} Mb/s skew {} ms dropped {}",
                s.backend,
                s.fps,
                s.bitrate / 1_000_000.0,
                s.skew_ms.map_or_else(|| "n/a".into(), |v| v.to_string()),
                s.dropped
            ))
        } else {
            None
        }
    }

    /// Whether to ask the sender for a keyframe now, at most once a second.
    fn keyframe_due(&mut self) -> bool {
        let due = self.reconnecting
            && self
                .last_request
                .is_none_or(|at| at.elapsed() > Duration::from_secs(1));
        if due {
            self.last_request = Some(Instant::now());
        }
        due
    }
}

async fn run(
    subscription: VideoSubscription,
    mut gpu: Gpu,
    mut decoder: VideoDecoder,
    controls: Arc<Controls>,
    stats: Arc<Mutex<ViewerStats>>,
) -> Result<(), String> {
    let mut playback = Playback::new();
    let mut tick = tokio::time::interval(Duration::from_millis(8));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if controls.closed.load(Ordering::Relaxed) || !subscription.valid() {
            return Ok(());
        }
        let group = playback.group.as_mut();
        tokio::select! {
            next = subscription.next_group() => {
                let Some(next) = next else { return Ok(()) };
                playback.start_group(next, &mut decoder)?;
            }
            frame = async {
                match group {
                    Some(group) => group.next_frame().await,
                    None => std::future::pending().await,
                }
            } => playback.receive(frame, &mut decoder, &stats)?,
            _ = tick.tick() => {
                if !subscription.valid() {
                    return Ok(());
                }
                playback.schedule(&subscription, &stats);
                let text = playback.overlay(&stats, controls.overlay.load(Ordering::Relaxed));
                gpu.draw(playback.picture.as_ref(), text.as_deref())?;
                if playback.keyframe_due() {
                    let _ = tokio::time::timeout(
                        Duration::from_millis(500),
                        subscription.request_keyframe(),
                    )
                    .await;
                }
            }
        }
    }
}
