//! Playback-worker mixing. Feed the result into the device ring; do not run
//! decoding or mixing from the device callback.

/// Samples per channel in a 10 ms worker quantum at 48 kHz.
pub const SAMPLES_PER_CHANNEL: usize = 480;
/// One interleaved stereo playback quantum.
pub type StereoFrame = [f32; SAMPLES_PER_CHANNEL * 2];

/// Invalid gain or non-finite PCM. Rejected inputs leave accumulated audio intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Linear gain must be finite and between zero and four (+12 dB).
    #[error("gain must be finite and within 0..=4")]
    Gain,
    /// PCM contains NaN or infinity.
    #[error("non-finite PCM")]
    Pcm,
}

pub(crate) fn validate_gain(gain: f32) -> Result<(), Error> {
    if gain.is_finite() && (0.0..=4.0).contains(&gain) {
        Ok(())
    } else {
        Err(Error::Gain)
    }
}

/// Stereo sum, user/master gains, UI injection, and a linked soft-knee limiter.
/// Inputs are summed in f64 to avoid overflow before limiting; construction and
/// every operation are allocation-free. `finish` clears the sum for the next frame.
pub struct Mixer {
    sum: [f64; SAMPLES_PER_CHANNEL * 2],
    output: StereoFrame,
    master_gain: f32,
    limited_samples: u64,
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

impl Mixer {
    /// Construct an empty mixer with unity master gain.
    pub fn new() -> Self {
        Self {
            sum: [0.0; SAMPLES_PER_CHANNEL * 2],
            output: [0.0; SAMPLES_PER_CHANNEL * 2],
            master_gain: 1.0,
            limited_samples: 0,
        }
    }

    /// Set the master linear gain (0 through 4), including UI sounds.
    pub fn set_master_gain(&mut self, gain: f32) -> Result<(), Error> {
        validate_gain(gain)?;
        self.master_gain = gain;
        Ok(())
    }

    /// Add one track with the user's linear gain (0 through 4).
    pub fn add_track(&mut self, frame: &StereoFrame, gain: f32) -> Result<(), Error> {
        validate_gain(gain)?;
        if !frame.iter().all(|sample| sample.is_finite()) {
            return Err(Error::Pcm);
        }
        for (sum, sample) in self.sum.iter_mut().zip(frame) {
            *sum += f64::from(*sample) * f64::from(gain);
        }
        Ok(())
    }

    /// Inject a stereo UI sound at unity gain before master gain and limiting.
    pub fn add_ui(&mut self, frame: &StereoFrame) -> Result<(), Error> {
        self.add_track(frame, 1.0)
    }

    /// Number of stereo sample pairs above the soft knee since construction.
    pub fn limited_samples(&self) -> u64 {
        self.limited_samples
    }

    /// Finish a 10 ms frame and reset the accumulator. The knee starts at 0.8;
    /// a linked gain preserves stereo balance and smoothly approaches ±1.
    pub fn finish(&mut self) -> &StereoFrame {
        for (source, output) in self
            .sum
            .as_chunks_mut::<2>()
            .0
            .iter_mut()
            .zip(self.output.as_chunks_mut::<2>().0)
        {
            let left = source[0] * f64::from(self.master_gain);
            let right = source[1] * f64::from(self.master_gain);
            let peak = left.abs().max(right.abs());
            let gain = if peak > 0.8 {
                self.limited_samples += 1;
                (0.8 + 0.2 * ((peak - 0.8) / 0.2).tanh()) / peak
            } else {
                1.0
            };
            // The limiter proves these finite outputs lie in [-1, 1].
            #[allow(clippy::cast_possible_truncation)]
            {
                output[0] = (left * gain) as f32;
                output[1] = (right * gain) as f32;
            }
            *source = [0.0; 2];
        }
        &self.output
    }
}
