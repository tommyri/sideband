//! Capture-worker VAD contracts and a reproducible speech/keyboard fixture.
use std::time::Duration;

use ss_audio::vad::{Config, Error, FRAME_SAMPLES, GateEvent, Mode, Vad};

const QUIET: [f32; FRAME_SAMPLES] = [0.001; FRAME_SAMPLES];
const SPEECH: [f32; FRAME_SAMPLES] = [0.1; FRAME_SAMPLES];

#[test]
fn attack_preroll_and_hangover_preserve_pcm_and_emit_single_edges() {
    let mut vad = Vad::new(Config::default()).unwrap();
    for value in [0.001, 0.002, 0.003] {
        let out = vad.process(&[value; FRAME_SAMPLES]).unwrap();
        assert!(!out.open);
        assert_eq!(out.audio().len(), 0);
    }
    let out = vad.process(&SPEECH).unwrap();
    assert_eq!(out.event, Some(GateEvent::Open));
    let frames: Vec<_> = out.audio().collect();
    assert_eq!(
        frames,
        [
            &[0.002; FRAME_SAMPLES][..],
            &[0.003; FRAME_SAMPLES],
            &SPEECH
        ]
    );
    assert_eq!(vad.process(&SPEECH).unwrap().event, None);
    for tick in 1..=30 {
        let out = vad.process(&QUIET).unwrap();
        assert_eq!(out.audio().next().unwrap(), QUIET);
        assert_eq!(out.open, tick < 30);
        assert_eq!(out.event, (tick == 30).then_some(GateEvent::Close));
    }
    // Reopening immediately must not duplicate previously transmitted tail PCM.
    let out = vad.process(&SPEECH).unwrap();
    assert_eq!(out.event, Some(GateEvent::Open));
    assert_eq!(out.audio().len(), 1);
    assert_eq!(vad.force_close(), Some(GateEvent::Close));
    assert_eq!(vad.force_close(), None);
    assert_eq!(vad.process(&SPEECH).unwrap().audio().len(), 1);
}

#[test]
fn zero_and_fractional_hangover_and_resumed_speech() {
    for (duration, tail) in [(Duration::ZERO, 0), (Duration::from_millis(11), 2)] {
        let mut vad = Vad::new(Config {
            hangover: duration,
            ..Config::default()
        })
        .unwrap();
        vad.process(&SPEECH).unwrap();
        for tick in 1..=tail.max(1) {
            let out = vad.process(&QUIET).unwrap();
            assert_eq!(out.audio().len(), usize::from(tail > 0));
            assert_eq!(out.event, (tick == tail.max(1)).then_some(GateEvent::Close));
        }
    }
    let mut vad = Vad::new(Config::default()).unwrap();
    vad.process(&SPEECH).unwrap();
    for _ in 0..29 {
        assert!(vad.process(&QUIET).unwrap().open);
    }
    assert_eq!(vad.process(&SPEECH).unwrap().event, None);
    for _ in 0..29 {
        assert!(vad.process(&QUIET).unwrap().open);
    }
    assert_eq!(vad.process(&QUIET).unwrap().event, Some(GateEvent::Close));
}

#[test]
fn stereo_antiphase_and_one_sided_audio_do_not_cancel() {
    for right in [-0.1, 0.0] {
        let mut vad = Vad::new(Config {
            channels: 2,
            ..Config::default()
        })
        .unwrap();
        let mut frame = [0.0; FRAME_SAMPLES * 2];
        for pair in frame.as_chunks_mut::<2>().0 {
            pair.copy_from_slice(&[0.1, right]);
        }
        assert_eq!(vad.process(&frame).unwrap().event, Some(GateEvent::Open));
        let out = vad.process(&frame).unwrap();
        assert_eq!(out.audio().next().unwrap(), frame);
        let meter = out.meter.unwrap();
        assert!((meter.rms_dbfs + 20.0).abs() < 0.001);
        assert!((meter.peak_dbfs + 20.0).abs() < 0.001);
        assert!(meter.probability.is_none());
    }
}

#[test]
fn meter_runs_at_fifty_hz_even_while_closed() {
    let mut vad = Vad::new(Config::default()).unwrap();
    for tick in 1..=100 {
        let out = vad.process(&QUIET).unwrap();
        assert_eq!(out.meter.is_some(), tick % 2 == 0);
        if let Some(meter) = out.meter {
            assert!((meter.rms_dbfs + 60.0).abs() < 0.001);
        }
    }
}

