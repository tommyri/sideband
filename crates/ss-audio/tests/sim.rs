//! Deterministic network simulation: no sleeps, sockets, or audio hardware.
#![allow(clippy::cast_precision_loss)] // Bounded synthetic signal indices.

use std::time::Duration;

use ss_audio::{
    codec::Encoder,
    jitter::PacketHeader,
    receive::{Stats, TrackReceiver},
};
use ss_core::VoiceProfile;

struct Network {
    delay_ms: u64,
    jitter_ms: u64,
    loss_percent: u32,
    reorder_percent: u32,
    burst_packets: usize,
}
struct Arrival {
    at: u64,
    header: PacketHeader,
    payload: Vec<u8>,
}
#[derive(Debug)]
struct Report {
    stats: Stats,
    first_output_ms: u64,
    max_gap_ms: u64,
    gap_count: u64,
    applied_delay_ms: u128,
    /// Largest wait between capture and the speaker, network included.
    max_delay_ms: u64,
}

fn random(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state
}

/// Sender media time, in milliseconds, of the quantum `render` just produced.
fn rendered_media_ms(receiver: &TrackReceiver) -> Option<u64> {
    receiver
        .playout_timestamp()
        .map(|timestamp| u64::from(timestamp.saturating_sub(480)) / 48)
}

fn simulate(network: &Network, seed: u32) -> Report {
    const PACKETS: u16 = 500;
    let mut random_state = seed;
    let mut encoder = Encoder::new(VoiceProfile::Voice).unwrap();
    let mut receiver = TrackReceiver::new(VoiceProfile::Voice).unwrap();
    let mut arrivals = Vec::new();
    let mut burst_remaining = 0;
    for seq in 0..PACKETS {
        let pcm: Vec<_> = (0..960)
            .map(|i| {
                let sample = usize::from(seq) * 960 + i;
                0.251_188_64 * (std::f32::consts::TAU * 200.0 * sample as f32 / 48_000.0).sin()
            })
            .collect();
        let payload = encoder.encode(&pcm).unwrap().to_vec();
        let mut lost = false;
        // Keep endpoints so the measurement has an unambiguous burst interval.
        if seq > 0 && seq < PACKETS - 1 {
            if burst_remaining == 0
                && random(&mut random_state) % (100 * u32::try_from(network.burst_packets).unwrap())
                    < network.loss_percent
            {
                burst_remaining = network.burst_packets;
            }
            if burst_remaining > 0 {
                burst_remaining -= 1;
                lost = true;
            }
        }
        if lost {
            continue;
        }
        let jitter = u64::from(random(&mut random_state)) % (2 * network.jitter_ms + 1);
        let mut delay = network.delay_ms - network.jitter_ms + jitter;
        if random(&mut random_state) % 100 < network.reorder_percent {
            delay += 40;
        }
        if seq == 0 {
            delay = network.delay_ms;
        }
        arrivals.push(Arrival {
            at: u64::from(seq) * 20 + delay,
            header: PacketHeader {
                seq,
                timestamp: u32::from(seq) * 960,
                talk_start: seq == 0,
                talk_end: seq == PACKETS - 1,
            },
            payload,
        });
    }
    arrivals.sort_by_key(|packet| packet.at);
    let mut cursor = 0;
    let mut first_output = None;
    let mut gap = 0;
    let mut max_gap = 0;
    let mut gap_count = 0;
    let mut max_delay = 0;
    for tick in 0..1030 {
        // The worker wakes up to 3 ms late; a perfect tick cannot show lateness.
        let now_ms = tick * 10 + u64::from(random(&mut random_state) % 4);
        while let Some(packet) = arrivals.get(cursor).filter(|packet| packet.at <= now_ms) {
            receiver
                .push(
                    packet.header,
                    &packet.payload,
                    Duration::from_millis(packet.at),
                )
                .unwrap();
            cursor += 1;
        }
        let output = receiver.render(Duration::from_millis(now_ms));
        assert!(output.iter().all(|s| s.is_finite()));
        let energy = output.iter().map(|s| s * s).sum::<f32>() / 960.0;
        if energy > 1e-6 {
            first_output.get_or_insert(now_ms);
        }
        if let Some(media) = rendered_media_ms(&receiver) {
            max_delay = max_delay.max(now_ms.saturating_sub(media));
        }
        // A gap proxy is RMS below -60 dBFS after warm-up and before final audio.
        if (200..10_000).contains(&now_ms) {
            if energy < 1e-6 {
                if gap == 0 {
                    gap_count += 1;
                }
                gap += 10;
                max_gap = max_gap.max(gap);
            } else {
                gap = 0;
            }
        }
    }
    Report {
        stats: receiver.stats(),
        first_output_ms: first_output.unwrap(),
        max_gap_ms: max_gap,
        gap_count,
        applied_delay_ms: receiver.applied_delay().as_millis(),
        max_delay_ms: max_delay,
    }
}

