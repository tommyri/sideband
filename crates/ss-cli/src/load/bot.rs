//! One synthetic user: connect with its token, sit in voice, talk in bursts,
//! post text, churn, and reconnect after a storm or a lost session.

use std::{
    collections::HashMap,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use bytes::Bytes;
use ss_core::Id;
use ss_net::media::{DatagramHeader, TALK_END, TALK_START};
use ss_proto::{
    v1::{self, client_envelope, server_envelope},
    validate::id_to_bytes,
};
use tokio::{sync::watch, time::MissedTickBehavior};

use super::{
    plan::{Behavior, Frame, Rng, Talk},
    session::{self, Credential, Session, SessionError, Target},
    stats::Stats,
};
use crate::{bots::Preroll, graph::audio_packet};

/// Longest wait between reconnect attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(10);
/// Relay samples above this are clock or wrap artefacts and are skipped.
const MAX_RELAY_SAMPLES: u32 = 48_000 * 10;

/// Everything bots share. Read-only apart from the atomics in `stats`.
pub struct Shared {
    /// Server address and pins.
    pub target: Target,
    /// Talk, text, and churn parameters.
    pub behavior: Behavior,
    /// Pre-encoded Opus frames.
    pub voice: Arc<Preroll>,
    /// Counters for reports.
    pub stats: Arc<Stats>,
    /// Zero point of the 48 kHz send clock written into datagrams.
    pub epoch: Instant,
    /// Channel for text messages.
    pub text_channel: Id,
    /// `true` ends the run.
    pub stop: watch::Receiver<bool>,
    /// Each increment is one reconnect storm.
    pub storm: watch::Receiver<u64>,
}

/// One bot's identity.
pub struct Bot {
    /// Username.
    pub name: String,
    /// Session token.
    pub token: String,
    /// Voice channel to sit in.
    pub channel: Id,
    /// Schedule generator.
    pub rng: Rng,
    /// Delay before the first connect.
    pub start_after: Duration,
}

enum Exit {
    Stop,
    Storm(Instant),
    Lost(String),
}

/// Runs until `shared.stop` flips.
pub async fn run(mut bot: Bot, shared: Arc<Shared>) {
    let mut stop = shared.stop.clone();
    if wait_or_stop(&mut stop, bot.start_after).await {
        return;
    }
    let stats = &shared.stats;
    let mut storm_started: Option<Instant> = None;
    let mut backoff = Duration::from_millis(500);
    loop {
        if *stop.borrow() {
            return;
        }
        let started = Instant::now();
        match connect(&bot, &shared).await {
            Ok((session, track)) => {
                Stats::bump(&stats.sessions);
                stats.join.record(started.elapsed());
                if let Some(storm) = storm_started.take() {
                    stats.storm_rejoin.record(storm.elapsed());
                }
                backoff = Duration::from_millis(500);
                match live(&mut bot, &shared, session, track).await {
                    Exit::Stop => return,
                    Exit::Storm(at) => storm_started = Some(at),
                    Exit::Lost(reason) => {
                        Stats::bump(&stats.forced);
                        stats.note_forced(&reason);
                        tracing::warn!(bot = %bot.name, %reason, "session lost");
                    }
                }
            }
            Err(error) => {
                Stats::bump(&stats.connect_failures);
                tracing::debug!(bot = %bot.name, %error, "connect failed");
                let jitter = backoff.mul_f64(bot.rng.unit() * 0.5);
                if wait_or_stop(&mut stop, backoff + jitter).await {
                    return;
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

async fn connect(bot: &Bot, shared: &Shared) -> Result<(Session, u32), SessionError> {
    let mut session = session::open(&shared.target, Credential::Token(&bot.token)).await?;
    let track = session.join_voice(bot.channel).await?;
    Ok((session, track))
}

/// Seated-session loop. Counts itself in `online` and `in_voice` while it runs.
#[allow(clippy::too_many_lines)]
async fn live(bot: &mut Bot, shared: &Shared, mut session: Session, track: u32) -> Exit {
    let stats = &shared.stats;
    let behavior = &shared.behavior;
    let _online = Gauge::up(stats, Gauge::ONLINE);
    let mut in_voice = Some(Gauge::up(stats, Gauge::VOICE));
    let mut track = track;
    let mut stop = shared.stop.clone();
    let mut storm = shared.storm.clone();
    storm.mark_unchanged();
    let mut talk = Talk::new(behavior, &mut bot.rng);
    let mut tick = tokio::time::interval(shared.voice.frame);
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut seq = 0_u16;
    let mut frame_index = 0_usize;
    let mut sequences = HashMap::new();
    let mut text_count = 0_u64;
    let now = tokio::time::Instant::now();
    let mut next_text = behavior.next_text(&mut bot.rng).map(|wait| now + wait);
    let mut next_churn = behavior.next_leave(&mut bot.rng).map(|wait| now + wait);
    let far = now + Duration::from_hours(24 * 365);
    let exit = loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break Exit::Stop;
                }
            }
            changed = storm.changed() => {
                if changed.is_ok() {
                    break Exit::Storm(Instant::now());
                }
            }
            event = session.events.recv() => {
                let Some(event) = event else {
                    break Exit::Lost(closed_reason(&session));
                };
                if let Some(id) = session::own_track(&event, &session.user_id) {
                    track = id;
                } else if let Some(server_envelope::Msg::TrackRemoved(removed)) = &event.msg {
                    sequences.remove(&removed.track_id);
                }
            }
            datagram = session.connection.read_datagram() => {
                match datagram {
                    Ok(bytes) => observe(stats, shared.epoch, &mut sequences, &bytes),
                    Err(error) => break Exit::Lost(error.to_string()),
                }
            }
            _ = tick.tick() => {
                if in_voice.is_none() {
                    continue;
                }
                let flags = match talk.tick(behavior, &mut bot.rng, shared.voice.frame) {
                    Frame::Silent => continue,
                    Frame::Start => TALK_START,
                    Frame::Middle => 0,
                    Frame::End => TALK_END,
                };
                let payload = &shared.voice.payloads[frame_index % shared.voice.payloads.len()];
                let Ok(packet) = audio_packet(track, seq, clock(shared.epoch), flags, payload) else {
                    Stats::bump(&stats.send_errors);
                    continue;
                };
                if session.connection.send_datagram(packet).is_ok() {
                    Stats::bump(&stats.sent);
                } else {
                    Stats::bump(&stats.send_errors);
                }
                seq = seq.wrapping_add(1);
                frame_index += 1;
            }
            () = tokio::time::sleep_until(next_text.unwrap_or(far)), if next_text.is_some() => {
                text_count += 1;
                send_text(&session, shared, &bot.name, text_count);
                next_text = behavior
                    .next_text(&mut bot.rng)
                    .map(|wait| tokio::time::Instant::now() + wait);
            }
            () = tokio::time::sleep_until(next_churn.unwrap_or(far)), if next_churn.is_some() => {
                let now = tokio::time::Instant::now();
                if in_voice.is_some() {
                    if let Err(error) = session.leave_voice().await {
                        break Exit::Lost(error.to_string());
                    }
                    Stats::bump(&stats.churn_leaves);
                    in_voice = None;
                    talk = Talk::new(behavior, &mut bot.rng);
                    let away = behavior.away.mul_f64(0.5 + bot.rng.unit());
                    next_churn = Some(now + away);
                } else {
                    match session.join_voice(bot.channel).await {
                        Ok(id) => track = id,
                        Err(error) => break Exit::Lost(error.to_string()),
                    }
                    // Packets that were not forwarded while away are not losses.
                    sequences.clear();
                    in_voice = Some(Gauge::up(stats, Gauge::VOICE));
                    next_churn = behavior
                        .next_leave(&mut bot.rng)
                        .map(|wait| tokio::time::Instant::now() + wait);
                }
            }
        }
    };
    if !matches!(exit, Exit::Lost(_)) {
        session.connection.close(0, b"load bot leaving");
    }
    exit
}

fn send_text(session: &Session, shared: &Shared, name: &str, count: u64) {
    let control = Arc::clone(&session.control);
    let stats = Arc::clone(&shared.stats);
    let message = client_envelope::Msg::SendMessage(v1::SendMessage {
        channel_id: id_to_bytes(shared.text_channel),
        content: format!(
            "{name} load message {count}: the quick brown fox jumps over the lazy dog"
        ),
        nonce: Some(format!("{name}-{count}")),
        ..v1::SendMessage::default()
    });
    Stats::bump(&stats.text_sent);
    tokio::spawn(async move {
        let started = Instant::now();
        match session::call(&control, "SendMessage", message).await {
            Ok(_) => {
                Stats::bump(&stats.text_acked);
                stats.text_ack.record(started.elapsed());
            }
            Err(error) => {
                Stats::bump(&stats.text_failed);
                tracing::debug!(%error, "message failed");
            }
        }
    });
}

/// Counts one received datagram: relay delay from its timestamp and sequence
/// gaps per track.
pub fn observe(stats: &Stats, epoch: Instant, sequences: &mut HashMap<u32, u16>, bytes: &Bytes) {
    Stats::bump(&stats.received);
    let Ok((header, _)) = DatagramHeader::parse(bytes) else {
        return;
    };
    let delay = clock(epoch).wrapping_sub(header.timestamp);
    if delay < MAX_RELAY_SAMPLES {
        stats
            .relay
            .record(Duration::from_micros(u64::from(delay) * 1_000 / 48));
    }
    match sequences.entry(header.track_id) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(header.seq);
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            let step = header.seq.wrapping_sub(*entry.get());
            if step == 0 {
                return;
            }
            if step < 0x8000 {
                stats.lost.fetch_add(u64::from(step - 1), Ordering::Relaxed);
                entry.insert(header.seq);
            } else {
                Stats::bump(&stats.reordered);
            }
        }
    }
}