#[test]
fn calibration_uses_two_seconds_and_rejects_brief_impulses() {
    let mut vad = Vad::new(Config::default()).unwrap();
    vad.process(&SPEECH).unwrap();
    assert_eq!(vad.start_calibration(), Some(GateEvent::Close));
    for tick in 0..200 {
        assert_eq!(vad.calibration_remaining(), Some(200 - tick));
        let out = vad
            .process(if tick % 10 == 0 { &SPEECH } else { &QUIET })
            .unwrap();
        assert!(!out.open);
        assert_eq!(out.audio().len(), 0);
        assert_eq!(out.event, None);
        assert_eq!(out.calibration.is_some(), tick == 199);
        if let Some(calibration) = out.calibration {
            assert!((calibration.noise_floor_dbfs + 60.0).abs() < 0.001);
            assert!((calibration.threshold_dbfs + 48.0).abs() < 0.001);
        }
    }
    assert_eq!(vad.calibration_remaining(), None);
    assert!((vad.config().threshold_dbfs + 48.0).abs() < 0.001);
    assert_eq!(vad.process(&SPEECH).unwrap().audio().len(), 1);
    vad.start_calibration();
    vad.force_close();
    assert_eq!(vad.calibration_remaining(), None);
}

#[test]
fn silence_calibration_and_validation_are_bounded() {
    let mut vad = Vad::new(Config::default()).unwrap();
    vad.start_calibration();
    for _ in 0..200 {
        vad.process(&[0.0; FRAME_SAMPLES]).unwrap();
    }
    assert_eq!(vad.config().threshold_dbfs, -96.0);
    assert!(!vad.process(&[0.0; FRAME_SAMPLES]).unwrap().open);
    for config in [
        Config {
            channels: 0,
            ..Config::default()
        },
        Config {
            channels: 3,
            ..Config::default()
        },
        Config {
            threshold_dbfs: f32::NAN,
            ..Config::default()
        },
        Config {
            threshold_dbfs: 1.0,
            ..Config::default()
        },
        Config {
            probability_threshold: f32::INFINITY,
            ..Config::default()
        },
        Config {
            probability_threshold: -0.1,
            ..Config::default()
        },
        Config {
            hangover: Duration::MAX,
            ..Config::default()
        },
    ] {
        assert!(Vad::new(config).is_err());
    }
    assert_eq!(vad.set_threshold(-97.0), Err(Error::Threshold));
    assert_eq!(vad.config().threshold_dbfs, -96.0);
}

#[test]
fn malformed_frames_leave_history_meter_and_calibration_unchanged() {
    let mut vad = Vad::new(Config::default()).unwrap();
    vad.start_calibration();
    vad.process(&QUIET).unwrap();
    assert!(matches!(vad.process(&[]), Err(Error::FrameLength)));
    for value in [f32::NAN, f32::INFINITY, -1.001, 1.001] {
        assert!(matches!(
            vad.process(&[value; FRAME_SAMPLES]),
            Err(Error::Samples)
        ));
    }
    assert_eq!(vad.calibration_remaining(), Some(199));
    assert!(vad.process(&QUIET).unwrap().meter.is_some());
    vad.force_close();
    vad.set_threshold(-10.0).unwrap();
    assert!(!vad.process(&SPEECH).unwrap().open);
    vad.set_threshold(-30.0).unwrap();
    assert_eq!(vad.process(&SPEECH).unwrap().event, Some(GateEvent::Open));
}

fn fixture() -> Vec<f32> {
    // Generated canonical mono PCM16 WAV; fail if the fixture format changes.
    let wav = include_bytes!("fixtures/vad/speech-keyboard.wav");
    assert_eq!(&wav[..4], b"RIFF");
    assert_eq!(&wav[8..16], b"WAVEfmt ");
    assert_eq!(u16::from_le_bytes(wav[20..22].try_into().unwrap()), 1);
    assert_eq!(u16::from_le_bytes(wav[22..24].try_into().unwrap()), 1);
    assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 48_000);
    assert_eq!(u16::from_le_bytes(wav[34..36].try_into().unwrap()), 16);
    assert_eq!(&wav[36..40], b"data");
    wav[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
        .collect()
}

