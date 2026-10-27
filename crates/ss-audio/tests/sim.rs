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
}

fn random(state: &mut u32) -> u32 {
    *state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *state
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
    for tick in 0..1030 {
        let now_ms = tick * 10;
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
        assert_eq!(report.stats.decode_errors, 0);
        assert!(report.first_output_ms <= 70);
        assert!(report.applied_delay_ms <= 40);
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
