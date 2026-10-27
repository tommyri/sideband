//! Control-stream command handlers.
//!
//! [`roles`] answers role and override commands. The connection sends the
//! ack, then the event.

mod roles;

pub use roles::{CommandReply, Error, Roles};
