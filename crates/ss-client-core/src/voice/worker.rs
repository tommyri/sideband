//! Dedicated audio worker. Device callbacks remain inside `ss-audio`.

use std::{
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
#[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
use ss_audio::device::{MutedSpeechActivity, VoiceProcessingConfig, VoiceProcessingStream};
use ss_audio::{
    device::{
        self, CaptureStream, DeviceEvent, DeviceManager, Direction, GraphConfig, PlaybackStream,
    },
    mixer::StereoFrame,
    processing::{Chain, EchoPath},
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
};

pub(super) enum Update {
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
    Bitrate(u32),
    DeviceBackend {
        backend: DeviceBackend,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Clone, Default)]
pub(super) struct WorkerStats {
    pub graph: GraphSnapshot,
    pub underruns: u64,
    pub overruns: u64,
    pub processing_latency: Duration,
    pub processing_over_budget: u64,
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
}

pub(super) struct WorkerHandle {
    pub updates: SyncSender<Update>,
    pub incoming: SyncSender<IncomingMedia>,
    pub shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl WorkerHandle {
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

struct Devices {
    manager: DeviceManager,
    #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
    capture_config: GraphConfig,
    capture: Option<(CaptureStream, Consumer<f32>)>,
    playback: Option<(PlaybackStream, Producer<f32>)>,
    #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
    voice_processing: Option<(VoiceProcessingStream, Consumer<f32>, Producer<f32>)>,
    #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
    voice_muted: bool,
    #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
    voice_mode: ss_audio::device::MicrophoneMode,
}

impl Devices {
    fn capture_consumer_mut(&mut self) -> Option<&mut Consumer<f32>> {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        if let Some((_, consumer, _)) = &mut self.voice_processing {
            return Some(consumer);
        }
        self.capture.as_mut().map(|(_, consumer)| consumer)
    }

    fn playback_producer_mut(&mut self) -> Option<&mut Producer<f32>> {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        if let Some((_, _, producer)) = &mut self.voice_processing {
            return Some(producer);
        }
        self.playback.as_mut().map(|(_, producer)| producer)
    }

    fn delays(&self) -> (Duration, Duration) {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        if self.voice_processing.is_some() {
            // Apple's AEC owns its timing model; the software chain is off.
            return (Duration::ZERO, Duration::ZERO);
        }
        let capture = self
            .capture
            .as_ref()
            .and_then(|(stream, _)| stream.capture_delay())
            .unwrap_or_default();
        let playback = self
            .playback
            .as_ref()
            .and_then(|(stream, _)| stream.playback_delay())
            .unwrap_or_default();
        (capture, playback)
    }

    fn counter_values(&self) -> (u64, u64) {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        if let Some((stream, _, _)) = &self.voice_processing {
            return (
                stream.playback_counters().underruns(),
                stream.capture_counters().overruns(),
            );
        }
        let underruns = self
            .playback
            .as_ref()
            .map_or(0, |(stream, _)| stream.counters().underruns());
        let overruns = self
            .capture
            .as_ref()
            .map_or(0, |(stream, _)| stream.counters().overruns());
        (underruns, overruns)
    }

    #[cfg_attr(
        not(all(target_os = "macos", feature = "macos-voiceio")),
        allow(clippy::unused_self)
    )]
    fn voice_processing_active(&self) -> bool {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        {
            self.voice_processing.is_some()
        }
        #[cfg(not(all(target_os = "macos", feature = "macos-voiceio")))]
        {
            false
        }
    }

    #[cfg_attr(
        not(all(target_os = "macos", feature = "macos-voiceio")),
        allow(clippy::unused_self)
    )]
    fn voice_processing_stopped(&self) -> bool {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        {
            self.voice_processing
                .as_ref()
                .is_some_and(|(stream, _, _)| stream.is_running().is_ok_and(|running| !running))
        }
        #[cfg(not(all(target_os = "macos", feature = "macos-voiceio")))]
        {
            false
        }
    }

    #[cfg_attr(
        not(all(target_os = "macos", feature = "macos-voiceio")),
        allow(clippy::unused_self)
    )]
    fn sync_voice_processing(
        &mut self,
        muted: bool,
        events: &tokio::sync::broadcast::Sender<ClientEvent>,
    ) {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        if let Some((stream, _, _)) = &mut self.voice_processing {
            if muted != self.voice_muted {
                match stream.set_muted(muted) {
                    Ok(()) => self.voice_muted = muted,
                    Err(error) => {
                        let _ = events.send(ClientEvent::VoiceDevice(
                            VoiceDeviceState::Unavailable(error.to_string()),
                        ));
                    }
                }
            }
            if let Some(activity) = stream.take_muted_speech_activity() {
                let _ = events.send(ClientEvent::VoiceDevice(VoiceDeviceState::MutedSpeech {
                    active: activity == MutedSpeechActivity::Started,
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
        #[cfg(not(all(target_os = "macos", feature = "macos-voiceio")))]
        let _ = (muted, events);
    }

    #[cfg_attr(
        not(all(target_os = "macos", feature = "macos-voiceio")),
        allow(clippy::unused_self)
    )]
    fn reopen_voice_processing(&mut self, config: &AudioConfig) -> Result<(), String> {
        #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
        {
            let DeviceBackend::AppleVoiceProcessing {
                bypass,
                automatic_gain_control,
            } = config.device_backend
            else {
                return Ok(());
            };
            self.voice_processing = None;
            let streams = self
                .manager
                .open_voice_processing(
                    self.capture_config,
                    GraphConfig::STEREO,
                    VoiceProcessingConfig {
                        bypass,
                        automatic_gain_control,
                        muted: self.voice_muted,
                    },
                )
                .map_err(|error| error.to_string())?;
            self.voice_processing = Some(streams);
            self.voice_mode = ss_audio::device::MicrophoneMode::Unknown;
            Ok(())
        }
        #[cfg(not(all(target_os = "macos", feature = "macos-voiceio")))]
        {
            let _ = config;
            Err("Apple VoiceProcessingIO is not enabled in this build".to_owned())
        }
    }
}

fn open_devices(
    manager: DeviceManager,
    config: &AudioConfig,
    capture_config: GraphConfig,
    events: &tokio::sync::broadcast::Sender<ClientEvent>,
) -> Result<Devices, String> {
    match config.device_backend {
        DeviceBackend::Portable => {
            let capture = match manager.open_capture(config.input_device.as_ref(), capture_config) {
                Ok(capture) => capture,
                Err(error) => {
                    send_device_error(events, &error);
                    return Err(error.to_string());
                }
            };
            let playback =
                match manager.open_playback(config.output_device.as_ref(), GraphConfig::STEREO) {
                    Ok(playback) => playback,
                    Err(error) => {
                        send_device_error(events, &error);
                        return Err(error.to_string());
                    }
                };
            Ok(Devices {
                manager,
                #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
                capture_config,
                capture: Some(capture),
                playback: Some(playback),
                #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
                voice_processing: None,
                #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
                voice_muted: config.start_muted,
                #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
                voice_mode: ss_audio::device::MicrophoneMode::Unknown,
            })
        }
        DeviceBackend::AppleVoiceProcessing { .. } => {
            open_voice_processing_devices(manager, config, capture_config, events)
        }
    }
}

#[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
fn open_voice_processing_devices(
    manager: DeviceManager,
    config: &AudioConfig,
    capture_config: GraphConfig,
    events: &tokio::sync::broadcast::Sender<ClientEvent>,
) -> Result<Devices, String> {
    if config.input_device.is_some() || config.output_device.is_some() {
        return Err("Apple VoiceProcessingIO requires the system-default devices".to_owned());
    }
    let DeviceBackend::AppleVoiceProcessing {
        bypass,
        automatic_gain_control,
    } = config.device_backend
    else {
        unreachable!("called only for the Apple backend");
    };
    let streams = manager
        .open_voice_processing(
            capture_config,
            GraphConfig::STEREO,
            VoiceProcessingConfig {
                bypass,
                automatic_gain_control,
                muted: config.start_muted,
            },
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
    Ok(Devices {
        manager,
        capture_config,
        capture: None,
        playback: None,
        voice_processing: Some(streams),
        voice_muted: config.start_muted,
        voice_mode,
    })
}

#[cfg(not(all(target_os = "macos", feature = "macos-voiceio")))]
fn open_voice_processing_devices(
    manager: DeviceManager,
    config: &AudioConfig,
    capture_config: GraphConfig,
    events: &tokio::sync::broadcast::Sender<ClientEvent>,
) -> Result<Devices, String> {
    let _ = (manager, config, capture_config, events);
    Err("Apple VoiceProcessingIO is unavailable on this platform or build".to_owned())
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
    devices: Option<Devices>,
    chain: Chain,
    vad: Option<Vad>,
    epoch: Instant,
    capture: Vec<f32>,
    render_tap: Vec<StereoFrame>,
    test_sample: usize,
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
    ) -> Result<Self, String> {
        let graph = VoiceGraph::new(profile, bitrate).map_err(|error| error.to_string())?;
        let channels = u16::try_from(graph.channels()).map_err(|error| error.to_string())?;
        let processing = match config.device_backend {
            DeviceBackend::Portable => config.processing,
            DeviceBackend::AppleVoiceProcessing { .. } => {
                ss_audio::processing::ProcessingConfig::OFF
            }
        };
        let chain = Chain::new(processing, channels).map_err(|error| error.to_string())?;
        let vad = match config.gate {
            TalkGate::VoiceActivation(mut vad) => {
                vad.channels = u8::try_from(graph.channels()).map_err(|error| error.to_string())?;
                Some(Vad::new(vad).map_err(|error| error.to_string())?)
            }
            TalkGate::PushToTalk | TalkGate::OpenMic => None,
        };
        let devices = if config.source == AudioSource::Device {
            let manager = DeviceManager::new();
            let capture_config = graph_config(graph.channels());
            Some(open_devices(manager, &config, capture_config, &events)?)
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
            devices,
            chain,
            vad,
            epoch: Instant::now(),
            capture: vec![0.0; quantum],
            render_tap: Vec::with_capacity(8),
            test_sample: 0,
        })
    }

    fn run(&mut self) {
        let mut next = Instant::now();
        while !self.stop.load(Ordering::Acquire) {
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
                next += Duration::from_millis(10);
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

    fn drain_updates(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::Tracks { own, remote } => {
                    self.graph.set_track_id(own);
                    self.graph.sync_tracks(&remote, self.epoch.elapsed());
                }
                Update::User { user, gain, muted } => self.graph.set_user(user, gain, muted),
                Update::Cue(cue) => self.graph.play_cue(cue),
                Update::Bitrate(bitrate) => {
                    if let Err(error) = self.graph.set_bitrate(bitrate) {
                        tracing::warn!(%error, bitrate, "encoder adaptation was rejected");
                    }
                }
                Update::DeviceBackend { backend, reply } => {
                    let result = self.switch_device_backend(backend);
                    let _ = reply.send(result);
                }
            }
        }
    }

    fn switch_device_backend(&mut self, backend: DeviceBackend) -> Result<(), String> {
        if self.config.device_backend == backend {
            return Ok(());
        }
        let previous = self.config.device_backend;
        let channels = u16::try_from(self.graph.channels()).map_err(|error| error.to_string())?;
        let processing = match backend {
            DeviceBackend::Portable => self.config.processing,
            DeviceBackend::AppleVoiceProcessing { .. } => {
                ss_audio::processing::ProcessingConfig::OFF
            }
        };
        let chain = Chain::new(processing, channels).map_err(|error| error.to_string())?;
        self.devices = None;
        self.config.device_backend = backend;
        if self.config.source == AudioSource::Device {
            let manager = DeviceManager::new();
            match open_devices(
                manager,
                &self.config,
                graph_config(self.graph.channels()),
                &self.events,
            ) {
                Ok(devices) => self.devices = Some(devices),
                Err(error) => {
                    self.config.device_backend = previous;
                    let manager = DeviceManager::new();
                    self.devices = open_devices(
                        manager,
                        &self.config,
                        graph_config(self.graph.channels()),
                        &self.events,
                    )
                    .ok();
                    return Err(error);
                }
            }
        }
        self.chain = chain;
        self.render_tap.clear();
        Ok(())
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

    fn capture_vad(&mut self, blocked: bool) {
        let Some(vad) = self.vad.as_mut() else { return };
        if blocked {
            let _ = vad.force_close();
            let _ = self.graph.push_capture(&self.capture, false);
            return;
        }
        let Ok(output) = vad.process(&self.capture) else {
            return;
        };
        let close = matches!(output.event, Some(ss_audio::vad::GateEvent::Close));
        for frame in output.audio() {
            let _ = self.graph.push_capture(frame, true);
        }
        if close {
            self.capture.fill(0.0);
            let _ = self.graph.push_capture(&self.capture, false);
        }
    }

    fn render_once(&mut self) {
        let deaf = self.shared.deaf.load(Ordering::Relaxed);
        let mixed = self.graph.render(self.epoch.elapsed(), deaf);
        self.render_tap.push(mixed);
        if let Some(devices) = &mut self.devices
            && let Some(producer) = devices.playback_producer_mut()
        {
            let _ = push_playback(producer, &mixed);
        }
        let (underruns, overruns) = self.counters();
        *self.shared.stats.write() = WorkerStats {
            graph: self.graph.snapshot(),
            underruns,
            overruns,
            processing_latency: self.chain.monitor().latency(),
            processing_over_budget: self.chain.monitor().over_budget(),
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
            playback_queue: Duration::from_millis(10),
            playback_device,
            capture_device,
            capture_queue: Duration::ZERO,
        }
    }

    fn counters(&self) -> (u64, u64) {
        let Some(devices) = &self.devices else {
            return (0, 0);
        };
        devices.counter_values()
    }

    fn device_events(&mut self) {
        let event_tx = self.events.clone();
        let Some(devices) = &mut self.devices else {
            return;
        };
        devices.sync_voice_processing(self.shared.muted.load(Ordering::Relaxed), &self.events);
        let mut reopen_capture = false;
        let mut reopen_playback = false;
        let mut reopen_voice_processing = false;
        for event in devices.manager.poll() {
            if let DeviceEvent::SilentInput { access, .. } = &event {
                let _ = self
                    .events
                    .send(ClientEvent::VoiceDevice(VoiceDeviceState::SilentInput {
                        permission_denied: *access == device::MicrophoneAccess::Denied,
                    }));
            }
            if let DeviceEvent::LowQualityInput { sample_rate, .. } = &event {
                let _ = self.events.send(ClientEvent::VoiceDevice(
                    VoiceDeviceState::LowQualityInput {
                        sample_rate: *sample_rate,
                    },
                ));
            }
            if let Some((stream, _)) = &devices.capture {
                reopen_capture |= event.requires_reopen(
                    Direction::Capture,
                    stream.device_id(),
                    self.config.input_device.is_none(),
                );
            }
            if let Some((stream, _)) = &devices.playback {
                reopen_playback |= event.requires_reopen(
                    Direction::Playback,
                    stream.device_id(),
                    self.config.output_device.is_none(),
                );
            }
            if devices.voice_processing_active()
                && matches!(event, DeviceEvent::DefaultChanged { .. })
            {
                reopen_voice_processing = true;
            }
        }
        reopen_voice_processing |= devices.voice_processing_stopped();
        if reopen_voice_processing {
            if let Err(error) = devices.reopen_voice_processing(&self.config) {
                let _ = event_tx.send(ClientEvent::VoiceDevice(VoiceDeviceState::Unavailable(
                    error,
                )));
            }
            return;
        }
        if reopen_capture {
            devices.capture = None;
            match devices.manager.open_capture(
                self.config.input_device.as_ref(),
                graph_config(self.graph.channels()),
            ) {
                Ok(stream) => devices.capture = Some(stream),
                Err(error) => send_device_error(&event_tx, &error),
            }
        }
        if reopen_playback {
            devices.playback = None;
            match devices
                .manager
                .open_playback(self.config.output_device.as_ref(), GraphConfig::STEREO)
            {
                Ok(stream) => devices.playback = Some(stream),
                Err(error) => send_device_error(&event_tx, &error),
            }
        }
    }
}

fn send_device_error(events: &tokio::sync::broadcast::Sender<ClientEvent>, error: &device::Error) {
    let state = if matches!(error, device::Error::PermissionDenied) {
        VoiceDeviceState::PermissionDenied
    } else {
        VoiceDeviceState::Unavailable(error.to_string())
    };
    let _ = events.send(ClientEvent::VoiceDevice(state));
}

fn graph_config(channels: usize) -> GraphConfig {
    if channels == 2 {
        GraphConfig::STEREO
    } else {
        GraphConfig::MONO
    }
}

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
