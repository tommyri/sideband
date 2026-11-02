#![allow(clippy::cast_precision_loss)] // Synthetic signal indices stay below 48000.

use ss_audio::{
    codec::Encoder,
    jitter::{Admission, JitterBuffer, PacketHeader, Playout},
    mixer::{Mixer, StereoFrame},
    receive::TrackReceiver,
};
use ss_core::VoiceProfile;
use std::time::Duration;

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}
fn header(seq: u16) -> PacketHeader {
    PacketHeader {
        seq,
        timestamp: u32::from(seq) * 960,
        talk_start: false,
        talk_end: false,
    }
}
fn packet() -> Vec<u8> {
    Encoder::new(VoiceProfile::Voice)
        .unwrap()
        .encode(&[0.1; 960])
        .unwrap()
        .to_vec()
}
fn released(playout: Playout) -> u16 {
    match playout {
        Playout::Packet { packet, .. } => packet.header.seq,
        _ => panic!("expected packet"),
    }
}

#[test]
fn orders_packets_across_sequence_and_timestamp_wrap_and_rejects_replays() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    let mut h = header(65534);
    h.timestamp = u32::MAX - 959;
    jitter.push(h, &packet, ms(0)).unwrap();
    h.seq = 0;
    h.timestamp = 960;
    jitter.push(h, &packet, ms(35)).unwrap();
    h.seq = 65535;
    h.timestamp = 0;
    jitter.push(h, &packet, ms(39)).unwrap();
    assert_eq!(
        jitter.push(h, &packet, ms(39)).unwrap(),
        Admission::Duplicate
    );
    assert!(matches!(jitter.pop_for_playout(ms(39)), Playout::Wait));
    assert_eq!(released(jitter.pop_for_playout(ms(40))), 65534);
    assert_eq!(released(jitter.pop_for_playout(ms(60))), 65535);
    assert_eq!(jitter.push(h, &packet, ms(61)).unwrap(), Admission::Late);
    assert_eq!(released(jitter.pop_for_playout(ms(80))), 0);
    assert_eq!(jitter.stats().duplicate, 1);
    assert_eq!(jitter.stats().late, 1);
}

#[test]
fn reordered_first_packet_is_not_declared_late_before_playout() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    jitter.push(header(11), &packet, ms(30)).unwrap();
    assert_eq!(
        jitter.push(header(10), &packet, ms(40)).unwrap(),
        Admission::Queued
    );
    assert_eq!(released(jitter.pop_for_playout(ms(50))), 10);
    assert_eq!(released(jitter.pop_for_playout(ms(70))), 11);
}

#[test]
fn fec_looks_ahead_without_consuming_the_following_packet() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    jitter.push(header(0), &packet, ms(0)).unwrap();
    jitter.push(header(2), &packet, ms(40)).unwrap();
    assert_eq!(released(jitter.pop_for_playout(ms(40))), 0);
    match jitter.pop_for_playout(ms(60)) {
        Playout::Loss {
            following: Some(packet),
            missing: 1,
            expanded: false,
        } => assert_eq!(packet.header.seq, 2),
        _ => panic!("expected FEC opportunity"),
    }
    assert_eq!(
        jitter.push(header(1), &packet, ms(61)).unwrap(),
        Admission::Late
    );
    assert_eq!(released(jitter.pop_for_playout(ms(80))), 2);
    assert_eq!(jitter.stats().lost, 1);
}

#[test]
fn queued_bursts_resume_after_end_and_do_not_bridge_silence_with_losses() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    let mut first = header(0);
    first.talk_start = true;
    first.talk_end = true;
    let mut next = header(1);
    next.timestamp = 960;
    next.talk_start = true;
    jitter.push(first, &packet, ms(0)).unwrap();
    jitter.push(next, &packet, ms(20)).unwrap();
    assert_eq!(released(jitter.pop_for_playout(ms(40))), 0);
    assert!(matches!(jitter.pop_for_playout(ms(50)), Playout::Wait));
    assert_eq!(released(jitter.pop_for_playout(ms(60))), 1);
    assert_eq!(jitter.stats().lost, 0);
}

