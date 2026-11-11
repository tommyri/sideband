//! Dedicated audio worker. Device callbacks remain inside `ss-audio`.

use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use parking_lot::{Mutex, RwLock};
use rtrb::{Consumer, Producer};
use ss_audio::{
    device::{self, Attempt, DeviceEvent, Direction, GraphConfig, Recovery, Route},
    mixer::StereoFrame,
    processing::{Chain, EchoPath, MAX_RENDER_BATCH},
    vad::Vad,
};
use ss_core::{Id, VoiceProfile};

use crate::{
    ClientEvent,
    connection::{IncomingMedia, MediaLink},
};

use super::{
    AudioConfig, AudioSource, Cue, DeviceBackend, TalkGate, VoiceDeviceState,
    graph::{GraphSnapshot, TrackSpec, VoiceGraph},
    host::{self, Duplex, DuplexConfig, Host, Hosts, Input, Output},
};

pub(super) enum Update {
    Configure {
        config: AudioConfig,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    Tracks {
        own: u32,
        remote: Vec<TrackSpec>,
    },
    User {
        user: Id,
        gain: f32,
        muted: bool,
    },
    Cue(Cue),
    Alert(Cue),
    Bitrate(u32),
    DeviceBackend {
        backend: DeviceBackend,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Clone, Default)]
pub(super) struct WorkerStats {
    pub graph: GraphSnapshot,
    pub input_dbfs: f32,
    pub underruns: u64,
    pub overruns: u64,
    pub processing_latency: Duration,
    pub processing_monitor: Option<ss_audio::processing::Monitor>,
    pub input_rate: Option<u32>,
    pub output_rate: Option<u32>,
    pub backend: Option<&'static str>,
    pub processing_over_budget: u64,
    /// Playback queue level and the pacer's adjustments.
    pub playback: PacingStats,
    /// The worker thread runs in the OS real-time audio class.
    pub realtime: bool,
}

#[derive(Debug)]
pub(super) struct PttState {
    held: bool,
    released: Option<Instant>,
}

impl PttState {
    fn open(&self, now: Instant, delay: Duration) -> bool {
        self.held
            || self
                .released
                .is_some_and(|released| now.saturating_duration_since(released) < delay)
    }
}

pub(super) struct Shared {
    pub stats: RwLock<WorkerStats>,
    pub ptt: Mutex<PttState>,
    pub muted: AtomicBool,
    pub deaf: AtomicBool,
    pub cues_enabled: AtomicBool,
    pub controls_enabled: AtomicBool,
}

pub(super) struct WorkerHandle {
    pub updates: SyncSender<Update>,
    pub incoming: SyncSender<IncomingMedia>,
    pub shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    pub fn release_ptt(&self) {
        *self.shared.ptt.lock() = PttState {
            held: false,
            released: None,
        };
    }

    pub fn set_ptt(&self, pressed: bool) {
        let mut state = self.shared.ptt.lock();
        if pressed {
            state.held = true;
            state.released = None;
        } else if state.held {
            state.held = false;
            state.released = Some(Instant::now());
        }
    }

