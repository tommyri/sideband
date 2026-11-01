//! Voice activation on 10 ms, 48 kHz capture frames. Runs on the capture
//! worker, not the device callback. Audio is only gated, never denoised.

use std::time::Duration;

/// Samples per channel in a detector quantum (10 ms at 48 kHz).
pub const FRAME_SAMPLES: usize = 480;
/// Number of previous, unsent quanta released at an opening edge.
pub const PRE_ROLL_FRAMES: usize = 2;
/// Time spent listening to background noise during calibration.
pub const CALIBRATION_DURATION: Duration = Duration::from_secs(2);
const CALIBRATION_FRAMES: usize = 200;
const MAX_SAMPLES: usize = FRAME_SAMPLES * 2;

/// Detection mode. Energy is always available; RNNoise is opt-in at build and
/// runtime. Enabling the feature does not turn noise suppression on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Open on RMS energy exceeding the configured threshold.
    #[default]
    Energy,
    /// Require both RMS energy and RNNoise speech probability to exceed their
    /// thresholds. Uses the bundled model; no network/model downloads at runtime.
    #[cfg(feature = "rnnoise")]
    Rnnoise,
}

/// Detector settings. Construction validates every value before allocating.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Interleaved input channels, 1 or 2. Stereo uses the louder channel's
    /// energy, so anti-phase audio cannot cancel the activity detector.
    pub channels: u8,
    /// RMS threshold in dBFS, in -96..=0. Full-scale DC is 0 dBFS.
    pub threshold_dbfs: f32,
    /// Quiet tail retained after speech, in 0..=5 s, rounded up to 10 ms.
    pub hangover: Duration,
    /// Energy or optional neural classification.
    pub mode: Mode,
    /// RNNoise speech threshold in 0..=1; ignored in Energy mode.
    pub probability_threshold: f32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            channels: 1,
            threshold_dbfs: -40.0,
            hangover: Duration::from_millis(300),
            mode: Mode::Energy,
            probability_threshold: 0.6,
        }
    }
}
impl Config {
    fn validate(self) -> Result<Self, Error> {
        if !matches!(self.channels, 1 | 2) {
            return Err(Error::Channels);
        }
        threshold(self.threshold_dbfs)?;
        if self.hangover > Duration::from_secs(5) {
            return Err(Error::Hangover);
        }
        if !self.probability_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.probability_threshold)
        {
            return Err(Error::Probability);
        }
        Ok(self)
    }
}

/// Invalid configuration or PCM; rejected frames do not advance detector time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Only mono and stereo are supported.
    #[error("VAD requires one or two channels")]
    Channels,
    /// Threshold must be finite and in -96..=0 dBFS.
    #[error("VAD threshold must be in -96..=0 dBFS")]
    Threshold,
    /// Hangover is bounded to five seconds.
    #[error("VAD hangover must be at most five seconds")]
    Hangover,
    /// Probability threshold must be finite and in 0..=1.
    #[error("VAD probability threshold must be in 0..=1")]
    Probability,
    /// One exact 10 ms interleaved quantum is required.
    #[error("VAD requires exactly 480 samples per channel")]
    FrameLength,
    /// PCM must be finite normalized f32 in -1..=1.
    #[error("VAD input must be finite normalized PCM")]
    Samples,
}

/// A transition to pass to the voice session's talk gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateEvent {
    /// Start a talk burst; release the output's pre-roll before the current frame.
    Open,
    /// End the burst after any audio included in this output. With zero
    /// hangover this output is empty, so flush the previously buffered encoder.
    Close,
}

/// UI meter, produced once per two quanta (50 Hz), without reading a clock.
#[derive(Debug, Clone, Copy)]
pub struct Meter {
    /// RMS over the last 20 ms, using the louder channel of each 10 ms frame.
    pub rms_dbfs: f32,
    /// Maximum absolute sample over the last 20 ms, in dBFS.
    pub peak_dbfs: f32,
    /// Latest speech probability, or None in energy-only mode.
    pub probability: Option<f32>,
    /// Current trigger threshold (including completed auto-calibration).
    pub threshold_dbfs: f32,
}