/// The send clock: 48 kHz samples since `epoch`, wrapping like the wire field.
pub fn clock(epoch: Instant) -> u32 {
    let micros = epoch.elapsed().as_micros();
    let samples = micros * 48 / 1_000;
    // Truncation is the wire format's wrap.
    #[allow(clippy::cast_possible_truncation)]
    {
        samples as u32
    }
}

fn closed_reason(session: &Session) -> String {
    session.control.terminal_error().map_or_else(
        || "control stream closed".to_owned(),
        |error| error.to_string(),
    )
}

/// Returns true when `stop` flipped before `wait` elapsed.
async fn wait_or_stop(stop: &mut watch::Receiver<bool>, wait: Duration) -> bool {
    if *stop.borrow() {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(wait) => *stop.borrow(),
        changed = stop.changed() => changed.is_err() || *stop.borrow(),
    }
}

/// Increments a gauge and decrements it on drop.
struct Gauge<'a> {
    counter: &'a std::sync::atomic::AtomicU64,
}

impl<'a> Gauge<'a> {
    const ONLINE: u8 = 0;
    const VOICE: u8 = 1;

    fn up(stats: &'a Stats, which: u8) -> Self {
        let counter = if which == Self::ONLINE {
            &stats.online
        } else {
            &stats.in_voice
        };
        counter.fetch_add(1, Ordering::Relaxed);
        Self { counter }
    }
}