    pub async fn shutdown(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

pub(super) async fn start(
    profile: VoiceProfile,
    bitrate: u32,
    config: AudioConfig,
    media: MediaLink,
    events: tokio::sync::broadcast::Sender<ClientEvent>,
) -> Result<WorkerHandle, String> {
    let (updates, update_rx) = mpsc::sync_channel(256);
    let (incoming, incoming_rx) = mpsc::sync_channel(1024);
    let shared = Arc::new(Shared {
        stats: RwLock::new(WorkerStats::default()),
        ptt: Mutex::new(PttState {
            held: false,
            released: None,
        }),
        muted: AtomicBool::new(config.start_muted),
        deaf: AtomicBool::new(config.start_deafened),
        cues_enabled: AtomicBool::new(true),
        controls_enabled: AtomicBool::new(true),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let thread_shared = Arc::clone(&shared);
    let thread_stop = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("ss-client-voice".to_owned())
        .spawn(move || {
            let result = Runtime::new(
                profile,
                bitrate,
                config,
                media,
                events,
                update_rx,
                incoming_rx,
                thread_shared,
                thread_stop,
                host::system(),
            );
            match result {
                Ok(mut runtime) => {
                    let _ = ready_tx.send(Ok(()));
                    runtime.run();
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            }
        })
        .map_err(|error| error.to_string())?;
    match ready_rx.await {
        Ok(Ok(())) => Ok(WorkerHandle {
            updates,
            incoming,
            shared,
            stop,
            thread: Some(thread),
        }),
        Ok(Err(error)) => {
            let _ = thread.join();
            Err(error)
        }
        Err(_) => Err("voice worker stopped during startup".to_owned()),
    }
}

/// Shared with setup jobs, which open streams while the worker keeps
/// running. The worker only ever `try_lock`s it.
type SharedHost = Arc<Mutex<Box<dyn Host>>>;

/// An open, or one that failed, waiting to be applied on the worker.
type Opened<S> = Option<Result<S, device::Error>>;

struct Devices {
    host: SharedHost,
    capture_config: GraphConfig,
    capture: Option<Input>,
    playback: Option<Output>,
    voice_processing: Option<Duplex>,
    /// What is wanted but not open, and when to try again.
    recover: Recover,
    /// A failure was already reported for the stream that is closed now:
    /// capture (or Apple's unit), then playback. Retries stay quiet.
    notified: [bool; 2],
    /// The routes last sent to the UI.
    reported: Option<(Route, Route)>,
    voice_muted: bool,
    voice_mode: ss_audio::device::MicrophoneMode,
}

enum Recover {
    Portable {
        capture: Recovery,
        playback: Recovery,
    },
    /// Apple's unit follows the system defaults: it is open, or reopening is
    /// due at this time.
    Duplex { due: Option<Instant> },
}

/// The part of [`Devices`] that may leave the worker thread. Apple's
/// VoiceProcessingIO unit holds an Objective-C block and is not `Send`, so it
/// is opened and closed on the worker.
struct Portable {
    host: SharedHost,
    capture: Option<Input>,
    playback: Option<Output>,
}

impl Devices {
    /// Portable streams for `config`. A direction that did not open is
    /// retried.
    fn from_portable(
        portable: Portable,
        config: &AudioConfig,
        capture_config: GraphConfig,
    ) -> Self {
        let now = Instant::now();
        let mut capture = Recovery::new(Direction::Capture, config.input_device.clone(), now);
        if let Some((stream, _)) = &portable.capture {
            capture.opened(stream.device_id().clone(), now);
        }
        let mut playback = Recovery::new(Direction::Playback, config.output_device.clone(), now);
        if let Some((stream, _)) = &portable.playback {
            playback.opened(stream.device_id().clone(), now);
        }
        Self {
            host: portable.host,
            capture_config,
            capture: portable.capture,
            playback: portable.playback,
            voice_processing: None,
            recover: Recover::Portable { capture, playback },
            notified: [false; 2],
            reported: None,
            voice_muted: config.start_muted,
            voice_mode: ss_audio::device::MicrophoneMode::Unknown,
        }
    }

    /// Apple's unit, or `None` to retry opening it.
    fn from_duplex(
        host: SharedHost,
        unit: Option<Duplex>,
        config: &AudioConfig,
        capture_config: GraphConfig,
        voice_mode: ss_audio::device::MicrophoneMode,
    ) -> Self {
        Self {
            host,
            capture_config,
            capture: None,
            playback: None,
            recover: Recover::Duplex {
                due: unit.is_none().then(Instant::now),
            },
            voice_processing: unit,
            notified: [false; 2],
            reported: None,
            voice_muted: config.start_muted,
            voice_mode,
        }
    }

    /// Close Apple's unit here, on the worker, and keep what may move.
    fn into_portable(self) -> Portable {
        Portable {
            host: self.host,
            capture: self.capture,
            playback: self.playback,
        }
    }

    fn backend(&self) -> Option<&'static str> {
        if self.voice_processing.is_some() {
            return Some("Apple VoiceProcessingIO");
        }
        (self.capture.is_some() || self.playback.is_some()).then_some("CPAL")
    }

    fn capture_consumer_mut(&mut self) -> Option<&mut Consumer<f32>> {
        if let Some((_, consumer, _)) = &mut self.voice_processing {
            return Some(consumer);
        }
        self.capture.as_mut().map(|(_, consumer)| consumer)
    }

    fn playback_producer_mut(&mut self) -> Option<&mut Producer<f32>> {
        if let Some((_, _, producer)) = &mut self.voice_processing {
            return Some(producer);
        }
        self.playback.as_mut().map(|(_, producer)| producer)
    }

    /// Playback audio queued beyond the producer's ring: a resampler.
    fn playback_downstream(&self) -> Duration {
        self.playback
            .as_ref()
            .map_or(Duration::ZERO, |(stream, _)| stream.downstream_queue())
    }

    fn delays(&self) -> (Duration, Duration) {
        if self.voice_processing.is_some() {
            // Apple's AEC owns its timing model; the software chain is off.
            return (Duration::ZERO, Duration::ZERO);
        }
        let capture = self
            .capture
            .as_ref()
            .and_then(|(stream, _)| stream.delay())
            .unwrap_or_default();
        let playback = self
            .playback
            .as_ref()
            .and_then(|(stream, _)| stream.delay())
            .unwrap_or_default();
        (capture, playback)
    }

    fn counter_values(&self) -> (u64, u64) {
        if let Some((stream, _, _)) = &self.voice_processing {
            return stream.counters();
        }
        let underruns = self
            .playback
            .as_ref()
            .map_or(0, |(stream, _)| stream.underruns());
        let overruns = self
            .capture
            .as_ref()
            .map_or(0, |(stream, _)| stream.overruns());
        (underruns, overruns)
    }

    fn sync_voice_processing(
        &mut self,
        muted: bool,
        events: &tokio::sync::broadcast::Sender<ClientEvent>,
    ) {
        let Some((stream, _, _)) = &mut self.voice_processing else {
            return;
        };
        if muted != self.voice_muted {
            match stream.set_muted(muted) {
                Ok(()) => self.voice_muted = muted,
                Err(error) => {
                    let _ = events.send(ClientEvent::VoiceDevice(VoiceDeviceState::Unavailable(
                        error,
                    )));
                }
            }
        }
        if let Some(active) = stream.take_muted_speech() {
            let _ = events.send(ClientEvent::VoiceDevice(VoiceDeviceState::MutedSpeech {
                active,
            }));
        }
        let mode = device::microphone_mode_status().active;
        if mode != self.voice_mode {
            self.voice_mode = mode;
            let _ = events.send(ClientEvent::VoiceDevice(
                VoiceDeviceState::AppleVoiceProcessing {
                    microphone_mode: mode,
                },
            ));
        }
    }

    /// Where each direction is: capture (or Apple's unit), then playback.
    fn routes(&self) -> (Route, Route) {
        match &self.recover {
            Recover::Portable { capture, playback } => (capture.route(), playback.route()),
            Recover::Duplex { .. } => {
                let route = if self.voice_processing.is_some() {
                    Route::Wanted
                } else {
                    Route::Missing
                };
                (route, route)
            }
        }
    }

    /// Tell the UI when a stream went missing, moved to the fallback, or
    /// recovered.
    fn report_routes(&mut self, events: &tokio::sync::broadcast::Sender<ClientEvent>) {
        let routes = self.routes();
        if self.reported == Some(routes) {
            return;
        }
        self.reported = Some(routes);
        let _ = events.send(ClientEvent::VoiceDevice(VoiceDeviceState::Routes {
            capture: routes.0,
            playback: routes.1,
        }));
    }

    /// Report the first failure after a stream closed; retries stay quiet.
    fn failed(
        &mut self,
        slot: usize,
        error: &device::Error,
        events: &tokio::sync::broadcast::Sender<ClientEvent>,
    ) {
        if !std::mem::replace(&mut self.notified[slot], true) {
            send_device_error(events, error);
        }
    }

    /// Take a device event into account. A lost stream is dropped off the
    /// worker; Apple's unit is dropped here.
    fn observe(&mut self, event: &DeviceEvent, now: Instant, setup: &Setup) {
        match &mut self.recover {
            Recover::Portable { capture, playback } => {
                if capture.observe(event, now) {
                    setup.dispose(self.capture.take());
                }
                if playback.observe(event, now) {
                    setup.dispose(self.playback.take());
                }
            }
            Recover::Duplex { due } => match event {
                DeviceEvent::DefaultChanged { .. } => {
                    self.voice_processing = None;
                    *due = Some(now);
                }
                DeviceEvent::Added(_) if self.voice_processing.is_none() => *due = Some(now),
                _ => {}
            },
        }
    }

    /// Settings Apply: try a missing or fallback stream again now.
    fn retry_now(&mut self, now: Instant) {
        match &mut self.recover {
            Recover::Portable { capture, playback } => {
                capture.retry_now(now);
                playback.retry_now(now);
            }
            Recover::Duplex { due } => {
                if self.voice_processing.is_none() {
                    *due = Some(now);
                }
            }
        }
    }

    /// Reopen Apple's unit on the worker when it stopped or is due; see
    /// [`Portable`]. `true` when a new unit is in.
    fn recover_voice_processing(
        &mut self,
        now: Instant,
        config: &AudioConfig,
        events: &tokio::sync::broadcast::Sender<ClientEvent>,
    ) -> bool {
        let Recover::Duplex { due } = &mut self.recover else {
            return false;
        };
        if self
            .voice_processing
            .as_ref()
            .is_some_and(|(stream, _, _)| stream.stopped())
        {
            self.voice_processing = None;
            *due = Some(now);
        }
        if self.voice_processing.is_some() || due.is_none_or(|due| due > now) {
            return false;
        }
        let opened = open_voice_processing(
            &**self.host.lock(),
            config.device_backend,
            self.capture_config,
            self.voice_muted,
        );
        match opened {
            Ok(unit) => {
                self.voice_processing = Some(unit);
                *due = None;
                self.voice_mode = ss_audio::device::MicrophoneMode::Unknown;
                self.notified[0] = false;
                true
            }
            Err(error) => {
                *due = Some(Instant::now() + device::RETRY_INTERVAL);
                self.failed(0, &error, events);
                false
            }
        }
    }

    /// The portable opens that are due, as a job for the setup thread. The
    /// current streams keep running until their replacements are in.
    fn reopen_job(&self, now: Instant) -> Option<impl FnOnce() -> Apply + Send + 'static> {
        let Recover::Portable { capture, playback } = &self.recover else {
            return None;
        };
        let capture = capture.attempt(now);
        let playback = playback.attempt(now);
        if capture.is_none() && playback.is_none() {
            return None;
        }
        let host = Arc::clone(&self.host);
        let capture_config = self.capture_config;
        Some(move || -> Apply {
            let host = host.lock();
            let capture = capture
                .map(|attempt| open_with(&attempt, |id| host.open_capture(id, capture_config)));
            let playback = playback.map(|attempt| {
                open_with(&attempt, |id| host.open_playback(id, GraphConfig::STEREO))
            });
            drop(host);
            Box::new(move |runtime: &mut Runtime| {
                runtime.busy = false;
                runtime.install_reopened(capture, playback);
            })
        })
    }
}

/// Open the device `attempt` asks for, then the default if it allows that.
fn open_with<S>(
    attempt: &Attempt,
    open: impl Fn(Option<&device::DeviceId>) -> Result<S, device::Error>,
) -> Result<S, device::Error> {
    match open(attempt.device.as_ref()) {
        Err(_) if attempt.fallback => open(None),
        result => result,
    }
}

/// Devices for a settings change or backend switch, built on the setup thread.
enum Replacement {
    /// Portable streams for the new settings, or for the previous ones when
    /// the new ones failed (`result` holds that error).
    Opened {
        portable: Box<Portable>,
        result: Result<(), String>,
    },
    /// Apple's unit is involved; the worker opens it. `failed` is the error
    /// from the new settings when only the fallback is left to open.
    OnWorker { failed: Option<String> },
}

impl Replacement {
    /// Runs on the setup thread.
    fn open(
        hosts: &Hosts,
        config: &AudioConfig,
        previous: &AudioConfig,
        channels: usize,
        events: &tokio::sync::broadcast::Sender<ClientEvent>,
    ) -> Self {
        let capture_config = graph_config(channels, config);
        if config.device_backend != DeviceBackend::Portable {
            return Self::OnWorker { failed: None };
        }
        let error = match open_portable(hosts(), config, capture_config, events) {
            Ok(portable) => {
                return Self::Opened {
                    portable: Box::new(portable),
                    result: Ok(()),
                };
            }
            Err(error) => error,
        };
        if previous.device_backend != DeviceBackend::Portable {
            return Self::OnWorker {
                failed: Some(error),
            };
        }
        Self::Opened {
            portable: Box::new(open_available(hosts(), previous, capture_config)),
            result: Err(error),
        }
    }

    /// Runs on the worker.
    fn install(
        self,
        runtime: &mut Runtime,
        config: &AudioConfig,
        previous: &AudioConfig,
    ) -> Result<(), String> {
        let channels = runtime.graph.channels();
        runtime.pacer.reset();
        match self {
            Self::Opened { portable, result } => {
                let applied = if result.is_ok() { config } else { previous };
                let capture_config = graph_config(channels, applied);
                runtime.devices = Some(Devices::from_portable(*portable, applied, capture_config));
                result
            }
            Self::OnWorker { failed } => {
                let events = runtime.events.clone();
                let hosts = Arc::clone(&runtime.hosts);
                let result = match failed {
                    Some(error) => Err(error),
                    None => open_devices(hosts(), config, graph_config(channels, config), &events)
                        .map(|devices| runtime.devices = Some(devices)),
                };
                if result.is_err() {
                    let capture_config = graph_config(channels, previous);
                    runtime.devices =
                        Some(open_previous(hosts(), previous, capture_config, &events));
                }
                result
            }
        }
    }
}

fn open_voice_processing(
    host: &dyn Host,
    backend: DeviceBackend,
    capture_config: GraphConfig,
    muted: bool,
) -> Result<Duplex, device::Error> {
    let DeviceBackend::AppleVoiceProcessing {
        bypass,
        automatic_gain_control,
    } = backend
    else {
        return Err(device::Error::Unsupported);
    };
    host.open_duplex(
        capture_config,
        DuplexConfig {
            bypass,
            automatic_gain_control,
            muted,
        },
    )
}

/// Open exactly the configured devices, or fail.
fn open_portable(
    host: Box<dyn Host>,
    config: &AudioConfig,
    capture_config: GraphConfig,
    events: &tokio::sync::broadcast::Sender<ClientEvent>,
) -> Result<Portable, String> {
    let capture = match host.open_capture(config.input_device.as_ref(), capture_config) {
        Ok(capture) => capture,
        Err(error) => {
            send_device_error(events, &error);
            return Err(error.to_string());
        }
    };
    let playback = match host.open_playback(config.output_device.as_ref(), GraphConfig::STEREO) {
        Ok(playback) => playback,
        Err(error) => {
            send_device_error(events, &error);
            return Err(error.to_string());
        }
    };
    Ok(Portable {
        host: Arc::new(Mutex::new(host)),
        capture: Some(capture),
        playback: Some(playback),
    })
}

/// Open what can be opened for settings that worked before: the default
/// stands in for a missing selection, and a direction that does not open at
/// all is retried later.
fn open_available(
    host: Box<dyn Host>,
    config: &AudioConfig,
    capture_config: GraphConfig,
) -> Portable {
    let fallback = |device: &Option<device::DeviceId>| Attempt {
        device: device.clone(),
        fallback: device.is_some(),
    };
    let capture = open_with(&fallback(&config.input_device), |id| {
        host.open_capture(id, capture_config)
    })
    .ok();
    let playback = open_with(&fallback(&config.output_device), |id| {
        host.open_playback(id, GraphConfig::STEREO)
    })
    .ok();
    Portable {
        host: Arc::new(Mutex::new(host)),
        capture,
        playback,
    }
}

fn open_devices(
    host: Box<dyn Host>,
    config: &AudioConfig,
    capture_config: GraphConfig,
    events: &tokio::sync::broadcast::Sender<ClientEvent>,
) -> Result<Devices, String> {
    match config.device_backend {
        DeviceBackend::Portable => open_portable(host, config, capture_config, events)
            .map(|portable| Devices::from_portable(portable, config, capture_config)),
        DeviceBackend::AppleVoiceProcessing { .. } => {
            open_voice_processing_devices(host, config, capture_config, events)
        }
    }
}

/// Devices for settings that worked before. Whatever does not open now is
/// retried.
fn open_previous(
    host: Box<dyn Host>,
    previous: &AudioConfig,
    capture_config: GraphConfig,
    events: &tokio::sync::broadcast::Sender<ClientEvent>,
) -> Devices {
    match previous.device_backend {
        DeviceBackend::Portable => Devices::from_portable(
            open_available(host, previous, capture_config),
            previous,
            capture_config,
        ),
        DeviceBackend::AppleVoiceProcessing { .. } => {
            let host = Arc::new(Mutex::new(host));
            let unit = open_voice_processing(
                &**host.lock(),
                previous.device_backend,
                capture_config,
                previous.start_muted,
            )
            .inspect_err(|error| send_device_error(events, error))
            .ok();
            let missing = unit.is_none();
            let mut devices = Devices::from_duplex(
                host,
                unit,
                previous,
                capture_config,
                ss_audio::device::MicrophoneMode::Unknown,
            );
            // Its error was just sent; the retries stay quiet.
            devices.notified[0] = missing;
            devices
        }
    }
}

fn open_voice_processing_devices(
    host: Box<dyn Host>,
    config: &AudioConfig,
    capture_config: GraphConfig,
    events: &tokio::sync::broadcast::Sender<ClientEvent>,
) -> Result<Devices, String> {
    if !host.duplex_supported() {
        return Err("Apple VoiceProcessingIO is unavailable on this platform or build".to_owned());
    }
    if config.input_device.is_some() || config.output_device.is_some() {
        return Err("Apple VoiceProcessingIO requires the system-default devices".to_owned());
    }
    let unit = open_voice_processing(
        &*host,
        config.device_backend,
        capture_config,
        config.start_muted,
    )
    .map_err(|error| {
        send_device_error(events, &error);
        error.to_string()
    })?;
    let voice_mode = device::microphone_mode_status().active;
    let _ = events.send(ClientEvent::VoiceDevice(
        VoiceDeviceState::AppleVoiceProcessing {
            microphone_mode: voice_mode,
        },
    ));
    Ok(Devices::from_duplex(
        Arc::new(Mutex::new(host)),
        Some(unit),
        config,
        capture_config,
        voice_mode,
    ))
}

/// Builds the processing chain and voice activation for `config`.
fn build_processing(config: &AudioConfig, channels: usize) -> Result<(Chain, Option<Vad>), String> {
    let processing = match config.device_backend {
        DeviceBackend::Portable => config.processing,
        DeviceBackend::AppleVoiceProcessing { .. } => ss_audio::processing::ProcessingConfig::OFF,
    };
    let chain = Chain::new(
        processing,
        u16::try_from(channels).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let vad = match config.gate {
        TalkGate::VoiceActivation(mut vad) => {
            vad.channels = u8::try_from(channels).map_err(|error| error.to_string())?;
            Some(Vad::new(vad).map_err(|error| error.to_string())?)
        }
        TalkGate::PushToTalk | TalkGate::OpenMic => None,
    };
    Ok((chain, vad))
}

/// Work applied on the worker thread once the setup thread has finished.
type Apply = Box<dyn FnOnce(&mut Runtime) + Send>;
type Job = Box<dyn FnOnce() -> Option<Apply> + Send>;
type Reply = tokio::sync::oneshot::Sender<Result<(), String>>;

/// A normal-priority thread that builds and drops what would stall the
/// audio worker: processing chains and VAD (settings Apply), device streams
/// (reopen, backend switch) and the device manager, whose drop joins a
/// thread that sleeps up to 500 ms. Jobs run in order.
struct Setup {
    jobs: Option<mpsc::Sender<Job>>,
    applies: Receiver<Apply>,
    thread: Option<JoinHandle<()>>,
}

impl Setup {
    fn new() -> Self {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (apply_tx, applies) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("ss-client-voice-setup".to_owned())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    if let Some(apply) = job()
                        && apply_tx.send(apply).is_err()
                    {
                        break;
                    }
                }
            })
            .inspect_err(|error| tracing::warn!(%error, "voice setup thread did not start"))
            .ok();
        Self {
            jobs: thread.is_some().then_some(jobs),
            applies,
            thread,
        }
    }

    /// Run `job` off the worker. Its result is applied by [`Runtime::poll_setup`].
    /// Without a setup thread the job is handed back to run inline.
    fn build(&self, job: impl FnOnce() -> Apply + Send + 'static) -> Result<(), Job> {
        let job: Job = Box::new(move || Some(job()));
        match &self.jobs {
            Some(jobs) => jobs.send(job).map_err(|error| error.0),
            None => Err(job),
        }
    }

    fn stopped(&self) -> bool {
        self.thread.as_ref().is_some_and(JoinHandle::is_finished)
    }

    /// Drop `value` off the worker.
    fn dispose<T: Send + 'static>(&self, value: T) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Box::new(move || {
                drop(value);
                None
            }));
        }
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Runtime {
    graph: VoiceGraph,
    config: AudioConfig,
    media: MediaLink,
    events: tokio::sync::broadcast::Sender<ClientEvent>,
    updates: Receiver<Update>,
    incoming: Receiver<IncomingMedia>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    hosts: Hosts,
    devices: Option<Devices>,
    chain: Chain,
    vad: Option<Vad>,
    epoch: Instant,
    capture: Vec<f32>,
    capture_queue: Duration,
    render_tap: Vec<StereoFrame>,
    test_sample: usize,
    input_dbfs: f32,
    pacer: Pacer,
    realtime: bool,
    setup: Setup,
    /// A setup job is in flight; device and settings changes wait for it.
    busy: bool,
    deferred: VecDeque<Update>,
}

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    fn new(
        profile: VoiceProfile,
        bitrate: u32,
        config: AudioConfig,
        media: MediaLink,
        events: tokio::sync::broadcast::Sender<ClientEvent>,
        updates: Receiver<Update>,
        incoming: Receiver<IncomingMedia>,
        shared: Arc<Shared>,
        stop: Arc<AtomicBool>,
        hosts: Hosts,
    ) -> Result<Self, String> {
        let mut graph = VoiceGraph::new(profile, bitrate).map_err(|error| error.to_string())?;
        graph.configure(&config);
        let (chain, vad) = build_processing(&config, graph.channels())?;
        let devices = if config.source == AudioSource::Device {
            let capture_config = graph_config(graph.channels(), &config);
            Some(open_devices(hosts(), &config, capture_config, &events)?)
        } else {
            None
        };
        let quantum = graph.quantum();
        Ok(Self {
            graph,
            config,
            media,
            events,
            updates,
            incoming,
            shared,
            stop,
            hosts,
            devices,
            chain,
            vad,
            epoch: Instant::now(),
            capture: vec![0.0; quantum],
            capture_queue: Duration::ZERO,
            render_tap: Vec::with_capacity(MAX_RENDER_BATCH + 2),
            test_sample: 0,
            input_dbfs: -96.0,
            pacer: Pacer::default(),
            realtime: false,
            setup: Setup::new(),
            busy: false,
            deferred: VecDeque::new(),
        })
    }

    fn run(&mut self) {
        // After setup, so opening devices does not count against the
        // real-time budget.
        match device::promote_audio_thread(QUANTUM) {
            Ok(()) => self.realtime = true,
            Err(error) => tracing::info!(%error, "voice worker keeps normal priority"),
        }
        let mut next = Instant::now();
        while !self.stop.load(Ordering::Acquire) {
            self.poll_setup();
            self.drain_updates();
            self.drain_incoming();
            self.device_events();
            let now = Instant::now();
            let captured = match self.config.source {
                AudioSource::Device => self.pull_capture(),
                AudioSource::Test if now >= next => {
                    self.test_tone();
                    true
                }
                AudioSource::Silent if now >= next => {
                    self.capture.fill(0.0);
                    true
                }
                AudioSource::Test | AudioSource::Silent => false,
            };
            if captured {
                self.capture_once(now);
            }
            if now >= next {
                self.render_once();
                next += QUANTUM;
                if next + Duration::from_millis(30) < now {
                    next = now;
                }
            } else if !captured {
                thread::sleep(Duration::from_millis(1));
            }
            for packet in self.graph.pop_outgoing() {
                self.media.send(packet);
            }
        }
    }

    /// Apply finished setup jobs, then any settings change that waited.
    fn poll_setup(&mut self) {
        while let Ok(apply) = self.setup.applies.try_recv() {
            apply(self);
        }
        if self.busy && self.setup.stopped() {
            // A job panicked. Its reply is dropped, which callers see as an
            // error; later changes run inline rather than wait forever.
            tracing::warn!("voice setup thread stopped");
            self.setup.jobs = None;
            self.busy = false;
        }
        while !self.busy
            && let Some(update) = self.deferred.pop_front()
        {
            self.update(update);
        }
    }

    /// Run `job` on the setup thread and mark the worker busy until it is
    /// applied.
    fn spawn_setup(&mut self, job: impl FnOnce() -> Apply + Send + 'static) {
        self.busy = true;
        if let Err(job) = self.setup.build(job)
            && let Some(apply) = job()
        {
            apply(self);
        }
    }

    fn drain_updates(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            self.update(update);
        }
    }

    fn update(&mut self, update: Update) {
        match update {
            Update::Configure { .. } | Update::DeviceBackend { .. } if self.busy => {
                self.deferred.push_back(update);
            }
            Update::Configure { config, reply } => self.configure(config, reply),
            Update::Tracks { own, remote } => {
                self.graph.set_track_id(own);
                self.graph.sync_tracks(&remote, self.epoch.elapsed());
            }
            Update::User { user, gain, muted } => self.graph.set_user(user, gain, muted),
            Update::Alert(cue) => {
                if self.shared.cues_enabled.load(Ordering::Relaxed) {
                    self.graph.play_cue(cue);
                }
            }
            Update::Cue(cue) => {
                if self.shared.cues_enabled.load(Ordering::Relaxed)
                    && self.shared.controls_enabled.load(Ordering::Relaxed)
                {
                    self.graph.play_cue(cue);
                }
            }
            Update::Bitrate(bitrate) => {
                if let Err(error) = self.graph.set_bitrate(bitrate) {
                    tracing::warn!(%error, bitrate, "encoder adaptation was rejected");
                }
            }
            Update::DeviceBackend { backend, reply } => self.switch_device_backend(backend, reply),
        }
    }

    /// Settings Apply. The chain, VAD and any device reopen are built on the
    /// setup thread; the worker keeps rendering and replies once they are in.
    fn configure(&mut self, mut config: AudioConfig, reply: Reply) {
        if !(0.0..=2.0).contains(&config.master_gain)
            || !(0.0..=1.0).contains(&config.cue_gain)
            || !(0.0..=0.5).contains(&config.attenuation)
            || config.release_delay > Duration::from_secs(2)
            || config.jitter_min < Duration::from_millis(20)
            || config.jitter_max > Duration::from_millis(200)
            || config.jitter_min > config.jitter_max
        {
            let _ = reply.send(Err("Invalid audio settings".into()));
            return;
        }
        config.source = self.config.source;
        config.start_muted = self.shared.muted.load(Ordering::Relaxed);
        config.start_deafened = self.shared.deaf.load(Ordering::Relaxed);
        let reopen = config.source == AudioSource::Device
            && (self.config.input_device != config.input_device
                || self.config.input_channel != config.input_channel
                || self.config.output_device != config.output_device
                || self.config.device_backend != config.device_backend);
        let channels = self.graph.channels();
        let previous = self.config.clone();
        let events = self.events.clone();
        let hosts = Arc::clone(&self.hosts);
        self.spawn_setup(move || -> Apply {
            let built = build_processing(&config, channels);
            Box::new(move |runtime: &mut Runtime| {
                runtime.busy = false;
                let (chain, vad) = match built {
                    Ok(built) => built,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return;
                    }
                };
                if reopen {
                    // Only a valid configuration closes the current devices.
                    // They close and the new ones open on the setup thread;
                    // the next apply only swaps them in.
                    let old = runtime.devices.take().map(Devices::into_portable);
                    runtime.spawn_setup(move || -> Apply {
                        drop(old);
                        let replacement =
                            Replacement::open(&hosts, &config, &previous, channels, &events);
                        Box::new(move |runtime: &mut Runtime| {
                            runtime.busy = false;
                            let result = replacement.install(runtime, &config, &previous);
                            runtime.finish_configure(config, chain, vad, result, reply);
                        })
                    });
                    return;
                }
                runtime.retry_devices(Box::new(move |runtime: &mut Runtime| {
                    runtime.finish_configure(config, chain, vad, Ok(()), reply);
                }));
            })
        });
    }

    /// Swap in a configuration whose devices are already in place.
    fn finish_configure(
        &mut self,
        config: AudioConfig,
        chain: Chain,
        vad: Option<Vad>,
        result: Result<(), String>,
        reply: Reply,
    ) {
        if let Err(error) = result {
            self.setup.dispose((chain, vad));
            let _ = reply.send(Err(error));
            return;
        }
        *self.shared.ptt.lock() = PttState {
            held: false,
            released: None,
        };
        self.graph.configure(&config);
        self.config = config;
        let old_chain = std::mem::replace(&mut self.chain, chain);
        let old_vad = std::mem::replace(&mut self.vad, vad);
        self.setup.dispose((old_chain, old_vad));
        self.render_tap.clear();
        let _ = reply.send(Ok(()));
    }

    fn switch_device_backend(&mut self, backend: DeviceBackend, reply: Reply) {
        if self.config.device_backend == backend {
            let _ = reply.send(Ok(()));
            return;
        }
        let mut config = self.config.clone();
        config.device_backend = backend;
        let channels = self.graph.channels();
        self.spawn_setup(move || -> Apply {
            let built = build_processing(&config, channels);
            Box::new(move |runtime: &mut Runtime| {
                runtime.busy = false;
                let chain = match built {
                    Ok((chain, vad)) => {
                        runtime.setup.dispose(vad);
                        chain
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return;
                    }
                };
                let previous = runtime.config.clone();
                if config.source != AudioSource::Device {
                    runtime.finish_backend(backend, chain, Ok(()), reply);
                    return;
                }
                let old = runtime.devices.take().map(Devices::into_portable);
                let events = runtime.events.clone();
                let hosts = Arc::clone(&runtime.hosts);
                runtime.spawn_setup(move || -> Apply {
                    drop(old);
                    let replacement =
                        Replacement::open(&hosts, &config, &previous, channels, &events);
                    Box::new(move |runtime: &mut Runtime| {
                        runtime.busy = false;
                        let result = replacement.install(runtime, &config, &previous);
                        runtime.finish_backend(backend, chain, result, reply);
                    })
                });
            })
        });
    }

    fn finish_backend(
        &mut self,
        backend: DeviceBackend,
        chain: Chain,
        result: Result<(), String>,
        reply: Reply,
    ) {
        if let Err(error) = result {
            self.setup.dispose(chain);
            let _ = reply.send(Err(error));
            return;
        }
        self.config.device_backend = backend;
        let old_chain = std::mem::replace(&mut self.chain, chain);
        self.setup.dispose(old_chain);
        self.render_tap.clear();
        let _ = reply.send(Ok(()));
    }

    fn drain_incoming(&mut self) {
        while let Ok(packet) = self.incoming.try_recv() {
            let arrival = packet
                .at
                .checked_duration_since(self.epoch)
                .unwrap_or_default();
            self.graph.push_datagram(&packet.bytes, arrival);
        }
    }

    fn capture_once(&mut self, now: Instant) {
        let energy = self.capture.iter().map(|s| s * s).sum::<f32>()
            / f32::from(u16::try_from(self.capture.len()).unwrap_or(960));
        self.input_dbfs = (10.0 * energy.max(1e-10).log10()).clamp(-96.0, 0.0);
        let path = self.echo_path();
        self.chain.set_echo_delay(path.delay());
        if let Err(error) = self.chain.process(&mut self.capture, &self.render_tap) {
            tracing::warn!(%error, "capture processing frame was rejected");
        }
        self.render_tap.clear();
        let blocked =
            self.shared.muted.load(Ordering::Relaxed) || self.shared.deaf.load(Ordering::Relaxed);
        match self.config.gate {
            TalkGate::PushToTalk => {
                let gate = !blocked && self.shared.ptt.lock().open(now, self.config.release_delay);
                let _ = self.graph.push_capture(&self.capture, gate);
            }
            TalkGate::OpenMic => {
                let _ = self.graph.push_capture(&self.capture, !blocked);
            }
            TalkGate::VoiceActivation(_) => self.capture_vad(blocked),
        }
    }

    /// Every captured quantum reaches the graph once: as gated audio, or as a
    /// closed quantum that ends the burst and keeps the capture clock running.
    fn capture_vad(&mut self, blocked: bool) {
        if let Some(vad) = self.vad.as_mut() {
            if blocked {
                let _ = vad.force_close();
            } else if let Ok(output) = vad.process(&self.capture) {
                if output.event == Some(ss_audio::vad::GateEvent::Open) {
                    // Pre-roll replays quanta the graph already counted closed.
                    self.graph
                        .retract_closed(output.audio().len().saturating_sub(1));
                }
                let mut gated = false;
                for frame in output.audio() {
                    let _ = self.graph.push_capture(frame, true);
                    gated = true;
                }
                if gated {
                    return;
                }
            }
        }
        // Closed, muted, calibrating, or a rejected (non-finite) quantum.
        let _ = self.graph.push_capture(&self.capture, false);
    }

    fn render_once(&mut self) {
        let deaf = self.shared.deaf.load(Ordering::Relaxed);
        if !self.shared.cues_enabled.load(Ordering::Relaxed) {
            self.graph.clear_cues();
        }
        let mixed = self.graph.render(self.epoch.elapsed(), deaf);
        let (underruns, overruns) = self.counters();
        if let Some(devices) = &mut self.devices {
            let downstream = devices.playback_downstream();
            if let Some(producer) = devices.playback_producer_mut() {
                self.pacer.push(
                    producer,
                    downstream,
                    underruns,
                    &mixed,
                    &mut self.render_tap,
                );
            }
        }
        // Only capture drains the tap. Keep the newest frames the echo
        // canceller takes in one go, so a closed or stalled capture stream
        // does not grow it.
        let stale = self.render_tap.len().saturating_sub(MAX_RENDER_BATCH);
        self.render_tap.drain(..stale);
        let now = Instant::now();
        let graph = self.graph.snapshot();
        *self.media.audio_clocks.lock() = graph
            .tracks
            .iter()
            .filter_map(|(&id, t)| {
                t.timestamp
                    .map(|timestamp| (id, crate::media::AudioClock { timestamp, at: now }))
            })
            .collect();
        *self.media.capture_clock.lock() = Some(crate::media::AudioClock {
            timestamp: self.graph.capture_timestamp(),
            at: now,
        });
        *self.shared.stats.write() = WorkerStats {
            graph,
            input_dbfs: self.input_dbfs,
            underruns,
            overruns,
            processing_latency: self.chain.monitor().latency(),
            processing_monitor: Some(self.chain.monitor()),
            input_rate: self
                .devices
                .as_ref()
                .and_then(|d| d.capture.as_ref().map(|(s, _)| s.device_rate())),
            output_rate: self
                .devices
                .as_ref()
                .and_then(|d| d.playback.as_ref().map(|(s, _)| s.device_rate())),
            backend: self.devices.as_ref().and_then(Devices::backend),
            processing_over_budget: self.chain.monitor().over_budget(),
            playback: self.pacer.stats(),
            realtime: self.realtime,
        };
    }

    fn pull_capture(&mut self) -> bool {
        let Some(devices) = &mut self.devices else {
            return false;
        };
        let Some(consumer) = devices.capture_consumer_mut() else {
            return false;
        };
        if consumer.slots() < self.capture.len() {
            return false;
        }
        let Ok(chunk) = consumer.read_chunk(self.capture.len()) else {
            return false;
        };
        let (first, second) = chunk.as_slices();
        self.capture[..first.len()].copy_from_slice(first);
        self.capture[first.len()..].copy_from_slice(second);
        chunk.commit_all();
        // Newer audio already waiting behind this quantum.
        let frames = consumer.slots() / self.graph.channels().max(1);
        self.capture_queue =
            Duration::from_micros(u64::try_from(frames).unwrap_or(u64::MAX) * 1_000_000 / 48_000);
        true
    }

    fn test_tone(&mut self) {
        let channels = self.graph.channels();
        for index in 0..self.capture.len() / channels {
            #[allow(clippy::cast_precision_loss)]
            let phase =
                (self.test_sample + index) as f32 * 440.0 * std::f32::consts::TAU / 48_000.0;
            for channel in 0..channels {
                self.capture[index * channels + channel] = phase.sin() * 0.25;
            }
        }
        self.test_sample += self.capture.len() / channels;
    }

    fn echo_path(&self) -> EchoPath {
        let (capture_device, playback_device) = self
            .devices
            .as_ref()
            .map_or((Duration::ZERO, Duration::ZERO), Devices::delays);
        EchoPath {
            playback_queue: self.pacer.stats().queue,
            playback_device,
            capture_device,
            capture_queue: self.capture_queue,
        }
    }

    fn counters(&self) -> (u64, u64) {
        let Some(devices) = &self.devices else {
            return (0, 0);
        };
        devices.counter_values()
    }

    /// Poll the host, follow lost and returning devices, and retry what is
    /// wanted but not open.
    fn device_events(&mut self) {
        let Some(devices) = &mut self.devices else {
            return;
        };
        devices.sync_voice_processing(self.shared.muted.load(Ordering::Relaxed), &self.events);
        if self.busy {
            return;
        }
        // A setup job may hold the host while it opens a stream.
        let Some(host) = devices.host.try_lock() else {
            return;
        };
        let events = host.poll();
        drop(host);
        let now = Instant::now();
        for event in &events {
            notify_device_event(&self.events, event);
            devices.observe(event, now, &self.setup);
        }
        if devices.recover_voice_processing(now, &self.config, &self.events) {
            self.pacer.reset();
        }
        // A reopen reports the routes once it is in, so a stream that is
        // replaced at once never shows as missing.
        match devices.reopen_job(now) {
            Some(job) => self.spawn_setup(job),
            None => devices.report_routes(&self.events),
        }
    }

    /// Swap in streams a reopen job opened, and schedule the next try for
    /// those that did not open.
    fn install_reopened(&mut self, capture: Opened<Input>, playback: Opened<Output>) {
        let portable = self
            .devices
            .as_mut()
            .filter(|devices| matches!(devices.recover, Recover::Portable { .. }));
        let Some(devices) = portable else {
            // The devices were replaced meanwhile; drop these off the worker.
            self.setup
                .dispose((capture.and_then(Result::ok), playback.and_then(Result::ok)));
            return;
        };
        let Recover::Portable {
            capture: capture_route,
            playback: playback_route,
        } = &mut devices.recover
        else {
            return;
        };
        let now = Instant::now();
        let mut errors = [None, None];
        match capture {
            Some(Ok(input)) => {
                capture_route.opened(input.0.device_id().clone(), now);
                self.setup.dispose(devices.capture.replace(input));
                devices.notified[0] = false;
                // Rendered audio from before the gap is not this stream's echo.
                self.render_tap.clear();
            }
            Some(Err(error)) => {
                capture_route.failed(now);
                errors[0] = devices.capture.is_none().then_some(error);
            }
            None => {}
        }
        match playback {
            Some(Ok(output)) => {
                playback_route.opened(output.0.device_id().clone(), now);
                self.setup.dispose(devices.playback.replace(output));
                devices.notified[1] = false;
                self.pacer.reset();
            }
            Some(Err(error)) => {
                playback_route.failed(now);
                errors[1] = devices.playback.is_none().then_some(error);
            }
            None => {}
        }
        for (slot, error) in errors.into_iter().enumerate() {
            if let Some(error) = error {
                devices.failed(slot, &error, &self.events);
            }
        }
        devices.report_routes(&self.events);
    }

    /// Try missing or fallback streams again now, then run `then`. Settings
    /// Apply goes through here, so applying the same settings reopens a
    /// closed stream.
    fn retry_devices(&mut self, then: Apply) {
        let now = Instant::now();
        let Some(devices) = &mut self.devices else {
            then(self);
            return;
        };
        devices.retry_now(now);
        if devices.recover_voice_processing(now, &self.config, &self.events) {
            self.pacer.reset();
        }
        let Some(job) = devices.reopen_job(now) else {
            devices.report_routes(&self.events);
            then(self);
            return;
        };
        self.spawn_setup(move || -> Apply {
            let reopened = job();
            Box::new(move |runtime: &mut Runtime| {
                reopened(runtime);
                then(runtime);
            })
        });
    }
}

