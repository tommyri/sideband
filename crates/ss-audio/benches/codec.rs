use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use ss_audio::codec::{Decoder, Encoder};
use ss_core::VoiceProfile;

fn codec(criterion: &mut Criterion) {
    for profile in [
        VoiceProfile::Voice,
        VoiceProfile::Music,
        VoiceProfile::LowLatency,
    ] {
        let mut encoder = Encoder::new(profile).unwrap();
        let mut decoder = Decoder::for_profile(profile).unwrap();
        let channels = usize::from(profile.encoder_params().channels);
        // Independent stereo tones prevent the music benchmark collapsing to mono.
        #[allow(clippy::cast_precision_loss)] // Buffer indices are below 1920.
        let pcm: Vec<f32> = (0..encoder.input_samples())
            .map(|i| {
                0.251_188_64
                    * (std::f32::consts::TAU
                        * (1000.0 + 700.0 * (i % channels) as f32)
                        * (i / channels) as f32
                        / 48_000.0)
                        .sin()
            })
            .collect();
        let mut frame = || {
            let packet = encoder.encode(black_box(&pcm)).unwrap();
            black_box(decoder.decode(Some(packet), false).unwrap());
        };
        for _ in 0..100 {
            frame();
        }
        let start = Instant::now();
        for _ in 0..1000 {
            frame();
        }
        let mean = start.elapsed() / 1000;
        assert!(
            mean < Duration::from_millis(1),
            "{profile}: encode+decode mean {mean:?} exceeds 1 ms"
        );
        criterion.bench_function(profile.as_str(), |b| b.iter(&mut frame));
    }
}

criterion_group! { name = benches; config = Criterion::default().sample_size(30).warm_up_time(Duration::from_secs(1)).measurement_time(Duration::from_secs(2)); targets = codec }
criterion_main!(benches);
