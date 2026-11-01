//! Lock-free counters and histograms shared by every bot in the process.
//!
//! The receive path records one histogram sample per datagram, so recording
//! is a single relaxed atomic add. Reports diff two snapshots.

use std::{
    collections::BTreeMap,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// Sub-buckets per power of two. Eight keeps the relative error near 6 %.
const SUB: u32 = 8;
/// Values up to 2^40 µs (about 12 days) get their own bucket.
const BUCKETS: usize = 8 + 38 * SUB as usize;

/// Log-linear histogram of microsecond values.
pub struct Histogram {
    buckets: Box<[AtomicU64]>,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: (0..BUCKETS).map(|_| AtomicU64::new(0)).collect(),
        }
    }
}

impl Histogram {
    /// Adds one sample.
    pub fn record(&self, value: Duration) {
        let micros = u64::try_from(value.as_micros()).unwrap_or(u64::MAX);
        self.buckets[bucket(micros)].fetch_add(1, Ordering::Relaxed);
    }

    /// Copies the bucket counts.
    pub fn snapshot(&self) -> Distribution {
        Distribution {
            counts: self
                .buckets
                .iter()
                .map(|count| count.load(Ordering::Relaxed))
                .collect(),
        }
    }
}

/// Bucket counts at one instant. Subtract two to get an interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distribution {
    counts: Vec<u64>,
}