/// Pass input conditions the UI explains on to it.
fn notify_device_event(events: &tokio::sync::broadcast::Sender<ClientEvent>, event: &DeviceEvent) {
    let state = match event {
        DeviceEvent::SilentInput { access, .. } => VoiceDeviceState::SilentInput {
            permission_denied: *access == device::MicrophoneAccess::Denied,
        },
        DeviceEvent::LowQualityInput { sample_rate, .. } => VoiceDeviceState::LowQualityInput {
            sample_rate: *sample_rate,
        },
        _ => return,
    };
    let _ = events.send(ClientEvent::VoiceDevice(state));
}

fn send_device_error(events: &tokio::sync::broadcast::Sender<ClientEvent>, error: &device::Error) {
    let state = if matches!(error, device::Error::PermissionDenied) {
        VoiceDeviceState::PermissionDenied
    } else {
        VoiceDeviceState::Unavailable(error.to_string())
    };
    let _ = events.send(ClientEvent::VoiceDevice(state));
}

fn graph_config(channels: usize, config: &AudioConfig) -> GraphConfig {
    let graph = if channels == 2 {
        GraphConfig::STEREO
    } else {
        GraphConfig::MONO
    };
    graph.with_input(config.input_channel)
}

/// Playback queue counters, published with the worker stats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PacingStats {
    /// Audio queued ahead of the latest quantum when it was handed over.
    pub queue: Duration,
    /// Quanta dropped because the queue was full.
    pub overflow: u64,
    /// Quanta dropped to shorten the queue.
    pub trimmed: u64,
    /// Silent quanta inserted to lengthen it.
    pub padded: u64,
    /// Trims and pads that had to cut into audible output.
    pub forced: u64,
}

