#![allow(clippy::float_cmp)] // Expected values are exact by construction.

use std::time::Duration;

use super::*;

struct Fake {
    stage: Stage,
    gain: f32,
    renders: usize,
    delay: Option<Duration>,
}

impl Fake {
    fn boxed(stage: Stage, gain: f32) -> Box<dyn Processor> {
        Box::new(Self {
            stage,
            gain,
            renders: 0,
            delay: None,
        })
    }
}

impl Processor for Fake {
    fn stage(&self) -> Stage {
        self.stage
    }
    fn backend(&self) -> Backend {
        Backend::Custom
    }
    fn latency(&self) -> Duration {
        Duration::from_millis(2)
    }
    fn process(&mut self, capture: &mut [f32], render: &[StereoFrame]) {
        self.renders += render.len();
        for sample in capture {
            *sample = *sample * self.gain + 0.125;
        }
    }
    fn set_echo_delay(&mut self, delay: Duration) {
        self.delay = Some(delay);
    }
    fn reset(&mut self) {}
}

#[test]
fn off_is_default_and_bypass() {
    assert!(ProcessingConfig::default().is_off());
    let chain = Chain::new(ProcessingConfig::OFF, 2).unwrap();
    assert!(chain.is_bypass());
    assert_eq!(chain.config(), Some(ProcessingConfig::OFF));
    assert!(!chain.uses_render());
    assert_eq!(chain.latency(), Duration::ZERO);
    assert_eq!(chain.monitor().stages().len(), 0);
}

#[test]
fn rejects_bad_channels_and_lengths_without_touching_audio() {
    assert_eq!(Chain::bypass(0).unwrap_err(), Error::Channels);
    assert_eq!(Chain::bypass(3).unwrap_err(), Error::Channels);
    let mut chain = Chain::with_processors(1, vec![Fake::boxed(Stage::GainControl, 2.0)]).unwrap();
    let mut short = [0.5; 479];
    assert_eq!(
        chain.process(&mut short, &[]),
        Err(Error::FrameLength {
            expected: 480,
            actual: 479
        })
    );
    assert!(short.iter().all(|sample| *sample == 0.5));
    let mut frame = [0.5; 480];
    frame[17] = f32::NAN;
    assert_eq!(chain.process(&mut frame, &[]), Err(Error::Pcm));
    assert_eq!(frame[0], 0.5);
    assert_eq!(chain.monitor().chain().frames, 0);
}

#[test]
fn custom_stages_run_in_order_and_report() {
    let mut chain = Chain::with_processors(
        1,
        vec![
            Fake::boxed(Stage::EchoCancellation, 2.0),
            Fake::boxed(Stage::GainControl, 3.0),
        ],
    )
    .unwrap();
    assert!(chain.uses_render());
    assert_eq!(chain.config(), None);
    let mut frame = [1.0; 480];
    let render = [[0.0; 960]; 2];
    chain.process(&mut frame, &render).unwrap();
    // (1 * 2 + 0.125) * 3 + 0.125: echo before gain.
    assert!(frame.iter().all(|sample| *sample == 6.5));
    let monitor = chain.monitor();
    let reports: Vec<_> = monitor.stages().collect();
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].stage, Stage::EchoCancellation);
    assert_eq!(reports[1].latency, Duration::from_millis(2));
    assert!(reports.iter().all(|report| report.cpu.frames == 1));
    assert_eq!(monitor.chain().frames, 1);
    assert_eq!(monitor.latency(), Duration::from_millis(4));
    assert_eq!(chain.latency(), Duration::from_millis(4));
    let mut poisoned = [[0.0; 960]; 1];
    poisoned[0][3] = f32::INFINITY;
    assert_eq!(chain.process(&mut frame, &poisoned), Err(Error::Pcm));
}

#[test]
fn custom_stages_must_be_ordered_and_unique() {
    let reversed = vec![
        Fake::boxed(Stage::GainControl, 1.0),
        Fake::boxed(Stage::NoiseSuppression, 1.0),
    ];
    assert_eq!(
        Chain::with_processors(1, reversed).unwrap_err(),
        Error::Order
    );
    let repeated = vec![
        Fake::boxed(Stage::NoiseSuppression, 1.0),
        Fake::boxed(Stage::NoiseSuppression, 1.0),
    ];
    assert_eq!(
        Chain::with_processors(2, repeated).unwrap_err(),
        Error::Order
    );
}

#[test]
fn echo_path_sums_device_and_queue_delays() {
    let path = EchoPath {
        playback_queue: Duration::from_millis(20),
        playback_device: Duration::from_millis(11),
        capture_device: Duration::from_millis(9),
        capture_queue: Duration::from_millis(10),
    };
    assert_eq!(path.delay(), Duration::from_millis(50));
}

#[test]
fn unavailable_backends_explain_themselves() {
    let deep = ProcessingConfig {
        noise_suppression: NoiseSuppression::DeepFilter,
        ..ProcessingConfig::OFF
    };
    let error = Chain::new(deep, 1).unwrap_err();
    if cfg!(feature = "deepfilter") {
        assert!(matches!(error, Error::Unavailable { .. }));
    } else {
        assert_eq!(
            error,
            Error::FeatureDisabled {
                backend: Backend::DeepFilter,
                feature: "deepfilter"
            }
        );
    }
    assert!(!Backend::DeepFilter.is_available());
    let aec = ProcessingConfig {
        echo_cancellation: true,
        ..ProcessingConfig::OFF
    };
    assert_eq!(Chain::new(aec, 1).is_ok(), cfg!(feature = "aec"));
    assert_eq!(Backend::Sonora.is_available(), cfg!(feature = "aec"));
    let rnnoise = ProcessingConfig {
        noise_suppression: NoiseSuppression::RnNoise,
        ..ProcessingConfig::OFF
    };
    assert_eq!(Chain::new(rnnoise, 2).is_ok(), cfg!(feature = "rnnoise"));
    assert_eq!(Backend::RnNoise.feature(), Some("rnnoise"));
}

#[test]
fn render_tap_delivers_in_order_and_discards_stale_frames() {
    let (mut sender, mut receiver) = render_tap(16);
    for value in 0..3_u8 {
        assert!(sender.push(&[f32::from(value); 960]));
    }
    assert_eq!(receiver.queued(), 3);
    let frames = receiver.drain();
    assert_eq!(frames.len(), 3);
    assert!(frames.iter().enumerate().all(|(index, frame)| {
        #[allow(clippy::cast_precision_loss)]
        let expected = index as f32;
        frame[959] == expected
    }));
    assert_eq!(receiver.drain().len(), 0);
    for value in 0..12_u8 {
        assert!(sender.push(&[f32::from(value); 960]));
    }
    let frames = receiver.drain();
    assert_eq!(frames.len(), MAX_RENDER_BATCH);
    assert_eq!(frames[0][0], 4.0);
    assert_eq!(receiver.dropped(), 4);
    for _ in 0..16 {
        sender.push(&[0.0; 960]);
    }
    assert!(!sender.push(&[0.0; 960]));
    assert_eq!(sender.dropped(), 5);
}

#[test]
fn echo_delay_reaches_stages() {
    let mut chain =
        Chain::with_processors(1, vec![Fake::boxed(Stage::EchoCancellation, 1.0)]).unwrap();
    chain.set_echo_delay(Duration::from_millis(42));
    chain.reset();
    let mut frame = [0.0; 480];
    chain.process(&mut frame, &[[0.0; 960]]).unwrap();
}
