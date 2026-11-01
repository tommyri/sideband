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
                retry_after_ms: None,
            })),
        }
    }

    /// Builds a `RATE_LIMITED` [`Error`] that says when to retry.
    ///
    /// `retry_after` is rounded up to whole milliseconds and saturates at
    /// `u32::MAX`, so a client never retries early.
    #[must_use]
    pub fn rate_limited(
        request_id: u32,
        message: impl Into<String>,
        retry_after: std::time::Duration,
    ) -> Self {
        let mut envelope = Self::error(request_id, ErrorCode::RateLimited, message);
        if let Some(server_envelope::Msg::Error(error)) = envelope.msg.as_mut() {
            error.retry_after_ms = Some(retry_after_ms(retry_after));
        }
        envelope
    }
}

fn retry_after_ms(wait: std::time::Duration) -> u32 {
    let micros = wait.as_micros();
    let millis = micros.div_ceil(1000);
    u32::try_from(millis).unwrap_or(u32::MAX)
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
        assert_eq!(error.retry_after_ms, None);
    }

    #[test]
    fn rate_limited_rounds_the_wait_up_to_whole_milliseconds() {
        use std::time::Duration;
        for (wait, expected) in [
            (Duration::ZERO, 0),
            (Duration::from_micros(1), 1),
            (Duration::from_millis(1500), 1500),
            (Duration::from_micros(1_500_001), 1501),
            (Duration::from_secs(u64::MAX), u32::MAX),
        ] {
            let envelope = ServerEnvelope::rate_limited(4, "slow down", wait);
            let Some(server_envelope::Msg::Error(error)) = envelope.msg else {
                panic!("expected error");
            };
            assert_eq!(error.code, i32::from(ErrorCode::RateLimited));
            assert_eq!(error.request_id, 4);
            assert_eq!(error.retry_after_ms, Some(expected), "{wait:?}");
        }
    }
}