/// Keeps the playback queue between [`LOW`] and [`HIGH`].
///
/// The worker renders a quantum every 10 ms of `Instant` time, which is also
/// the jitter buffers' clock. The device takes audio on its own clock, so the
/// queue between them drifts (100 ppm is 360 ms an hour), and every underrun
/// whose silence the worker later replays raises it for good. The pacer
/// watches the lowest queue ahead of a new quantum over a half-second window,
/// which is what is left when the device has just taken its share. Above
/// [`HIGH`] it drops quanta down to [`TARGET`]; below [`LOW`], or when the
/// device ran dry and the queue is still empty a quantum later, it inserts
/// one silent quantum. It waits up to a
/// three seconds for a quantum below −60 dBFS to drop or pad next to. If none
/// comes (music, open mic), it cuts with 2.5 ms fades on either side.
#[derive(Debug, Default)]
pub(super) struct Pacer {
    window_low: Option<Duration>,
    window_quanta: u32,
    underruns: u64,
    starved: bool,
    phase: Phase,
    stats: PacingStats,
}

#[derive(Debug, Default, Clone, Copy)]
enum Phase {
    #[default]
    Steady,
    /// Drop `remaining` quanta, quiet ones by preference.
    Trim { remaining: u32, waited: u32 },
    /// Forced trim: drop `remaining` quanta, then fade in.
    Cut { remaining: u32 },
    /// Insert one silent quantum next to a quiet one.
    Pad { waited: u32 },
    /// Fade in the next quantum after a forced cut or pad.
    FadeIn,
}

