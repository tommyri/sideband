//! Control-stream command handlers.
//!
//! [`roles`] answers role and override commands. The connection sends the
//! ack, then the event.

mod channels;
mod roles;

pub use channels::Channels;
pub use roles::{CommandReply, Error, Roles};