#[test]
fn keyboard_fixture_is_closed_and_energy_speech_attack_is_under_twenty_ms() {
    let samples = fixture();
    let mut vad = Vad::new(Config::default()).unwrap();
    let mut first_open = None;
    for (tick, frame) in samples.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
        let out = vad.process(frame).unwrap();
        if out.event == Some(GateEvent::Open) {
            first_open.get_or_insert(tick);
        }
        if !(200..430).contains(&tick) {
            assert!(!out.open, "keyboard frame {tick}");
        }
    }
    assert!(
        matches!(first_open, Some(200 | 201)),
        "first open: {first_open:?}"
    );
}

#[test]
fn energy_processing_and_calibration_do_not_allocate() {
    assert_no_allocations(Mode::Energy);
}

fn assert_no_allocations(mode: Mode) {
    for channels in [1, 2] {
        let mut vad = Vad::new(Config {
            channels,
            mode,
            ..Config::default()
        })
        .unwrap();
        let samples = fixture();
        let mut interleaved = [0.0; FRAME_SAMPLES * 2];
        let allocations = allocation_counter::measure(|| {
            // Includes the first non-silent frame, gate edges, and median sort.
            for frame in samples.as_chunks::<FRAME_SAMPLES>().0 {
                for (pair, value) in interleaved
                    .chunks_exact_mut(usize::from(channels))
                    .zip(frame)
                {
                    pair.fill(*value);
                }
                let output = vad
                    .process(&interleaved[..FRAME_SAMPLES * usize::from(channels)])
                    .unwrap();
                for frame in output.audio() {
                    std::hint::black_box(frame);
                }
            }
            vad.start_calibration();
            for _ in 0..200 {
                vad.process(&interleaved[..FRAME_SAMPLES * usize::from(channels)])
                    .unwrap();
            }
            vad.force_close();
        });
        assert_eq!(allocations.count_total, 0);
    }
}

#[cfg(feature = "rnnoise")]
#[test]
fn neural_processing_and_calibration_do_not_allocate() {
    assert_no_allocations(Mode::Rnnoise);
}

#[cfg(feature = "rnnoise")]
#[test]
fn neural_mode_rejects_more_loud_keyboard_without_losing_speech() {
    let samples = fixture();
    let mut metrics = Vec::new();
    for mode in [Mode::Energy, Mode::Rnnoise] {
        let mut vad = Vad::new(Config {
            mode,
            ..Config::default()
        })
        .unwrap();
        let mut false_active = 0;
        let mut speech_active = 0;
        let mut first_speech = None;
        for (tick, frame) in samples.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
            // Same fixture/config for both modes, with a louder input gain.
            let loud: [f32; FRAME_SAMPLES] = std::array::from_fn(|i| frame[i] * 4.0);
            let out = vad.process(&loud).unwrap();
            if out.open && (!(200..430).contains(&tick)) {
                false_active += 1;
            }
            if out.open && (200..400).contains(&tick) {
                speech_active += 1;
                first_speech.get_or_insert(tick);
            }
            if mode == Mode::Rnnoise
                && let Some(meter) = out.meter
            {
                assert!((0.0..=1.0).contains(&meter.probability.unwrap()));
            }
        }
        eprintln!(
            "{mode:?}: false-active={false_active}, speech-active={speech_active}, first speech={first_speech:?}"
        );
        metrics.push((false_active, speech_active));
    }
    assert!(metrics[1].0 < metrics[0].0);
    assert!(
        metrics[1].1 >= 180,
        "neural detection must retain at least 90% of speech quanta"
    );
}

#[cfg(feature = "rnnoise")]
#[test]
fn neural_preroll_recovers_onset_without_denoising_transmitted_pcm() {
    let samples: Vec<_> = fixture().into_iter().map(|s| s * 4.0).collect();
    let mut vad = Vad::new(Config {
        mode: Mode::Rnnoise,
        ..Config::default()
    })
    .unwrap();
    let mut saw_open = false;
    for (tick, frame) in samples.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
        let out = vad.process(frame).unwrap();
        if out.event == Some(GateEvent::Open) && !saw_open {
            // This recording's first two speech quanta must be in the pre-roll.
            assert!((200..=202).contains(&tick));
            assert_eq!(out.audio().len(), 3);
            saw_open = true;
        }
        let first = tick + 1 - out.audio().len();
        for (offset, emitted) in out.audio().enumerate() {
            let start = (first + offset) * FRAME_SAMPLES;
            assert_eq!(emitted, &samples[start..start + FRAME_SAMPLES]);
        }
    }
    assert!(saw_open);
}
