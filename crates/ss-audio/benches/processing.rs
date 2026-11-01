//! Processing-chain CPU budget: each stage under 1.5 ms and the whole chain
//! under 3 ms per 10 ms frame. Stages are benchmarked when their feature is
//! compiled in:
//!
//! ```sh
//! cargo bench -p ss-audio --all-features --bench processing
//! ```

#![allow(clippy::cast_precision_loss)] // Frame counts stay small.

#[path = "../tests/processing/signals.rs"]
mod signals;

use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use ss_audio::{
    mixer::StereoFrame,
    processing::{
        CHAIN_BUDGET, Chain, NoiseSuppression, ProcessingConfig, STAGE_BUDGET, SuppressionLevel,
    },
};

use signals::{FRAME, RATE};

struct Input {
    capture: Vec<Vec<f32>>,
    render: Vec<StereoFrame>,
}

/// Four seconds of near-end speech, fan noise, and echo of far-end speech,
/// interleaved for `channels`, with the far end as stereo render frames.
fn input(channels: usize) -> Input {
    let far = signals::speech(4.0, 3, 0.1).samples;
    let echo = signals::room_echo(&far, 40 * RATE / 1000, 0.5, 1);
    let near = signals::speech(4.0, 8, 0.05).samples;
    let fan = signals::fan(4.0, 2, 0.015);
    let mono: Vec<f32> = (0..far.len())
        .map(|index| echo[index] + near[index] + fan[index])
        .collect();
    let capture = mono
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|frame| {
            frame
                .iter()
                .flat_map(|sample| std::iter::repeat_n(*sample, channels))
                .collect()
        })
        .collect();
    let render = far
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|frame| {
            let mut stereo = [0.0; FRAME * 2];
            for (pair, sample) in stereo.as_chunks_mut::<2>().0.iter_mut().zip(frame) {
                *pair = [*sample; 2];
            }
            stereo
        })
        .collect();
    Input { capture, render }
}

fn configs() -> Vec<(&'static str, ProcessingConfig, Duration)> {
    let mut configs = Vec::new();
    if cfg!(feature = "aec") {
        configs.extend([
            (
                "aec3",
                ProcessingConfig {
                    echo_cancellation: true,
                    ..ProcessingConfig::OFF
                },
                STAGE_BUDGET,
            ),
            (
                "sonora-ns",
                ProcessingConfig {
                    noise_suppression: NoiseSuppression::Sonora(SuppressionLevel::Moderate),
                    ..ProcessingConfig::OFF
                },
                STAGE_BUDGET,
            ),
            (
                "agc2",
                ProcessingConfig {
                    gain_control: true,
                    ..ProcessingConfig::OFF
                },
                STAGE_BUDGET,
            ),
        ]);
    }
    if cfg!(feature = "rnnoise") {
        configs.push((
            "rnnoise",
            ProcessingConfig {
                noise_suppression: NoiseSuppression::RnNoise,
                ..ProcessingConfig::OFF
            },
            STAGE_BUDGET,
        ));
    }
    if cfg!(all(feature = "aec", feature = "rnnoise")) {
        configs.push((
            "chain",
            ProcessingConfig {
                echo_cancellation: true,
                noise_suppression: NoiseSuppression::RnNoise,
                gain_control: true,
            },
            CHAIN_BUDGET,
        ));
    }
    configs
}

fn processing(criterion: &mut Criterion) {
    for channels in [1_u16, 2] {
        let input = input(usize::from(channels));
        for (name, config, budget) in configs() {
            let mut chain = Chain::new(config, channels).unwrap();
            let mut frame = vec![0.0; FRAME * usize::from(channels)];
            let mut index = 0;
            let mut step = || {
                let at = index % input.capture.len();
                frame.copy_from_slice(&input.capture[at]);
                chain
                    .process(black_box(&mut frame), &input.render[at..=at])
                    .unwrap();
                black_box(&frame);
                index += 1;
            };
            for _ in 0..100 {
                step();
            }
            let frames = 1000;
            let start = Instant::now();
            for _ in 0..frames {
                step();
            }
            let mean = start.elapsed() / frames;
            println!("{name} ({channels} ch): mean {mean:?} per 10 ms frame, budget {budget:?}");
            assert!(
                mean < budget,
                "{name} ({channels} ch): mean {mean:?} exceeds {budget:?}"
            );
            criterion.bench_function(&format!("{name}-{channels}ch"), |b| b.iter(&mut step));
        }
    }
}

criterion_group! { name = benches; config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2)); targets = processing }
criterion_main!(benches);
