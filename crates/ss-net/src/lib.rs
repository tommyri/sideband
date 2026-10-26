//! QUIC transport shared by the server and the client.
//!
//! Endpoint setup, trust-on-first-use certificate pinning, the control
//! stream, and media datagram headers live here. No accounts, channels, or
//! forwarding policy.

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-net";

/// Crates this layer is built on.
#[must_use]
pub const fn foundation() -> &'static [&'static str] {
    &[ss_core::NAME, ss_proto::NAME]
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(crate::NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn sits_on_proto() {
        assert_eq!(crate::foundation(), ["ss-core", "ss-proto"]);
    }
}