impl Distribution {
    /// Samples in this distribution.
    pub fn count(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// Samples recorded since `earlier`.
    #[must_use]
    pub fn since(&self, earlier: &Self) -> Self {
        Self {
            counts: self
                .counts
                .iter()
                .zip(&earlier.counts)
                .map(|(now, then)| now.saturating_sub(*then))
                .collect(),
        }
    }

    /// Upper edge of the bucket holding quantile `q` (`0.0..=1.0`).
    pub fn quantile(&self, q: f64) -> Option<Duration> {
        let total = self.count();
        if total == 0 {
            return None;
        }
        let rank = q.clamp(0.0, 1.0) * as_f64(total);
        let mut seen = 0_u64;
        for (index, count) in self.counts.iter().enumerate() {
            seen += count;
            if *count > 0 && as_f64(seen) >= rank {
                return Some(Duration::from_micros(upper(index)));
            }
        }
        None
    }

    /// Largest recorded bucket edge.
    pub fn max(&self) -> Option<Duration> {
        self.quantile(1.0)
    }
}

fn bucket(micros: u64) -> usize {
    if micros < u64::from(SUB) {
        return usize::try_from(micros).unwrap_or(0);
    }
    let msb = micros.ilog2();
    let sub = (micros >> (msb - 3)) & u64::from(SUB - 1);
    let index = (msb - 2) * SUB + u32::try_from(sub).unwrap_or(0);
    usize::try_from(index)
        .unwrap_or(BUCKETS - 1)
        .min(BUCKETS - 1)
}

fn upper(index: usize) -> u64 {
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    if index < SUB {
        return u64::from(index);
    }
    let msb = index / SUB + 2;
    let sub = u64::from(index % SUB);
    let base = 1_u64 << msb;
    base + (sub + 1) * (base >> 3) - 1
}

fn as_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

/// Every counter a report line prints.
#[derive(Default)]
pub struct Stats {
    /// Bots with an authenticated session.
    pub online: AtomicU64,
    /// Bots seated in voice with a track.
    pub in_voice: AtomicU64,
    /// Audio datagrams handed to Quinn.
    pub sent: AtomicU64,
    /// `send_datagram` refusals.
    pub send_errors: AtomicU64,
    /// Audio datagrams read by any bot.
    pub received: AtomicU64,
    /// Sequence gaps seen by receivers, summed across tracks.
    pub lost: AtomicU64,
    /// Datagrams that arrived after a later sequence number.
    pub reordered: AtomicU64,
    /// Receive minus send time from the datagram timestamp.
    pub relay: Histogram,
    /// Messages sent.
    pub text_sent: AtomicU64,
    /// Messages the server acknowledged.
    pub text_acked: AtomicU64,
    /// Messages the server rejected or never answered.
    pub text_failed: AtomicU64,
    /// `SendMessage` to `Ack`.
    pub text_ack: Histogram,
    /// Successful connect + Hello + join.
    pub sessions: AtomicU64,
    /// Failed connect, Hello, or join attempts.
    pub connect_failures: AtomicU64,
    /// Connect start to seated in voice.
    pub join: Histogram,
    /// Sessions the bot did not end: server close, idle timeout, reset.
    pub forced: AtomicU64,
    /// Voice leaves initiated by churn.
    pub churn_leaves: AtomicU64,
    /// Reconnect storms started.
    pub storms: AtomicU64,
    /// Storm signal to seated again, per bot.
    pub storm_rejoin: Histogram,
    /// Why forced sessions ended. Rare, so a lock is fine.
    forced_reasons: Mutex<BTreeMap<String, u64>>,
}

impl Stats {
    /// Increments `counter` by one.
    pub fn bump(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    /// Records why a session ended without the bot asking.
    pub fn note_forced(&self, reason: &str) {
        let mut reasons = self
            .forced_reasons
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *reasons.entry(reason.to_owned()).or_default() += 1;
    }

    /// Forced-disconnect reasons with their counts.
    pub fn forced_reasons(&self) -> BTreeMap<String, u64> {
        self.forced_reasons
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Copies every counter for a report.
    pub fn snapshot(&self) -> Snapshot {
        let load = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        Snapshot {
            online: load(&self.online),
            in_voice: load(&self.in_voice),
            sent: load(&self.sent),
            send_errors: load(&self.send_errors),
            received: load(&self.received),
            lost: load(&self.lost),
            reordered: load(&self.reordered),
            relay: self.relay.snapshot(),
            text_sent: load(&self.text_sent),
            text_acked: load(&self.text_acked),
            text_failed: load(&self.text_failed),
            text_ack: self.text_ack.snapshot(),
            sessions: load(&self.sessions),
            connect_failures: load(&self.connect_failures),
            join: self.join.snapshot(),
            forced: load(&self.forced),
            churn_leaves: load(&self.churn_leaves),
            storms: load(&self.storms),
            storm_rejoin: self.storm_rejoin.snapshot(),
        }
    }
}

/// Plain copy of [`Stats`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct Snapshot {
    pub online: u64,
    pub in_voice: u64,
    pub sent: u64,
    pub send_errors: u64,
    pub received: u64,
    pub lost: u64,
    pub reordered: u64,
    pub relay: Distribution,
    pub text_sent: u64,
    pub text_acked: u64,
    pub text_failed: u64,
    pub text_ack: Distribution,
    pub sessions: u64,
    pub connect_failures: u64,
    pub join: Distribution,
    pub forced: u64,
    pub churn_leaves: u64,
    pub storms: u64,
    pub storm_rejoin: Distribution,
}

/// Header for [`csv_row`].
pub const CSV_HEADER: &str = "t_s,online,in_voice,sent,received,lost,relay_p50_ms,relay_p99_ms,relay_max_ms,text_acked,text_p50_ms,text_p99_ms,forced,sessions,connect_failures";

/// One CSV line for the interval between `earlier` and `now`.
pub fn csv_row(elapsed: Duration, now: &Snapshot, earlier: &Snapshot) -> String {
    let relay = now.relay.since(&earlier.relay);
    let text = now.text_ack.since(&earlier.text_ack);
    format!(
        "{:.0},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        elapsed.as_secs_f64(),
        now.online,
        now.in_voice,
        now.sent - earlier.sent,
        now.received - earlier.received,
        now.lost - earlier.lost,
        millis(relay.quantile(0.5)),
        millis(relay.quantile(0.99)),
        millis(relay.max()),
        now.text_acked - earlier.text_acked,
        millis(text.quantile(0.5)),
        millis(text.quantile(0.99)),
        now.forced,
        now.sessions,
        now.connect_failures,
    )
}

/// Milliseconds with two decimals, or empty for no samples.
pub fn millis(value: Option<Duration>) -> String {
    value.map_or_else(String::new, |value| {
        format!("{:.2}", value.as_secs_f64() * 1_000.0)
    })
}

/// Loss as a percent of expected packets (received plus gaps).
pub fn loss_percent(received: u64, lost: u64) -> f64 {
    let expected = received + lost;
    if expected == 0 {
        0.0
    } else {
        as_f64(lost) * 100.0 / as_f64(expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_monotonic_and_cover_their_values() {
        let mut previous = 0;
        for micros in (0..200_000_u64).step_by(7).chain([u64::MAX / 2, u64::MAX]) {
            let index = bucket(micros);
            assert!(index >= previous, "bucket order broke at {micros}");
            previous = index;
            if index < BUCKETS - 1 {
                assert!(upper(index) >= micros, "{micros} above its bucket edge");
                if index > 0 {
                    assert!(upper(index - 1) < micros, "{micros} below its bucket");
                }
            }
        }
    }

    #[test]
    fn quantiles_stay_within_the_bucket_error() {
        let histogram = Histogram::default();
        for micros in 1..=10_000_u64 {
            histogram.record(Duration::from_micros(micros));
        }
        let all = histogram.snapshot();
        assert_eq!(all.count(), 10_000);
        let median = all.quantile(0.5).unwrap().as_secs_f64() * 1e6;
        assert!((median - 5_000.0).abs() / 5_000.0 < 0.13, "median {median}");
        let p99 = all.quantile(0.99).unwrap().as_secs_f64() * 1e6;
        assert!((p99 - 9_900.0).abs() / 9_900.0 < 0.13, "p99 {p99}");
        assert!(all.max().unwrap() >= Duration::from_micros(10_000));
    }

    #[test]
    fn interval_distribution_subtracts_the_earlier_snapshot() {
        let histogram = Histogram::default();
        histogram.record(Duration::from_millis(1));
        let first = histogram.snapshot();
        histogram.record(Duration::from_millis(50));
        let interval = histogram.snapshot().since(&first);
        assert_eq!(interval.count(), 1);
        assert!(interval.quantile(0.0).unwrap() >= Duration::from_millis(50));
        assert_eq!(Histogram::default().snapshot().quantile(0.5), None);
    }

    #[test]
    fn csv_row_matches_the_header() {
        let stats = Stats::default();
        let earlier = stats.snapshot();
        Stats::bump(&stats.sent);
        stats.relay.record(Duration::from_micros(700));
        let row = csv_row(Duration::from_secs(10), &stats.snapshot(), &earlier);
        assert_eq!(row.split(',').count(), CSV_HEADER.split(',').count());
        assert!(row.starts_with("10,0,0,1,"));
    }

    #[test]
    fn loss_is_a_share_of_expected_packets() {
        assert!(loss_percent(0, 0).abs() < f64::EPSILON);
        assert!((loss_percent(999, 1) - 0.1).abs() < 1e-9);
    }
}
