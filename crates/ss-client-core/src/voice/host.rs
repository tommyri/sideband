//! The worker's view of the device layer: `ss_audio`'s [`DeviceManager`] in
//! builds, and a scripted mock in tests.

use std::{sync::Arc, time::Duration};

use rtrb::{Consumer, Producer};
use ss_audio::device::{
    self, CaptureStream, DeviceEvent, DeviceId, DeviceManager, GraphConfig, PlaybackStream,
};

/// An open capture stream and the ring it fills.
pub(super) type Input = (Box<dyn InputStream>, Consumer<f32>);
/// An open playback stream and the ring it drains.
pub(super) type Output = (Box<dyn OutputStream>, Producer<f32>);
/// Apple's full-duplex unit with its capture and playback rings.
pub(super) type Duplex = (Box<dyn DuplexStream>, Consumer<f32>, Producer<f32>);
/// Builds a fresh host. A settings change or backend switch drops the old one
/// on the setup thread and starts another.
pub(super) type Hosts = Arc<dyn Fn() -> Box<dyn Host> + Send + Sync>;

/// The real device manager.
pub(super) fn system() -> Hosts {
    Arc::new(|| Box::new(DeviceManager::new()))
}

/// Settings for Apple's VoiceProcessingIO unit.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(
    not(all(target_os = "macos", feature = "macos-voiceio")),
    allow(dead_code, reason = "read only by the macOS unit")
)]
pub(super) struct DuplexConfig {
    pub bypass: bool,
    pub automatic_gain_control: bool,
    pub muted: bool,
}

/// Enumerates devices, reports device events, and opens streams.
pub(super) trait Host: Send {
    /// Events queued since the last call.
    fn poll(&self) -> Vec<DeviceEvent>;
    /// Open an input; `None` is the host default.
    fn open_capture(
        &self,
        id: Option<&DeviceId>,
        config: GraphConfig,
    ) -> Result<Input, device::Error>;
    /// Open an output; `None` is the host default.
    fn open_playback(
        &self,
        id: Option<&DeviceId>,
        config: GraphConfig,
    ) -> Result<Output, device::Error>;
    /// This build can open Apple's VoiceProcessingIO unit.
    fn duplex_supported(&self) -> bool;
    /// Open Apple's unit on the system defaults.
    fn open_duplex(
        &self,
        capture: GraphConfig,
        config: DuplexConfig,
    ) -> Result<Duplex, device::Error>;
}

/// What the worker reads from an open input.
pub(super) trait InputStream: Send {
    fn device_id(&self) -> &DeviceId;
    fn device_rate(&self) -> u32;
    fn delay(&self) -> Option<Duration>;
    fn overruns(&self) -> u64;
}

/// What the worker reads from an open output.
pub(super) trait OutputStream: Send {
    fn device_id(&self) -> &DeviceId;
    fn device_rate(&self) -> u32;
    fn delay(&self) -> Option<Duration>;
    fn underruns(&self) -> u64;
    /// Audio queued beyond the producer's ring: a resampler.
    fn downstream_queue(&self) -> Duration;
}

/// Apple's unit. It holds an Objective-C block and is not `Send`.
pub(super) trait DuplexStream {
    fn set_muted(&mut self, muted: bool) -> Result<(), String>;
    /// `Some(true)` when muted speech started, `Some(false)` when it ended.
    fn take_muted_speech(&self) -> Option<bool>;
    /// Playback underruns and capture overruns.
    fn counters(&self) -> (u64, u64);
    /// The unit reports that it is no longer running.
    fn stopped(&self) -> bool;
}

impl Host for DeviceManager {
    fn poll(&self) -> Vec<DeviceEvent> {
        Self::poll(self)
    }

    fn open_capture(
        &self,
        id: Option<&DeviceId>,
        config: GraphConfig,
    ) -> Result<Input, device::Error> {
        let (stream, consumer) = Self::open_capture(self, id, config)?;
        Ok((Box::new(stream), consumer))
    }

