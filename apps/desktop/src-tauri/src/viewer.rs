//! Independent native viewer windows; T-073 supplies the channel/source UI.
use crate::{commands::parse_server_id, state::AppState, types::ApiError, viewer_gpu::Gpu};
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
// Close the window and remove diagnostics if any setup step fails.
struct SetupGuard(Option<Arc<tauri::Window>>);
impl Drop for SetupGuard {
    fn drop(&mut self) {
        if let Some(window) = self.0.take() {
            window
                .app_handle()
                .state::<Viewers>()
                .0
                .lock()
                .unwrap()
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
pub fn stream_viewers(state: State<'_, Viewers>) -> Vec<ViewerStats> {
    state
        .0
        .lock()
        .unwrap()
        .values()
        .map(|s| s.lock().unwrap().clone())
        .collect()
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
        let clients = state.clients.lock().await;
        let client = clients
            .get(&server)
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
    let fullscreen = CheckMenuItem::with_id(
        &app,
        format!("{label}-fullscreen"),
        "Fullscreen",
        true,
        false,
        Some("F11"),
    )
    .map_err(error)?;
    let top = CheckMenuItem::with_id(
        &app,
        format!("{label}-top"),
        "Always on top",
        true,
        false,
        None::<&str>,
    )
    .map_err(error)?;
    let overlay = CheckMenuItem::with_id(
        &app,
        format!("{label}-stats"),
        "Show stream statistics",
        true,
        true,
        Some("CmdOrCtrl+I"),
    )
    .map_err(error)?;
    let menu = Menu::with_items(
        &app,
        &[
            &Submenu::with_items(&app, "View", true, &[&fullscreen, &top, &overlay])
                .map_err(error)?,
        ],
    )
    .map_err(error)?;
    window.set_menu(menu).map_err(error)?;
    let fullscreen_id = fullscreen.id().as_ref().to_owned();
    let top_id = top.id().as_ref().to_owned();
    let stats_id = overlay.id().as_ref().to_owned();
    let weak = Arc::downgrade(&window);
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
    let flags = controls.clone();
    window.on_window_event(move |event| {
        if matches!(
            event,
            WindowEvent::Destroyed | WindowEvent::CloseRequested { .. }
        ) {
            flags.closed.store(true, Ordering::Relaxed);
        }
    });
    let stats = Arc::new(Mutex::new(ViewerStats {
        label: label.clone(),
        server: server_id,
        track: track_id,
        state: "reconnecting".into(),
        ..Default::default()
    }));
    app.state::<Viewers>()
        .0
        .lock()
        .unwrap()
        .insert(label.clone(), stats.clone());
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
                    stats.lock().unwrap().backend = decoder.backend().into();
                    let _ = ready.take().unwrap().send(Ok(()));
                    run(subscription, gpu, decoder, controls, stats).await
                })
            })();
            if let Some(ready) = ready {
                let _ = ready.send(result.clone());
            }
            if let Err(message) = result {
                let _ = app.emit("ss://viewer-error", (name.clone(), message));
            }
            let _ = window.close();
            app.state::<Viewers>().0.lock().unwrap().remove(&name);
        })
        .map_err(error)?;
    started.await.map_err(error)?.map_err(error)?;
    setup.0 = None;
    Ok(label)
}
async fn run(
    subscription: VideoSubscription,
    mut gpu: Gpu,
    mut decoder: VideoDecoder,
    controls: Arc<Controls>,
    stats: Arc<Mutex<ViewerStats>>,
) -> Result<(), String> {
    let mut group: Option<GroupReader> = None;
    let mut picture = None;
    let mut queued = None;
    let mut timing = Playout::default();
    let mut tick = tokio::time::interval(Duration::from_millis(8));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_frame = Instant::now();
    let mut last_request = Instant::now() - Duration::from_secs(2);
    let mut reconnecting = true;
    let mut bytes = 0_u64;
    let mut frames = 0_u64;
    let mut measure = Instant::now();
    loop {
        if controls.closed.load(Ordering::Relaxed) || !subscription.valid() {
            return Ok(());
        }
        tokio::select! {
            next=subscription.next_group()=>{
                let Some(next)=next else {return Ok(());};
                group=Some(next);queued=None;decoder.reset().map_err(|e|e.to_string())?;
                // Sender starts each group with an IDR, so stale references never cross resets.
                timing.reset();
            },
            next=async {match group.as_mut(){Some(g)=>g.next_frame().await,None=>std::future::pending().await}}=>{
                match next {
                    Ok(Some(frame))=>{
                        bytes+=frame.payload.len() as u64;
                        match decoder.decode(&frame.payload,frame.timestamp,frame.keyframe){
                            Ok(decoded)=>{for frame in decoded {if queued.is_some(){stats.lock().unwrap().dropped+=1;}queued=Some(frame);}last_frame=Instant::now();},
                            Err(_)=>{stats.lock().unwrap().decode_errors+=1;group=None;queued=None;picture=None;reconnecting=true;decoder.recover().map_err(|e|e.to_string())?;stats.lock().unwrap().backend=decoder.backend().into();}
                        }
                    },
                    Ok(None)=>{group=None;},
                    Err(_)=>{group=None;queued=None;picture=None;reconnecting=true;decoder.reset().map_err(|e|e.to_string())?;stats.lock().unwrap().backend=decoder.backend().into();}
                }
            },
            _=tick.tick()=>{
                if !subscription.valid(){return Ok(());}
                if last_frame.elapsed()>Duration::from_secs(1){reconnecting=true;picture=None;queued=None;}
                if let Some(frame)=queued.as_ref(){
                    let timestamp=(frame.timestamp.as_micros()*48/1000) as u32;
                    let audio=subscription.audio_clock().and_then(|c|c.now());
                    match timing.timing(timestamp,Instant::now(),audio){
                        Timing::Present=>{stats.lock().unwrap().skew_ms=audio.map(|a|delta(timestamp,a)/48);picture=queued.take();frames+=1;reconnecting=false;},
                        Timing::Drop=>{queued=None;stats.lock().unwrap().dropped+=1;},Timing::Wait(_)=>{}
                    }
                }
                let text={let mut s=stats.lock().unwrap();s.state=if reconnecting{"reconnecting"}else{"playing"}.into();
                    if measure.elapsed()>=Duration::from_secs(1){let seconds=measure.elapsed().as_secs_f64();s.fps=frames as f64/seconds;s.bitrate=bytes as f64*8.0/seconds;frames=0;bytes=0;measure=Instant::now();}
                    if reconnecting {Some("Reconnecting stream...".to_owned())}else if controls.overlay.load(Ordering::Relaxed){Some(format!("{} {:.0} FPS {:.1} Mb/s skew {} ms dropped {}",s.backend,s.fps,s.bitrate/1_000_000.0,s.skew_ms.map_or("n/a".into(),|v|v.to_string()),s.dropped))}else{None}};
                gpu.draw(picture.as_ref(),text.as_deref())?;
                if reconnecting && last_request.elapsed()>Duration::from_secs(1){last_request=Instant::now();let _=tokio::time::timeout(Duration::from_millis(500),subscription.request_keyframe()).await;}
            }
        }
    }
}
