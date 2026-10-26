//! Control-message schema and framing.
//!
//! Protobuf messages, the length-prefixed control-stream codec, and protocol
//! version negotiation live here. This crate may depend on `ss-core` only.

/// Package name. Dependents reference this so the crate graph stays linked.
pub const NAME: &str = "ss-proto";

/// Crates this layer is built on.
#[must_use]
pub const fn foundation() -> &'static [&'static str] {
    &[ss_core::NAME]
}

#[cfg(test)]
mod tests {
    #[test]
    fn name_matches_package() {
        assert_eq!(crate::NAME, env!("CARGO_PKG_NAME"));
    }

    #[test]
    fn sits_on_core() {
        assert_eq!(crate::foundation(), ["ss-core"]);
    }
}