#[test]
fn adaptation_waits_for_silence_and_bounds_delay() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    for seq in 0..3 {
        jitter
            .push(header(seq), &packet, ms(u64::from(seq) * 20))
            .unwrap();
    }
    assert_eq!(jitter.target_delay(), ms(20));
    assert_eq!(released(jitter.pop_for_playout(ms(40))), 0);
    assert_eq!(jitter.applied_delay(), ms(40)); // Active audio is not shortened.
    jitter.set_decoded_silence(true);
    jitter.push(header(3), &packet, ms(60)).unwrap();
    match jitter.pop_for_playout(ms(60)) {
        Playout::Packet {
            packet,
            discarded: Some(discarded),
        } => {
            assert_eq!(discarded.header.seq, 1);
            assert_eq!(packet.header.seq, 2);
        }
        _ => panic!("expected silence contraction"),
    }
    assert_eq!(jitter.applied_delay(), ms(20));
    assert_eq!(jitter.stats().contracted, 1);
    jitter.set_decoded_silence(false);
    assert_eq!(released(jitter.pop_for_playout(ms(80))), 3);
    for now in (100..200).step_by(20) {
        assert!(matches!(
            jitter.pop_for_playout(ms(now)),
            Playout::Loss {
                expanded: false,
                ..
            }
        ));
    }
    // A late packet still informs timing, without replaying its committed slot.
    assert_eq!(
        jitter.push(header(4), &packet, ms(200)).unwrap(),
        Admission::Late
    );
    assert!(jitter.target_delay() > ms(20));
    assert!(matches!(
        jitter.pop_for_playout(ms(200)),
        Playout::Loss {
            expanded: false,
            ..
        }
    ));
    assert_eq!(jitter.applied_delay(), ms(20));
    let losses = jitter.stats().lost;
    jitter.set_decoded_silence(true);
    assert!(matches!(
        jitter.pop_for_playout(ms(220)),
        Playout::Loss { expanded: true, .. }
    ));
    assert_eq!(jitter.stats().lost, losses);
    assert_eq!(jitter.applied_delay(), ms(40));
    assert!(jitter.target_delay() <= ms(200));
}

#[test]
fn stale_jitter_observations_expire_and_queue_is_bounded() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    jitter.push(header(0), &packet, ms(0)).unwrap();
    jitter.push(header(1), &packet, ms(200)).unwrap();
    assert_eq!(jitter.target_delay(), ms(200));
    // A later burst carries a new baseline; old jitter must age out.
    let mut restart = header(2);
    restart.talk_start = true;
    restart.timestamp = 240_000;
    jitter.push(restart, &packet, ms(5000)).unwrap();
    restart.seq = 3;
    restart.timestamp += 960;
    restart.talk_start = false;
    jitter.push(restart, &packet, ms(5020)).unwrap();
    assert_eq!(jitter.target_delay(), ms(20));
    let mut bounded = JitterBuffer::new(VoiceProfile::Voice);
    for seq in 0..64 {
        bounded.push(header(seq), &packet, ms(0)).unwrap();
    }
    assert!(bounded.push(header(64), &packet, ms(0)).is_err());
    assert_eq!(bounded.queued(), 64);
    assert_eq!(bounded.stats().overflow, 1);
}

#[test]
fn speaking_flags_ignore_reordering_and_time_out() {
    let packet = packet();
    let mut track = TrackReceiver::new(VoiceProfile::Voice).unwrap();
    let mut h = header(0);
    h.talk_start = true;
    track.push(h, &packet, ms(0)).unwrap();
    assert!(track.speaking(ms(199)));
    assert!(!track.speaking(ms(200)));
    let mut end = header(2);
    end.talk_end = true;
    track.push(end, &packet, ms(30)).unwrap();
    assert!(!track.speaking(ms(30)));
    track.push(header(1), &packet, ms(35)).unwrap();
    assert!(!track.speaking(ms(35)));
    // A duplicate START cannot revive the speaker after END.
    assert_eq!(
        track.push(h, &packet, ms(40)).unwrap(),
        Admission::Duplicate
    );
    assert!(!track.speaking(ms(40)));
}

