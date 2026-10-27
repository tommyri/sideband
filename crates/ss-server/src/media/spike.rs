//! Opaque audio fan-out. No payload copies, recipient lists, logging, or awaits
//! on the forwarding path. Quinn owns its transport buffers and scheduling.

use crate::voice::Registry;
use bytes::Bytes;
use ss_core::Id;
use ss_net::media::DatagramHeader;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// Cumulative application-level media counters, excluding transport packet loss.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Received datagrams, including invalid input.
    pub received: u64,
    /// Recipient datagrams accepted by Quinn.
    pub forwarded: u64,
    /// Malformed, unowned, or muted-source datagrams rejected before fan-out.
    pub rejected: u64,
    /// Recipient datagrams dropped for a full queue, closed peer, or MTU error.
    pub dropped: u64,
}

/// Validates track ownership and forwards shared Bytes to channel peers.
pub struct Router {
    registry: Arc<Registry>,
    received: AtomicU64,
    forwarded: AtomicU64,
    rejected: AtomicU64,
    dropped: AtomicU64,
}

impl Router {
    /// Uses the same registry as the control dispatcher.
    pub fn new(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            received: AtomicU64::new(0),
            forwarded: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Forward one complete wire datagram; the payload remains opaque.
    pub fn forward(&self, sender: Id, bytes: &Bytes) {
        self.received.fetch_add(1, Ordering::Relaxed);
        let Ok((header, _)) = DatagramHeader::parse(bytes) else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        let state = self.registry.state.lock();
        let membership = state.peers.get(&sender).and_then(|p| p.membership);
        let Some(member) = membership.filter(|m| m.track == header.track_id && !m.muted) else {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return;
        };
        if let Some(recipients) = state.channels.get(&member.channel) {
            for id in recipients {
                if *id == sender {
                    continue;
                }
                let peer = &state.peers[id];
                if peer.membership.is_some_and(|m| m.deaf) {
                    continue;
                }
                // All application datagram sends hold this lock. Queue capacity
                // can only increase between this check and send, preventing
                // Quinn's otherwise invisible drop-oldest policy.
                if peer.connection.datagram_send_buffer_space() < bytes.len()
                    || peer.connection.send_datagram(bytes.clone()).is_err()
                {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                } else {
                    self.forwarded.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Read counters without touching the membership lock.
    pub fn stats(&self) -> Stats {
        Stats {
            received: self.received.load(Ordering::Relaxed),
            forwarded: self.forwarded.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}
