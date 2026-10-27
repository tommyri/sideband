//! Control-stream command handlers.
//!
//! [`roles`] answers role and override commands. [`presence`] answers
//! `SetPresence`. [`profile`] answers `UpdateProfile`. The connection sends
//! the ack, then the event, and publishes the event for other sessions.

mod channels;
pub(crate) mod messages;
mod presence;
mod profile;
mod roles;

pub use channels::Channels;
pub use messages::Messages;
pub use presence::Presence;
pub(crate) use profile::Profiles;
pub use roles::{CommandReply, Error, Roles};