/// Completed calibration. Median frame energy rejects short keyboard impulses;
/// the threshold is 12 dB above that floor, clamped to the supported range.
#[derive(Debug, Clone, Copy)]
pub struct Calibration {
    /// Median RMS level over the two-second capture window.
    pub noise_floor_dbfs: f32,
    /// Applied threshold, in -96..=0 dBFS.
    pub threshold_dbfs: f32,
}

/// Result of processing a frame. Borrows preallocated buffers until the next
/// mutable call. Feed `audio()` in order; do not feed the input a second time.
pub struct Output<'a> {
    /// Transition, if any. Open and Close appear once per edge.
    pub event: Option<GateEvent>,
    /// Whether the gate remains open after this quantum.
    pub open: bool,
    /// Present every second successfully processed frame.
    pub meter: Option<Meter>,
    /// Present exactly when a requested calibration completes.
    pub calibration: Option<Calibration>,
    frames: &'a [[f32; MAX_SAMPLES]],
    samples: usize,
}
impl Output<'_> {
    /// Zero to three unmodified capture frames, oldest first. Normally one
    /// while open; on Open, up to two unsent preceding frames plus the trigger.
    pub fn audio(&self) -> impl ExactSizeIterator<Item = &[f32]> {
        self.frames.iter().map(|frame| &frame[..self.samples])
    }
}

/// Energy/probability detector with pre-roll, hangover, and calibration.
/// Create on the capture worker: optional RNNoise warms its FFT caches there.
/// `process` performs no allocation, locking, logging, clock reads, or I/O.
/// One complete qualifying quantum opens the gate (10 ms attack).
pub struct Vad {
    config: Config,
    power_threshold: f64,
    hangover_frames: u16,
    quiet: u16,
    open: bool,
    history: [[f32; MAX_SAMPLES]; PRE_ROLL_FRAMES],
    history_sent: [bool; PRE_ROLL_FRAMES],
    history_len: usize,
    output: [[f32; MAX_SAMPLES]; PRE_ROLL_FRAMES + 1],
    meter_frames: u8,
    meter_power: f64,
    meter_peak: f32,
    calibration_power: [f64; CALIBRATION_FRAMES],
    calibration_frames: Option<usize>,
    #[cfg(feature = "rnnoise")]
    neural: Option<Neural>,
}
impl Vad {
    /// Allocate optional neural state and warm its FFT caches. Mono and stereo
    /// have fixed capacity; setup must run on the eventual processing thread.
    pub fn new(config: Config) -> Result<Self, Error> {
        let config = config.validate()?;
        // Validation caps this at 500; rounding up avoids cutting a short tail.
        let hangover_frames = u16::try_from(config.hangover.as_nanos().div_ceil(10_000_000))
            .map_err(|_| Error::Hangover)?;
        Ok(Self {
            config,
            power_threshold: power(config.threshold_dbfs),
            hangover_frames,
            quiet: 0,
            open: false,
            history: [[0.0; MAX_SAMPLES]; PRE_ROLL_FRAMES],
            history_sent: [false; PRE_ROLL_FRAMES],
            history_len: 0,
            output: [[0.0; MAX_SAMPLES]; PRE_ROLL_FRAMES + 1],
            meter_frames: 0,
            meter_power: 0.0,
            meter_peak: 0.0,
            calibration_power: [0.0; CALIBRATION_FRAMES],
            calibration_frames: None,
            #[cfg(feature = "rnnoise")]
            neural: (config.mode == Mode::Rnnoise).then(|| Neural::new(config.channels)),
        })
    }
    /// Current configuration, including an auto-calibrated threshold.
    pub fn config(&self) -> Config {
        self.config
    }
    /// Change the energy threshold without resetting the talk burst.
    pub fn set_threshold(&mut self, dbfs: f32) -> Result<(), Error> {
        threshold(dbfs)?;
        self.config.threshold_dbfs = dbfs;
        self.power_threshold = power(dbfs);
        Ok(())
    }
    /// Force the gate shut and discard pre-roll. The caller must handle the
    /// returned Close edge. Use when muting or pausing capture, so stale frames
    /// cannot be transmitted on the next opening. Calibration is cancelled.
    pub fn force_close(&mut self) -> Option<GateEvent> {
        let event = self.open.then_some(GateEvent::Close);
        self.open = false;
        self.quiet = 0;
        self.history_len = 0;
        self.calibration_frames = None;
        event
    }
    /// Listen for two seconds of background noise while transmitting no audio.
    /// The caller handles a returned Close edge if a burst was active.
    pub fn start_calibration(&mut self) -> Option<GateEvent> {
        let event = self.force_close();
        self.calibration_frames = Some(0);
        event
    }
    /// Number of 10 ms quanta still needed, or None when not calibrating.
    pub fn calibration_remaining(&self) -> Option<usize> {
        self.calibration_frames
            .map(|count| CALIBRATION_FRAMES - count)
    }

