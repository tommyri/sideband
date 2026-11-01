//! Optional capture processing: echo cancellation, noise suppression, and
//! automatic gain control (decision D6).
//!
//! Every stage is off unless configured. A [`Chain`] with no stages returns
//! from [`Chain::process`] without reading or writing a sample, so the frame
//! that reaches the gate and encoder is the frame the capture ring produced.
//!
//! When stages are enabled the order is fixed: echo cancellation, then noise
//! suppression, then gain control. The chain runs on the capture worker,
//! the thread that drains the capture ring, and never inside a `cpal`
//! callback. It processes one 10 ms, 48 kHz interleaved frame per call and
//! is budgeted at [`CHAIN_BUDGET`] per frame and [`STAGE_BUDGET`] per stage.
//! [`Monitor`] publishes each stage's added latency and measured CPU time
//! through atomics, so the settings UI can read them from another thread.
//!
//! The echo canceller needs the far-end signal. The playback worker pushes
//! each mixer frame into a [`RenderSender`] before the playback ring; the
//! capture worker drains the matching [`RenderReceiver`] and passes those
//! frames to [`Chain::process`]. [`EchoPath`] turns the T-010 device delays
//! into the delay hint.
//!
//! Backends: `sonora` (feature `aec`) supplies AEC3, its noise suppressor,
//! and AGC2; `nnnoiseless` (feature `rnnoise`) supplies RNNoise. The
//! `deepfilter` feature is reserved: see [`DEEPFILTER_UNAVAILABLE`].

mod meter;
mod render;

#[cfg(feature = "aec")]
mod apm;
#[cfg(feature = "rnnoise")]
mod rnnoise;
#[cfg(test)]
mod tests;

use std::{fmt, time::Duration};

pub use meter::{CpuStats, Monitor, StageReport};
pub use render::{MAX_RENDER_BATCH, RenderReceiver, RenderSender, render_tap};

use crate::mixer::{SAMPLES_PER_CHANNEL, StereoFrame};
use meter::Meter;

/// Per-frame CPU budget for the whole chain on a 10 ms frame.
pub const CHAIN_BUDGET: Duration = Duration::from_millis(3);
/// Per-frame CPU budget for one stage on a 10 ms frame.
pub const STAGE_BUDGET: Duration = Duration::from_micros(1500);

/// Why DeepFilterNet3 is not built, even with the `deepfilter` feature.
pub const DEEPFILTER_UNAVAILABLE: &str = "the crates.io deep_filter release has no inference; \
     the git-only build is outside the dependency policy and the low-latency model exceeds \
     the 10 MB budget";

/// One position in the fixed chain order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// Removes the far-end signal that the microphone hears from the speakers.
    EchoCancellation,
    /// Attenuates stationary and non-speech noise.
    NoiseSuppression,
    /// Brings a wandering microphone level toward a steady speech level.
    GainControl,
}

impl fmt::Display for Stage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::EchoCancellation => "echo cancellation",
            Self::NoiseSuppression => "noise suppression",
            Self::GainControl => "gain control",
        })
    }
}

/// The library that implements a stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// `sonora`, the pure-Rust WebRTC audio processing port (feature `aec`).
    Sonora,
    /// RNNoise through `nnnoiseless` (feature `rnnoise`).
    RnNoise,
    /// DeepFilterNet3 (feature `deepfilter`, currently never available).
    DeepFilter,
    /// A [`Processor`] supplied through [`Chain::with_processors`].
    Custom,
}

impl Backend {
    /// The Cargo feature that compiles this backend, if any.
    #[must_use]
    pub const fn feature(self) -> Option<&'static str> {
        match self {
            Self::Sonora => Some("aec"),
            Self::RnNoise => Some("rnnoise"),
            Self::DeepFilter => Some("deepfilter"),
            Self::Custom => None,
        }
    }

    /// Whether [`Chain::new`] can build this backend in the current binary.
    #[must_use]
    pub const fn is_available(self) -> bool {
        match self {
            Self::Sonora => cfg!(feature = "aec"),
            Self::RnNoise => cfg!(feature = "rnnoise"),
            Self::DeepFilter => false,
            Self::Custom => true,
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Sonora => "sonora",
            Self::RnNoise => "RNNoise",
            Self::DeepFilter => "DeepFilterNet3",
            Self::Custom => "custom",
        })
    }
}

