//! Processing-chain fixtures: bypass identity, echo return loss enhancement,
//! noise suppression SNR and speech loss, and measured stage latency.
//!
//! `cargo test -p ss-audio --all-features --test processing -- --nocapture`
//! prints the measured figures.

#![allow(clippy::cast_precision_loss)] // Sample counts stay far below 2^24.

mod signals;

use ss_audio::processing::{Chain, Error, ProcessingConfig};

use signals::{FRAME, Rng};

#[cfg(any(feature = "aec", feature = "rnnoise"))]
fn stereo(mono: &[f32]) -> ss_audio::mixer::StereoFrame {
    let mut frame = [0.0; FRAME * 2];
    for (pair, sample) in frame.as_chunks_mut::<2>().0.iter_mut().zip(mono) {
        *pair = [*sample; 2];
    }
    frame
}

/// Run `capture` through `chain` frame by frame, feeding `far` as render.
#[cfg(any(feature = "aec", feature = "rnnoise"))]
fn run(chain: &mut Chain, capture: &[f32], far: Option<&[f32]>) -> Vec<f32> {
    let mut output = capture.to_vec();
    for (index, frame) in output.as_chunks_mut::<FRAME>().0.iter_mut().enumerate() {
        let render = far.map(|far| stereo(&far[index * FRAME..(index + 1) * FRAME]));
        chain
            .process(frame, render.as_slice())
            .expect("finite 10 ms frame");
    }
    output
}

fn arbitrary_frame(rng: &mut Rng, samples: usize) -> Vec<f32> {
    (0..samples)
        .map(|index| match index % 97 {
            0 => f32::from_bits(u32::try_from(rng.next_u64() >> 32).unwrap()),
            1 => -0.0,
            2 => f32::MIN_POSITIVE / 4.0,
            _ => rng.signed(),
        })
        .collect()
}

