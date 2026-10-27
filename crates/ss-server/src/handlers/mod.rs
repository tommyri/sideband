//! Control-stream command handlers.
//!
//! [`roles`] answers role and override commands. [`presence`] answers
//! `SetPresence`. The connection sends the ack, then the event, and publishes
//! the event for other sessions.

mod channels;
pub(crate) mod messages;
mod presence;
mod roles;

pub use channels::Channels;
pub use messages::Messages;
pub use presence::Presence;
pub use roles::{CommandReply, Error, Roles};
