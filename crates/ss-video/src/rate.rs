//! Sender-wide bitrate control for the full rendition.
//!
//! Additive increase, multiplicative decrease, with hysteresis. The target
//! moves within the preset (one eighth of its bitrate up to all of it) and
//! is an average the encoder holds over many frames: the caller applies it
//! with a live bitrate change that never forces a keyframe.
//!
//! The full rendition is lowered only when the sender's own uplink is short,
//! or when no viewer can be moved to a low rendition instead: with one
//! rendition, any congested viewer counts; with two, only every full-rendition
//! viewer at once, which points at the uplink or the server rather than at
//! one slow viewer.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// How often the caller should feed [`RateController::update`].
pub const RATE_TICK: Duration = Duration::from_millis(500);
/// Bad ticks in a row before a decrease.
const BAD_TICKS: u32 = 2;
/// Good ticks in a row before each increase.
const GOOD_TICKS: u32 = 3;
/// No increase this long after a decrease.
const HOLD: Duration = Duration::from_secs(5);
/// Round-trip rise over the recent minimum that means a queue on the uplink.
const RTT_RISE: Duration = Duration::from_millis(60);
/// Samples the recent minimum round trip spans (twenty seconds).
const RTT_WINDOW: usize = 40;

/// Cumulative counters from the sender's own QUIC connection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkSample {
    /// Video bytes written to group streams.
    pub written: u64,
    /// UDP bytes sent and not declared lost.
    pub delivered: u64,
    /// Smoothed round-trip time.
    pub rtt: Duration,
}

/// The server's latest per-rendition report, reduced to what the rate needs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ViewerReport {
    /// Viewers receiving the full rendition.
    pub full_subscribers: u32,
    /// Of those, viewers whose link fell behind or whose group was reset.
    pub full_congested: u32,
    /// Whether a low rendition is published, so a slow viewer can move to it.
    pub low_available: bool,
}

impl ViewerReport {
    fn congested(self) -> bool {
        if self.low_available {
            self.full_subscribers >= 2 && self.full_congested >= self.full_subscribers
        } else {
            self.full_congested >= 1
        }
    }
}

/// Why the last change happened, for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RateReason {
    /// The preset bitrate, unchanged.
    #[default]
    Preset,
    /// The uplink fell behind or queued.
    Uplink,
    /// Viewers fell behind and none could move to a low rendition.
    Viewers,
    /// Recovering towards the preset.
    Recovering,
}

/// Additive-increase, multiplicative-decrease controller for one rendition.
#[derive(Debug)]
pub struct RateController {
    max: u64,
    min: u64,
    target: u64,
    bad: u32,
    good: u32,
    hold_until: Option<Instant>,
    last: Option<LinkSample>,
    rtts: VecDeque<Duration>,
    viewers: bool,
    reason: RateReason,
}

impl RateController {
    /// Start at the preset bitrate, which is also the ceiling.
    #[must_use]
    pub fn new(preset_bitrate: u64) -> Self {
        Self {
            max: preset_bitrate,
            min: (preset_bitrate / 8).max(100_000),
            target: preset_bitrate,
            bad: 0,
            good: 0,
            hold_until: None,
            last: None,
            rtts: VecDeque::with_capacity(RTT_WINDOW),
            viewers: false,
            reason: RateReason::Preset,
        }
    }

    /// Current target in bits per second.
    #[must_use]
    pub const fn target(&self) -> u64 {
        self.target
    }

    /// Why the target is where it is.
    #[must_use]
    pub const fn reason(&self) -> RateReason {
        self.reason
    }

    /// Record a fresh server report; it stands until the next one.
    pub fn report(&mut self, report: ViewerReport) {
        self.viewers = report.congested();
    }

