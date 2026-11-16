//! Per-subscriber rendition choice and the link signal that drives it.
//!
//! Quinn buffers far more than a GOP per stream, so a viewer on a slow link
//! rarely overflows the relay's chunk queue: the backlog sits in Quinn's send
//! buffer instead. Each connection's [`Estimator`] therefore compares the video
//! bytes written to it with the bytes Quinn sent and did not declare lost,
//! over the last half second. Quinn counts a packet the bottleneck dropped as
//! sent until it declares it lost a few round trips later, so a rise in
//! round-trip time over its recent minimum is the earlier signal of a queue
//! building on the path. Either one held for 300 ms while video flows is
//! congestion.
//!
//! A congested subscriber moves down to the low rendition at once: its full
//! groups are cancelled and the low rendition's next group (a keyframe the
//! publisher is asked for) is forwarded instead. Moving up is a probe after a
//! quiet period: the next full group is forwarded at a lower stream priority
//! while the low rendition keeps flowing, and the subscriber stays on the full
//! rendition only if its link keeps up until the following full group.
use super::{
    Counters, MAX_RENDITIONS, Ordering, Streams, Subscriber, cancel_all, report_low_change,
    request_keyframe, stats_envelope,
};
use ss_core::Id;
use ss_proto::v1;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64},
    time::{Duration, Instant},
};

/// How often each connection samples its link.
pub(super) const TICK: Duration = Duration::from_millis(100);
/// Ticks in the comparison window (half a second).
const WINDOW: usize = 5;
/// Consecutive over-budget ticks before a link counts as congested.
const PERSIST: u32 = 3;
/// Round-trip rise over the recent minimum that means a queue on the path.
const RTT_RISE: Duration = Duration::from_millis(60);
/// Ticks the recent minimum round-trip time spans (ten seconds), so a
/// lasting route change becomes the new baseline.
const RTT_WINDOW: usize = 100;
/// Ticks between `SubscriberStats` reports to a publisher.
pub(super) const REPORT_TICKS: u32 = 10;
/// First quiet period before probing the full rendition again.
const QUIET_FIRST: Duration = Duration::from_secs(4);
/// Longest quiet period; bounds recovery after an impairment ends.
const QUIET_MAX: Duration = Duration::from_secs(16);
/// A probe must last this long before the next full group confirms it.
const PROBE_MIN: Duration = Duration::from_secs(1);
/// A move down this soon after a move up doubles the quiet period.
const FLAP: Duration = Duration::from_secs(10);
/// Stream priority of a probe group, below every regular group.
pub(super) const PROBE_PRIORITY: i32 = -1;

/// Video bytes written to one connection and what its link did with them.
#[derive(Default)]
pub(super) struct Link {
    pub(super) written: AtomicU64,
    /// Groups reset because this viewer fell behind (overflow or eviction).
    pub(super) resets: AtomicU32,
    congested: AtomicBool,
    backlog_ms: AtomicU32,
}
impl Link {
    pub(super) fn congested(&self) -> bool {
        self.congested.load(Ordering::Relaxed)
    }
    pub(super) fn backlog_ms(&self) -> u32 {
        self.backlog_ms.load(Ordering::Relaxed)
    }
}