#[test]
fn ten_ms_output_handles_every_profile_and_has_no_streaming_allocations() {
    for profile in [
        VoiceProfile::Voice,
        VoiceProfile::Music,
        VoiceProfile::LowLatency,
    ] {
        let mut encoder = Encoder::new(profile).unwrap();
        let mut track = TrackReceiver::new(profile).unwrap();
        let mut mixer = Mixer::new();
        let pcm: Vec<f32> = (0..encoder.input_samples())
            .map(|i| 0.25 * (i as f32 * 0.1).sin())
            .collect();
        let frame_ms = u64::try_from(profile.encoder_params().frame_size.as_millis()).unwrap();
        let mut seq = 0;
        let allocations = allocation_counter::measure(|| {
            for tick in 0..100 {
                let now = ms(tick * 10);
                if (tick * 10).is_multiple_of(frame_ms) {
                    let payload = encoder.encode(&pcm).unwrap();
                    let h = PacketHeader {
                        seq,
                        timestamp: u32::from(seq) * u32::try_from(frame_ms * 48).unwrap(),
                        talk_start: seq == 0,
                        talk_end: false,
                    };
                    track.push(h, payload, now).unwrap();
                    seq += 1;
                }
                let output = track.render(now);
                assert_eq!(output.len(), 960);
                assert!(output.iter().all(|s| s.is_finite()));
                if tick >= 10 {
                    assert!(output.iter().any(|s| s.abs() > 0.001));
                }
                mixer.add_track(output, 1.0).unwrap();
                let output_mix = mixer.finish();
                assert!(output_mix.iter().all(|s| s.abs() <= 1.0));
            }
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(track.stats().jitter.underruns, 0);
        assert_eq!(track.stats().decode_errors, 0);
    }
}

#[test]
fn mixer_sums_gains_injects_sounds_and_limits_linked_stereo_without_clipping() {
    let mut mixer = Mixer::new();
    let mut sine: StereoFrame = [0.0; 960];
    for (i, pair) in sine.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        *pair = [0.251_188_64 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin(); 2];
    }
    for _ in 0..3 {
        mixer.add_track(&sine, 1.0).unwrap();
    }
    assert!(mixer.finish().iter().all(|s| s.abs() < 1.0));
    assert_eq!(mixer.limited_samples(), 0);
    let mut loud = [0.0; 960];
    for pair in loud.as_chunks_mut::<2>().0 {
        *pair = [2.0, 1.0];
    }
    mixer.add_track(&loud, 1.0).unwrap();
    for pair in mixer.finish().as_chunks::<2>().0 {
        assert!(pair[0] <= 1.0 && pair[0] > 0.8);
        assert!((pair[0] - pair[1] * 2.0).abs() < 1e-6);
    }
    assert_eq!(mixer.limited_samples(), 480);
    assert_eq!(mixer.finish(), &[0.0; 960]); // Sum resets each frame.
    mixer.set_master_gain(0.5).unwrap();
    mixer.add_track(&[0.1; 960], 2.0).unwrap();
    mixer.add_ui(&[0.2; 960]).unwrap();
    assert!(mixer.add_track(&[f32::NAN; 960], 1.0).is_err());
    assert!(mixer.set_master_gain(f32::INFINITY).is_err());
    for s in mixer.finish() {
        assert!((*s - 0.2).abs() < 1e-6);
    }
}

#[cfg(feature = "dred")]
#[test]
fn dred_recovers_buffered_burst_without_allocating_or_replaying_late_originals() {
    let mut encoder = Encoder::new(VoiceProfile::Voice).unwrap();
    encoder.set_dred_duration_ms(1000).unwrap();
    encoder.set_packet_loss(30).unwrap();
    let mut receiver = TrackReceiver::new(VoiceProfile::Voice).unwrap();
    let mut packets = Vec::new();
    for seq in 0..100 {
        let pcm: Vec<_> = (0..960)
            .map(|i| {
                0.25 * (std::f32::consts::TAU * 200.0 * (seq * 960 + i) as f32 / 48_000.0).sin()
            })
            .collect();
        packets.push(encoder.encode(&pcm).unwrap().to_vec());
    }
    let allocations = allocation_counter::measure(|| {
        for tick in 0..205u16 {
            let now = ms(u64::from(tick) * 10);
            let seq = tick / 2;
            if tick.is_multiple_of(2) && seq < 100 && !(60..=62).contains(&seq) {
                let mut metadata = header(seq);
                metadata.talk_end = seq == 99;
                receiver
                    .push(metadata, &packets[usize::from(seq)], now)
                    .unwrap();
            }
            // The worker committed DRED for slots 61/62 when packet 63 arrived.
            if tick == 127 {
                assert_eq!(
                    receiver.push(header(62), &packets[62], now).unwrap(),
                    Admission::Late
                );
            }
            let frame = receiver.render(now);
            assert!(frame.iter().all(|s| s.is_finite()));
            if (11..204).contains(&tick) {
                assert!(frame.iter().any(|s| s.abs() > 0.001));
            }
        }
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(receiver.stats().dred, 2);
    assert!(receiver.stats().plc >= 1);
    assert_eq!(receiver.stats().jitter.lost, 3);
    assert_eq!(receiver.stats().jitter.late, 1);
    assert_eq!(receiver.stats().decode_errors, 0);
}

#[test]
fn muted_track_gain_does_not_make_voiced_audio_eligible_for_contraction() {
    let mut encoder = Encoder::new(VoiceProfile::Voice).unwrap();
    let mut muted = TrackReceiver::new(VoiceProfile::Voice).unwrap();
    let mut reference = TrackReceiver::new(VoiceProfile::Voice).unwrap();
    muted.set_gain(0.0).unwrap();
    let pcm: Vec<_> = (0..960)
        .map(|i| 0.25 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin())
        .collect();
    for tick in 0..100u16 {
        let now = ms(u64::from(tick) * 10);
        if tick.is_multiple_of(2) {
            let packet = encoder.encode(&pcm).unwrap();
            muted.push(header(tick / 2), packet, now).unwrap();
            reference.push(header(tick / 2), packet, now).unwrap();
        }
        if tick == 50 {
            muted.set_gain(2.0).unwrap();
        }
        let audio = muted.render(now);
        let original = reference.render(now);
        let gain = if tick < 50 { 0.0 } else { 2.0 };
        for (sample, expected) in audio.iter().zip(original) {
            assert!((*sample - *expected * gain).abs() < 1e-6);
        }
        for pair in audio.as_chunks::<2>().0 {
            assert_eq!(pair[0], pair[1]);
        }
    }
    assert_eq!(muted.applied_delay(), ms(40));
    assert_eq!(muted.stats().jitter.contracted, 0);
}

#[test]
fn worker_stall_drops_stale_audio_and_resumes_with_fresh_packets() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    jitter.push(header(0), &packet, ms(0)).unwrap();
    jitter.push(header(1), &packet, ms(20)).unwrap();
    assert!(matches!(jitter.pop_for_playout(ms(300)), Playout::Wait));
    assert_eq!(jitter.stats().underruns, 1);
    assert_eq!(jitter.queued(), 0);
    assert_eq!(
        jitter.push(header(1), &packet, ms(310)).unwrap(),
        Admission::Late
    );
    jitter.push(header(2), &packet, ms(320)).unwrap();
    assert!(matches!(jitter.pop_for_playout(ms(320)), Playout::Wait));
    assert_eq!(released(jitter.pop_for_playout(ms(340))), 2);
}

#[test]
fn startup_reordering_cannot_extend_the_sequence_window_repeatedly() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    jitter.push(header(1000), &packet, ms(0)).unwrap();
    jitter.push(header(937), &packet, ms(1)).unwrap();
    assert!(jitter.push(header(874), &packet, ms(2)).is_err());
    assert_eq!(jitter.queued(), 2);
    assert_eq!(jitter.stats().overflow, 1);
}

#[test]
fn long_idle_pause_resets_an_expired_delay_estimate_at_the_next_burst() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    jitter.push(header(0), &packet, ms(0)).unwrap();
    assert_eq!(released(jitter.pop_for_playout(ms(40))), 0);
    // A late arrival is measured and raises the estimate to its ceiling.
    jitter.push(header(1), &packet, ms(200)).unwrap();
    assert_eq!(jitter.target_delay(), ms(200));
    assert_eq!(released(jitter.pop_for_playout(ms(200))), 1);
    assert!(matches!(jitter.pop_for_playout(ms(400)), Playout::Wait));
    let mut fresh = header(2);
    fresh.talk_start = true;
    fresh.timestamp = 240_000;
    jitter.push(fresh, &packet, ms(5000)).unwrap();
    assert_eq!(jitter.target_delay(), ms(40));
    assert_eq!(jitter.applied_delay(), ms(40));
    assert_eq!(released(jitter.pop_for_playout(ms(5040))), 2);
}

