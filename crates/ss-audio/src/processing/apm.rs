//! `sonora` stages: AEC3, the WebRTC noise suppressor, and AGC2.
//!
//! Each stage owns its own `AudioProcessing` instance configured with that
//! one component, so stages can be mixed with RNNoise and measured alone.
//! Processing runs at the full 48 kHz internal rate: the default 32 kHz
//! would resample and drop everything above 16 kHz.

use std::time::Duration;

use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{
        AdaptiveDigital, EchoCanceller, GainController2, MaxProcessingRate, NoiseSuppressionLevel,
        Pipeline,
    },
};

use super::{Backend, Processor, Stage, SuppressionLevel};
use crate::{
    mixer::{SAMPLES_PER_CHANNEL, StereoFrame},
    resample::GRAPH_RATE,
};

/// Capture-path delay in 48 kHz samples, measured as the impulse-response
/// peak by the fixture tests. AEC3 and the suppressor run on 16 kHz bands
/// from the three-band filter bank (about 46 samples): AEC3 adds its 4 ms
/// block framing, the suppressor its 6 ms overlap-add. AGC2 runs full band.
const fn delay_samples(stage: Stage) -> u64 {
    match stage {
        Stage::EchoCancellation => 238,
        Stage::NoiseSuppression => 334,
        Stage::GainControl => 0,
    }
}

type Plane = [f32; SAMPLES_PER_CHANNEL];

pub(super) struct Apm {
    engine: AudioProcessing,
    stage: Stage,
    channels: usize,
    input: [Plane; 2],
    output: [Plane; 2],
    render: Plane,
    render_out: Plane,
}

impl Apm {
    pub(super) fn new(stage: Stage, level: Option<SuppressionLevel>, channels: usize) -> Self {
        let mut config = Config {
            pipeline: Pipeline {
                maximum_internal_processing_rate: MaxProcessingRate::Rate48kHz,
                multi_channel_render: false,
                multi_channel_capture: true,
                ..Pipeline::default()
            },
            ..Config::default()
        };
        match stage {
            Stage::EchoCancellation => config.echo_canceller = Some(EchoCanceller::default()),
            Stage::NoiseSuppression => {
                config.noise_suppression = Some(sonora::config::NoiseSuppression {
                    level: match level.unwrap_or_default() {
                        SuppressionLevel::Low => NoiseSuppressionLevel::Low,
                        SuppressionLevel::Moderate => NoiseSuppressionLevel::Moderate,
                        SuppressionLevel::High => NoiseSuppressionLevel::High,
                        SuppressionLevel::VeryHigh => NoiseSuppressionLevel::VeryHigh,
                    },
                    analyze_linear_aec_output_when_available: false,
                });
            }
            Stage::GainControl => {
                config.gain_controller2 = Some(GainController2 {
                    input_volume_controller: false,
                    adaptive_digital: Some(AdaptiveDigital::default()),
                    ..GainController2::default()
                });
            }
        }
        let engine = build(config, channels);
        Self {
            engine,
            stage,
            channels,
            input: [[0.0; SAMPLES_PER_CHANNEL]; 2],
            output: [[0.0; SAMPLES_PER_CHANNEL]; 2],
            render: [0.0; SAMPLES_PER_CHANNEL],
            render_out: [0.0; SAMPLES_PER_CHANNEL],
        }
    }

    fn analyze_render(&mut self, render: &[StereoFrame]) {
        for frame in render {
            for (mono, pair) in self.render.iter_mut().zip(frame.as_chunks::<2>().0) {
                *mono = f32::midpoint(pair[0], pair[1]);
            }
            let result = self
                .engine
                .process_render_f32(&[&self.render], &mut [&mut self.render_out]);
            // Formats are fixed at construction; sonora only rejects formats.
            debug_assert!(result.is_ok(), "render format rejected: {result:?}");
        }
    }
}

impl Processor for Apm {
    fn stage(&self) -> Stage {
        self.stage
    }

    fn backend(&self) -> Backend {
        Backend::Sonora
    }

    fn latency(&self) -> Duration {
        Duration::from_nanos(delay_samples(self.stage) * 1_000_000_000 / u64::from(GRAPH_RATE))
    }

    fn process(&mut self, capture: &mut [f32], render: &[StereoFrame]) {
        if self.stage == Stage::EchoCancellation {
            self.analyze_render(render);
        }
        let [left_in, right_in] = &mut self.input;
        let [left_out, right_out] = &mut self.output;
        let result = if self.channels == 1 {
            left_in.copy_from_slice(capture);
            self.engine.process_capture_f32(&[left_in], &mut [capture])
        } else {
            for ((left, right), pair) in left_in
                .iter_mut()
                .zip(right_in.iter_mut())
                .zip(capture.as_chunks::<2>().0)
            {
                *left = pair[0];
                *right = pair[1];
            }
            let result = self
                .engine
                .process_capture_f32(&[left_in, right_in], &mut [left_out, right_out]);
            for ((pair, left), right) in capture
                .as_chunks_mut::<2>()
                .0
                .iter_mut()
                .zip(left_out.iter())
                .zip(right_out.iter())
            {
                *pair = [*left, *right];
            }
            result
        };
        debug_assert!(result.is_ok(), "capture format rejected: {result:?}");
    }

    fn set_echo_delay(&mut self, delay: Duration) {
        if self.stage == Stage::EchoCancellation {
            let millis = i32::try_from(delay.as_millis()).unwrap_or(i32::MAX);
            // Out-of-range hints are clamped to 0..=500 ms and still applied.
            let _ = self.engine.set_stream_delay_ms(millis);
        }
    }

    fn reset(&mut self) {
        let delay = self.engine.stream_delay_ms();
        self.engine = build(self.engine.config().clone(), self.channels);
        if self.stage == Stage::EchoCancellation {
            let _ = self.engine.set_stream_delay_ms(delay);
        }
    }
}

/// Capture is `channels` at 48 kHz; the render reference is downmixed mono.
fn build(config: Config, channels: usize) -> AudioProcessing {
    let capture = StreamConfig::new(GRAPH_RATE, if channels == 2 { 2 } else { 1 });
    AudioProcessing::builder()
        .config(config)
        .capture_config(capture)
        .render_config(StreamConfig::new(GRAPH_RATE, 1))
        .build()
}