/// Quanta per measurement window (500 ms).
const WINDOW: u32 = 50;
/// Quanta to wait for a quiet one before cutting into audio (3 s). Drift is
/// slow (200 ppm is 0.6 ms in that time); a pause in speech is worth waiting for.
const PATIENCE: u32 = 300;
/// Pad when the queue left after a device callback falls below this.
const LOW: Duration = Duration::from_micros(2_500);
/// Where a trim brings the queue.
const TARGET: Duration = Duration::from_millis(10);
/// Peak at or below −60 dBFS counts as quiet, as in the jitter buffer.
const QUIET: f32 = 0.001;
/// Stereo samples in a forced cut's fade.
const FADE: usize = 240;

impl Pacer {
    pub(super) fn stats(&self) -> PacingStats {
        self.stats
    }

    /// Forget the measurement after the ring was replaced.
    pub(super) fn reset(&mut self) {
        *self = Self {
            stats: self.stats,
            ..Self::default()
        };
    }

    /// Hand one rendered quantum to the playback ring, adjusting the queue
    /// when it is out of band. `downstream` is audio queued beyond the ring
    /// (a resampler); `underruns` is the device's cumulative count. Whatever
    /// reaches the ring is appended to `tap`, the echo canceller's reference.
    pub(super) fn push(
        &mut self,
        producer: &mut Producer<f32>,
        downstream: Duration,
        underruns: u64,
        frame: &StereoFrame,
        tap: &mut Vec<StereoFrame>,
    ) {
        let queue = queued(producer) + downstream;
        self.stats.queue = queue;
        let starved = underruns > self.underruns;
        self.underruns = underruns;
        let quiet = frame.iter().all(|sample| sample.abs() <= QUIET);
        if let Phase::Steady = self.phase {
            if self.starved && queue < LOW {
                // The device ran dry and the queue stayed empty for another
                // quantum, so this was not a stall the worker is replaying.
                // Pad now rather than wait for a quiet quantum.
                self.emit(producer, &[0.0; 960], tap);
                self.stats.padded += 1;
                self.stats.forced += u64::from(!quiet);
                self.restart_window();
                self.starved = false;
            } else {
                self.starved = starved && queue < LOW;
                self.observe(queue);
            }
        }
        let mut frame = *frame;
        self.phase = match self.phase {
            Phase::Steady => {
                self.emit(producer, &frame, tap);
                Phase::Steady
            }
            Phase::Trim { remaining, .. } if quiet => {
                self.stats.trimmed += 1;
                self.settle(remaining - 1, Phase::Steady)
            }
            Phase::Trim { remaining, waited } if waited + 1 >= PATIENCE => {
                fade_out(&mut frame);
                self.emit(producer, &frame, tap);
                self.stats.forced += 1;
                Phase::Cut { remaining }
            }
            Phase::Trim { remaining, waited } => {
                self.emit(producer, &frame, tap);
                Phase::Trim {
                    remaining,
                    waited: waited + 1,
                }
            }
            Phase::Cut { remaining } => {
                self.stats.trimmed += 1;
                self.settle(remaining - 1, Phase::FadeIn)
            }
            Phase::Pad { waited } if quiet || waited + 1 >= PATIENCE => {
                let next = if quiet {
                    Phase::Steady
                } else {
                    fade_out(&mut frame);
                    self.stats.forced += 1;
                    Phase::FadeIn
                };
                self.emit(producer, &frame, tap);
                self.emit(producer, &[0.0; 960], tap);
                self.stats.padded += 1;
                self.restart_window();
                next
            }
            Phase::Pad { waited } => {
                self.emit(producer, &frame, tap);
                Phase::Pad { waited: waited + 1 }
            }
            Phase::FadeIn => {
                fade_in(&mut frame);
                self.emit(producer, &frame, tap);
                self.restart_window();
                Phase::Steady
            }
        };
    }

    fn observe(&mut self, queue: Duration) {
        let low = self.window_low.map_or(queue, |low| low.min(queue));
        self.window_low = Some(low);
        self.window_quanta += 1;
        if self.window_quanta < WINDOW {
            return;
        }
        self.restart_window();
        if low > HIGH {
            let excess = low.saturating_sub(TARGET).as_micros() / QUANTUM.as_micros();
            self.phase = Phase::Trim {
                remaining: u32::try_from(excess).unwrap_or(u32::MAX).max(1),
                waited: 0,
            };
        } else if low < LOW {
            self.phase = Phase::Pad { waited: 0 };
        }
    }