/// Aggressiveness of the `sonora` (WebRTC) noise suppressor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum SuppressionLevel {
    /// About 6 dB.
    Low,
    /// About 12 dB.
    #[default]
    Moderate,
    /// About 18 dB.
    High,
    /// About 21 dB.
    VeryHigh,
}

/// Which noise suppressor, if any, runs in the chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum NoiseSuppression {
    /// No noise suppression.
    #[default]
    Off,
    /// The WebRTC suppressor from `sonora` (feature `aec`). Adds about 7 ms.
    Sonora(SuppressionLevel),
    /// RNNoise (feature `rnnoise`). Adds one 10 ms frame of latency.
    RnNoise,
    /// DeepFilterNet3. Rejected with [`Error::Unavailable`] in every build.
    DeepFilter,
}

impl NoiseSuppression {
    /// The backend behind this choice, or `None` when off.
    #[must_use]
    pub const fn backend(self) -> Option<Backend> {
        match self {
            Self::Off => None,
            Self::Sonora(_) => Some(Backend::Sonora),
            Self::RnNoise => Some(Backend::RnNoise),
            Self::DeepFilter => Some(Backend::DeepFilter),
        }
    }
}

/// The user's processing settings. [`Default`] is everything off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct ProcessingConfig {
    /// `sonora` AEC3 with its enforced high-pass filter (feature `aec`).
    pub echo_cancellation: bool,
    /// The noise suppressor.
    pub noise_suppression: NoiseSuppression,
    /// `sonora` AGC2 adaptive digital gain and limiter (feature `aec`).
    pub gain_control: bool,
}

impl ProcessingConfig {
    /// Everything off: the capture path is untouched.
    pub const OFF: Self = Self {
        echo_cancellation: false,
        noise_suppression: NoiseSuppression::Off,
        gain_control: false,
    };

    /// True when no stage is enabled.
    #[must_use]
    pub fn is_off(&self) -> bool {
        *self == Self::OFF
    }
}

/// Invalid settings or frames. A rejected frame is left unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The graph channel count must be mono or stereo.
    #[error("channel count must be 1 or 2")]
    Channels,
    /// A capture frame was not one 10 ms interleaved frame.
    #[error("expected {expected} interleaved samples, got {actual}")]
    FrameLength {
        /// `480 * channels`.
        expected: usize,
        /// The length passed in.
        actual: usize,
    },
    /// Capture or render PCM contains NaN or infinity.
    #[error("non-finite PCM")]
    Pcm,
    /// Custom processors were not in echo, noise, gain order, or repeated a stage.
    #[error("stages must be unique and ordered echo, noise, gain")]
    Order,
    /// The backend was not compiled into this binary.
    #[error("{backend} needs the `{feature}` build feature")]
    FeatureDisabled {
        /// The requested backend.
        backend: Backend,
        /// The Cargo feature that would compile it.
        feature: &'static str,
    },
    /// The backend cannot be built at all; `reason` says why.
    #[error("{backend} is unavailable: {reason}")]
    Unavailable {
        /// The requested backend.
        backend: Backend,
        /// Short explanation for logs and the settings UI.
        reason: &'static str,
    },
}

/// One processing stage.
///
/// [`Chain`] validates every frame before calling [`Processor::process`]:
/// `capture` holds exactly `480 * channels` finite interleaved samples at
/// 48 kHz. Implementations run on the capture worker. They should not
/// allocate per frame, and must not block.
pub trait Processor: Send {
    /// Where the stage sits in the chain.
    fn stage(&self) -> Stage;

    /// The library behind the stage, for the settings UI.
    fn backend(&self) -> Backend;

    /// Algorithmic delay the stage adds to the capture path.
    fn latency(&self) -> Duration;

    /// Process one 10 ms capture frame in place.
    ///
    /// `render` holds the far-end mixer frames that arrived since the
    /// previous call, oldest first. It is usually one frame, sometimes zero
    /// or two because the playback and capture clocks differ. Stages other
    /// than echo cancellation ignore it.
    fn process(&mut self, capture: &mut [f32], render: &[StereoFrame]);

    /// Hint the loudspeaker-to-microphone delay. Only echo cancellation uses it.
    fn set_echo_delay(&mut self, delay: Duration) {
        let _ = delay;
    }

    /// Forget adaptive state, for example after a device change.
    fn reset(&mut self);
}

