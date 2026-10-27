//! Control-stream command handlers.
//!
//! [`roles`] answers role and override commands. [`presence`] answers
//! `SetPresence`. [`profile`] answers `UpdateProfile`. [`dm`] answers
//! direct messages. The connection sends
//! the ack, then the event, and publishes the event for other sessions.

mod channels;
mod dm;
pub(crate) mod messages;
mod presence;
mod profile;
mod roles;

pub use channels::Channels;
pub(crate) use dm::DirectMessages;
pub use messages::Messages;
pub use presence::Presence;
pub(crate) use profile::Profiles;
pub use roles::{CommandReply, Error, Roles};
