//! Synthetic senders for load tests. Each bot joins the same channel and loops
//! a one-second Opus buffer with fresh sequence numbers.

use std::{sync::Arc, time::Duration};

use ss_audio::codec::Encoder;
use ss_core::VoiceProfile;
use ss_net::media::TALK_START;
use tokio::{sync::watch, task::JoinHandle, time::MissedTickBehavior};

use crate::{
    call::{CallError, Options, join_client},
    graph::{GraphError, audio_packet},
};

/// Spawn `options.bots` clients. A bot that fails to join logs and exits.
pub(crate) fn spawn(options: &Options, stop: &watch::Receiver<bool>) -> Vec<JoinHandle<()>> {
    (1..=options.bots)
        .map(|index| {
            let options = options.clone();
            let stop = stop.clone();
            tokio::spawn(async move {
                let name = format!("{}-bot-{index}", options.name);
                if let Err(error) = run_bot(&options, &name, stop).await {
                    tracing::warn!(%name, %error, "bot exited");
                }
            })
        })
        .collect()
}

async fn run_bot(
    options: &Options,
    name: &str,
    mut stop: watch::Receiver<bool>,
) -> Result<(), CallError> {
    let joined = join_client(options, name).await?;
    let voice = preroll(options.profile).map_err(CallError::Graph)?;
    let connection = Arc::new(joined.connection);
    let mut events = joined.events;
    let _control = joined.control;
    let reader = Arc::clone(&connection);
    let mut drain_stop = stop.clone();
    let drain = tokio::spawn(async move {
        loop {
            tokio::select! {
                changed = drain_stop.changed() => {
                    if changed.is_err() || *drain_stop.borrow() {
                        break;
                    }
                }
                event = events.recv() => {
                    if event.is_none() {
                        break;
                    }
                }
                datagram = reader.read_datagram() => {
                    if datagram.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut tick = tokio::time::interval(voice.frame);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut index = 0_usize;
    let mut seq = 0_u16;
    let mut timestamp = 0_u32;
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            _ = tick.tick() => {
                let payload = &voice.payloads[index % voice.payloads.len()];
                let flags = if seq == 0 { TALK_START } else { 0 };
                let packet = audio_packet(joined.track_id, seq, timestamp, flags, payload)
                    .map_err(GraphError::Media)
                    .map_err(CallError::Graph)?;
                if connection.send_datagram(packet).is_err() {
                    break;
                }
                seq = seq.wrapping_add(1);
                timestamp = timestamp.wrapping_add(voice.step);
                index += 1;
            }
        }
    }
    drain.abort();
    Ok(())
}

struct Preroll {
    payloads: Vec<Vec<u8>>,
    frame: Duration,
    step: u32,
}

fn preroll(profile: VoiceProfile) -> Result<Preroll, GraphError> {
    let params = profile.encoder_params();
    let mut encoder = Encoder::new(profile)?;
    let channels = usize::from(params.channels);
    let millis = u32::try_from(params.frame_size.as_millis()).unwrap_or(20);
    let step = millis.saturating_mul(48);
    let mut frame = vec![0.0_f32; encoder.input_samples()];
    let mut payloads = Vec::with_capacity(50);
    for index in 0..50_u32 {
        write_tone(
            &mut frame,
            channels,
            usize::try_from(index).unwrap_or(0) * usize::try_from(step).unwrap_or(960),
        );
        payloads.push(encoder.encode(&frame)?.to_vec());
    }
    Ok(Preroll {
        payloads,
        frame: params.frame_size,
        step,
    })
}

fn write_tone(frame: &mut [f32], channels: usize, start: usize) {
    for index in 0..frame.len() / channels.max(1) {
        let time = f64_from_usize(start.saturating_add(index)) / 48_000.0;
        let value = f32_from_f64(0.2 * (2.0 * std::f64::consts::PI * 440.0 * time).sin());
        for channel in 0..channels {
            frame[index * channels + channel] = value;
        }
    }
}

fn f64_from_usize(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn f32_from_f64(value: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    {
        value as f32
    }
}
