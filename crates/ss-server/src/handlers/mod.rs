//! Control-stream command handlers.
//!
//! [`roles`] answers role and override commands. [`presence`] answers
//! `SetPresence`. [`profile`] answers `UpdateProfile`. [`dm`] answers
//! direct messages. [`bots`] answers bot account commands. The connection sends
//! the ack, then the event, and publishes the event for other sessions.

pub mod bots;
mod channels;
mod dm;
pub(crate) mod messages;
mod moderation;
mod presence;
mod profile;
mod roles;
mod webhooks;

pub(crate) use bots::Bots;
pub use channels::Channels;
pub(crate) use dm::DirectMessages;
pub use messages::Messages;
pub(crate) use moderation::Moderation;
pub use moderation::{ban_account, grant_admin, unban_account};
pub use presence::Presence;
pub(crate) use profile::{Profiles, member};
pub use roles::{CommandReply, Error, Roles};
pub use webhooks::Webhooks;