/// Offered-against-delivered comparison over a sliding half-second window.
#[derive(Default)]
pub(super) struct Estimator {
    window: VecDeque<(u64, u64)>,
    rtts: VecDeque<Duration>,
    last: Option<(u64, u64)>,
    over: u32,
    resets: u32,
    /// Bytes offered and delivered over the last window, for logs.
    pub(super) totals: (u64, u64),
}
impl Estimator {
    /// Feed cumulative video bytes written, UDP bytes delivered and the
    /// smoothed round-trip time. Returns whether the link is congested now,
    /// and its estimated excess backlog in milliseconds.
    pub(super) fn sample(&mut self, written: u64, sent: u64, rtt: Duration) -> (bool, u32) {
        if self.rtts.len() == RTT_WINDOW {
            self.rtts.pop_front();
        }
        self.rtts.push_back(rtt);
        let floor = self.rtts.iter().min().copied().unwrap_or(rtt);
        let (last_written, last_sent) = self.last.unwrap_or((written, sent));
        self.last = Some((written, sent));
        if self.window.len() == WINDOW {
            self.window.pop_front();
        }
        self.window.push_back((
            written.saturating_sub(last_written),
            sent.saturating_sub(last_sent),
        ));
        let (offered, delivered) = self
            .window
            .iter()
            .fold((0_u64, 0_u64), |(o, d), (w, s)| (o + w, d + s));
        // Quinn adds headers, acknowledgements and retransmissions, and the
        // connection also carries voice and control, so a link that keeps up
        // delivers at least what was offered. A keyframe burst clears within
        // a few ticks; a short link stays over for the whole window.
        self.totals = (offered, delivered);
        let over = offered > delivered + delivered / 2 + 32 * 1024
            || (offered > 0 && rtt.saturating_sub(floor) >= RTT_RISE);
        self.over = if over { self.over + 1 } else { 0 };
        let excess = offered.saturating_sub(delivered);
        let backlog = excess
            .saturating_mul(500)
            .checked_div(delivered)
            .unwrap_or(if offered == 0 { 0 } else { 1000 });
        (
            self.over >= PERSIST,
            u32::try_from(backlog.min(60_000)).unwrap_or(u32::MAX),
        )
    }
    /// Whether the link's reset count grew since the last call.
    pub(super) fn new_resets(&mut self, resets: u32) -> bool {
        let grew = resets != self.resets;
        self.resets = resets;
        grew
    }
}

/// What to do with a newly arrived group for one subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Forward {
    Skip,
    Normal,
    /// Forward at [`PROBE_PRIORITY`] while the low rendition keeps flowing.
    Probe,
    /// The probe held: move up, forward normally, drop the low rendition.
    Up,
}

/// Effect of congestion on one subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Congestion {
    None,
    /// Move to the low rendition; cancel its full groups.
    Down,
    /// The probe fell behind; cancel it and keep the low rendition.
    ProbeFailed,
}

/// Effect of a new preference cap on one subscriber.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Cap {
    Unchanged,
    /// Move to the low rendition at its next group.
    Down,
    /// A probe of the full rendition was running; cancel its groups.
    ProbeEnded,
}