    /// Continue a trim with `remaining` quanta, or finish it with `done`.
    fn settle(&mut self, remaining: u32, done: Phase) -> Phase {
        match (remaining, self.phase) {
            (0, _) => {
                if matches!(done, Phase::Steady) {
                    self.restart_window();
                }
                done
            }
            (_, Phase::Cut { .. }) => Phase::Cut { remaining },
            (_, Phase::Trim { waited, .. }) => Phase::Trim { remaining, waited },
            (_, phase) => phase,
        }
    }

    fn restart_window(&mut self) {
        self.window_low = None;
        self.window_quanta = 0;
    }

    fn emit(
        &mut self,
        producer: &mut Producer<f32>,
        frame: &StereoFrame,
        tap: &mut Vec<StereoFrame>,
    ) {
        if push_playback(producer, frame) {
            tap.push(*frame);
        } else {
            self.stats.overflow += 1;
        }
    }
}

#[allow(clippy::cast_precision_loss)] // FADE is 240.
fn fade_out(frame: &mut StereoFrame) {
    let start = frame.len() - FADE;
    for (index, sample) in frame[start..].iter_mut().enumerate() {
        *sample *= 1.0 - (index / 2 + 1) as f32 / (FADE / 2) as f32;
    }
}

#[allow(clippy::cast_precision_loss)] // FADE is 240.
fn fade_in(frame: &mut StereoFrame) {
    for (index, sample) in frame[..FADE].iter_mut().enumerate() {
        *sample *= (index / 2) as f32 / (FADE / 2) as f32;
    }
}

fn queued(producer: &Producer<f32>) -> Duration {
    let samples = producer.buffer().capacity() - producer.slots();
    Duration::from_micros(u64::try_from(samples / 2).unwrap_or(u64::MAX) * 1_000_000 / 48_000)
}

/// One rendered quantum.
const QUANTUM: Duration = Duration::from_millis(10);
/// Trim when the queue never falls below this for a whole window.
const HIGH: Duration = Duration::from_millis(20);

