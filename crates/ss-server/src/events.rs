//! Domain events shared by every authenticated connection.
//!
//! [`EventBus::publish`] wraps the protobuf message in an [`Arc`]. Each
//! subscriber encodes its own frame after the visibility check. The `Bytes`
//! fields inside the message stay shared across those encodes.

use std::sync::Arc;

use ss_core::Id;
use ss_proto::v1::server_envelope;
use tokio::sync::broadcast;

/// How many events a slow subscriber can miss before it is lagged.
const CAPACITY: usize = 256;

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
        Self { tx }
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
}