#[test]
fn disabled_chain_leaves_every_bit_alone_without_allocating() {
    let mut rng = Rng::new(7);
    for channels in [1_u16, 2] {
        let samples = FRAME * usize::from(channels);
        let mut chain = Chain::new(ProcessingConfig::OFF, channels).unwrap();
        assert!(chain.is_bypass());
        let frames: Vec<Vec<f32>> = (0..50)
            .map(|_| arbitrary_frame(&mut rng, samples))
            .collect();
        let mut processed = frames.clone();
        let render = [[0.25; FRAME * 2]; 2];
        let allocations = allocation_counter::measure(|| {
            for frame in &mut processed {
                chain.process(frame, &render).unwrap();
            }
        });
        assert_eq!(allocations.count_total, 0);
        for (before, after) in frames.iter().zip(&processed) {
            assert!(
                before
                    .iter()
                    .zip(after)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
        assert_eq!(chain.monitor().chain().frames, 0);
        let mut wrong = vec![0.0; samples + 1];
        assert!(matches!(
            chain.process(&mut wrong, &[]),
            Err(Error::FrameLength { .. })
        ));
    }
}

#[cfg(feature = "aec")]
mod echo {
    use std::time::Duration;

    use ss_audio::processing::{Chain, ProcessingConfig, Stage};

    use super::{
        run,
        signals::{FRAME, RATE, Rng, db, energy, room_echo, speech},
    };

    const SECONDS: usize = 10;

    struct Fixture {
        far: Vec<f32>,
        echo: Vec<f32>,
        near: Vec<f32>,
        floor: Vec<f32>,
    }

    /// Far-end speech for 7 s, then 3 s of near-end-only speech; near-end
    /// speech also overlaps the far end from 5 s to 7 s (double talk).
    fn fixture() -> Fixture {
        let total = SECONDS * RATE;
        let mut far = speech(SECONDS as f32, 11, 0.1).samples;
        far[7 * RATE..].iter_mut().for_each(|x| *x = 0.0);
        let echo = room_echo(&far, 40 * RATE / 1000, 0.5, 5);
        let mut near = speech(SECONDS as f32, 23, 0.05).samples;
        near[..5 * RATE].iter_mut().for_each(|x| *x = 0.0);
        let mut rng = Rng::new(3);
        let floor = (0..total).map(|_| 1e-4 * rng.signed()).collect();
        Fixture {
            far,
            echo,
            near,
            floor,
        }
    }

    fn aec_chain(hint: Duration) -> Chain {
        let config = ProcessingConfig {
            echo_cancellation: true,
            ..ProcessingConfig::OFF
        };
        let mut chain = Chain::new(config, 1).unwrap();
        assert!(chain.uses_render());
        chain.set_echo_delay(hint);
        chain
    }

    /// The hint matches the 40 ms bulk delay, then is 30 ms short; AEC3's
    /// delay estimator has to find the rest.
    #[test]
    fn cancels_injected_far_end_echo_by_at_least_20_db() {
        for hint in [40, 10] {
            cancel_echo(Duration::from_millis(hint));
        }
    }

    fn cancel_echo(hint: Duration) {
        let fixture = fixture();
        let capture: Vec<f32> = fixture
            .echo
            .iter()
            .zip(&fixture.near)
            .zip(&fixture.floor)
            .map(|((echo, near), floor)| echo + near + floor)
            .collect();
        let mut chain = aec_chain(hint);
        let output = run(&mut chain, &capture, Some(&fixture.far));
        // Far-end single talk after two seconds of convergence.
        let single = 2 * RATE..5 * RATE;
        let erle = db(
            energy(&fixture.echo[single.clone()]),
            energy(&output[single]),
        );
        // Near-end single talk: the canceller should pass the talker.
        let alone = 7 * RATE + FRAME..SECONDS * RATE;
        let near_loss = db(energy(&fixture.near[alone.clone()]), energy(&output[alone]));
        let report = chain.monitor().stages().next().unwrap();
        println!(
            "AEC3, {hint:?} hint: ERLE {erle:.1} dB, near-end loss {near_loss:.2} dB, \
             debug-build cpu mean {:?} max {:?}",
            report.cpu.mean, report.cpu.max
        );
        assert_eq!(report.stage, Stage::EchoCancellation);
        assert!(erle >= 20.0, "ERLE {erle:.1} dB");
        assert!(near_loss < 3.0, "near-end loss {near_loss:.2} dB");
    }
}

#[cfg(any(feature = "aec", feature = "rnnoise"))]
mod noise {
    use ss_audio::processing::{Chain, NoiseSuppression, ProcessingConfig};

    use super::{
        run,
        signals::{FRAME, RATE, db, energy, fan, speech},
    };

    const SECONDS: usize = 10;

    struct Measured {
        snr_in: f64,
        snr_out: f64,
        speech_loss: f64,
    }

    /// Simple SNR on known parts. Active frames carry clean speech within
    /// 15 dB of its level; pause frames carry none. Residual noise is the
    /// output power in pauses. Speech is the output power in active frames
    /// minus that residual, compared with the clean speech in those frames.
    fn measure(suppressor: NoiseSuppression) -> Measured {
        let clean = speech(SECONDS as f32, 41, 0.05).samples;
        let noise = fan(SECONDS as f32, 9, 0.05 / 10_f32.sqrt()); // 10 dB below speech
        let mixture: Vec<f32> = clean.iter().zip(&noise).map(|(s, n)| s + n).collect();
        let config = ProcessingConfig {
            noise_suppression: suppressor,
            ..ProcessingConfig::OFF
        };
        let mut chain = Chain::new(config, 1).unwrap();
        let output = run(&mut chain, &mixture, None);
        let delay = usize::try_from(
            (chain.latency().as_nanos() * RATE as u128 + 500_000_000) / 1_000_000_000,
        )
        .unwrap();
        let threshold = f64::from(0.05_f32 * 0.178).powi(2) * FRAME as f64;
        let [mut speech_in, mut speech_out, mut pause_in, mut pause_out] = [0.0_f64; 4];
        let (mut active, mut pauses) = (0_u32, 0_u32);
        // Skip two seconds of convergence and the tail the delay pushes out.
        for frame in 2 * RATE / FRAME..(SECONDS * RATE - delay) / FRAME {
            let span = frame * FRAME..(frame + 1) * FRAME;
            let level = energy(&clean[span.clone()]);
            let out = energy(&output[span.start + delay..span.end + delay]);
            if level > threshold {
                speech_in += level;
                speech_out += out;
                active += 1;
            } else if level == 0.0 {
                pause_in += energy(&noise[span]);
                pause_out += out;
                pauses += 1;
            }
        }
        assert!(
            active > 200 && pauses > 200,
            "{active} active, {pauses} pause frames"
        );
        let (speech_in, speech_out) = (
            speech_in / f64::from(active),
            speech_out / f64::from(active),
        );
        let (noise_in, residual) = (pause_in / f64::from(pauses), pause_out / f64::from(pauses));
        let measured = Measured {
            snr_in: db(speech_in, noise_in),
            snr_out: db(speech_out - residual, residual),
            speech_loss: db(speech_in, speech_out - residual),
        };
        let report = chain.monitor().stages().next().unwrap();
        println!(
            "{suppressor:?}: SNR {:.1} -> {:.1} dB, speech loss {:.2} dB, latency {:?}, \
             debug-build cpu mean {:?} max {:?}",
            measured.snr_in,
            measured.snr_out,
            measured.speech_loss,
            report.latency,
            report.cpu.mean,
            report.cpu.max
        );
        measured
    }

    #[cfg(feature = "rnnoise")]
    #[test]
    fn rnnoise_improves_snr_with_at_most_1_db_speech_loss() {
        let measured = measure(NoiseSuppression::RnNoise);
        assert!(measured.snr_out - measured.snr_in >= 10.0);
        assert!(
            measured.speech_loss <= 1.0,
            "loss {:.2} dB",
            measured.speech_loss
        );
    }

    /// The WebRTC suppressor trades more speech for its noise reduction on
    /// this fixture (about 1.1 dB at moderate), so it is held to 1.5 dB and
    /// RNNoise is the recommended suppressor.
    #[cfg(feature = "aec")]
    #[test]
    fn sonora_suppressor_improves_snr() {
        let measured = measure(NoiseSuppression::Sonora(
            ss_audio::processing::SuppressionLevel::Moderate,
        ));
        assert!(measured.snr_out - measured.snr_in >= 10.0);
        assert!(
            measured.speech_loss <= 1.5,
            "loss {:.2} dB",
            measured.speech_loss
        );
    }

    #[cfg(feature = "rnnoise")]
    #[test]
    fn rnnoise_processes_stereo_channels_independently() {
        let config = ProcessingConfig {
            noise_suppression: NoiseSuppression::RnNoise,
            ..ProcessingConfig::OFF
        };
        let mut chain = Chain::new(config, 2).unwrap();
        let left = speech(2.0, 5, 0.05).samples;
        let mut output: Vec<f32> = left.iter().flat_map(|sample| [*sample, 0.0]).collect();
        for frame in output.as_chunks_mut::<{ FRAME * 2 }>().0 {
            chain.process(frame, &[]).unwrap();
        }
        let right: f64 = output
            .iter()
            .skip(1)
            .step_by(2)
            .map(|x| f64::from(*x).powi(2))
            .sum();
        let left_out: f64 = output
            .iter()
            .step_by(2)
            .map(|x| f64::from(*x).powi(2))
            .sum();
        assert!(right < 1e-9 && left_out > 1.0);
    }
}

/// Clicks every 100 ms; the median position of the output peak after each
/// click, past the first second, is the stage's capture-path delay.
#[cfg(any(feature = "aec", feature = "rnnoise"))]
fn impulse_delay(config: ProcessingConfig) -> (usize, usize) {
    use signals::RATE;
    let mut chain = Chain::new(config, 1).unwrap();
    let period = RATE / 10;
    let mut clicks = vec![0.0_f32; 4 * RATE];
    for click in 0..40 {
        clicks[click * period + 123] = 0.5;
    }
    let output = run(&mut chain, &clicks, None);
    let mut peaks: Vec<usize> = (10..39)
        .map(|click| {
            let start = click * period + 123;
            output[start..start + period - 200]
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .map_or(0, |(offset, _)| offset)
        })
        .collect();
    peaks.sort_unstable();
    let reported = (chain.latency().as_nanos() * RATE as u128 + 500_000_000) / 1_000_000_000;
    (peaks[peaks.len() / 2], usize::try_from(reported).unwrap())
}

#[cfg(any(feature = "aec", feature = "rnnoise"))]
#[test]
fn every_stage_adds_the_latency_it_reports() {
    use ss_audio::processing::NoiseSuppression;
    let mut configs = Vec::new();
    if cfg!(feature = "aec") {
        configs.extend([
            ProcessingConfig {
                echo_cancellation: true,
                ..ProcessingConfig::OFF
            },
            ProcessingConfig {
                noise_suppression: NoiseSuppression::Sonora(
                    ss_audio::processing::SuppressionLevel::Moderate,
                ),
                ..ProcessingConfig::OFF
            },
            ProcessingConfig {
                gain_control: true,
                ..ProcessingConfig::OFF
            },
        ]);
    }
    if cfg!(feature = "rnnoise") {
        configs.push(ProcessingConfig {
            noise_suppression: NoiseSuppression::RnNoise,
            ..ProcessingConfig::OFF
        });
    }
    if cfg!(all(feature = "aec", feature = "rnnoise")) {
        configs.push(ProcessingConfig {
            echo_cancellation: true,
            noise_suppression: NoiseSuppression::RnNoise,
            gain_control: true,
        });
    }
    for config in configs {
        let (measured, reported) = impulse_delay(config);
        println!("{config:?}: impulse peak {measured} samples, reported {reported}");
        assert!(measured.abs_diff(reported) <= 2);
    }
}

#[cfg(feature = "aec")]
mod gain {
    use ss_audio::processing::{Chain, ProcessingConfig};

    use super::{
        run,
        signals::{RATE, db, energy, speech},
    };

    fn agc(rms: f32) -> (f64, f32) {
        let config = ProcessingConfig {
            gain_control: true,
            ..ProcessingConfig::OFF
        };
        let mut chain = Chain::new(config, 1).unwrap();
        let input = speech(10.0, 3, rms).samples;
        let output = run(&mut chain, &input, None);
        let settled = 6 * RATE..10 * RATE;
        let peak = output.iter().fold(0.0_f32, |peak, x| peak.max(x.abs()));
        (
            db(energy(&output[settled.clone()]), energy(&input[settled])),
            peak,
        )
    }

    #[test]
    fn raises_a_quiet_talker_and_limits_a_loud_one() {
        let (quiet, quiet_peak) = agc(0.003);
        let (normal, normal_peak) = agc(0.05);
        println!("AGC2: -50 dBFS talker +{quiet:.1} dB, -26 dBFS talker +{normal:.1} dB");
        assert!(quiet >= 20.0, "quiet talker gain {quiet:.1} dB");
        assert!(normal < quiet && normal < 10.0);
        assert!(quiet_peak <= 1.0 && normal_peak <= 1.0);
    }
}

/// RNNoise and the WebRTC suppressor reuse their buffers. sonora's AEC3 and
/// AGC2 allocate inside the dependency every frame (a few dozen small
/// allocations); that is acceptable on the capture worker, never in a
/// device callback, and is recorded in the T-053 notes.
#[cfg(any(feature = "aec", feature = "rnnoise"))]
#[test]
fn suppressors_do_not_allocate_after_construction() {
    use ss_audio::processing::NoiseSuppression;
    let mut suppressors = Vec::new();
    if cfg!(feature = "aec") {
        suppressors.push(NoiseSuppression::Sonora(
            ss_audio::processing::SuppressionLevel::High,
        ));
    }
    if cfg!(feature = "rnnoise") {
        suppressors.push(NoiseSuppression::RnNoise);
    }
    let input = signals::speech(3.0, 2, 0.05).samples;
    for suppressor in suppressors {
        for channels in [1_u16, 2] {
            let config = ProcessingConfig {
                noise_suppression: suppressor,
                ..ProcessingConfig::OFF
            };
            let mut chain = Chain::new(config, channels).unwrap();
            let mut frames: Vec<Vec<f32>> = input
                .as_chunks::<FRAME>()
                .0
                .iter()
                .map(|frame| {
                    frame
                        .iter()
                        .flat_map(|x| std::iter::repeat_n(*x, usize::from(channels)))
                        .collect()
                })
                .collect();
            let allocations = allocation_counter::measure(|| {
                for frame in &mut frames {
                    chain.process(frame, &[]).unwrap();
                }
            });
            assert_eq!(allocations.count_total, 0, "{suppressor:?}, {channels} ch");
        }
    }
}