#[test]
fn thirty_ms_plus_minus_ten_with_two_percent_loss_meets_playout_budget() {
    let network = Network {
        delay_ms: 30,
        jitter_ms: 10,
        loss_percent: 2,
        reorder_percent: 0,
        burst_packets: 1,
    };
    for seed in [7, 42, 12345, 0xdead_beef] {
        let report = simulate(&network, seed);
        eprintln!("2% loss seed {seed}: {report:?}");
        assert_eq!(report.stats.jitter.underruns, 0);
        // Every packet arrived inside the 40 ms target, so none may be late.
        assert_eq!(report.stats.jitter.late, 0);
        assert_eq!(report.stats.decode_errors, 0);
        // First arrival (30 ms), the 40 ms target and up to 3 ms of wake-up.
        assert!(report.first_output_ms <= 73);
        assert!(report.applied_delay_ms <= 40);
        // Network (30 ms), the buffer (at most 40 ms) and one 10 ms quantum of
        // render phase and wake-up lateness. Anything above is a lagging release.
        assert!(report.max_delay_ms <= 80, "{report:?}");
        assert!(report.max_gap_ms <= 20);
        assert!(report.stats.jitter.lost > 0);
        assert!(report.stats.fec_attempts + report.stats.plc > 0);
    }
}

#[test]
fn five_percent_burst_loss_and_reordering_report_gaps_and_use_recovery() {
    let network = Network {
        delay_ms: 30,
        jitter_ms: 10,
        loss_percent: 5,
        reorder_percent: 10,
        burst_packets: 3,
    };
    let report = simulate(&network, 42);
    eprintln!(
        "5% loss / three-packet bursts: {report:?}; audible-gap proxy count={}",
        report.gap_count
    );
    assert_eq!(report.stats.jitter.underruns, 0);
    assert_eq!(report.stats.decode_errors, 0);
    assert!(report.stats.jitter.lost > 0);
    assert!(report.stats.plc > 0);
    assert!(report.stats.fec_attempts > 0);
    assert!(report.stats.jitter.late > 0);
}

/// What a stalled playout worker looked like from the speaker.
#[derive(Debug)]
struct StallReport {
    stats: Stats,
    /// Output delay minus the applied jitter delay, just before the stall.
    before_ms: i64,
    /// The same figure for the last rendered quantum.
    after_ms: i64,
    /// Time from the end of the stall until the excess stayed under a frame.
    recovered_after_ms: Option<u64>,
}

