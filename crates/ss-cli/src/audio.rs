//! Device thread. The cpal callback stays inside `ss-audio`. This thread moves
//! 10 ms quanta between the rings, the codec graph, and the socket tasks.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bytes::Bytes;
use rtrb::{Consumer, Producer};
use ss_audio::{
    device::{self, CaptureStream, DeviceEvent, DeviceManager, GraphConfig, PlaybackStream},
    mixer::StereoFrame,
};
use tokio::sync::{mpsc, oneshot};

use crate::{
    call::{CallError, Gate},
    graph::VoiceGraph,
    measure::{self, Correlator, Probe},
    wav::WavWriter,
};

/// A datagram stamped on the session clock when QUIC delivered it.
pub(crate) struct Incoming {
    pub(crate) bytes: Bytes,
    pub(crate) at: Duration,
}

/// Track membership discovered after the audio thread starts.
pub(crate) enum TrackUpdate {
    /// Start a jitter buffer for this remote track.
    Add(u32),
    /// The publisher left the channel.
    Remove(u32),
}

/// Snapshot the stats line reads once a second.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LiveStats {
    /// Opus payload bytes produced.
    pub(crate) sent_bytes: u64,
    /// Jitter slots committed as lost.
    pub(crate) lost: u64,
    /// Packets admitted by a jitter buffer.
    pub(crate) accepted: u64,
    /// PLC invocations.
    pub(crate) plc: u64,
    /// Largest applied jitter delay.
    pub(crate) buffer: Duration,
    /// Tracks currently marked speaking.
    pub(crate) speakers: usize,
    /// Playback underruns.
    pub(crate) underruns: u64,
    /// Capture overruns.
    pub(crate) overruns: u64,
}

/// Everything the voice thread owns besides the open devices.
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct AudioConfig {
    /// Encoder, decoders, and mixer.
    pub(crate) graph: VoiceGraph,
    /// How captured frames become transmit gates.
    pub(crate) gate: Gate,
    /// Shared with the terminal thread.
    pub(crate) talking: Arc<AtomicBool>,
    /// Inject measurement clicks on this client.
    pub(crate) measure: bool,
    /// Cross-correlate playback on this client.
    pub(crate) correlate: bool,
    /// Open the default input. The measurement ear only plays.
    pub(crate) capture: bool,
    /// Fail the call when the default devices cannot be opened.
    pub(crate) devices_required: bool,
    /// Click pairing shared with the other in-process client.
    pub(crate) probe: Arc<Probe>,
    /// Mixed output path. Only one side of a measurement records.
    pub(crate) record: Option<std::path::PathBuf>,
    /// Session clock origin shared with the receive task.
    pub(crate) epoch: Instant,
    /// Process-wide stop flag. The terminal thread sets it in raw mode.
    pub(crate) stop: Arc<AtomicBool>,
    /// Datagrams from the receive task.
    pub(crate) incoming: mpsc::Receiver<Incoming>,
    /// Membership changes.
    pub(crate) tracks: mpsc::Receiver<TrackUpdate>,
    /// Encoded datagrams for the send task.
    pub(crate) outgoing: mpsc::Sender<Bytes>,
    /// Published every render.
    pub(crate) stats: Arc<Mutex<LiveStats>>,
    /// Lines for the stats task to print without interleaving.
    pub(crate) log: mpsc::UnboundedSender<String>,
}

struct Devices {
    manager: DeviceManager,
    capture: Option<(CaptureStream, Consumer<f32>)>,
    playback: Option<(PlaybackStream, Producer<f32>)>,
}

/// Open devices on a dedicated thread and run until `stop` is set.
pub(crate) async fn start(config: AudioConfig) -> Result<JoinHandle<()>, CallError> {
    let (ready_tx, ready_rx) = oneshot::channel();
    let handle = thread::Builder::new()
        .name("ss-cli-voice".to_owned())
        .spawn(move || thread_main(config, ready_tx))?;
    match ready_rx.await {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(error)) => Err(CallError::Device(error)),
        Err(_) => Err(CallError::AudioStopped),
    }
}

