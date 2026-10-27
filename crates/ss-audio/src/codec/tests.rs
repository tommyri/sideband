#![allow(clippy::cast_precision_loss)] // Signal indices/durations are bounded in tests.

use super::*;
use ss_core::VoiceProfile;

fn sine(frame: usize, samples: usize, frequency: f32) -> Vec<f32> {
    (0..samples)
        .map(|i| {
            0.251_188_64
                * (std::f32::consts::TAU * frequency * (frame * samples + i) as f32 / 48_000.0)
                    .sin()
        })
        .collect()
}

#[test]
fn voice_roundtrip_snr_after_warmup() {
    let mut encoder = Encoder::new(VoiceProfile::Voice).unwrap();
    let mut decoder = Decoder::new(1).unwrap();
    let delay = encoder.lookahead_samples().unwrap() as usize;
    let input = sine(0, 48_000, 1000.0);
    let mut output = Vec::new();
    for frame in input.as_chunks::<960>().0 {
        let packet = encoder.encode(frame).unwrap();
        output.extend_from_slice(decoder.decode(Some(packet), false).unwrap());
    }
    let (mut signal, mut noise) = (0.0f64, 0.0f64);
    // Opus changes SILK/CELT modes during the tone. Their filters have different
    // phase responses in addition to the reported lookahead. Estimate phase per
    // 20 ms window, but keep the original amplitude: gain error, harmonics and
    // other distortion still count as noise. This is tone SNR, not waveform SNR.
    // Limit the correction and frame-to-frame change to four samples at 48 kHz
    // (0.083 ms). That covers the pinned codec's SILK/CELT filter response while
    // rejecting polarity inversions, arbitrary jumps, or accumulating drift.
    let phase_bound = std::f64::consts::TAU * 1000.0 * 4.0 / 48_000.0;
    let mut previous_phase: Option<f64> = None;
    for frame in output[4800 + delay..].as_chunks::<960>().0 {
        let (mut sin_projection, mut cos_projection) = (0.0, 0.0);
        for (i, sample) in frame.iter().enumerate() {
            let angle = std::f64::consts::TAU * 1000.0 * i as f64 / 48_000.0;
            sin_projection += f64::from(*sample) * angle.sin();
            cos_projection += f64::from(*sample) * angle.cos();
        }
        let phase = cos_projection.atan2(sin_projection);
        assert!(
            phase.abs() <= phase_bound,
            "excessive phase correction: {phase} rad"
        );
        if let Some(previous) = previous_phase {
            assert!(
                (phase - previous).abs() <= phase_bound,
                "phase discontinuity: {previous} -> {phase} rad"
            );
        }
        previous_phase = Some(phase);
        for (i, sample) in frame.iter().enumerate() {
            let angle = std::f64::consts::TAU * 1000.0 * i as f64 / 48_000.0;
            let reference = 10.0f64.powf(-12.0 / 20.0) * (angle + phase).sin();
            signal += reference.powi(2);
            noise += (reference - f64::from(*sample)).powi(2);
        }
    }
    let snr = 10.0 * (signal / noise).log10();
    let mut waveform_signal = 0.0f64;
    let mut waveform_error = 0.0f64;
    for i in 4800..input.len() - delay {
        waveform_signal += f64::from(input[i]).powi(2);
        waveform_error += f64::from(input[i] - output[i + delay]).powi(2);
    }
    let waveform_snr = 10.0 * (waveform_signal / waveform_error).log10();
    eprintln!(
        "voice tone SNR with bounded phase alignment: {snr:.2} dB; waveform SNR: {waveform_snr:.2} dB"
    );
    assert!(snr >= 20.0, "SNR {snr:.2} dB, delay {delay}");
}