/// One subscriber's rendition state.
pub(super) struct Selection {
    switchable: bool,
    rendition: u8,
    /// Lowest rendition index the viewer's preference allows (0 or 1).
    cap: u8,
    probe: Option<Instant>,
    quiet_since: Instant,
    quiet: Duration,
    last_up: Option<Instant>,
}
impl Selection {
    /// A viewer that cannot decode other renditions stays on rendition 0.
    pub(super) fn new(switchable: bool, now: Instant) -> Self {
        Self {
            switchable,
            rendition: 0,
            cap: 0,
            probe: None,
            quiet_since: now,
            quiet: QUIET_FIRST,
            last_up: None,
        }
    }
    pub(super) const fn rendition(&self) -> u8 {
        self.rendition
    }
    /// Whether this viewer is sent the low rendition now (on it, or probing up from it).
    pub(super) const fn receives_low(&self) -> bool {
        self.rendition == 1
    }
    /// Apply a preference cap. Returns true when it moves the viewer down;
    /// its full group in flight then ends normally and the low rendition's
    /// next group follows. A probe running under a new cap is ended at once.
    pub(super) fn set_cap(&mut self, cap: u8, now: Instant) -> Cap {
        if !self.switchable {
            return Cap::Unchanged;
        }
        self.cap = cap;
        if cap == 1 && self.probe.take().is_some() {
            return Cap::ProbeEnded;
        }
        if cap == 1 && self.rendition == 0 {
            self.rendition = 1;
            return Cap::Down;
        }
        if cap == 0 && self.rendition == 1 && self.probe.is_none() {
            // A lifted cap is not a congestion signal: probe at the next group.
            self.quiet_since = now.checked_sub(self.quiet).unwrap_or(now);
        }
        Cap::Unchanged
    }
    /// Decide for a group of `rendition` on a track with `renditions` renditions.
    pub(super) fn on_group(
        &mut self,
        rendition: u8,
        renditions: usize,
        congested: bool,
        now: Instant,
    ) -> Forward {
        if !self.switchable || renditions < 2 {
            return if rendition == 0 {
                Forward::Normal
            } else {
                Forward::Skip
            };
        }
        if rendition == self.rendition {
            return Forward::Normal;
        }
        if rendition != 0 {
            return Forward::Skip;
        }
        match self.probe {
            Some(started) if self.cap == 0 && now.duration_since(started) >= PROBE_MIN => {
                self.rendition = 0;
                self.probe = None;
                self.quiet = QUIET_FIRST.max(self.quiet / 2);
                self.last_up = Some(now);
                Forward::Up
            }
            Some(_) => Forward::Probe,
            None if self.cap == 0
                && !congested
                && now.duration_since(self.quiet_since) >= self.quiet =>
            {
                self.probe = Some(now);
                Forward::Probe
            }
            None => Forward::Skip,
        }
    }
    /// The subscriber's link is congested now.
    pub(super) fn congested(&mut self, renditions: usize, now: Instant) -> Congestion {
        if !self.switchable || renditions < 2 {
            return Congestion::None;
        }
        self.quiet_since = now;
        if self.probe.take().is_some() {
            self.quiet = (self.quiet * 2).min(QUIET_MAX);
            return Congestion::ProbeFailed;
        }
        if self.rendition == 0 {
            self.rendition = 1;
            if self.last_up.is_some_and(|up| now.duration_since(up) < FLAP) {
                self.quiet = (self.quiet * 2).min(QUIET_MAX);
            }
            return Congestion::Down;
        }
        Congestion::None
    }
}

/// Lowest rendition index a viewer preference allows: 1 when the full
/// rendition exceeds a nonzero height or frame-rate limit and a low one exists.
pub(super) fn cap(renditions: &[v1::Rendition], max_height: u32, max_fps: u32) -> u8 {
    let Some(full) = renditions.first() else {
        return 0;
    };
    let fits =
        (max_height == 0 || full.height <= max_height) && (max_fps == 0 || full.fps <= max_fps);
    u8::from(renditions.len() >= MAX_RENDITIONS && !fits)
}

impl Streams {
    /// One link sample for `conn`: congestion moves its subscriptions down,
    /// and every [`REPORT_TICKS`] its own publications report to it.
    pub(super) fn tick(
        &self,
        conn: Id,
        connection: &quinn::Connection,
        estimator: &mut Estimator,
        tick: u32,
    ) {
        let link = self.rooms.read().video.links.get(&conn).cloned();
        if let Some(link) = link {
            // Bytes Quinn declared lost never arrived; without them, the
            // signal waits for the congestion window to shrink first.
            let stats = connection.stats();
            let sent = stats.udp_tx.bytes.saturating_sub(stats.path.lost_bytes);
            let (congested, backlog) =
                estimator.sample(link.written.load(Ordering::Relaxed), sent, connection.rtt());
            link.congested.store(congested, Ordering::Relaxed);
            link.backlog_ms.store(backlog, Ordering::Relaxed);
            let resets = estimator.new_resets(link.resets.load(Ordering::Relaxed));
            if congested || resets {
                self.congestion(conn, estimator.totals, resets);
            }
        }
        if tick.is_multiple_of(REPORT_TICKS) {
            self.report(conn);
        }
    }