    /// Process exact interleaved normalized PCM. Errors leave all state intact.
    /// Returned audio is bit-identical to input, including pre-roll; RNNoise's
    /// denoised output is discarded. Continue calling while the gate is closed.
    pub fn process(&mut self, input: &[f32]) -> Result<Output<'_>, Error> {
        let channels = usize::from(self.config.channels);
        let samples = FRAME_SAMPLES * channels;
        if input.len() != samples {
            return Err(Error::FrameLength);
        }
        if input.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
            return Err(Error::Samples);
        }
        let (energy, peak) = level(input, channels);
        #[cfg(feature = "rnnoise")]
        let probability = self.probability(input);
        #[cfg(not(feature = "rnnoise"))]
        let probability = None;
        let calibration = self.calibrate(energy);
        let calibrating = calibration.is_some() || self.calibration_frames.is_some();
        let meter = self.meter(energy, peak, probability);
        let mut event = None;
        let mut count = 0;
        if !calibrating {
            let active = energy > self.power_threshold
                && probability.is_none_or(|p| p > self.config.probability_threshold);
            if active {
                self.quiet = 0;
                if !self.open {
                    self.open = true;
                    event = Some(GateEvent::Open);
                    for i in 0..self.history_len {
                        if !self.history_sent[i] {
                            self.output[count] = self.history[i];
                            count += 1;
                        }
                    }
                }
            } else if self.open {
                self.quiet = self.quiet.saturating_add(1);
            }
            let send_current = self.open && (active || self.hangover_frames > 0);
            if send_current {
                self.output[count][..samples].copy_from_slice(input);
                count += 1;
            }
            if self.open && !active && self.quiet >= self.hangover_frames {
                self.open = false;
                event = Some(GateEvent::Close);
            }
            self.remember(input, send_current);
        }
        Ok(Output {
            event,
            open: self.open,
            meter,
            calibration,
            frames: &self.output[..count],
            samples,
        })
    }

    fn remember(&mut self, input: &[f32], sent: bool) {
        if self.history_len == PRE_ROLL_FRAMES {
            self.history[0] = self.history[1];
            self.history_sent[0] = self.history_sent[1];
            self.history_len -= 1;
        }
        self.history[self.history_len][..input.len()].copy_from_slice(input);
        self.history_sent[self.history_len] = sent;
        self.history_len += 1;
    }
    fn calibrate(&mut self, energy: f64) -> Option<Calibration> {
        let count = self.calibration_frames.as_mut()?;
        self.calibration_power[*count] = energy;
        *count += 1;
        if *count < CALIBRATION_FRAMES {
            return None;
        }
        self.calibration_frames = None;
        self.history_len = 0;
        self.calibration_power.sort_unstable_by(f64::total_cmp);
        let noise_floor_dbfs = db(f64::midpoint(
            self.calibration_power[99],
            self.calibration_power[100],
        ));
        let threshold_dbfs = (noise_floor_dbfs + 12.0).clamp(-96.0, 0.0);
        self.config.threshold_dbfs = threshold_dbfs;
        self.power_threshold = power(threshold_dbfs);
        Some(Calibration {
            noise_floor_dbfs,
            threshold_dbfs,
        })
    }
    fn meter(&mut self, energy: f64, peak: f32, probability: Option<f32>) -> Option<Meter> {
        self.meter_frames += 1;
        self.meter_power += energy;
        self.meter_peak = self.meter_peak.max(peak);
        if self.meter_frames < 2 {
            return None;
        }
        let meter = Meter {
            rms_dbfs: db(self.meter_power * 0.5),
            peak_dbfs: db(f64::from(self.meter_peak).powi(2)),
            probability,
            threshold_dbfs: self.config.threshold_dbfs,
        };
        self.meter_frames = 0;
        self.meter_power = 0.0;
        self.meter_peak = 0.0;
        Some(meter)
    }
    #[cfg(feature = "rnnoise")]
    fn probability(&mut self, input: &[f32]) -> Option<f32> {
        self.neural
            .as_mut()
            .map(|neural| neural.process(input, usize::from(self.config.channels)))
    }
}
fn threshold(value: f32) -> Result<(), Error> {
    if value.is_finite() && (-96.0..=0.0).contains(&value) {
        Ok(())
    } else {
        Err(Error::Threshold)
    }
}
fn power(dbfs: f32) -> f64 {
    10.0_f64.powf(f64::from(dbfs) / 10.0)
}
#[allow(
    clippy::cast_possible_truncation,
    reason = "normalized PCM levels fit f32"
)]
fn db(energy: f64) -> f32 {
    (10.0 * energy.max(1.0e-12).log10()) as f32
}
fn level(input: &[f32], channels: usize) -> (f64, f32) {
    let mut energy = [0.0_f64; 2];
    let mut peak = 0.0_f32;
    for frame in input.chunks_exact(channels) {
        for (ch, value) in frame.iter().enumerate() {
            energy[ch] += f64::from(*value).powi(2);
            peak = peak.max(value.abs());
        }
    }
    (energy[0].max(energy[1]) / 480.0, peak)
}

