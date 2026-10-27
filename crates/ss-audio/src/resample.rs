//! Sample-rate conversion between a device clock and the 48 kHz graph.
//!
//! The worker thread owns this type. The device callback never calls it.
//! Equal rates copy samples through. Any other rate uses rubato's asynchronous
//! sinc resampler with a fixed input chunk, which is the current shape of
//! `SincFixedIn`.

use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction, audioadapter_buffers::direct::InterleavedSlice,
};

use crate::mixer::SAMPLES_PER_CHANNEL;

/// Graph rate. Device I/O is converted to and from this rate.
pub const GRAPH_RATE: u32 = 48_000;

/// A fixed-input sinc resampler, or a copy when both rates are [`GRAPH_RATE`].
pub struct RateConverter {
    channels: usize,
    frame_samples: usize,
    resampler: Option<Async<f32>>,
    input_frames: usize,
    output_frames_cap: usize,
    pending_in: Vec<f32>,
    in_read: usize,
    pending_out: Vec<f32>,
    out_read: usize,
    input_chunk: Vec<f32>,
    output_chunk: Vec<f32>,
}

/// Invalid rates or a sinc resampler that rubato refused to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// A rate or channel count was zero.
    #[error("sample rate and channel count must be non-zero")]
    Config,
    /// Rubato rejected the sinc parameters.
    #[error("could not build the sinc resampler")]
    Resampler,
}

impl RateConverter {
    /// Convert `input_rate` to `output_rate` for interleaved `channels`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when a rate or the channel count is zero.
    /// [`Error::Resampler`] when the sinc filter cannot be constructed.
    pub fn new(input_rate: u32, output_rate: u32, channels: usize) -> Result<Self, Error> {
        if input_rate == 0 || output_rate == 0 || channels == 0 {
            return Err(Error::Config);
        }
        let frame_samples = SAMPLES_PER_CHANNEL * channels;
        let (resampler, input_frames, output_frames_cap) = if input_rate == output_rate {
            (None, 0, 0)
        } else {
            let ratio = f64::from(output_rate) / f64::from(input_rate);
            let parameters = SincInterpolationParameters::new(256, WindowFunction::BlackmanHarris2)
                .interpolation(SincInterpolationType::Cubic);
            let resampler = Async::<f32>::new_sinc(
                ratio,
                1.1,
                &parameters,
                SAMPLES_PER_CHANNEL,
                channels,
                FixedAsync::Input,
            )
            .map_err(|_| Error::Resampler)?;
            let input_frames = resampler.input_frames_next();
            let output_frames_cap = resampler.output_frames_max();
            (Some(resampler), input_frames, output_frames_cap)
        };
        let input_samples = input_frames * channels;
        let output_samples = output_frames_cap * channels;
        Ok(Self {
            channels,
            frame_samples,
            resampler,
            input_frames,
            output_frames_cap,
            pending_in: Vec::with_capacity(input_samples.max(frame_samples)),
            in_read: 0,
            pending_out: Vec::with_capacity(output_samples.max(frame_samples) * 2),
            out_read: 0,
            input_chunk: vec![0.0; input_samples],
            output_chunk: vec![0.0; output_samples],
        })
    }

