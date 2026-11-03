//! Headless SocialSpeak client.
//!
//! Connection management, the mirrored server state, and the voice session
//! live here. The desktop shell and `ss-cli` are both thin wrappers over
//! this crate.

pub mod diagnostics;
pub mod media;

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-client-core";

/// Crates this layer is built on.
#[must_use]
pub const fn foundation() -> &'static [&'static str] {
    &[ss_core::NAME, ss_net::NAME, ss_audio::NAME, ss_input::NAME]
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(crate::NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn sits_on_net_audio_and_input() {
        assert_eq!(
            crate::foundation(),
            ["ss-core", "ss-net", "ss-audio", "ss-input"]
        );
    }
}

mod commands;
pub mod connection;
mod error;
pub mod events;
mod history;
pub mod state;
pub mod storage;
pub mod voice;
pub use connection::{Client, ClientSet, Credentials, ServerAddr};
pub use error::Error;
pub use events::{ClientEvent, ConnectionState};
/// Protocol command and entity types used at the client boundary.
pub use ss_proto::v1;
pub use state::{State, StateStore};
pub use voice::{
    AudioConfig, AudioSource, Cue, SpeakerStats, TalkGate, VoiceDeviceState, VoiceSession,
    VoiceStats,
};