/// Measured pieces of the path from the render tap to the capture worker.
///
/// The sum is the delay `sonora` expects: the time from handing a far-end
/// frame to the canceller until the speaker plays it, plus the time from
/// the microphone hearing it until the capture worker processes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EchoPath {
    /// Audio queued in the playback ring after the tap.
    pub playback_queue: Duration,
    /// [`crate::device::PlaybackStream::playback_delay`].
    pub playback_device: Duration,
    /// [`crate::device::CaptureStream::capture_delay`].
    pub capture_device: Duration,
    /// Audio waiting in the capture ring when this frame is processed.
    pub capture_queue: Duration,
}

impl EchoPath {
    /// Total delay hint for [`Chain::set_echo_delay`].
    #[must_use]
    pub fn delay(&self) -> Duration {
        self.playback_queue + self.playback_device + self.capture_device + self.capture_queue
    }
}

/// The configured stages in order, plus their meters.
///
/// Construct and run it on the capture worker: some backends warm
/// thread-local FFT plans on first use. Construction allocates; processing
/// reuses buffers allocated here. Rebuild the chain to change settings.
pub struct Chain {
    stages: Vec<Box<dyn Processor>>,
    channels: usize,
    uses_render: bool,
    config: Option<ProcessingConfig>,
    meter: Meter,
}

impl fmt::Debug for Chain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Chain")
            .field("channels", &self.channels)
            .field("config", &self.config)
            .field("stages", &self.stages.len())
            .finish_non_exhaustive()
    }
}

impl Chain {
    /// A chain with every stage off.
    ///
    /// # Errors
    ///
    /// [`Error::Channels`] unless `channels` is 1 or 2.
    pub fn bypass(channels: u16) -> Result<Self, Error> {
        Self::new(ProcessingConfig::OFF, channels)
    }

    /// Build the stages `config` enables for `channels` (1 or 2).
    ///
    /// # Errors
    ///
    /// [`Error::Channels`] for another channel count,
    /// [`Error::FeatureDisabled`] when a backend is not compiled in, and
    /// [`Error::Unavailable`] for DeepFilterNet3.
    pub fn new(config: ProcessingConfig, channels: u16) -> Result<Self, Error> {
        let channels = validate_channels(channels)?;
        let mut stages: Vec<Box<dyn Processor>> = Vec::with_capacity(3);
        if config.echo_cancellation {
            stages.push(sonora_stage(Stage::EchoCancellation, None, channels)?);
        }
        match config.noise_suppression {
            NoiseSuppression::Off => {}
            NoiseSuppression::Sonora(level) => {
                stages.push(sonora_stage(
                    Stage::NoiseSuppression,
                    Some(level),
                    channels,
                )?);
            }
            NoiseSuppression::RnNoise => stages.push(rnnoise_stage(channels)?),
            NoiseSuppression::DeepFilter => {
                return Err(if cfg!(feature = "deepfilter") {
                    Error::Unavailable {
                        backend: Backend::DeepFilter,
                        reason: DEEPFILTER_UNAVAILABLE,
                    }
                } else {
                    Error::FeatureDisabled {
                        backend: Backend::DeepFilter,
                        feature: "deepfilter",
                    }
                });
            }
        }
        if config.gain_control {
            stages.push(sonora_stage(Stage::GainControl, None, channels)?);
        }
        Ok(Self::assemble(stages, channels, Some(config)))
    }

    /// Build a chain from caller-supplied stages, for tests or a platform
    /// backend. Stages must be unique and in echo, noise, gain order.
    ///
    /// # Errors
    ///
    /// [`Error::Channels`] or [`Error::Order`].
    pub fn with_processors(channels: u16, stages: Vec<Box<dyn Processor>>) -> Result<Self, Error> {
        let channels = validate_channels(channels)?;
        if stages
            .windows(2)
            .any(|pair| pair[0].stage() >= pair[1].stage())
        {
            return Err(Error::Order);
        }
        Ok(Self::assemble(stages, channels, None))
    }

    fn assemble(
        stages: Vec<Box<dyn Processor>>,
        channels: usize,
        config: Option<ProcessingConfig>,
    ) -> Self {
        let meter = Meter::new(
            stages
                .iter()
                .map(|stage| (stage.stage(), stage.backend(), stage.latency())),
        );
        let uses_render = stages
            .iter()
            .any(|stage| stage.stage() == Stage::EchoCancellation);
        Self {
            stages,
            channels,
            uses_render,
            config,
            meter,
        }
    }

