//! Control-stream schema and framing for SocialSpeak protocol v1.
//!
//! Protobuf messages, the length-prefixed control-stream codec, and protocol
//! version checks live here. The wire rules are specified in
//! [docs/plan/02-protocol.md](../../../docs/plan/02-protocol.md).
//!
//! This crate depends on `ss-core` only among SocialSpeak crates. It does not
//! convert to `ss_core::Id` or `ss_core::Permissions`; those types arrive in
//! T-003. Until then, check 16-byte ids and permission bitsets with
//! [`validate_id`] and [`validate_permissions`].

// Generated oneofs put every control message in one enum. Boxing the variants
// would change the public match types, so the size lint is allowed here only.
#[allow(
    clippy::large_enum_variant,
    clippy::missing_const_for_fn,
    clippy::struct_excessive_bools
)]
pub mod v1 {
    //! Generated `socialspeak.v1` messages.
    //!
    //! Compiled from `proto/socialspeak/v1` at build time with `prost`. Field
    //! numbers and `oneof` names match
    //! [docs/plan/02-protocol.md](../../../docs/plan/02-protocol.md) and are
    //! not renumbered.
    include!(concat!(env!("OUT_DIR"), "/socialspeak.v1.rs"));
}

mod envelope;
pub mod frame;
pub mod validate;
pub mod version;

pub use crate::{
    frame::{FrameError, HEADER_LEN, MAX_FRAME_LEN, decode, encode, read_frame, write_frame},
    validate::{
        ADMIN_PERMISSIONS, EVERYONE_PERMISSIONS, ID_LEN, IdError, KNOWN_PERMISSION_BITS,
        PermissionError, validate_id, validate_ids, validate_optional_id, validate_permissions,
    },
    version::{
        PROTOCOL_VERSION, ProtocolVersionRange, SUPPORTED_RANGE, is_supported_version,
        unsupported_version,
    },
};

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
