//! Permission-aware datagram fan-out. The hot path never copies payloads or awaits.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use bytes::Bytes;
use parking_lot::Mutex;
use ss_core::Id;
use ss_net::media::DatagramHeader;

use crate::{media::Stats, voice::rooms::Rooms};

/// Forwards datagrams for [`crate::voice::Voice`].
pub struct Forwarder {
    rooms: Arc<Mutex<Rooms>>,
    received: AtomicU64,
    forwarded: AtomicU64,
    rejected: AtomicU64,
    dropped: AtomicU64,
    rate_limited: AtomicU64,
    budgets: Mutex<HashMap<Id, Budget>>,
}

const WINDOW: Duration = Duration::from_secs(1);
const DISCONNECT_AFTER: u8 = 3;

struct Budget {
    started: Instant,
    bytes: usize,
    violations: u8,
}

enum LimitOutcome {
    Allowed,
    Dropped,
    Disconnect(Duration),
}

/// Result that requires action outside the synchronous forwarding path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardOutcome {
    /// The datagram was handled without closing the sender.
    Complete,
    /// Repeated excess datagrams require a control notice and disconnect.
    Disconnect(Duration),
}

impl Forwarder {
    pub(crate) fn new(rooms: Arc<Mutex<Rooms>>) -> Self {
        Self {
            rooms,
            received: AtomicU64::new(0),
            forwarded: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            rate_limited: AtomicU64::new(0),
            budgets: Mutex::new(HashMap::new()),
        }
    }

    /// Forward one complete wire datagram. The payload stays opaque.
    pub fn forward(&self, sender: Id, bytes: &Bytes) -> ForwardOutcome {
        self.received.fetch_add(1, Ordering::Relaxed);
        let Ok((header, _)) = DatagramHeader::parse(bytes) else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return ForwardOutcome::Complete;
        };
        let rooms = self.rooms.lock();
        let Some(seat) = rooms.seat(sender) else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return ForwardOutcome::Complete;
        };
        if seat.track != header.track_id
            || seat.flags.self_mute()
            || seat.flags.server_mute()
            || !seat.flags.can_speak()
        {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return ForwardOutcome::Complete;
        }
        match self.limit(sender, bytes.len(), seat.max_bitrate_bps) {
            LimitOutcome::Allowed => {}
            LimitOutcome::Dropped => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                self.rate_limited.fetch_add(1, Ordering::Relaxed);
                return ForwardOutcome::Complete;
            }
            LimitOutcome::Disconnect(wait) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                self.rate_limited.fetch_add(1, Ordering::Relaxed);
                return ForwardOutcome::Disconnect(wait);
            }
        }
        let Some(recipients) = rooms.members(seat.channel) else {
            return ForwardOutcome::Complete;
        };
        for id in recipients {
            if *id == sender {
                continue;
            }
            let Some((connection, deaf)) = rooms.send_target(*id) else {
                continue;
            };
            if deaf {
                continue;
            }
            if connection.datagram_send_buffer_space() < bytes.len()
                || connection.send_datagram(bytes.clone()).is_err()
            {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            } else {
                self.forwarded.fetch_add(1, Ordering::Relaxed);
            }
        }
        ForwardOutcome::Complete
    }

    fn limit(&self, sender: Id, bytes: usize, max_bitrate_bps: u32) -> LimitOutcome {
        let now = Instant::now();
        let mut budgets = self.budgets.lock();
        let budget = budgets.entry(sender).or_insert(Budget {
            started: now,
            bytes: 0,
            violations: 0,
        });
        if now.duration_since(budget.started) >= WINDOW {
            budget.started = now;
            budget.bytes = 0;
            budget.violations = 0;
        }
        let max_bytes = usize::try_from(max_bitrate_bps).unwrap_or(usize::MAX) * 3 / 2 / 8;
        if budget.bytes.saturating_add(bytes) <= max_bytes {
            budget.bytes += bytes;
            return LimitOutcome::Allowed;
        }
        budget.violations = budget.violations.saturating_add(1);
        if budget.violations >= DISCONNECT_AFTER {
            LimitOutcome::Disconnect(WINDOW.saturating_sub(now.duration_since(budget.started)))
        } else {
            LimitOutcome::Dropped
        }
    }

    /// Counters since process start. Does not take the membership lock.
    pub fn stats(&self) -> Stats {
        Stats {
            received: self.received.load(Ordering::Relaxed),
            forwarded: self.forwarded.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            rate_limited: self.rate_limited.load(Ordering::Relaxed),
        }
    }
}