fn push_playback(producer: &mut Producer<f32>, frame: &StereoFrame) -> bool {
    if producer.slots() < frame.len() {
        return false;
    }
    let Ok(mut chunk) = producer.write_chunk(frame.len()) else {
        return false;
    };
    let (first, second) = chunk.as_mut_slices();
    first.copy_from_slice(&frame[..first.len()]);
    second.copy_from_slice(&frame[first.len()..]);
    chunk.commit_all();
    true
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use ss_audio::vad;

    use super::{
        super::graph::tests::{Expected, assert_bursts},
        *,
    };

    const HANGOVER: usize = 5;
    const PRE_ROLL: usize = ss_audio::vad::PRE_ROLL_FRAMES;

    fn runtime(profile: VoiceProfile, gate: TalkGate) -> Runtime {
        let config = AudioConfig {
            source: AudioSource::Silent,
            gate,
            release_delay: Duration::ZERO,
            ..AudioConfig::default()
        };
        build(profile, config, host::system()).unwrap()
    }

    fn build(profile: VoiceProfile, config: AudioConfig, hosts: Hosts) -> Result<Runtime, String> {
        let (_, update_rx) = mpsc::sync_channel(1);
        let (_, incoming_rx) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            stats: RwLock::new(WorkerStats::default()),
            ptt: Mutex::new(PttState {
                held: false,
                released: None,
            }),
            muted: AtomicBool::new(false),
            deaf: AtomicBool::new(false),
            cues_enabled: AtomicBool::new(true),
            controls_enabled: AtomicBool::new(true),
        });
        let mut runtime = Runtime::new(
            profile,
            profile.encoder_params().default_bitrate_bps,
            config,
            MediaLink::new(),
            tokio::sync::broadcast::channel(64).0,
            update_rx,
            incoming_rx,
            shared,
            Arc::new(AtomicBool::new(false)),
            hosts,
        )?;
        runtime.graph.set_track_id(1);
        Ok(runtime)
    }

    fn voice_activation() -> TalkGate {
        TalkGate::VoiceActivation(vad::Config {
            hangover: Duration::from_millis(10 * HANGOVER as u64),
            ..vad::Config::default()
        })
    }

    /// Feed one quantum per entry. `talk` is the user's intent: speech for
    /// voice activation, the key for push-to-talk, unmuted for open mic. The
    /// other gates always hear speech, so leaked audio would be audible.
    fn drive(runtime: &mut Runtime, talk: &[bool], level: f32) -> Vec<(usize, Bytes)> {
        let start = Instant::now();
        let mut packets = Vec::new();
        for (index, &on) in talk.iter().enumerate() {
            let speech = on || !matches!(runtime.config.gate, TalkGate::VoiceActivation(_));
            runtime.capture.fill(if speech { level } else { 0.0 });
            match runtime.config.gate {
                TalkGate::PushToTalk => {
                    *runtime.shared.ptt.lock() = PttState {
                        held: on,
                        released: None,
                    };
                }
                TalkGate::OpenMic => runtime.shared.muted.store(!on, Ordering::Relaxed),
                TalkGate::VoiceActivation(_) => {}
            }
            runtime.capture_once(start + Duration::from_millis(10) * u32::try_from(index).unwrap());
            packets.extend(runtime.graph.pop_outgoing().into_iter().map(|p| (index, p)));
        }
        packets
    }

    /// Bursts of 1..=6 talking quanta, so the gated count is odd and even for
    /// every gate, each preceded and followed by `gap` idle quanta.
    fn script(gate: TalkGate, gap: usize) -> (Vec<bool>, Vec<Expected>) {
        let (lead, tail) = match gate {
            TalkGate::VoiceActivation(_) => (PRE_ROLL, HANGOVER),
            TalkGate::PushToTalk | TalkGate::OpenMic => (0, 0),
        };
        let mut talk = vec![false; gap];
        let mut expected = Vec::new();
        for length in 1..=6 {
            let start = talk.len() - lead;
            talk.extend(std::iter::repeat_n(true, length));
            expected.push(Expected {
                start: u32::try_from(start * 480).unwrap(),
                gated_end: u32::try_from((talk.len() + tail) * 480).unwrap(),
            });
            talk.extend(std::iter::repeat_n(false, gap));
        }
        (talk, expected)
    }

    /// A device that takes `period` frames per callback on its own clock,
    /// `ppm` fast (negative: slow) against the worker's `Instant`.
    struct Device {
        consumer: Consumer<f32>,
        period: usize,
        interval_us: f64,
        next_us: f64,
        underruns: u64,
    }

    impl Device {
        fn pull(&mut self) {
            let wanted = self.period * 2;
            let available = self.consumer.slots().min(wanted);
            if let Ok(chunk) = self.consumer.read_chunk(available) {
                chunk.commit_all();
            }
            if available < wanted {
                self.underruns += 1;
            }
            self.next_us += self.interval_us;
        }
    }

    #[derive(Debug, Default)]
    struct RingReport {
        stats: PacingStats,
        /// Lowest and highest queue ahead of a new quantum after warm-up.
        low: Duration,
        high: Duration,
        /// Device underruns after warm-up.
        underruns: u64,
        /// When the queue last left the band, in seconds.
        last_excursion: Option<u64>,
        /// Lowest queue ahead of a new quantum in the last ten seconds.
        settled_low: Duration,
    }

    const WARM_UP_US: u64 = 5_000_000;

    /// Drive the real pacer the way `Worker::run` does: a quantum every 10 ms
    /// of `Instant` time, catching up at most 30 ms after a stall.
    fn simulate_ring(
        ppm: f64,
        period: usize,
        seconds: u64,
        stalls: &[(u64, u64)],
        loud: impl Fn(u64) -> bool,
    ) -> RingReport {
        let (mut producer, consumer) = rtrb::RingBuffer::new(24_000);
        let mut device = Device {
            consumer,
            period,
            #[allow(clippy::cast_precision_loss)]
            interval_us: period as f64 / (48_000.0 * (1.0 + ppm / 1e6)) * 1e6,
            next_us: 0.0,
            underruns: 0,
        };
        let mut tone = [0.0; 960];
        for (index, pair) in tone.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let value = 0.25 * (index as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin();
            *pair = [value; 2];
        }
        let silence = [0.0; 960];
        let mut pacer = Pacer::default();
        let mut tap = Vec::new();
        let mut report = RingReport {
            low: Duration::MAX,
            settled_low: Duration::MAX,
            ..RingReport::default()
        };
        let band = HIGH + Duration::from_micros(1_000_000 * period as u64 / 48_000) + QUANTUM;
        let end = seconds * 1_000_000;
        let mut now = 0;
        let mut next = 0;
        let mut warm_underruns = 0;
        while now < end {
            #[allow(clippy::cast_precision_loss)]
            while device.next_us <= now as f64 {
                device.pull();
            }
            if let Some(&(at, length)) = stalls
                .iter()
                .find(|(at, length)| (*at..at + length).contains(&now))
            {
                now = at + length;
                continue;
            }
            if now < next {
                now = next;
                continue;
            }
            let frame = if loud(now / 10_000) { &tone } else { &silence };
            pacer.push(
                &mut producer,
                Duration::ZERO,
                device.underruns,
                frame,
                &mut tap,
            );
            tap.clear();
            next += 10_000;
            if next + 30_000 < now {
                next = now;
            }
            if now == WARM_UP_US {
                warm_underruns = device.underruns;
            }
            if now >= WARM_UP_US {
                let queue = pacer.stats().queue;
                report.low = report.low.min(queue);
                report.high = report.high.max(queue);
                if queue > band {
                    report.last_excursion = Some(now / 1_000_000);
                }
                if now + 10_000_000 >= end {
                    report.settled_low = report.settled_low.min(queue);
                }
            }
        }
        report.stats = pacer.stats();
        report.underruns = device.underruns - warm_underruns;
        report
    }

    #[test]
    fn a_slow_playback_clock_keeps_the_queue_in_band_for_an_hour() {
        for period in [256, 512, 1024] {
            let report = simulate_ring(-200.0, period, 3_600, &[], |_| true);
            eprintln!("200 ppm slow, {period}-frame periods, continuous tone: {report:?}");
            let band = HIGH + Duration::from_micros(1_000_000 * period as u64 / 48_000) + QUANTUM;
            assert!(report.high <= band, "{report:?}");
            assert_eq!(report.underruns, 0, "{report:?}");
            assert_eq!(report.stats.overflow, 0, "{report:?}");
            // 200 ppm of an hour is 720 ms, or 72 quanta.
            assert!((68..=80).contains(&report.stats.trimmed), "{report:?}");
        }
    }

    #[test]
    fn a_fast_playback_clock_is_padded_before_it_underruns() {
        let report = simulate_ring(200.0, 512, 3_600, &[], |_| true);
        eprintln!("200 ppm fast, continuous tone: {report:?}");
        assert_eq!(report.underruns, 0, "{report:?}");
        assert!(
            report.high <= HIGH + Duration::from_millis(21),
            "{report:?}"
        );
        assert!((68..=80).contains(&report.stats.padded), "{report:?}");
    }

    #[test]
    fn speech_pauses_take_every_adjustment_without_cutting_audio() {
        for ppm in [-200.0, 200.0] {
            // One second of silence, then two of tone.
            let report = simulate_ring(ppm, 480, 600, &[], |quantum| quantum % 300 >= 100);
            eprintln!("{ppm} ppm with pauses: {report:?}");
            assert_eq!(report.stats.forced, 0, "{report:?}");
            assert_eq!(report.underruns, 0, "{report:?}");
            assert!(
                report.high <= HIGH + Duration::from_millis(20),
                "{report:?}"
            );
        }
    }

    #[test]
    fn short_stalls_do_not_raise_the_queue_for_good() {
        // The worker replays up to 30 ms after a stall, but the device has
        // already played silence for it, so each replay stays queued. Longer
        // stalls are skipped instead.
        for stall in [25_000, 150_000] {
            let stalls: Vec<_> = (1..=3).map(|n| (n * 10_000_000, stall)).collect();
            let report = simulate_ring(0.0, 512, 60, &stalls, |_| true);
            eprintln!("three stalls of {stall} us: {report:?}");
            assert!(report.settled_low <= HIGH, "{report:?}");
            assert!(report.last_excursion.unwrap_or(0) <= 33, "{report:?}");
            assert_eq!(report.stats.overflow, 0, "{report:?}");
        }
    }

    #[test]
    fn a_full_queue_counts_every_dropped_quantum() {
        let (mut producer, _consumer) = rtrb::RingBuffer::new(24_000);
        let mut pacer = Pacer::default();
        let mut tap = Vec::new();
        for _ in 0..30 {
            pacer.push(&mut producer, Duration::ZERO, 0, &[0.1; 960], &mut tap);
        }
        // A device that never pulls: 25 quanta fit, the rest are counted.
        assert_eq!(
            tap.len() as u64 + pacer.stats().overflow + pacer.stats().trimmed,
            30
        );
        assert!(pacer.stats().overflow > 0);
    }

    /// Run the worker's setup loop until `reply` resolves.
    fn settle(
        runtime: &mut Runtime,
        mut reply: tokio::sync::oneshot::Receiver<Result<(), String>>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            runtime.poll_setup();
            match reply.try_recv() {
                Ok(result) => return result,
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "settings never applied");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("{error}"),
            }
        }
    }

    #[test]
    fn settings_apply_off_the_worker_in_order_and_reject_invalid_values() {
        let mut runtime = runtime(VoiceProfile::Voice, TalkGate::OpenMic);
        let (first_tx, first) = tokio::sync::oneshot::channel();
        let (second_tx, second) = tokio::sync::oneshot::channel();
        runtime.update(Update::Configure {
            config: AudioConfig {
                gate: voice_activation(),
                master_gain: 1.5,
                ..runtime.config.clone()
            },
            reply: first_tx,
        });
        // The chain is built elsewhere: nothing changed yet, and a second
        // Apply waits behind the first instead of racing it.
        assert!(runtime.busy);
        assert!(runtime.vad.is_none());
        runtime.update(Update::Configure {
            config: AudioConfig {
                master_gain: 0.5,
                ..runtime.config.clone()
            },
            reply: second_tx,
        });
        assert_eq!(runtime.deferred.len(), 1);
        assert_eq!(settle(&mut runtime, first), Ok(()));
        assert_eq!(settle(&mut runtime, second), Ok(()));
        assert!(!runtime.busy);
        assert!((runtime.config.master_gain - 0.5).abs() < f32::EPSILON);
        assert!(matches!(runtime.config.gate, TalkGate::OpenMic));
        assert!(runtime.vad.is_none());

        let (tx, rx) = tokio::sync::oneshot::channel();
        runtime.update(Update::Configure {
            config: AudioConfig {
                master_gain: 3.0,
                ..runtime.config.clone()
            },
            reply: tx,
        });
        assert!(settle(&mut runtime, rx).is_err());
        assert!(!runtime.busy);
    }

    #[test]
    fn every_gate_ends_each_burst_once_and_restarts_after_a_capture_gap() {
        for profile in [
            VoiceProfile::Voice,
            VoiceProfile::Music,
            VoiceProfile::LowLatency,
        ] {
            for gate in [TalkGate::PushToTalk, TalkGate::OpenMic, voice_activation()] {
                let mut runtime = runtime(profile, gate);
                let (talk, expected) = script(gate, 20);
                let packets: Vec<_> = drive(&mut runtime, &talk, 0.1)
                    .into_iter()
                    .map(|(_, packet)| packet)
                    .collect();
                assert!(!runtime.graph.snapshot().talking, "{profile:?} {gate:?}");
                let frame = u32::try_from(profile.encoder_params().frame_size.as_millis()).unwrap();
                assert_bursts(&packets, frame * 48, &expected);
            }
        }
    }

    #[test]
    fn voice_activation_transmits_samples_above_full_scale() {
        let mut runtime = runtime(VoiceProfile::Voice, voice_activation());
        let (talk, expected) = script(voice_activation(), 20);
        let packets: Vec<_> = drive(&mut runtime, &talk, 1.25)
            .into_iter()
            .map(|(_, packet)| packet)
            .collect();
        assert_bursts(&packets, 960, &expected);
    }

    #[test]
    fn receiver_loses_no_leading_packets_across_voice_activation_bursts() {
        for profile in [VoiceProfile::Voice, VoiceProfile::LowLatency] {
            let mut sender = runtime(profile, voice_activation());
            let (talk, _) = script(voice_activation(), 40);
            let packets = drive(&mut sender, &talk, 0.1);
            let mut receiver = VoiceGraph::new(profile, 64_000).unwrap();
            receiver.set_track_id(2);
            receiver.sync_tracks(
                &[TrackSpec {
                    id: 1,
                    user: Id::generate(),
                    conn: Id::generate(),
                    gain: 1.0,
                    muted: false,
                }],
                Duration::ZERO,
            );
            let mut pending = packets.iter().peekable();
            for step in 0..talk.len() + 40 {
                // A packet leaves after the quantum that completes it and
                // crosses a steady 5 ms network.
                let now = Duration::from_millis(10) * u32::try_from(step).unwrap();
                while let Some((index, packet)) = pending.next_if(|(index, _)| *index < step) {
                    let arrival = Duration::from_millis(10) * u32::try_from(*index + 1).unwrap()
                        + Duration::from_millis(5);
                    receiver.push_datagram(packet, arrival);
                }
                receiver.render(now, false);
            }
            let stats = receiver.snapshot().tracks[&1].stats.jitter;
            assert_eq!(
                stats.accepted,
                packets.len() as u64,
                "{profile:?} {stats:?}"
            );
            assert_eq!(stats.late, 0, "{profile:?} {stats:?}");
            assert_eq!(stats.lost, 0, "{profile:?} {stats:?}");
        }
    }

    /// Unplug and replug devices under a running worker, through the mock host.
    mod recovery {
        use ss_audio::{
            device::{CaptureChannel, DeviceId, Direction},
            processing::MAX_RENDER_BATCH,
        };

        use super::{
            super::host::mock::{self, Shared as World},
            *,
        };

        const SECOND: Duration = Duration::from_secs(1);

        fn device_runtime(world: &World, config: AudioConfig) -> Runtime {
            let config = AudioConfig {
                source: AudioSource::Device,
                gate: TalkGate::OpenMic,
                ..config
            };
            build(VoiceProfile::Voice, config, mock::hosts(world)).unwrap()
        }

        fn id(raw: &str) -> DeviceId {
            DeviceId::new(raw)
        }

        /// The device a direction's stream is open on.
        fn open_on(runtime: &Runtime, direction: Direction) -> Option<DeviceId> {
            let devices = runtime.devices.as_ref()?;
            match direction {
                Direction::Capture => devices
                    .capture
                    .as_ref()
                    .map(|(stream, _)| stream.device_id().clone()),
                Direction::Playback => devices
                    .playback
                    .as_ref()
                    .map(|(stream, _)| stream.device_id().clone()),
            }
        }

        fn duplex_open(runtime: &Runtime) -> bool {
            runtime
                .devices
                .as_ref()
                .is_some_and(|devices| devices.voice_processing.is_some())
        }

        /// Run the worker's device loop for `limit`, or until `done`. Returns
        /// the time taken and the largest render tap seen.
        fn pump(
            runtime: &mut Runtime,
            world: &World,
            limit: Duration,
            done: impl Fn(&Runtime) -> bool,
        ) -> (Duration, usize) {
            let start = Instant::now();
            let mut next = start;
            let mut tap = 0;
            while start.elapsed() < limit && !done(runtime) {
                runtime.poll_setup();
                runtime.device_events();
                if Instant::now() >= next {
                    runtime.render_once();
                    next += QUANTUM;
                }
                world.lock().drain_output();
                tap = tap.max(runtime.render_tap.len());
                thread::sleep(Duration::from_millis(1));
            }
            (start.elapsed(), tap)
        }

        /// One unplug/replug script, run on its own thread: the worker is
        /// not `Send`, so each case builds its runtime there.
        fn in_parallel(cases: Vec<Box<dyn FnOnce() + Send>>) {
            let handles: Vec<_> = cases.into_iter().map(thread::spawn).collect();
            for handle in handles {
                handle.join().unwrap();
            }
        }

        /// Route changes and stream errors the worker reported, in order.
        fn reported(
            events: &mut tokio::sync::broadcast::Receiver<ClientEvent>,
        ) -> (Vec<(Route, Route)>, usize) {
            let mut routes = Vec::new();
            let mut errors = 0;
            while let Ok(event) = events.try_recv() {
                match event {
                    ClientEvent::VoiceDevice(VoiceDeviceState::Routes { capture, playback }) => {
                        routes.push((capture, playback));
                    }
                    ClientEvent::VoiceDevice(VoiceDeviceState::Unavailable(_)) => errors += 1,
                    _ => {}
                }
            }
            (routes, errors)
        }

        /// `route` in `direction`, the other one fine.
        fn only(direction: Direction, route: Route) -> (Route, Route) {
            match direction {
                Direction::Capture => (route, Route::Wanted),
                Direction::Playback => (Route::Wanted, route),
            }
        }

        fn selected(direction: Direction, id: Option<DeviceId>) -> AudioConfig {
            match direction {
                Direction::Capture => AudioConfig {
                    input_device: id,
                    ..AudioConfig::default()
                },
                Direction::Playback => AudioConfig {
                    output_device: id,
                    ..AudioConfig::default()
                },
            }
        }

        /// A headset that is the only device in `direction` disappears, the
        /// reopen fails, and it comes back.
        fn lost_and_returned(direction: Direction, select: bool, announce: bool) {
            let label = format!("{direction:?} selected={select} announce={announce}");
            let world = match direction {
                Direction::Capture => mock::world(&["headset"], &["speakers"]),
                Direction::Playback => mock::world(&["microphone"], &["headset"]),
            };
            let config = selected(direction, select.then(|| id("headset")));
            let mut runtime = device_runtime(&world, config);
            let mut events = runtime.events.subscribe();
            assert_eq!(open_on(&runtime, direction), Some(id("headset")), "{label}");
            pump(&mut runtime, &world, Duration::from_millis(50), |_| false);

            world.lock().unplug(direction, "headset");
            pump(&mut runtime, &world, Duration::from_millis(1_300), |_| {
                false
            });
            assert_eq!(open_on(&runtime, direction), None, "{label}");
            let wanted = select.then(|| id("headset"));
            let attempts = world.lock().attempts(Some(direction), wanted.as_ref());
            // The open at start, then a retry about every 500 ms.
            assert!(
                (3..=6).contains(&attempts),
                "{label}: {attempts} opens while the device was gone"
            );

            world.lock().plug(direction, "headset", true, announce);
            let (took, _) = pump(&mut runtime, &world, 2 * SECOND, |runtime| {
                open_on(runtime, direction).is_some()
            });
            assert_eq!(
                open_on(&runtime, direction),
                Some(id("headset")),
                "{label}: not reopened after {took:?}"
            );
            assert!(took < SECOND, "{label}: reopened after {took:?}");
            pump(&mut runtime, &world, Duration::from_millis(20), |_| false);
            let (routes, errors) = reported(&mut events);
            assert_eq!(
                routes,
                [
                    only(direction, Route::Wanted),
                    only(direction, Route::Missing),
                    only(direction, Route::Wanted),
                ],
                "{label}"
            );
            assert_eq!(errors, 1, "{label}: one notice, not one per retry");
        }

        #[test]
        fn a_lost_device_reopens_within_a_second_of_returning() {
            in_parallel(vec![
                Box::new(|| lost_and_returned(Direction::Capture, true, true)),
                Box::new(|| lost_and_returned(Direction::Capture, false, false)),
                Box::new(|| lost_and_returned(Direction::Playback, true, false)),
                Box::new(|| lost_and_returned(Direction::Playback, false, true)),
            ]);
        }

        #[test]
        fn a_closed_capture_keeps_the_render_tap_bounded() {
            let world = mock::world(&["headset"], &["speakers"]);
            let mut runtime =
                device_runtime(&world, selected(Direction::Capture, Some(id("headset"))));
            world.lock().unplug(Direction::Capture, "headset");
            let (_, tap) = pump(&mut runtime, &world, SECOND, |_| false);
            assert_eq!(open_on(&runtime, Direction::Capture), None);
            assert!(
                tap <= MAX_RENDER_BATCH,
                "{tap} rendered frames held for a capture that is closed"
            );
        }

        /// The selected device goes away while the default stays: the stream
        /// moves to the default, then back when the selected one returns.
        fn falls_back_and_returns(direction: Direction, announce: bool) {
            let label = format!("{direction:?} announce={announce}");
            let world = mock::world(&["builtin", "headset"], &["builtin", "headset"]);
            let mut runtime = device_runtime(&world, selected(direction, Some(id("headset"))));
            let mut events = runtime.events.subscribe();
            assert_eq!(open_on(&runtime, direction), Some(id("headset")), "{label}");
            pump(&mut runtime, &world, Duration::from_millis(50), |_| false);

            world.lock().unplug(direction, "headset");
            let (took, _) = pump(&mut runtime, &world, 2 * SECOND, |runtime| {
                open_on(runtime, direction) == Some(id("builtin"))
            });
            assert_eq!(
                open_on(&runtime, direction),
                Some(id("builtin")),
                "{label}: no fallback after {took:?}"
            );
            assert!(took < SECOND, "{label}: fell back after {took:?}");

            world.lock().plug(direction, "headset", false, announce);
            let (took, _) = pump(&mut runtime, &world, 2 * SECOND, |runtime| {
                open_on(runtime, direction) == Some(id("headset"))
            });
            assert_eq!(
                open_on(&runtime, direction),
                Some(id("headset")),
                "{label}: still on the fallback after {took:?}"
            );
            assert!(took < SECOND, "{label}: returned after {took:?}");
            pump(&mut runtime, &world, Duration::from_millis(20), |_| false);
            let (routes, errors) = reported(&mut events);
            assert_eq!(
                routes,
                [
                    only(direction, Route::Wanted),
                    only(direction, Route::Fallback),
                    only(direction, Route::Wanted),
                ],
                "{label}"
            );
            assert_eq!(errors, 0, "{label}");
        }

        #[test]
        fn a_missing_selected_device_falls_back_to_the_default_and_returns() {
            in_parallel(vec![
                Box::new(|| falls_back_and_returns(Direction::Capture, true)),
                Box::new(|| falls_back_and_returns(Direction::Capture, false)),
                Box::new(|| falls_back_and_returns(Direction::Playback, true)),
                Box::new(|| falls_back_and_returns(Direction::Playback, false)),
            ]);
        }

        fn voice_processing_returns(announce: bool) {
            let world = mock::world(&["headset"], &["headset"]);
            let mut runtime = device_runtime(
                &world,
                AudioConfig {
                    device_backend: DeviceBackend::AppleVoiceProcessing {
                        bypass: false,
                        automatic_gain_control: true,
                    },
                    ..AudioConfig::default()
                },
            );
            let mut events = runtime.events.subscribe();
            assert!(duplex_open(&runtime));
            pump(&mut runtime, &world, Duration::from_millis(50), |_| false);

            {
                let mut world = world.lock();
                world.unplug(Direction::Capture, "headset");
                world.unplug(Direction::Playback, "headset");
            }
            let (_, tap) = pump(&mut runtime, &world, Duration::from_millis(1_300), |_| {
                false
            });
            assert!(!duplex_open(&runtime), "announce={announce}");
            assert!(tap <= MAX_RENDER_BATCH, "announce={announce}: tap {tap}");
            let attempts = world.lock().attempts(None, None);
            assert!(
                (3..=6).contains(&attempts),
                "announce={announce}: {attempts} opens while the devices were gone"
            );

            {
                let mut world = world.lock();
                world.plug(Direction::Capture, "headset", true, announce);
                world.plug(Direction::Playback, "headset", true, announce);
            }
            let (took, _) = pump(&mut runtime, &world, 2 * SECOND, duplex_open);
            assert!(
                duplex_open(&runtime),
                "announce={announce}: not reopened after {took:?}"
            );
            assert!(
                took < SECOND,
                "announce={announce}: reopened after {took:?}"
            );
            pump(&mut runtime, &world, Duration::from_millis(20), |_| false);
            let (routes, errors) = reported(&mut events);
            let (wanted, missing) = (Route::Wanted, Route::Missing);
            assert_eq!(
                routes,
                [(wanted, wanted), (missing, missing), (wanted, wanted)],
                "announce={announce}"
            );
            assert_eq!(errors, 1, "announce={announce}");
        }

        #[test]
        fn voice_processing_reopens_within_a_second_of_its_devices_returning() {
            in_parallel(vec![
                Box::new(|| voice_processing_returns(true)),
                Box::new(|| voice_processing_returns(false)),
            ]);
        }

        #[test]
        fn apply_reopens_a_closed_stream_with_the_same_settings() {
            let world = mock::world(&["headset"], &["speakers"]);
            let mut runtime =
                device_runtime(&world, selected(Direction::Capture, Some(id("headset"))));
            world.lock().unplug(Direction::Capture, "headset");
            pump(&mut runtime, &world, Duration::from_millis(100), |_| false);
            assert_eq!(open_on(&runtime, Direction::Capture), None);
            // Back, but not announced, and before the next timed retry.
            world
                .lock()
                .plug(Direction::Capture, "headset", true, false);
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.update(Update::Configure {
                config: runtime.config.clone(),
                reply: tx,
            });
            assert_eq!(settle(&mut runtime, rx), Ok(()));
            assert_eq!(
                open_on(&runtime, Direction::Capture),
                Some(id("headset")),
                "Apply left the capture stream closed"
            );
        }

        #[test]
        fn apply_reopens_capture_on_the_chosen_input_channel() {
            let world = mock::world(&["interface"], &["speakers"]);
            let mut runtime = device_runtime(&world, AudioConfig::default());
            let asked = || world.lock().capture_config.map(|config| config.input);
            assert_eq!(asked(), Some(CaptureChannel::FIRST));
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.update(Update::Configure {
                config: AudioConfig {
                    input_channel: CaptureChannel::Index(1),
                    ..runtime.config.clone()
                },
                reply: tx,
            });
            assert_eq!(settle(&mut runtime, rx), Ok(()));
            assert_eq!(asked(), Some(CaptureChannel::Index(1)));
            assert!(open_on(&runtime, Direction::Capture).is_some());
        }

        #[test]
        fn a_failed_device_change_keeps_retrying_the_previous_devices() {
            let world = mock::world(&["headset"], &["speakers"]);
            let mut runtime =
                device_runtime(&world, selected(Direction::Capture, Some(id("headset"))));
            world.lock().unplug(Direction::Capture, "headset");
            pump(&mut runtime, &world, Duration::from_millis(100), |_| false);
            let (tx, rx) = tokio::sync::oneshot::channel();
            runtime.update(Update::Configure {
                config: AudioConfig {
                    input_device: Some(id("elsewhere")),
                    ..runtime.config.clone()
                },
                reply: tx,
            });
            assert!(settle(&mut runtime, rx).is_err());
            assert_eq!(runtime.config.input_device, Some(id("headset")));
            assert_eq!(open_on(&runtime, Direction::Playback), Some(id("speakers")));

            world.lock().plug(Direction::Capture, "headset", true, true);
            let (took, _) = pump(&mut runtime, &world, 2 * SECOND, |runtime| {
                open_on(runtime, Direction::Capture).is_some()
            });
            assert_eq!(
                open_on(&runtime, Direction::Capture),
                Some(id("headset")),
                "not reopened after {took:?}"
            );
        }
    }
}
