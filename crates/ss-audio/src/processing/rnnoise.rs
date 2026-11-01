//! RNNoise noise suppression through `nnnoiseless`, one state per channel.
//!
//! RNNoise reads 16-bit-scaled samples and overlap-adds 10 ms windows, so
//! its output trails its input by exactly one frame.

use std::time::Duration;

use nnnoiseless::DenoiseState;

use super::{Backend, Processor, Stage};
use crate::mixer::{SAMPLES_PER_CHANNEL, StereoFrame};

const SCALE: f32 = 32_768.0;

type Plane = [f32; SAMPLES_PER_CHANNEL];

pub(super) struct RnNoise {
    states: Vec<Box<DenoiseState<'static>>>,
    input: Plane,
    output: Plane,
}

impl RnNoise {
    pub(super) fn new(channels: usize) -> Self {
        let mut stage = Self {
            states: (0..channels).map(|_| DenoiseState::new()).collect(),
            input: [0.0; SAMPLES_PER_CHANNEL],
            output: [0.0; SAMPLES_PER_CHANNEL],
        };
        stage.warm();
        stage
    }

    /// Run one silent and one non-silent frame so lazily built FFT plans
    /// exist before the first real frame, then start from clean state.
    fn warm(&mut self) {
        let mut frame = [0.0; SAMPLES_PER_CHANNEL * 2];
        let channels = self.states.len();
        for (index, sample) in frame.iter_mut().enumerate() {
            *sample = if index % 7 == 0 { 0.1 } else { -0.01 };
        }
        self.process(&mut frame[..SAMPLES_PER_CHANNEL * channels], &[]);
        self.reset();
    }
}

impl Processor for RnNoise {
    fn stage(&self) -> Stage {
        Stage::NoiseSuppression
    }

    fn backend(&self) -> Backend {
        Backend::RnNoise
    }

    fn latency(&self) -> Duration {
        Duration::from_millis(10)
    }

    fn process(&mut self, capture: &mut [f32], _render: &[StereoFrame]) {
        let channels = self.states.len();
        for (channel, state) in self.states.iter_mut().enumerate() {
            for (input, sample) in self
                .input
                .iter_mut()
                .zip(capture.iter().skip(channel).step_by(channels))
            {
                *input = *sample * SCALE;
            }
            state.process_frame(&mut self.output, &self.input);
            for (sample, output) in capture
                .iter_mut()
                .skip(channel)
                .step_by(channels)
                .zip(self.output.iter())
            {
                *sample = *output / SCALE;
            }
        }
    }

    fn reset(&mut self) {
        for state in &mut self.states {
            *state = DenoiseState::new();
        }
    }
}
