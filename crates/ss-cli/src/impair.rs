//! In-process impairment for outgoing datagrams. Decisions depend only on the
//! packet sequence number, so a given call drops and delays the same packets
//! every time.

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use bytes::Bytes;

/// `loss`, `jitter`, or `reorder` could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ImpairError {
    /// A key other than `loss`, `jitter`, or `reorder`.
    #[error("unknown impairment key")]
    Key,
    /// A value was missing or not an integer in range.
    #[error("invalid impairment value for {0}")]
    Value(&'static str),
}

/// Loss and reorder are percents. Jitter is extra delay up to this many milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Impairment {
    /// Drop this percentage of packets.
    pub loss_percent: u8,
    /// Spread send times across `0..=jitter_ms`.
    pub jitter_ms: u16,
    /// Hold this percentage of packets for one extra 20 ms frame.
    pub reorder_percent: u8,
}

impl Impairment {
    /// Parse `loss=5,jitter=30,reorder=1`. Empty text is a no-op.
    pub fn parse(text: &str) -> Result<Self, ImpairError> {
        let mut impairment = Self::default();
        if text.is_empty() {
            return Ok(impairment);
        }
        for item in text.split(',') {
            let Some((key, value)) = item.split_once('=') else {
                return Err(ImpairError::Value("impairment"));
            };
            match key {
                "loss" => impairment.loss_percent = percent(value, "loss")?,
                "jitter" => {
                    impairment.jitter_ms =
                        value.parse().map_err(|_| ImpairError::Value("jitter"))?;
                }
                "reorder" => impairment.reorder_percent = percent(value, "reorder")?,
                _ => return Err(ImpairError::Key),
            }
        }
        Ok(impairment)
    }

    /// Whether this sequence number is discarded.
    #[must_use]
    pub fn drops(self, seq: u16) -> bool {
        self.loss_percent > 0 && u32::from(seq) % 100 < u32::from(self.loss_percent)
    }

    /// Extra delay applied before the datagram is written to QUIC.
    #[must_use]
    pub fn delay(self, seq: u16) -> Duration {
        let mut millis = 0_u64;
        if self.jitter_ms > 0 {
            millis += u64::from(seq % (self.jitter_ms.saturating_add(1)));
        }
        if self.reorder_percent > 0 && u32::from(seq) % 100 < u32::from(self.reorder_percent) {
            millis += 20;
        }
        Duration::from_millis(millis)
    }
}

fn percent(value: &str, key: &'static str) -> Result<u8, ImpairError> {
    let parsed: u8 = value.parse().map_err(|_| ImpairError::Value(key))?;
    if parsed > 100 {
        return Err(ImpairError::Value(key));
    }
    Ok(parsed)
}

/// Holds packets until their impaired send time. Not sorted: a reordered
/// packet stays queued while a later packet leaves.
#[derive(Debug, Default)]
pub struct Scheduler {
    impairment: Impairment,
    queue: VecDeque<(Instant, Bytes)>,
}

impl Scheduler {
    /// Queue packets according to `impairment`.
    #[must_use]
    pub fn new(impairment: Impairment) -> Self {
        Self {
            impairment,
            queue: VecDeque::new(),
        }
    }

    /// Drop or schedule one datagram. `seq` is the media header sequence.
    pub fn enqueue(&mut self, seq: u16, packet: Bytes, now: Instant) {
        if self.impairment.drops(seq) {
            return;
        }
        self.queue
            .push_back((now + self.impairment.delay(seq), packet));
    }

    /// Soonest queued send, if any packet is waiting.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.queue.iter().map(|(at, _)| *at).min()
    }

    /// Remove every packet whose send time has arrived. Later packets may
    /// leave before earlier ones when reorder delay says so.
    pub fn drain_due(&mut self, now: Instant) -> Vec<Bytes> {
        let mut due = Vec::new();
        let mut waiting = VecDeque::new();
        while let Some((at, packet)) = self.queue.pop_front() {
            if at <= now {
                due.push(packet);
            } else {
                waiting.push_back((at, packet));
            }
        }
        self.queue = waiting;
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_is_exact_over_each_hundred_packets() {
        let impairment = Impairment::parse("loss=5,jitter=30,reorder=1").unwrap();
        assert_eq!(impairment.loss_percent, 5);
        assert_eq!(impairment.jitter_ms, 30);
        assert_eq!(impairment.reorder_percent, 1);
        let drops = (0..1_000_u16).filter(|seq| impairment.drops(*seq)).count();
        assert_eq!(drops, 50);
    }

    #[test]
    fn full_loss_schedules_nothing_and_reorder_inverts_neighbors() {
        let mut scheduler = Scheduler::new(Impairment {
            loss_percent: 100,
            ..Impairment::default()
        });
        let now = Instant::now();
        scheduler.enqueue(1, Bytes::from_static(b"a"), now);
        assert_eq!(scheduler.drain_due(now).len(), 0);

        let mut scheduler = Scheduler::new(Impairment {
            reorder_percent: 1,
            ..Impairment::default()
        });
        scheduler.enqueue(0, Bytes::from_static(b"first"), now);
        scheduler.enqueue(1, Bytes::from_static(b"second"), now);
        assert_eq!(
            scheduler.drain_due(now),
            vec![Bytes::from_static(b"second")]
        );
        assert_eq!(
            scheduler.drain_due(now + Duration::from_millis(20)),
            vec![Bytes::from_static(b"first")]
        );
    }
}
