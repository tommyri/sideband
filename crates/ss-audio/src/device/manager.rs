//! Device inventory, default-device tracking, and stream ownership signals.
//!
//! Hot-plug is a poll. `cpal` does not expose one hook for every host. The
//! poll interval is [`POLL_INTERVAL`]. An open stream also sets a lost flag
//! from the host error callback, and the poll turns that into
//! [`DeviceEvent::Lost`] so the voice graph can open a new stream in-process.

use std::{
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait};
use rtrb::{Consumer, Producer};

use super::{
    Error, GraphConfig,
    stream::{CaptureStream, PlaybackStream},
    watch::{
        Defaults, DeviceEvent, DeviceId, DeviceInfo, Direction, diff_devices, is_low_quality_input,
    },
};

/// How often the inventory thread compares the host's device list.
pub const POLL_INTERVAL: Duration = Duration::from_millis(500);

struct ActiveStream {
    id: DeviceId,
    lost: Arc<AtomicBool>,
    reported: bool,
}

#[derive(Default)]
struct Active {
    capture: Option<ActiveStream>,
    playback: Option<ActiveStream>,
}

/// Enumerates devices and reports hot-plug, default changes, and lost streams.
pub struct DeviceManager {
    events: Receiver<DeviceEvent>,
    tx: SyncSender<DeviceEvent>,
    stop: Arc<AtomicBool>,
    active: Arc<Mutex<Active>>,
    thread: Option<JoinHandle<()>>,
}