    /// Feed one sample every [`RATE_TICK`]. Returns the new target when it changed.
    pub fn update(&mut self, now: Instant, sample: LinkSample) -> Option<u64> {
        let uplink = self.uplink(sample);
        let bad = uplink || self.viewers;
        let before = self.target;
        if bad {
            self.good = 0;
            self.bad += 1;
            if self.bad >= BAD_TICKS {
                self.bad = 0;
                self.target = (self.target * 3 / 4).max(self.min);
                self.hold_until = Some(now + HOLD);
                self.reason = if uplink {
                    RateReason::Uplink
                } else {
                    RateReason::Viewers
                };
            }
        } else {
            self.bad = 0;
            self.good += 1;
            if self.good >= GOOD_TICKS
                && self.hold_until.is_none_or(|until| now >= until)
                && self.target < self.max
            {
                self.good = 0;
                self.target = (self.target + self.max / 20).min(self.max);
                self.reason = if self.target == self.max {
                    RateReason::Preset
                } else {
                    RateReason::Recovering
                };
            }
        }
        (self.target != before).then_some(self.target)
    }

    fn uplink(&mut self, sample: LinkSample) -> bool {
        if self.rtts.len() == RTT_WINDOW {
            self.rtts.pop_front();
        }
        self.rtts.push_back(sample.rtt);
        let floor = self.rtts.iter().min().copied().unwrap_or(sample.rtt);
        let Some(last) = self.last.replace(sample) else {
            return false;
        };
        let offered = sample.written.saturating_sub(last.written);
        let delivered = sample.delivered.saturating_sub(last.delivered);
        // QUIC overhead, voice and control make a healthy link deliver more
        // than the video offered; a keyframe drains within the tick.
        offered > delivered + delivered / 2 + 32 * 1024
            || (offered > 0 && sample.rtt.saturating_sub(floor) >= RTT_RISE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy(n: u64) -> LinkSample {
        LinkSample {
            written: n * 250_000,
            delivered: n * 260_000,
            rtt: Duration::from_millis(20),
        }
    }

    #[test]
    fn one_slow_viewer_does_not_lower_the_full_rendition_when_low_exists() {
        let mut rate = RateController::new(4_000_000);
        let now = Instant::now();
        rate.report(ViewerReport {
            full_subscribers: 2,
            full_congested: 1,
            low_available: true,
        });
        for n in 0..20 {
            assert_eq!(rate.update(now + RATE_TICK * n, healthy(n.into())), None);
        }
        assert_eq!(rate.target(), 4_000_000);
    }

    #[test]
    fn without_a_low_rendition_a_slow_viewer_lowers_it_and_recovery_is_gradual() {
        let mut rate = RateController::new(4_000_000);
        let start = Instant::now();
        rate.report(ViewerReport {
            full_subscribers: 2,
            full_congested: 1,
            low_available: false,
        });
        let mut at = start;
        let mut n = 0;
        let mut step = |rate: &mut RateController| {
            n += 1;
            at += RATE_TICK;
            rate.update(at, healthy(n))
        };
        assert_eq!(step(&mut rate), None);
        assert_eq!(step(&mut rate), Some(3_000_000));
        assert_eq!(rate.reason(), RateReason::Viewers);
        for _ in 0..40 {
            step(&mut rate);
        }
        assert_eq!(rate.target(), 500_000);
        rate.report(ViewerReport::default());
        // Held for five seconds, then 5 % of the preset every 1.5 s.
        let mut ups = Vec::new();
        for _ in 0..30 {
            if let Some(target) = step(&mut rate) {
                ups.push(target);
            }
        }
        assert!(ups.windows(2).all(|w| w[1] == w[0] + 200_000), "{ups:?}");
        assert!(!ups.is_empty() && ups[0] == 700_000);
    }

    #[test]
    fn a_short_or_queueing_uplink_lowers_the_target() {
        let mut rate = RateController::new(8_000_000);
        let now = Instant::now();
        let mut sample = healthy(1);
        rate.update(now, sample);
        // Half of what was written got out.
        sample.written += 500_000;
        sample.delivered += 250_000;
        rate.update(now + RATE_TICK, sample);
        sample.written += 500_000;
        sample.delivered += 250_000;
        assert_eq!(rate.update(now + RATE_TICK * 2, sample), Some(6_000_000));
        assert_eq!(rate.reason(), RateReason::Uplink);
        let mut queued = RateController::new(8_000_000);
        let mut sample = healthy(1);
        queued.update(now, sample);
        sample.rtt = Duration::from_millis(150);
        for n in 2..4 {
            sample.written += 250_000;
            sample.delivered += 260_000;
            let changed = queued.update(now + RATE_TICK * n, sample);
            assert_eq!(changed.is_some(), n == 3);
        }
    }
}
