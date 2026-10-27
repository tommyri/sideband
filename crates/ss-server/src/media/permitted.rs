//! Permission-aware datagram fan-out. The hot path allocates nothing.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
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
}

impl Forwarder {
    pub(crate) fn new(rooms: Arc<Mutex<Rooms>>) -> Self {
        Self {
            rooms,
            received: AtomicU64::new(0),
            forwarded: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Forward one complete wire datagram. The payload stays opaque.
    pub fn forward(&self, sender: Id, bytes: &Bytes) {
        self.received.fetch_add(1, Ordering::Relaxed);
        let Ok((header, _)) = DatagramHeader::parse(bytes) else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let rooms = self.rooms.lock();
        let Some(seat) = rooms.seat(sender) else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if seat.track != header.track_id
            || seat.flags.self_mute()
            || seat.flags.server_mute()
            || !seat.flags.can_speak()
        {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let Some(recipients) = rooms.members(seat.channel) else {
            return;
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
    }

    /// Counters since process start. Does not take the membership lock.
    pub fn stats(&self) -> Stats {
        Stats {
            received: self.received.load(Ordering::Relaxed),
            forwarded: self.forwarded.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}