/// A sender that never ends its burst (open mic or music), a steady 30 ms
/// network, and an audio worker that stops for `stall_ms`. It renders like
/// `Worker::run`: every 10 ms, catching up missed quanta only when it is at
/// most 30 ms behind. Packets keep their network arrival stamps while the
/// worker is stalled, as `MediaLink::pump` stamps them.
fn simulate_stall(profile: VoiceProfile, stall_ms: u64, level: f32) -> StallReport {
    const NETWORK_MS: u64 = 30;
    const STALL_AT_MS: u64 = 5_000;
    const END_MS: u64 = 12_000;
    let frame_ms = u64::try_from(profile.encoder_params().frame_size.as_millis()).unwrap();
    let mut encoder = Encoder::new(profile).unwrap();
    let mut receiver = TrackReceiver::new(profile).unwrap();
    let channels = usize::from(profile.encoder_params().channels);
    let samples = encoder.input_samples() / channels;
    let mut arrivals = Vec::new();
    for seq in 0..u16::try_from(END_MS / frame_ms).unwrap() {
        let pcm: Vec<_> = (0..samples)
            .flat_map(|i| {
                let sample = usize::from(seq) * samples + i;
                let value =
                    level * (std::f32::consts::TAU * 300.0 * sample as f32 / 48_000.0).sin();
                std::iter::repeat_n(value, channels)
            })
            .collect();
        arrivals.push(Arrival {
            at: u64::from(seq) * frame_ms + NETWORK_MS,
            header: PacketHeader {
                seq,
                timestamp: u32::from(seq) * u32::try_from(frame_ms * 48).unwrap(),
                talk_start: seq == 0,
                talk_end: false,
            },
            payload: encoder.encode(&pcm).unwrap().to_vec(),
        });
    }
    let stall_end = STALL_AT_MS + stall_ms;
    let mut cursor = 0;
    let mut next = 0;
    let mut before = None;
    let mut excess = 0;
    let mut recovered = None;
    for now_ms in 0..END_MS {
        if (STALL_AT_MS..stall_end).contains(&now_ms) {
            continue;
        }
        while now_ms >= next {
            while let Some(packet) = arrivals.get(cursor).filter(|packet| packet.at <= now_ms) {
                receiver
                    .push(
                        packet.header,
                        &packet.payload,
                        Duration::from_millis(packet.at),
                    )
                    .unwrap();
                cursor += 1;
            }
            receiver.render(Duration::from_millis(now_ms));
            next += 10;
            if next + 30 < now_ms {
                next = now_ms;
            }
            let Some(media) = rendered_media_ms(&receiver) else {
                continue;
            };
            let delay = i64::try_from(now_ms - media - NETWORK_MS).unwrap();
            excess = delay - i64::try_from(receiver.applied_delay().as_millis()).unwrap();
            if now_ms < STALL_AT_MS {
                before = Some(excess);
            } else if excess >= i64::try_from(frame_ms).unwrap() {
                recovered = None;
            } else {
                recovered.get_or_insert(now_ms - stall_end);
            }
        }
    }
    StallReport {
        stats: receiver.stats(),
        before_ms: before.unwrap(),
        after_ms: excess,
        recovered_after_ms: recovered,
    }
}

#[test]
fn continuous_sender_returns_to_target_delay_after_a_worker_stall() {
    for profile in [
        VoiceProfile::Voice,
        VoiceProfile::Music,
        VoiceProfile::LowLatency,
    ] {
        let frame = i64::try_from(profile.encoder_params().frame_size.as_millis()).unwrap();
        for stall in [30, 80, 150, 190] {
            // A loud tone has no quiet frame to hide the skip in.
            let report = simulate_stall(profile, stall, 0.25);
            eprintln!("{profile:?} stall {stall} ms: {report:?}");
            assert!(report.before_ms < frame, "{profile:?} {report:?}");
            assert!(report.after_ms < frame, "{profile:?} {report:?}");
            let recovered = report.recovered_after_ms.expect("delay never recovered");
            assert!(recovered < 2_000, "{profile:?} {report:?}");
            assert_eq!(report.stats.jitter.underruns, 0, "{profile:?} {report:?}");
            assert_eq!(report.stats.jitter.late, 0, "{profile:?} {report:?}");
            assert_eq!(report.stats.decode_errors, 0, "{profile:?} {report:?}");
        }
    }
}

#[test]
fn quiet_sender_catches_up_in_silence_without_waiting() {
    for profile in [VoiceProfile::Voice, VoiceProfile::LowLatency] {
        let report = simulate_stall(profile, 150, 0.0);
        eprintln!("{profile:?} silent sender: {report:?}");
        let recovered = report.recovered_after_ms.expect("delay never recovered");
        // One skipped frame per release, well before the 500 ms patience.
        assert!(recovered <= 200, "{profile:?} {report:?}");
        assert!(report.stats.jitter.caught_up > 0, "{profile:?} {report:?}");
        assert_eq!(report.stats.jitter.late, 0, "{profile:?} {report:?}");
    }
}