#[test]
fn music_preserves_stereo_separation() {
    let mut encoder = Encoder::new(VoiceProfile::Music).unwrap();
    let mut decoder = Decoder::new(2).unwrap();
    // Exercise each channel independently to detect swaps, collapse, or leakage.
    for active in 0..2 {
        let (mut wanted, mut leaked) = (0.0f64, 0.0f64);
        for frame in 0..50 {
            let tone = sine(frame, 960, 1000.0);
            let mut pcm = vec![0.0; 1920];
            for (i, sample) in tone.into_iter().enumerate() {
                pcm[2 * i + active] = sample;
            }
            let packet = encoder.encode(&pcm).unwrap();
            let output = decoder.decode(Some(packet), false).unwrap();
            if frame >= 5 {
                for pair in output.as_chunks::<2>().0 {
                    wanted += f64::from(pair[active]).powi(2);
                    leaked += f64::from(pair[1 - active]).powi(2);
                }
            }
        }
        assert!(wanted > 100.0);
        assert!(leaked < wanted * 0.001, "wanted {wanted}, leaked {leaked}");
    }
}

#[test]
fn every_tenth_packet_lost_recovers_without_silent_frames() {
    let mut encoder = Encoder::new(VoiceProfile::Voice).unwrap();
    let mut decoder = Decoder::new(1).unwrap();
    let mut frames_decoded = 0;
    for frame in 0..101 {
        let pcm = sine(frame, 960, 1000.0);
        let packet = encoder.encode(&pcm).unwrap();
        if frame % 10 == 9 {
            continue;
        }
        if frame > 0 && frame % 10 == 0 {
            let recovered = decoder.decode(Some(packet), true).unwrap();
            assert_eq!(recovered.len(), 960);
            assert!(recovered.iter().map(|x| x * x).sum::<f32>() > 0.01);
            frames_decoded += 1;
        }
        let audio = decoder.decode(Some(packet), false).unwrap();
        assert_eq!(audio.len(), 960);
        if frame > 5 {
            assert!(audio.iter().map(|x| x * x).sum::<f32>() > 0.01);
        }
        frames_decoded += 1;
    }
    assert_eq!(frames_decoded, 101);
}

#[test]
fn profiles_metadata_plc_and_allocation_free_streaming() {
    for profile in [
        VoiceProfile::Voice,
        VoiceProfile::Music,
        VoiceProfile::LowLatency,
    ] {
        let mut encoder = Encoder::new(profile).unwrap();
        let mut decoder = Decoder::for_profile(profile).unwrap();
        let params = profile.encoder_params();
        let pcm = sine(0, encoder.input_samples(), 1000.0);
        assert_eq!(decoder.decode(None, false).unwrap().len(), pcm.len());
        let packet = encoder.encode(&pcm).unwrap();
        let info = OpusPacket::toc(packet).unwrap();
        assert_eq!(info.duration(), params.frame_size);
        assert!(info.channels() <= params.channels);
        assert_eq!(
            info.samples_per_channel() * usize::from(params.channels),
            pcm.len()
        );
        assert!(info.frames() > 0);
        decoder.decode(Some(packet), false).unwrap();
        let allocations = allocation_counter::measure(|| {
            for _ in 0..10 {
                let packet = encoder.encode(&pcm).unwrap();
                decoder.decode(Some(packet), true).unwrap();
                decoder.decode(Some(packet), false).unwrap();
                decoder.decode(None, false).unwrap();
            }
        });
        assert_eq!(allocations.count_total, 0);
    }
}

#[test]
fn rejects_invalid_inputs_without_changing_stream_configuration() {
    assert!(matches!(Decoder::new(0), Err(Error::Configuration)));
    assert!(matches!(Decoder::new(3), Err(Error::Configuration)));
    assert!(matches!(
        Decoder::with_frame_duration(1, std::time::Duration::from_millis(15)),
        Err(Error::Configuration)
    ));
    let mut encoder = Encoder::new(VoiceProfile::LowLatency).unwrap();
    assert_eq!(encoder.encode(&[0.0; 960]), Err(Error::Pcm));
    assert_eq!(encoder.encode(&[f32::NAN; 480]), Err(Error::Pcm));
    assert_eq!(encoder.encode(&[f32::INFINITY; 480]), Err(Error::Pcm));
    let mut decoder = Decoder::new(1).unwrap();
    assert_eq!(decoder.decode(Some(&[]), false), Err(Error::Packet));
    assert_eq!(
        decoder.decode(Some(&[0; MAX_PACKET_BYTES + 1]), false),
        Err(Error::Packet)
    );
    let packet = encoder.encode(&[0.0; 480]).unwrap();
    assert_eq!(decoder.decode(Some(packet), false), Err(Error::Duration));
    assert_eq!(decoder.decode(None, false).unwrap().len(), 960);
    // Code 3 requires a frame count byte; code 2 requires frame length data.
    for packet in [&[3][..], &[2][..], &[3, 0][..], &[3, 63][..]] {
        assert!(OpusPacket::toc(packet).is_err());
    }
}