    fn open_playback(
        &self,
        id: Option<&DeviceId>,
        config: GraphConfig,
    ) -> Result<Output, device::Error> {
        let (stream, producer) = Self::open_playback(self, id, config)?;
        Ok((Box::new(stream), producer))
    }

    fn duplex_supported(&self) -> bool {
        cfg!(all(target_os = "macos", feature = "macos-voiceio"))
    }

    #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
    fn open_duplex(
        &self,
        capture: GraphConfig,
        config: DuplexConfig,
    ) -> Result<Duplex, device::Error> {
        let (stream, consumer, producer) = self.open_voice_processing(
            capture,
            GraphConfig::STEREO,
            device::VoiceProcessingConfig {
                bypass: config.bypass,
                automatic_gain_control: config.automatic_gain_control,
                muted: config.muted,
            },
        )?;
        Ok((Box::new(stream), consumer, producer))
    }

    #[cfg(not(all(target_os = "macos", feature = "macos-voiceio")))]
    fn open_duplex(
        &self,
        capture: GraphConfig,
        config: DuplexConfig,
    ) -> Result<Duplex, device::Error> {
        let _ = (capture, config);
        Err(device::Error::Unsupported)
    }
}

impl InputStream for CaptureStream {
    fn device_id(&self) -> &DeviceId {
        Self::device_id(self)
    }

    fn device_rate(&self) -> u32 {
        Self::device_rate(self)
    }

    fn delay(&self) -> Option<Duration> {
        self.capture_delay()
    }

    fn overruns(&self) -> u64 {
        self.counters().overruns()
    }
}

impl OutputStream for PlaybackStream {
    fn device_id(&self) -> &DeviceId {
        Self::device_id(self)
    }

    fn device_rate(&self) -> u32 {
        Self::device_rate(self)
    }

    fn delay(&self) -> Option<Duration> {
        self.playback_delay()
    }

    fn underruns(&self) -> u64 {
        self.counters().underruns()
    }

    fn downstream_queue(&self) -> Duration {
        Self::downstream_queue(self)
    }
}

#[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
impl DuplexStream for device::VoiceProcessingStream {
    fn set_muted(&mut self, muted: bool) -> Result<(), String> {
        Self::set_muted(self, muted).map_err(|error| error.to_string())
    }

    fn take_muted_speech(&self) -> Option<bool> {
        self.take_muted_speech_activity()
            .map(|activity| activity == device::MutedSpeechActivity::Started)
    }

    fn counters(&self) -> (u64, u64) {
        (
            self.playback_counters().underruns(),
            self.capture_counters().overruns(),
        )
    }

    fn stopped(&self) -> bool {
        self.is_running().is_ok_and(|running| !running)
    }
}

/// A scripted device layer: devices come and go, and every open is logged.
#[cfg(test)]
pub(super) mod mock {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use parking_lot::Mutex;
    use rtrb::{Consumer, Producer, RingBuffer};
    use ss_audio::device::{
        self, DeviceEvent, DeviceId, DeviceInfo, Direction, GraphConfig, SAMPLES_PER_CHANNEL,
    };

    use super::{
        Duplex, DuplexConfig, DuplexStream, Host, Hosts, Input, InputStream, Output, OutputStream,
    };

    /// 250 ms of stereo, like the real rings.
    const RING: usize = SAMPLES_PER_CHANNEL * 2 * 25;