#[cfg(feature = "rnnoise")]
struct Neural {
    states: Vec<Box<nnnoiseless::DenoiseState<'static>>>,
    input: [f32; FRAME_SAMPLES],
    output: [f32; FRAME_SAMPLES],
}
#[cfg(feature = "rnnoise")]
impl Neural {
    fn new(channels: u8) -> Self {
        let mut neural = Self {
            states: (0..channels)
                .map(|_| nnnoiseless::DenoiseState::new())
                .collect(),
            input: [0.0; FRAME_SAMPLES],
            output: [0.0; FRAME_SAMPLES],
        };
        // Initialize the library's lazy FFT planner on this worker, outside
        // process. Reset recurrent state after warming, without retaining audio.
        let _ = neural.process(
            &[0.0; MAX_SAMPLES][..FRAME_SAMPLES * usize::from(channels)],
            usize::from(channels),
        );
        for state in &mut neural.states {
            *state = nnnoiseless::DenoiseState::new();
        }
        neural
    }
    fn process(&mut self, input: &[f32], channels: usize) -> f32 {
        let mut probability = 0.0_f32;
        for (channel, state) in self.states.iter_mut().enumerate() {
            for (dest, frame) in self.input.iter_mut().zip(input.chunks_exact(channels)) {
                *dest = (frame[channel] * 32768.0).clamp(-32768.0, 32767.0);
            }
            probability = probability.max(state.process_frame(&mut self.output, &self.input));
        }
        probability
    }
}