    /// The settings this chain was built from; `None` for
    /// [`Chain::with_processors`].
    #[must_use]
    pub const fn config(&self) -> Option<ProcessingConfig> {
        self.config
    }

    /// Interleaved channel count of every frame.
    #[must_use]
    pub const fn channels(&self) -> usize {
        self.channels
    }

    /// True when no stage runs and frames pass through untouched.
    #[must_use]
    pub fn is_bypass(&self) -> bool {
        self.stages.is_empty()
    }

    /// True when the chain consumes far-end frames, so the session should
    /// create a [`render_tap`].
    #[must_use]
    pub const fn uses_render(&self) -> bool {
        self.uses_render
    }

    /// Sum of the stages' added latency.
    #[must_use]
    pub fn latency(&self) -> Duration {
        self.stages.iter().map(|stage| stage.latency()).sum()
    }

    /// A handle for reading per-stage latency and CPU from any thread.
    #[must_use]
    pub fn monitor(&self) -> Monitor {
        self.meter.monitor()
    }

    /// Process one 10 ms capture frame in place.
    ///
    /// With no stages the call checks the length and returns: no sample is
    /// read, copied, or written. `render` holds the far-end frames drained
    /// from the [`RenderReceiver`] since the previous call.
    ///
    /// # Errors
    ///
    /// [`Error::FrameLength`] when `capture` is not `480 * channels` long,
    /// and [`Error::Pcm`] when an enabled chain sees non-finite capture or
    /// render samples. Both leave `capture` unchanged.
    pub fn process(&mut self, capture: &mut [f32], render: &[StereoFrame]) -> Result<(), Error> {
        let expected = SAMPLES_PER_CHANNEL * self.channels;
        if capture.len() != expected {
            return Err(Error::FrameLength {
                expected,
                actual: capture.len(),
            });
        }
        if self.stages.is_empty() {
            return Ok(());
        }
        if !capture.iter().all(|sample| sample.is_finite())
            || (self.uses_render
                && !render
                    .iter()
                    .all(|frame| frame.iter().all(|sample| sample.is_finite())))
        {
            return Err(Error::Pcm);
        }
        let mut timer = self.meter.start();
        for (index, stage) in self.stages.iter_mut().enumerate() {
            stage.process(capture, render);
            timer.lap(index);
        }
        timer.finish();
        Ok(())
    }

    /// Pass the loudspeaker-to-microphone delay to the echo canceller.
    /// Call it when [`EchoPath`] changes by more than a few milliseconds.
    pub fn set_echo_delay(&mut self, delay: Duration) {
        for stage in &mut self.stages {
            stage.set_echo_delay(delay);
        }
    }

    /// Reset every stage's adaptive state. Meters keep counting.
    pub fn reset(&mut self) {
        for stage in &mut self.stages {
            stage.reset();
        }
    }
}

fn validate_channels(channels: u16) -> Result<usize, Error> {
    match channels {
        1 | 2 => Ok(usize::from(channels)),
        _ => Err(Error::Channels),
    }
}

#[cfg(feature = "aec")]
#[allow(clippy::unnecessary_wraps)] // Matches the feature-off signature.
fn sonora_stage(
    stage: Stage,
    level: Option<SuppressionLevel>,
    channels: usize,
) -> Result<Box<dyn Processor>, Error> {
    Ok(Box::new(apm::Apm::new(stage, level, channels)))
}

#[cfg(not(feature = "aec"))]
fn sonora_stage(
    _stage: Stage,
    _level: Option<SuppressionLevel>,
    _channels: usize,
) -> Result<Box<dyn Processor>, Error> {
    Err(Error::FeatureDisabled {
        backend: Backend::Sonora,
        feature: "aec",
    })
}

#[cfg(feature = "rnnoise")]
#[allow(clippy::unnecessary_wraps)] // Matches the feature-off signature.
fn rnnoise_stage(channels: usize) -> Result<Box<dyn Processor>, Error> {
    Ok(Box::new(rnnoise::RnNoise::new(channels)))
}

#[cfg(not(feature = "rnnoise"))]
fn rnnoise_stage(_channels: usize) -> Result<Box<dyn Processor>, Error> {
    Err(Error::FeatureDisabled {
        backend: Backend::RnNoise,
        feature: "rnnoise",
    })
}