    /// One open attempt.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Open {
        pub direction: Option<Direction>,
        pub asked: Option<DeviceId>,
        pub opened: Option<DeviceId>,
    }

    #[derive(Default)]
    pub struct World {
        pub inputs: Vec<DeviceId>,
        pub outputs: Vec<DeviceId>,
        pub default_input: Option<DeviceId>,
        pub default_output: Option<DeviceId>,
        pub events: Vec<DeviceEvent>,
        pub opens: Vec<Open>,
        /// Streams opened last, as the real manager watches them.
        pub capture: Option<DeviceId>,
        pub playback: Option<DeviceId>,
        /// The device side of the open playback ring; tests drain it.
        pub device_output: Option<Consumer<f32>>,
        /// The device side of the open capture ring.
        pub device_input: Option<Producer<f32>>,
        /// What the last capture open asked for.
        pub capture_config: Option<GraphConfig>,
        /// The open duplex unit's stop flag.
        pub duplex: Option<Arc<AtomicBool>>,
    }

    impl World {
        fn list(&mut self, direction: Direction) -> &mut Vec<DeviceId> {
            match direction {
                Direction::Capture => &mut self.inputs,
                Direction::Playback => &mut self.outputs,
            }
        }

        fn default_mut(&mut self, direction: Direction) -> &mut Option<DeviceId> {
            match direction {
                Direction::Capture => &mut self.default_input,
                Direction::Playback => &mut self.default_output,
            }
        }

        /// Remove a device the way the real poll reports it: `Lost` for an
        /// open stream on it, `Removed`, and a new default if it was one.
        pub fn unplug(&mut self, direction: Direction, id: &str) {
            let id = DeviceId::new(id);
            self.list(direction).retain(|device| *device != id);
            let open = match direction {
                Direction::Capture => self.capture.as_ref(),
                Direction::Playback => self.playback.as_ref(),
            };
            if open == Some(&id) {
                self.events.push(DeviceEvent::Lost {
                    direction,
                    id: id.clone(),
                });
            }
            self.events.push(DeviceEvent::Removed(id.clone()));
            if self.default_mut(direction).as_ref() == Some(&id) {
                let next = self.list(direction).first().cloned();
                *self.default_mut(direction) = next.clone();
                self.events.push(DeviceEvent::DefaultChanged {
                    direction,
                    id: next,
                });
            }
            if let Some(stopped) = &self.duplex {
                stopped.store(true, Ordering::Release);
            }
        }

        /// Add a device back. `announce` queues `Added`; without it the
        /// worker only finds it by retrying.
        pub fn plug(&mut self, direction: Direction, id: &str, default: bool, announce: bool) {
            let id = DeviceId::new(id);
            self.list(direction).push(id.clone());
            if announce {
                self.events.push(DeviceEvent::Added(DeviceInfo {
                    id: id.clone(),
                    name: id.to_string(),
                    input: direction == Direction::Capture,
                    output: direction == Direction::Playback,
                    input_channels: None,
                }));
            }
            if default {
                *self.default_mut(direction) = Some(id.clone());
                if announce {
                    self.events.push(DeviceEvent::DefaultChanged {
                        direction,
                        id: Some(id),
                    });
                }
            }
        }

        /// Opens that asked for `asked` (`None`: the default), successful
        /// or not.
        pub fn attempts(&self, direction: Option<Direction>, asked: Option<&DeviceId>) -> usize {
            self.opens
                .iter()
                .filter(|open| open.direction == direction && open.asked.as_ref() == asked)
                .count()
        }

        /// Play out what the worker queued, as a device would.
        pub fn drain_output(&mut self) {
            if let Some(consumer) = &mut self.device_output
                && let Ok(chunk) = consumer.read_chunk(consumer.slots())
            {
                chunk.commit_all();
            }
        }

        fn resolve(&self, direction: Direction, id: Option<&DeviceId>) -> Option<DeviceId> {
            let (list, default) = match direction {
                Direction::Capture => (&self.inputs, &self.default_input),
                Direction::Playback => (&self.outputs, &self.default_output),
            };
            let id = id.or(default.as_ref())?;
            list.contains(id).then(|| id.clone())
        }

        fn open(&mut self, direction: Direction, asked: Option<&DeviceId>) -> Option<DeviceId> {
            let opened = self.resolve(direction, asked);
            self.opens.push(Open {
                direction: Some(direction),
                asked: asked.cloned(),
                opened: opened.clone(),
            });
            if let Some(id) = &opened {
                match direction {
                    Direction::Capture => self.capture = Some(id.clone()),
                    Direction::Playback => self.playback = Some(id.clone()),
                }
            }
            opened
        }
    }

    pub type Shared = Arc<Mutex<World>>;

    /// A world with `inputs` and `outputs`, the first of each the default.
    pub fn world(inputs: &[&str], outputs: &[&str]) -> Shared {
        let inputs: Vec<_> = inputs.iter().map(|id| DeviceId::new(*id)).collect();
        let outputs: Vec<_> = outputs.iter().map(|id| DeviceId::new(*id)).collect();
        Arc::new(Mutex::new(World {
            default_input: inputs.first().cloned(),
            default_output: outputs.first().cloned(),
            inputs,
            outputs,
            ..World::default()
        }))
    }

    pub fn hosts(world: &Shared) -> Hosts {
        let world = Arc::clone(world);
        Arc::new(move || {
            Box::new(MockHost {
                world: Arc::clone(&world),
            })
        })
    }

    struct MockHost {
        world: Shared,
    }

    struct Stream {
        id: DeviceId,
    }

    impl InputStream for Stream {
        fn device_id(&self) -> &DeviceId {
            &self.id
        }

        fn device_rate(&self) -> u32 {
            48_000
        }

        fn delay(&self) -> Option<std::time::Duration> {
            None
        }

        fn overruns(&self) -> u64 {
            0
        }
    }

    impl OutputStream for Stream {
        fn device_id(&self) -> &DeviceId {
            &self.id
        }

        fn device_rate(&self) -> u32 {
            48_000
        }

        fn delay(&self) -> Option<std::time::Duration> {
            None
        }

        fn underruns(&self) -> u64 {
            0
        }

        fn downstream_queue(&self) -> std::time::Duration {
            std::time::Duration::ZERO
        }
    }

    struct Unit {
        stopped: Arc<AtomicBool>,
    }

    impl DuplexStream for Unit {
        fn set_muted(&mut self, _muted: bool) -> Result<(), String> {
            Ok(())
        }

        fn take_muted_speech(&self) -> Option<bool> {
            None
        }

        fn counters(&self) -> (u64, u64) {
            (0, 0)
        }

        fn stopped(&self) -> bool {
            self.stopped.load(Ordering::Acquire)
        }
    }

    impl Host for MockHost {
        fn poll(&self) -> Vec<DeviceEvent> {
            std::mem::take(&mut self.world.lock().events)
        }

        fn open_capture(
            &self,
            id: Option<&DeviceId>,
            config: GraphConfig,
        ) -> Result<Input, device::Error> {
            let mut world = self.world.lock();
            let id = world
                .open(Direction::Capture, id)
                .ok_or(device::Error::NoDevice)?;
            let (producer, consumer) = RingBuffer::new(config.samples_per_frame() * 25);
            world.device_input = Some(producer);
            world.capture_config = Some(config);
            Ok((Box::new(Stream { id }), consumer))
        }

        fn open_playback(
            &self,
            id: Option<&DeviceId>,
            _config: GraphConfig,
        ) -> Result<Output, device::Error> {
            let mut world = self.world.lock();
            let id = world
                .open(Direction::Playback, id)
                .ok_or(device::Error::NoDevice)?;
            let (producer, consumer) = RingBuffer::new(RING);
            world.device_output = Some(consumer);
            Ok((Box::new(Stream { id }), producer))
        }

        fn duplex_supported(&self) -> bool {
            true
        }

        /// The unit needs a default input and a default output.
        fn open_duplex(
            &self,
            capture: GraphConfig,
            _config: DuplexConfig,
        ) -> Result<Duplex, device::Error> {
            let mut world = self.world.lock();
            let available = world.resolve(Direction::Capture, None).is_some()
                && world.resolve(Direction::Playback, None).is_some();
            let opened = available.then(|| DeviceId::new("voice-processing"));
            world.opens.push(Open {
                direction: None,
                asked: None,
                opened: opened.clone(),
            });
            opened.ok_or(device::Error::NoDevice)?;
            let stopped = Arc::new(AtomicBool::new(false));
            world.duplex = Some(Arc::clone(&stopped));
            let (_, consumer) = RingBuffer::new(capture.samples_per_frame() * 25);
            let (producer, output) = RingBuffer::new(RING);
            world.device_output = Some(output);
            Ok((Box::new(Unit { stopped }), consumer, producer))
        }
    }
}