    fn congestion(&self, conn: Id, (offered, delivered): (u64, u64), reset: bool) {
        let mut rooms = self.rooms.write();
        let state = &mut rooms.video;
        let now = Instant::now();
        for track in state.tracks.values_mut() {
            let renditions = track.renditions();
            let Some(sub) = track.viewers.get_mut(&conn) else {
                continue;
            };
            match sub.select.congested(renditions, now) {
                Congestion::None => {}
                Congestion::Down => {
                    tracing::info!(
                        %conn, track = track.wire.track_id, offered, delivered, reset,
                        "video viewer moved to the low rendition"
                    );
                    down(sub, &state.counters);
                    let starts_low = track.reported_low == 0;
                    report_low_change(&state.registry, track);
                    if !starts_low {
                        request_keyframe(&state.registry, track, 1);
                    }
                }
                Congestion::ProbeFailed => {
                    tracing::debug!(
                        %conn, track = track.wire.track_id, offered, delivered, reset,
                        "video probe of the full rendition fell behind"
                    );
                    // The low rendition kept flowing under the probe; a viewer
                    // that already moved to the probe falls back to it.
                    cancel_all(std::mem::take(&mut sub.groups[0]));
                }
            }
        }
    }

    fn report(&self, conn: Id) {
        let mut rooms = self.rooms.write();
        let state = &mut rooms.video;
        for track in state.tracks.values_mut().filter(|t| t.owner == conn) {
            let envelope = stats_envelope(track, true);
            state.registry.enqueue(conn, envelope);
        }
    }
}

fn down(sub: &mut Subscriber, counters: &Counters) {
    cancel_all(std::mem::take(&mut sub.groups[0]));
    counters.down.fetch_add(1, Ordering::Relaxed);
}

