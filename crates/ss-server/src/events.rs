//! Domain events shared by every authenticated connection.
//!
//! [`EventBus::publish`] wraps the protobuf message in an [`Arc`]. Each
//! subscriber encodes its own frame after the visibility check. The `Bytes`
//! fields inside the message stay shared across those encodes.
//!
//! A new message must reach a session that is signing in exactly once:
//! counted in its `Welcome` read states or delivered live, never both. The
//! sender commits and publishes `MessageCreated` under [`EventBus::hold`];
//! the session subscribes and fixes the read point of its snapshot under
//! [`EventBus::quiet`], so every message lands wholly on one side.

use std::sync::Arc;

use ss_core::Id;
use ss_proto::v1::server_envelope;
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard, broadcast};

/// How many events a subscriber may fall behind before it is lagged and
/// closed. Above the connection cap, so one command that moves every seated
/// member out of voice cannot lag every other session.
const CAPACITY: usize = 1024;

/// One published event. `origin` is the connection that already received it
/// on its command reply, so that connection skips the bus copy.
#[derive(Clone, Debug)]
pub struct ServerEvent {
    /// Connection that should not receive a second copy. Absent for presence
    /// changes caused by a disconnect.
    pub origin: Option<Id>,
    /// Shared payload. Encoding happens per recipient.
    pub msg: Arc<server_envelope::Msg>,
}

/// Fan-out channel. Sending never waits on a subscriber.
#[derive(Clone, Debug)]
pub struct EventBus {
    tx: broadcast::Sender<ServerEvent>,
    /// Exclusive from a message's commit to its publish; shared while a
    /// session subscribes and starts its snapshot's read transaction.
    horizon: Arc<RwLock<()>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// Broadcast bus with room for a burst of membership and channel events.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self {
            tx,
            horizon: Arc::new(RwLock::new(())),
        }
    }

    /// Publishes `msg`. A bus with no subscribers drops it.
    pub fn publish(&self, origin: Option<Id>, msg: server_envelope::Msg) {
        let _ = self.tx.send(ServerEvent {
            origin,
            msg: Arc::new(msg),
        });
    }

    /// A new receiver. It does not see events published before this call.
    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.tx.subscribe()
    }

    /// Keeps sessions from subscribing until dropped. Take it, holding the
    /// writer, just before committing a new message, and publish its
    /// `MessageCreated` before dropping it. Only sign-ins wait on it, for one
    /// query each; senders already queue on the writer.
    pub async fn hold(&self) -> RwLockWriteGuard<'_, ()> {
        self.horizon.write().await
    }

    /// Waits until no message is between its commit and its publish, and
    /// keeps it so until dropped. Subscribe and start the snapshot's read
    /// transaction under it, and nothing else: a sender waits meanwhile.
    pub async fn quiet(&self) -> RwLockReadGuard<'_, ()> {
        self.horizon.read().await
    }
}
