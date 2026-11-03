//! Shared domain primitives for Sideband.
//!
//! Ids, permissions, channel and user kinds, voice profiles, and timestamps
//! live here. Parsing failures are [`CoreError`]. This crate does not perform
//! network or storage I/O. The optional `sqlx` feature maps [`Id`] to a
//! 16-byte SQLite `BLOB`.

#![deny(missing_docs)]

mod channel;
mod error;
mod id;
mod permissions;
mod timestamp;
mod user;
mod voice;

pub use channel::ChannelKind;
pub use error::CoreError;
pub use id::Id;
pub use permissions::{PermissionOverride, Permissions};
pub use timestamp::Timestamp;
pub use user::UserKind;
pub use voice::{EncoderParams, OpusApplication, VoiceProfile};

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-core";

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(crate::NAME, env!("CARGO_PKG_NAME"));
    }
}
