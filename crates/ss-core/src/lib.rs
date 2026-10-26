//! Shared domain primitives for SocialSpeak.
//!
//! Ids, permissions, and other types that every crate may use live here.
//! This crate must not depend on the network, the audio graph, or the server.

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-core";

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(crate::NAME, env!("CARGO_PKG_NAME"));
    }
}