    /// `true` when samples pass through a sinc resampler.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.resampler.is_some()
    }

    /// Interleaved samples in one 10 ms output frame.
    #[must_use]
    pub const fn frame_samples(&self) -> usize {
        self.frame_samples
    }

    /// Samples available to [`Self::pull`], in interleaved graph order.
    #[must_use]
    pub fn available(&self) -> usize {
        self.pending_out.len().saturating_sub(self.out_read)
    }

    /// Append interleaved samples at the converter's input rate.
    ///
    /// # Errors
    ///
    /// [`Error::Resampler`] when a prepared chunk cannot be processed.
    pub fn push(&mut self, samples: &[f32]) -> Result<(), Error> {
        if samples.is_empty() {
            return Ok(());
        }
        if self.resampler.is_none() {
            self.pending_out.extend_from_slice(samples);
            return Ok(());
        }
        self.pending_in.extend_from_slice(samples);
        self.drain_input()
    }

    /// Copy up to `out.len()` interleaved output samples. Returns how many were written.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        self.compact_out();
        let available = self.available();
        let count = out.len().min(available);
        let start = self.out_read;
        out[..count].copy_from_slice(&self.pending_out[start..start + count]);
        self.out_read += count;
        count
    }

    /// Copy one 10 ms frame. `out` must be 480 samples per channel.
    pub fn pull_frame(&mut self, out: &mut [f32]) -> bool {
        if out.len() != self.frame_samples || self.available() < self.frame_samples {
            return false;
        }
        self.pull(out) == self.frame_samples
    }

    fn drain_input(&mut self) -> Result<(), Error> {
        let need = self.input_frames * self.channels;
        if need == 0 {
            return Ok(());
        }
        while self.pending_in.len().saturating_sub(self.in_read) >= need {
            let start = self.in_read;
            self.input_chunk
                .copy_from_slice(&self.pending_in[start..start + need]);
            self.in_read += need;
            let produced = self.process_chunk(need)?;
            let produced_samples = produced * self.channels;
            self.pending_out
                .extend_from_slice(&self.output_chunk[..produced_samples]);
            self.compact_in();
        }
        Ok(())
    }

    fn process_chunk(&mut self, input_samples: usize) -> Result<usize, Error> {
        let channels = self.channels;
        let input_frames = self.input_frames;
        let output_frames = self.output_frames_cap;
        let input =
            InterleavedSlice::new(&self.input_chunk[..input_samples], channels, input_frames)
                .map_err(|_| Error::Resampler)?;
        let mut output = InterleavedSlice::new_mut(
            &mut self.output_chunk[..output_frames * channels],
            channels,
            output_frames,
        )
        .map_err(|_| Error::Resampler)?;
        let resampler = self.resampler.as_mut().ok_or(Error::Resampler)?;
        let (_consumed, produced) = resampler
            .process_into_buffer(&input, &mut output, None)
            .map_err(|_| Error::Resampler)?;
        Ok(produced)
    }

    fn compact_in(&mut self) {
        if self.in_read > 0 && (self.in_read == self.pending_in.len() || self.in_read > 8_192) {
            self.pending_in.drain(..self.in_read);
            self.in_read = 0;
        }
    }

    fn compact_out(&mut self) {
        if self.out_read > 0 && self.out_read == self.pending_out.len() {
            self.pending_out.clear();
            self.out_read = 0;
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
mod tests {
    use super::{GRAPH_RATE, RateConverter};

    #[test]
    fn equal_rates_preserve_frames() {
        let mut converter = RateConverter::new(GRAPH_RATE, GRAPH_RATE, 1).unwrap();
        assert!(!converter.is_active());
        let input: Vec<f32> = (0..960).map(|index| index as f32).collect();
        converter.push(&input).unwrap();
        let mut frame = [0.0; 480];
        assert!(converter.pull_frame(&mut frame));
        assert_eq!(&frame[..], &input[..480]);
        assert!(converter.pull_frame(&mut frame));
        assert_eq!(&frame[..], &input[480..]);
        assert!(!converter.pull_frame(&mut frame));
    }

    #[test]
    fn rejects_a_zero_rate() {
        assert!(RateConverter::new(0, GRAPH_RATE, 1).is_err());
        assert!(RateConverter::new(GRAPH_RATE, GRAPH_RATE, 0).is_err());
    }

    #[test]
    fn cd_rate_sine_lands_on_a_48k_sine() {
        let input_rate = 44_100;
        let frequency = 1_000.0;
        let amplitude = 0.5;
        let input = sine(input_rate, frequency, 1.0, amplitude);
        let mut converter = RateConverter::new(input_rate, GRAPH_RATE, 1).unwrap();
        assert!(converter.is_active());
        converter.push(&input).unwrap();
        let mut output = vec![0.0; converter.available()];
        let written = converter.pull(&mut output);
        output.truncate(written);

        let skip = GRAPH_RATE as usize / 10;
        let end = output.len().saturating_sub(skip);
        assert!(end > skip + GRAPH_RATE as usize / 2);
        let steady = &output[skip..end];
        let measured = dominant_frequency(steady, GRAPH_RATE as f32);
        let expected_rms = amplitude / std::f32::consts::SQRT_2;
        let actual_rms = rms(steady);
        let decibels = 20.0 * (actual_rms / expected_rms).log10();
        assert!(
            (measured - frequency).abs() < 5.0,
            "frequency {measured} Hz"
        );
        assert!(
            decibels.abs() < 0.5,
            "amplitude error {decibels} dB (rms {actual_rms}, expected {expected_rms})"
        );
    }

    fn sine(rate: u32, frequency: f32, seconds: f32, amplitude: f32) -> Vec<f32> {
        let count = (rate as f32 * seconds) as usize;
        (0..count)
            .map(|index| {
                let phase = frequency * index as f32 / rate as f32;
                amplitude * (phase * std::f32::consts::TAU).sin()
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        let energy = samples.iter().map(|sample| sample * sample).sum::<f32>();
        (energy / samples.len() as f32).sqrt()
    }

    fn dominant_frequency(samples: &[f32], rate: f32) -> f32 {
        let mut crossings = 0usize;
        let mut first = None;
        let mut last = 0usize;
        for (index, pair) in samples.windows(2).enumerate() {
            if pair[0] <= 0.0 && pair[1] > 0.0 {
                if first.is_none() {
                    first = Some(index);
                }
                last = index;
                crossings += 1;
            }
        }
        let start = first.expect("sine crosses zero");
        let periods = (crossings - 1) as f32;
        periods * rate / (last - start) as f32
    }
}
