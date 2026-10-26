//! Headless SocialSpeak client.
//!
//! Connection management, the mirrored server state, and the voice session
//! live here. The desktop shell and `ss-cli` are both thin wrappers over
//! this crate.

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