#[test]
fn new_talk_start_rebases_speculative_losses_after_a_missing_talk_end() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    let mut first = header(0);
    first.talk_start = true;
    jitter.push(first, &packet, ms(0)).unwrap();
    assert_eq!(released(jitter.pop_for_playout(ms(40))), 0);
    // The previous burst ended without its TALK_END reaching us. PLC cannot
    // know that no sequence numbers were consumed while the sender was gated.
    assert!(matches!(
        jitter.pop_for_playout(ms(60)),
        Playout::Loss { .. }
    ));
    assert!(matches!(
        jitter.pop_for_playout(ms(80)),
        Playout::Loss { .. }
    ));
    let mut next = header(1);
    next.talk_start = true;
    next.timestamp = 4800;
    assert_eq!(
        jitter.push(next, &packet, ms(100)).unwrap(),
        Admission::Queued
    );
    assert_eq!(
        jitter.push(first, &packet, ms(110)).unwrap(),
        Admission::Late
    );
    assert!(matches!(jitter.pop_for_playout(ms(120)), Playout::Wait));
    assert_eq!(released(jitter.pop_for_playout(ms(140))), 1);
}

#[test]
fn capture_clock_gap_recovers_a_burst_when_both_boundary_flags_were_lost() {
    let packet = packet();
    let mut jitter = JitterBuffer::new(VoiceProfile::Voice);
    jitter.push(header(0), &packet, ms(0)).unwrap();
    assert_eq!(released(jitter.pop_for_playout(ms(40))), 0);
    assert!(matches!(
        jitter.pop_for_playout(ms(60)),
        Playout::Loss { .. }
    ));
    assert!(matches!(
        jitter.pop_for_playout(ms(80)),
        Playout::Loss { .. }
    ));
    let mut next = header(1);
    next.timestamp = 4800; // Capture resumed at 100 ms.
    assert_eq!(
        jitter.push(next, &packet, ms(100)).unwrap(),
        Admission::Queued
    );
    assert_eq!(released(jitter.pop_for_playout(ms(140))), 1);
}

#[test]
fn custom_jitter_bounds_survive_estimation_and_reject_invalid_ranges() {
    let mut buffer = JitterBuffer::new(VoiceProfile::Voice);
    buffer.set_bounds(ms(60), ms(80));
    assert_eq!(buffer.target_delay(), ms(60));
    let payload = packet();
    for seq in 0..10 {
        buffer
            .push(header(seq), &payload, ms(u64::from(seq) * 20))
            .unwrap();
        assert!((ms(60)..=ms(80)).contains(&buffer.target_delay()));
    }
    buffer.set_bounds(ms(190), ms(30));
    assert!((ms(60)..=ms(80)).contains(&buffer.target_delay()));
}