fn thread_main(mut config: AudioConfig, ready: oneshot::Sender<Result<(), device::Error>>) {
    let capture_config = if config.graph.channels() == 2 {
        GraphConfig::STEREO
    } else {
        GraphConfig::MONO
    };
    let devices = match open_devices(capture_config, config.capture) {
        Ok(devices) => {
            let _ = ready.send(Ok(()));
            Some(devices)
        }
        Err(error) if !config.devices_required => {
            let _ = config.log.send(format!(
                "audio devices unavailable ({}); continuing without device delay",
                device_message(&error)
            ));
            let _ = ready.send(Ok(()));
            None
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if config.measure {
        config.graph.set_measure(true);
    }
    if let Err(error) = run_loop(config, devices, capture_config) {
        tracing::error!(%error, "voice thread ended");
    }
}

fn open_devices(capture_config: GraphConfig, capture: bool) -> Result<Devices, device::Error> {
    let manager = DeviceManager::new();
    let capture = if capture {
        Some(manager.open_capture(None, capture_config)?)
    } else {
        None
    };
    let playback = manager.open_playback(None, GraphConfig::STEREO)?;
    Ok(Devices {
        manager,
        capture,
        playback: Some(playback),
    })
}

fn run_loop(
    mut config: AudioConfig,
    mut devices: Option<Devices>,
    capture_config: GraphConfig,
) -> Result<(), CallError> {
    let mut wav = config
        .record
        .as_deref()
        .map(WavWriter::create)
        .transpose()?;
    let mut correlator = config.correlate.then(Correlator::new);
    let mut capture_frame = vec![0.0_f32; config.graph.quantum()];
    let mut next_render = Instant::now();
    let mut next_reopen = Instant::now();
    let mut vad_until = Instant::now();
    loop {
        if config.stop.load(Ordering::Relaxed) {
            break;
        }
        drain_tracks(&mut config);
        drain_incoming(&mut config);
        note_devices(devices.as_mut());
        reopen_lost(devices.as_mut(), capture_config, &mut next_reopen);
        let pulled = pull_capture(devices.as_mut(), &mut capture_frame);
        if pulled {
            apply_gate(&config, &capture_frame, &mut vad_until);
            let delay = capture_delay(devices.as_ref());
            if config
                .graph
                .push_capture(&capture_frame, talking(&config))?
            {
                config.probe.mark(delay);
            }
            render_once(&mut config, devices.as_mut(), &mut wav, correlator.as_mut())?;
            next_render = Instant::now() + Duration::from_millis(10);
        } else if Instant::now() >= next_render {
            if config.measure
                && devices
                    .as_ref()
                    .is_none_or(|devices| devices.capture.is_none())
            {
                capture_frame.fill(0.0);
                if config
                    .graph
                    .push_capture(&capture_frame, talking(&config))?
                {
                    config.probe.mark(Duration::ZERO);
                }
            }
            render_once(&mut config, devices.as_mut(), &mut wav, correlator.as_mut())?;
            next_render += Duration::from_millis(10);
            if next_render + Duration::from_millis(30) < Instant::now() {
                next_render = Instant::now();
            }
        } else {
            thread::sleep(Duration::from_millis(1));
        }
        for packet in config.graph.pop_outgoing() {
            if config.outgoing.try_send(packet).is_err() {
                break;
            }
        }
    }
    Ok(())
}

fn drain_tracks(config: &mut AudioConfig) {
    while let Ok(update) = config.tracks.try_recv() {
        match update {
            TrackUpdate::Add(id) => config.graph.add_track(id),
            TrackUpdate::Remove(id) => config.graph.remove_track(id),
        }
    }
}

fn drain_incoming(config: &mut AudioConfig) {
    while let Ok(incoming) = config.incoming.try_recv() {
        config.graph.push_datagram(&incoming.bytes, incoming.at);
    }
}

fn render_once(
    config: &mut AudioConfig,
    devices: Option<&mut Devices>,
    wav: &mut Option<WavWriter>,
    correlator: Option<&mut Correlator>,
) -> Result<(), CallError> {
    let mixed = config.graph.render(config.epoch.elapsed());
    let (playback, underruns, overruns) = write_playback(devices, &mixed);
    if let Some(wav) = wav {
        wav.write(&mixed)?;
    }
    if let Some(correlator) = correlator
        && let Some(heard) = correlator.push(&mixed, Instant::now())
        && let Some(sample) = config.probe.hear(heard, playback)
    {
        let _ = config.log.send(measure::format_sample(sample));
    }
    publish(config, underruns, overruns);
    Ok(())
}

fn write_playback(devices: Option<&mut Devices>, frame: &StereoFrame) -> (Duration, u64, u64) {
    let Some(devices) = devices else {
        return (Duration::ZERO, 0, 0);
    };
    if let Some((_, producer)) = devices.playback.as_mut() {
        let _ = push_playback(producer, frame);
    }
    let playback = devices
        .playback
        .as_ref()
        .and_then(|(stream, _)| stream.playback_delay())
        .unwrap_or_default();
    let underruns = devices
        .playback
        .as_ref()
        .map_or(0, |(stream, _)| stream.counters().underruns());
    let overruns = devices
        .capture
        .as_ref()
        .map_or(0, |(stream, _)| stream.counters().overruns());
    (playback, underruns, overruns)
}

fn publish(config: &AudioConfig, underruns: u64, overruns: u64) {
    let snapshot = config.graph.snapshot();
    *config
        .stats
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = LiveStats {
        sent_bytes: snapshot.sent_bytes,
        lost: snapshot.lost,
        accepted: snapshot.accepted,
        plc: snapshot.plc,
        buffer: snapshot.buffer,
        speakers: snapshot.speakers,
        underruns,
        overruns,
    };
}

fn apply_gate(config: &AudioConfig, frame: &[f32], vad_until: &mut Instant) {
    if config.gate != Gate::Vad {
        return;
    }
    let now = Instant::now();
    if mean_square(frame) >= 1.0e-4 {
        *vad_until = now + Duration::from_millis(150);
        config.talking.store(true, Ordering::Relaxed);
    } else if now >= *vad_until {
        config.talking.store(false, Ordering::Relaxed);
    }
}

fn talking(config: &AudioConfig) -> bool {
    config.talking.load(Ordering::Relaxed)
}

fn note_devices(devices: Option<&mut Devices>) {
    let Some(devices) = devices else {
        return;
    };
    for event in devices.manager.poll() {
        if let DeviceEvent::LowQualityInput { .. } = event {
            tracing::warn!("input device is 8 or 16 kHz; speech will sound narrow");
        }
    }
}

fn reopen_lost(
    devices: Option<&mut Devices>,
    capture_config: GraphConfig,
    next_reopen: &mut Instant,
) {
    let Some(devices) = devices else {
        return;
    };
    if Instant::now() < *next_reopen {
        return;
    }
    let capture_lost = devices
        .capture
        .as_ref()
        .is_some_and(|(stream, _)| stream.is_lost());
    let playback_lost = devices
        .playback
        .as_ref()
        .is_some_and(|(stream, _)| stream.is_lost());
    if !capture_lost && !playback_lost {
        return;
    }
    *next_reopen = Instant::now() + Duration::from_secs(1);
    if capture_lost {
        devices.capture = None;
        match devices.manager.open_capture(None, capture_config) {
            Ok(opened) => devices.capture = Some(opened),
            Err(error) => tracing::warn!(error = %device_message(&error), "capture did not reopen"),
        }
    }
    if playback_lost {
        devices.playback = None;
        match devices.manager.open_playback(None, GraphConfig::STEREO) {
            Ok(opened) => devices.playback = Some(opened),
            Err(error) => {
                tracing::warn!(error = %device_message(&error), "playback did not reopen");
            }
        }
    }
}

fn pull_capture(devices: Option<&mut Devices>, frame: &mut [f32]) -> bool {
    let Some(devices) = devices else {
        return false;
    };
    let Some((_, consumer)) = devices.capture.as_mut() else {
        return false;
    };
    if consumer.slots() < frame.len() {
        return false;
    }
    let Ok(chunk) = consumer.read_chunk(frame.len()) else {
        return false;
    };
    let (first, second) = chunk.as_slices();
    frame[..first.len()].copy_from_slice(first);
    frame[first.len()..].copy_from_slice(second);
    chunk.commit_all();
    true
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

fn capture_delay(devices: Option<&Devices>) -> Duration {
    devices
        .and_then(|devices| devices.capture.as_ref())
        .and_then(|(stream, _)| stream.capture_delay())
        .unwrap_or_default()
}

fn mean_square(frame: &[f32]) -> f64 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame
        .iter()
        .map(|sample| {
            let value = f64::from(*sample);
            value * value
        })
        .sum();
    sum / f64_from_usize(frame.len())
}

fn f64_from_usize(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn device_message(error: &device::Error) -> String {
    match std::error::Error::source(error) {
        Some(source) => format!("{error}: {source}"),
        None => error.to_string(),
    }
}