proptest::proptest! {
    #[test]
    fn arbitrary_packets_do_not_panic(packet in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..1200)) {
        if let Ok(info) = OpusPacket::toc(&packet) {
            proptest::prop_assert!((1..=48).contains(&info.frames()));
            proptest::prop_assert!((120..=5760).contains(&info.samples_per_channel()));
            proptest::prop_assert!((1..=2).contains(&info.channels()));
        }
        let mut decoder = Decoder::new(1).unwrap();
        if let Ok(pcm) = decoder.decode(Some(&packet), false) {
            proptest::prop_assert_eq!(pcm.len(), 960);
            proptest::prop_assert!(pcm.iter().all(|x| x.is_finite()));
        }
    }
}

#[cfg(feature = "dred")]
#[test]
fn dred_recovers_burst_and_reuses_allocations() {
    for profile in [
        VoiceProfile::Voice,
        VoiceProfile::Music,
        VoiceProfile::LowLatency,
    ] {
        for lost_frames in [3, 10] {
            check_dred_burst(profile, lost_frames);
        }
    }
}

#[cfg(feature = "dred")]
fn check_dred_burst(profile: VoiceProfile, lost_frames: usize) {
    let mut encoder = Encoder::new(profile).unwrap();
    let mut decoder = Decoder::for_profile(profile).unwrap();
    let channels = usize::from(profile.encoder_params().channels);
    let frame_samples = encoder.input_samples() / channels;
    encoder.set_dred_duration_ms(1000).unwrap();
    // libopus budgets redundancy from the loss estimate, even with DRED enabled.
    encoder.set_packet_loss(30).unwrap();
    assert_eq!(encoder.set_dred_duration_ms(1001), Err(Error::DredDuration));
    assert_eq!(encoder.set_dred_duration_ms(15), Err(Error::DredDuration));
    let mut last = Vec::new();
    for frame in 0..100 {
        let mono = sine(frame, frame_samples, 200.0);
        let pcm: Vec<_> = mono
            .into_iter()
            .flat_map(|s| std::iter::repeat_n(s, channels))
            .collect();
        let packet = encoder.encode(&pcm).unwrap();
        if frame < 99 - lost_frames {
            decoder.decode(Some(packet), false).unwrap();
        }
        if frame == 99 {
            last.extend_from_slice(packet);
        }
    }
    assert_eq!(decoder.decode_dred(&last, 0), Err(Error::DredDuration));
    assert_eq!(
        decoder.decode_dred(&last, 48_000 / frame_samples + 1),
        Err(Error::DredDuration)
    );
    assert_eq!(
        decoder.decode_dred(&last, usize::MAX),
        Err(Error::DredDuration)
    );
    let allocations = allocation_counter::measure(|| {
        let recovered = decoder.decode_dred(&last, lost_frames).unwrap();
        assert_eq!(recovered.len(), lost_frames * frame_samples * channels);
        assert!(recovered.iter().all(|x| x.is_finite()));
        for frame in recovered.chunks_exact(frame_samples * channels) {
            assert!(frame.iter().map(|x| x * x).sum::<f32>() > 0.001);
        }
        decoder.decode(Some(&last), false).unwrap();
    });
    assert_eq!(allocations.count_total, 0);
    encoder.set_dred_duration_ms(0).unwrap();
    let pcm = sine(100, encoder.input_samples(), 200.0);
    let packet = encoder.encode(&pcm).unwrap();
    assert_eq!(decoder.decode_dred(packet, 1), Err(Error::DredUnavailable));
}
