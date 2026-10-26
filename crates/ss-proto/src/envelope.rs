//! Constructors for server envelopes.
//!
//! `Ack` and `Error` carry `request_id` both on the envelope and inside the
//! payload, matching the field list in
//! [docs/plan/02-protocol.md](../../../docs/plan/02-protocol.md). These
//! constructors set both copies to the same value.

use crate::v1::{Ack, Error, ErrorCode, ServerEnvelope, ack, server_envelope};

impl ServerEnvelope {
    /// Builds an [`Ack`] for `request_id` with `result`.
    ///
    /// `oneof` names on `result` (`empty`, `message_id`, `channel_id`,
    /// `invite`, `dm_channel`, `role`, `user`) are stable JSON keys.
    #[must_use]
    pub fn ack(request_id: u32, result: ack::Result) -> Self {
        Self {
            request_id,
            msg: Some(server_envelope::Msg::Ack(Ack {
                request_id,
                result: Some(result),
            })),
        }
    }

    /// Builds an [`Error`] for `request_id`.
    ///
    /// `min_version` and `max_version` are left unset. Version mismatches use
    /// [`crate::unsupported_version`], which fills them from [`crate::SUPPORTED_RANGE`].
    #[must_use]
    pub fn error(request_id: u32, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            request_id,
            msg: Some(server_envelope::Msg::Error(Error {
                request_id,
                code: i32::from(code),
                message: message.into(),
                min_version: None,
                max_version: None,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::v1::{ErrorCode, ServerEnvelope, ack, server_envelope};

    #[test]
    fn ack_copies_request_id_into_the_payload() {
        let envelope = ServerEnvelope::ack(11, ack::Result::Empty(crate::v1::Empty::default()));
        assert_eq!(envelope.request_id, 11);
        let Some(server_envelope::Msg::Ack(ack)) = envelope.msg else {
            panic!("expected ack");
        };
        assert_eq!(ack.request_id, 11);
        assert!(matches!(ack.result, Some(ack::Result::Empty(_))));
    }

    #[test]
    fn error_copies_request_id_and_leaves_version_bounds_empty() {
        let envelope = ServerEnvelope::error(12, ErrorCode::NotFound, "missing channel");
        assert_eq!(envelope.request_id, 12);
        let Some(server_envelope::Msg::Error(error)) = envelope.msg else {
            panic!("expected error");
        };
        assert_eq!(error.request_id, 12);
        assert_eq!(error.code, i32::from(ErrorCode::NotFound));
        assert_eq!(error.message, "missing channel");
        assert_eq!(error.min_version, None);
        assert_eq!(error.max_version, None);
    }
}