/// After a move up, the low rendition's groups for that viewer are surplus.
pub(super) fn up(sub: &mut Subscriber, counters: &Counters) {
    cancel_all(std::mem::take(&mut sub.groups[1]));
    counters.up.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn estimator_flags_a_queue_by_round_trip_and_rebaselines() {
        let mut e = Estimator::default();
        let mut written = 0;
        let mut flagged = Vec::new();
        for tick in 0..150 {
            written += 10_000;
            let rtt = if tick < 20 {
                MS
            } else {
                Duration::from_millis(90)
            };
            flagged.push(e.sample(written, written * 2, rtt).0);
        }
        assert!(!flagged[..20].iter().any(|f| *f));
        assert!(flagged[22]);
        // Ten seconds later the higher round trip is the new floor.
        assert!(!flagged[149]);
        // No video, no congestion, whatever the round trip.
        let mut idle = Estimator::default();
        idle.sample(0, 0, MS);
        for _ in 0..5 {
            assert!(!idle.sample(0, 0, Duration::from_millis(300)).0);
        }
    }

    #[test]
    fn estimator_ignores_bursts_and_flags_a_short_link() {
        let mut e = Estimator::default();
        let (mut written, mut sent) = (0_u64, 0_u64);
        // Healthy: a 200 kB keyframe drains within two ticks.
        for tick in 0..30 {
            written += if tick % 20 == 0 { 200_000 } else { 20_000 };
            sent += if tick % 20 == 0 {
                100_000
            } else {
                21_000 + 50_000 * u64::from(tick % 20 == 1)
            };
            assert!(!e.sample(written, sent, MS).0, "tick {tick}");
        }
        // Short: 225 kB/s offered, 75 kB/s delivered.
        let mut flagged = None;
        for tick in 0..20 {
            written += 22_500;
            sent += 7_500;
            if e.sample(written, sent, MS).0 && flagged.is_none() {
                flagged = Some(tick);
            }
        }
        assert!(flagged.is_some_and(|t| t < 10), "{flagged:?}");
    }

    #[test]
    fn selection_moves_down_at_once_and_probes_up_with_backoff() {
        let start = Instant::now();
        let at = |s: u64| start + Duration::from_millis(s);
        let mut sel = Selection::new(true, start);
        assert_eq!(sel.on_group(0, 2, false, at(0)), Forward::Normal);
        assert_eq!(sel.on_group(1, 2, false, at(0)), Forward::Skip);
        assert_eq!(sel.congested(2, at(1_000)), Congestion::Down);
        assert_eq!(sel.on_group(1, 2, false, at(1_100)), Forward::Normal);
        // Quiet for less than four seconds: no probe.
        assert_eq!(sel.on_group(0, 2, false, at(3_000)), Forward::Skip);
        assert_eq!(sel.on_group(0, 2, false, at(5_000)), Forward::Probe);
        assert!(sel.receives_low());
        assert_eq!(sel.congested(2, at(5_500)), Congestion::ProbeFailed);
        // Backoff doubled to eight seconds.
        assert_eq!(sel.on_group(0, 2, false, at(11_000)), Forward::Skip);
        assert_eq!(sel.on_group(0, 2, true, at(14_000)), Forward::Skip);
        assert_eq!(sel.on_group(0, 2, false, at(14_000)), Forward::Probe);
        assert_eq!(sel.on_group(0, 2, false, at(14_500)), Forward::Probe);
        assert_eq!(sel.on_group(0, 2, false, at(16_000)), Forward::Up);
        assert_eq!(sel.rendition(), 0);
        assert_eq!(sel.on_group(1, 2, false, at(16_000)), Forward::Skip);
    }

    #[test]
    fn quiet_period_is_capped_so_recovery_stays_bounded() {
        let start = Instant::now();
        let mut sel = Selection::new(true, start);
        let mut now = start;
        assert_eq!(sel.congested(2, now), Congestion::Down);
        for _ in 0..8 {
            now += sel.quiet;
            assert_eq!(sel.on_group(0, 2, false, now), Forward::Probe);
            assert_eq!(sel.congested(2, now), Congestion::ProbeFailed);
        }
        assert_eq!(sel.quiet, QUIET_MAX);
    }

    #[test]
    fn legacy_viewers_and_single_rendition_tracks_never_switch() {
        let now = Instant::now();
        let mut legacy = Selection::new(false, now);
        assert_eq!(legacy.congested(2, now), Congestion::None);
        assert_eq!(legacy.set_cap(1, now), Cap::Unchanged);
        assert_eq!(legacy.on_group(1, 2, false, now), Forward::Skip);
        let mut single = Selection::new(true, now);
        assert_eq!(single.congested(1, now), Congestion::None);
        assert_eq!(single.on_group(0, 1, true, now), Forward::Normal);
    }

    #[test]
    fn preference_caps_pick_the_highest_rendition_that_fits() {
        let renditions = [
            v1::Rendition {
                index: 0,
                width: 1920,
                height: 1080,
                fps: 30,
                ..Default::default()
            },
            v1::Rendition {
                index: 1,
                width: 640,
                height: 360,
                fps: 15,
                ..Default::default()
            },
        ];
        assert_eq!(cap(&renditions, 0, 0), 0);
        assert_eq!(cap(&renditions, 1080, 30), 0);
        assert_eq!(cap(&renditions, 720, 0), 1);
        assert_eq!(cap(&renditions, 0, 15), 1);
        assert_eq!(cap(&renditions[..1], 360, 0), 0);
        let now = Instant::now();
        let mut sel = Selection::new(true, now);
        assert_eq!(sel.set_cap(1, now), Cap::Down);
        assert_eq!(sel.on_group(0, 2, false, now + QUIET_MAX), Forward::Skip);
        assert_eq!(sel.set_cap(0, now), Cap::Unchanged);
        assert_eq!(sel.on_group(0, 2, false, now), Forward::Probe);
        // A cap set while that probe runs ends it; the next full group is
        // never a move up.
        assert_eq!(sel.set_cap(1, now), Cap::ProbeEnded);
        assert_eq!(sel.rendition(), 1);
        let later = now + Duration::from_secs(2);
        assert_eq!(sel.on_group(0, 2, false, later), Forward::Skip);
        assert_eq!(sel.on_group(1, 2, false, later), Forward::Normal);
    }
}