impl DeviceManager {
    /// Start polling the default host and any other hosts `cpal` exposes.
    #[must_use]
    pub fn new() -> Self {
        let (tx, events) = mpsc::sync_channel(256);
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(Active::default()));
        let thread_stop = Arc::clone(&stop);
        let thread_active = Arc::clone(&active);
        let thread_tx = tx.clone();
        let thread = thread::Builder::new()
            .name("ss-audio-devices".to_owned())
            .spawn(move || poll_loop(&thread_stop, &thread_active, &thread_tx))
            .ok();
        Self {
            events,
            tx,
            stop,
            active,
            thread,
        }
    }

    /// Devices visible right now.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceInfo> {
        snapshot().0
    }

    /// Current default input, if the host has one.
    #[must_use]
    pub fn default_input(&self) -> Option<DeviceId> {
        snapshot().1.input
    }

    /// Current default output, if the host has one.
    #[must_use]
    pub fn default_output(&self) -> Option<DeviceId> {
        snapshot().1.output
    }

    /// Events queued since the last call.
    #[must_use]
    pub fn poll(&self) -> Vec<DeviceEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    /// Open an input. `id` of `None` uses the host default.
    ///
    /// The consumer receives 48 kHz frames of `config.channels`. After
    /// [`DeviceEvent::Lost`], drop the stream and call this again.
    ///
    /// # Errors
    ///
    /// [`Error::Channels`] when `config.channels` is not 1 or 2.
    /// [`Error::NoDevice`] when the id or the default input is missing.
    /// [`Error::Unsupported`] and [`Error::Backend`] when the device rejects the stream.
    pub fn open_capture(
        &self,
        id: Option<&DeviceId>,
        config: GraphConfig,
    ) -> Result<(CaptureStream, Consumer<f32>), Error> {
        check_channels(config)?;
        let (device, device_id, name) = resolve_input(id)?;
        let (stream, consumer) = CaptureStream::open(&device, config, device_id.clone())?;
        self.note_quality(&name, &stream);
        self.watch(Direction::Capture, device_id, stream.lost_flag());
        Ok((stream, consumer))
    }

    /// Open an output. `id` of `None` uses the host default.
    ///
    /// The producer accepts 48 kHz frames of `config.channels`.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::open_capture`].
    pub fn open_playback(
        &self,
        id: Option<&DeviceId>,
        config: GraphConfig,
    ) -> Result<(PlaybackStream, Producer<f32>), Error> {
        check_channels(config)?;
        let (device, device_id, _) = resolve_output(id)?;
        let (stream, producer) = PlaybackStream::open(&device, config, device_id.clone())?;
        self.watch(Direction::Playback, device_id, stream.lost_flag());
        Ok((stream, producer))
    }

    fn note_quality(&self, name: &str, stream: &CaptureStream) {
        if !is_low_quality_input(name, stream.device_rate()) {
            return;
        }
        send(
            &self.tx,
            DeviceEvent::LowQualityInput {
                id: stream.device_id().clone(),
                sample_rate: stream.device_rate(),
            },
        );
    }

    fn watch(&self, direction: Direction, id: DeviceId, lost: Arc<AtomicBool>) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = ActiveStream {
            id,
            lost,
            reported: false,
        };
        match direction {
            Direction::Capture => active.capture = Some(slot),
            Direction::Playback => active.playback = Some(slot),
        }
    }
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DeviceManager {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn check_channels(config: GraphConfig) -> Result<(), Error> {
    if matches!(config.channels, 1 | 2) {
        Ok(())
    } else {
        Err(Error::Channels)
    }
}

fn resolve_input(id: Option<&DeviceId>) -> Result<(cpal::Device, DeviceId, String), Error> {
    let device = match id {
        Some(id) => find_device(id)?,
        None => cpal::default_host()
            .default_input_device()
            .ok_or(Error::NoDevice)?,
    };
    let (device_id, name) = identity(&device)?;
    Ok((device, device_id, name))
}

fn resolve_output(id: Option<&DeviceId>) -> Result<(cpal::Device, DeviceId, String), Error> {
    let device = match id {
        Some(id) => find_device(id)?,
        None => cpal::default_host()
            .default_output_device()
            .ok_or(Error::NoDevice)?,
    };
    let (device_id, name) = identity(&device)?;
    Ok((device, device_id, name))
}

fn find_device(id: &DeviceId) -> Result<cpal::Device, Error> {
    let parsed = cpal::DeviceId::from_str(id.as_str()).map_err(Error::Backend)?;
    let host = cpal::host_from_id(parsed.host()).map_err(Error::Backend)?;
    host.device_by_id(&parsed).ok_or(Error::NoDevice)
}

fn identity(device: &cpal::Device) -> Result<(DeviceId, String), Error> {
    let id = device.id().map_err(Error::Backend)?;
    let name = device.description().map_or_else(
        |_| device.to_string(),
        |description| description.name().to_owned(),
    );
    Ok((DeviceId::new(id.to_string()), name))
}

fn snapshot() -> (Vec<DeviceInfo>, Defaults) {
    let mut devices = Vec::new();
    for host_id in cpal::available_hosts() {
        let Ok(host) = cpal::host_from_id(host_id) else {
            continue;
        };
        let Ok(list) = host.devices() else {
            continue;
        };
        for device in list {
            let Ok((id, name)) = identity(&device) else {
                continue;
            };
            devices.push(DeviceInfo {
                id,
                name,
                input: device.supports_input(),
                output: device.supports_output(),
            });
        }
    }
    let host = cpal::default_host();
    let input = host
        .default_input_device()
        .and_then(|device| identity(&device).ok())
        .map(|(id, _)| id);
    let output = host
        .default_output_device()
        .and_then(|device| identity(&device).ok())
        .map(|(id, _)| id);
    (devices, Defaults { input, output })
}

fn poll_loop(stop: &AtomicBool, active: &Mutex<Active>, tx: &SyncSender<DeviceEvent>) {
    let (mut previous, mut previous_defaults) = snapshot();
    while !stop.load(Ordering::Acquire) {
        thread::sleep(POLL_INTERVAL);
        if stop.load(Ordering::Acquire) {
            break;
        }
        let (current, defaults) = snapshot();
        let mut guard = active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let capture_id = guard.capture.as_ref().map(|stream| stream.id.clone());
        let playback_id = guard.playback.as_ref().map(|stream| stream.id.clone());
        let mut events = diff_devices(
            &previous,
            &current,
            &previous_defaults,
            &defaults,
            capture_id.as_ref(),
            playback_id.as_ref(),
        );
        note_callback_loss(&mut guard.capture, Direction::Capture, &mut events);
        note_callback_loss(&mut guard.playback, Direction::Playback, &mut events);
        drop(guard);
        if !events.is_empty() {
            tracing::debug!(count = events.len(), "audio device inventory changed");
        }
        for event in events {
            send(tx, event);
        }
        previous = current;
        previous_defaults = defaults;
    }
}

fn note_callback_loss(
    slot: &mut Option<ActiveStream>,
    direction: Direction,
    events: &mut Vec<DeviceEvent>,
) {
    let Some(stream) = slot else {
        return;
    };
    if !stream.lost.load(Ordering::Acquire) || stream.reported {
        return;
    }
    stream.reported = true;
    let id = stream.id.clone();
    let already = events.iter().any(|event| {
        matches!(event, DeviceEvent::Lost { direction: lost_direction, id: lost_id }
            if *lost_direction == direction && lost_id == &id)
    });
    if !already {
        events.push(DeviceEvent::Lost { direction, id });
    }
}

fn send(tx: &SyncSender<DeviceEvent>, event: DeviceEvent) {
    match tx.try_send(event) {
        Ok(()) | Err(TrySendError::Disconnected(_)) => {}
        Err(TrySendError::Full(_)) => {
            tracing::debug!("dropping an audio device event because the queue is full");
        }
    }
}