impl Drop for Gauge<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::audio_packet;

    #[test]
    fn observe_counts_gaps_and_reorders_per_track() {
        let stats = Stats::default();
        let epoch = Instant::now();
        let mut sequences = HashMap::new();
        for seq in [0_u16, 1, 4, 3, 5] {
            let packet = audio_packet(9, seq, clock(epoch), 0, &[1, 2, 3]).unwrap();
            observe(&stats, epoch, &mut sequences, &packet);
        }
        let other = audio_packet(10, 65_535, clock(epoch), 0, &[1]).unwrap();
        observe(&stats, epoch, &mut sequences, &other);
        let wrapped = audio_packet(10, 0, clock(epoch), 0, &[1]).unwrap();
        observe(&stats, epoch, &mut sequences, &wrapped);
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.received, 7);
        assert_eq!(snapshot.lost, 2);
        assert_eq!(snapshot.reordered, 1);
        assert_eq!(snapshot.relay.count(), 7);
        assert!(snapshot.relay.max().unwrap() < Duration::from_millis(100));
    }

    #[test]
    fn relay_delay_survives_clock_wrap() {
        let stats = Stats::default();
        let epoch = Instant::now();
        let sent = clock(epoch).wrapping_sub(480);
        let packet = audio_packet(1, 0, sent, 0, &[0]).unwrap();
        observe(&stats, epoch, &mut HashMap::new(), &packet);
        let delay = stats.snapshot().relay.quantile(0.5).unwrap();
        assert!(delay >= Duration::from_millis(9), "delay {delay:?}");
        assert!(delay < Duration::from_millis(60), "delay {delay:?}");
    }
}
