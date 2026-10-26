//! Protocol version range.
//!
//! [docs/plan/02-protocol.md](../../../docs/plan/02-protocol.md) section 7:
//! `Hello.protocol_version` is 1. A server accepts a closed range and rejects
//! anything outside it with `Error` code `PROTOCOL_VERSION` plus `min_version`
//! and `max_version`.

use crate::v1::{Error, ErrorCode, ServerEnvelope, server_envelope};

/// Protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Inclusive range of `Hello.protocol_version` values this crate accepts.
pub const SUPPORTED_RANGE: ProtocolVersionRange = ProtocolVersionRange {
    min: PROTOCOL_VERSION,
    max: PROTOCOL_VERSION,
};

/// Inclusive minimum and maximum protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolVersionRange {
    /// Lowest accepted `Hello.protocol_version`.
    pub min: u32,
    /// Highest accepted `Hello.protocol_version`.
    pub max: u32,
}

impl ProtocolVersionRange {
    /// Returns whether `version` sits inside this range.
    #[must_use]
    pub const fn contains(self, version: u32) -> bool {
        version >= self.min && version <= self.max
    }
}

/// Returns whether `version` is inside [`SUPPORTED_RANGE`].
#[must_use]
pub const fn is_supported_version(version: u32) -> bool {
    SUPPORTED_RANGE.contains(version)
}

/// Builds a `PROTOCOL_VERSION` error carrying [`SUPPORTED_RANGE`].
///
/// The envelope `request_id` and the inner `Error.request_id` are both set to
/// `request_id`.
#[must_use]
pub fn unsupported_version(request_id: u32) -> ServerEnvelope {
    ServerEnvelope {
        request_id,
        msg: Some(server_envelope::Msg::Error(Error {
            request_id,
            code: i32::from(ErrorCode::ProtocolVersion),
            message: "unsupported protocol version".to_owned(),
            min_version: Some(SUPPORTED_RANGE.min),
            max_version: Some(SUPPORTED_RANGE.max),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::{PROTOCOL_VERSION, SUPPORTED_RANGE, is_supported_version, unsupported_version};
    use crate::v1::{ErrorCode, server_envelope};

    #[test]
    fn only_version_one_is_supported() {
        assert_eq!(PROTOCOL_VERSION, 1);
        assert!(is_supported_version(1));
        assert!(!is_supported_version(0));
        assert!(!is_supported_version(2));
        assert!(SUPPORTED_RANGE.contains(1));
        assert!(!SUPPORTED_RANGE.contains(0));
    }

    #[test]
    fn mismatch_error_carries_the_range() {
        let envelope = unsupported_version(4);
        assert_eq!(envelope.request_id, 4);
        let Some(server_envelope::Msg::Error(error)) = envelope.msg else {
            panic!("expected error");
        };
        assert_eq!(error.request_id, 4);
        assert_eq!(error.code, i32::from(ErrorCode::ProtocolVersion));
        assert_eq!(error.min_version, Some(1));
        assert_eq!(error.max_version, Some(1));
    }
}
