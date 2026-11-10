//! Errors across the headless-client boundary.

/// Transport, server, persistence, and local-state failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// QUIC/TLS setup, including actionable certificate fingerprints.
    #[error(transparent)]
    Network(#[from] ss_net::Error),
    /// Control stream failed. A mutating command must not be retried blindly.
    #[error(transparent)]
    Control(#[from] ss_net::control::ControlError),
    /// Server rejected a command.
    #[error("server error {code}: {message}")]
    Server {
        /// Protocol error code.
        code: i32,
        /// Explanation.
        message: String,
        /// How long to wait before the same request can succeed. Set for
        /// `RATE_LIMITED`.
        retry_after: Option<std::time::Duration>,
        /// Lowest protocol version the server accepts (`PROTOCOL_VERSION`).
        min_version: Option<u32>,
        /// Highest protocol version the server accepts (`PROTOCOL_VERSION`).
        max_version: Option<u32>,
    },
    /// No command is sent while disconnected or reconnecting.
    #[error("client is disconnected")]
    Disconnected,
    /// Malformed peer data or unexpected reply kind.
    #[error("protocol: {0}")]
    Protocol(&'static str),
    /// Deadline expired; delivery of a mutation may be uncertain.
    #[error("request timed out; its outcome may be unknown")]
    Timeout,
    /// Host persistence failed.
    #[error("storage: {0}")]
    Storage(String),
    /// HTTPS transport failed.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// Invalid HTTP JSON payload.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// State changed during a history request; retry against the current snapshot.
    #[error("state changed while loading history; retry the page")]
    StateChanged,
    /// Local voice device, codec, or configuration failure.
    #[error("voice: {0}")]
    Voice(String),
}

impl From<ss_proto::v1::Error> for Error {
    fn from(error: ss_proto::v1::Error) -> Self {
        Self::Server {
            code: error.code,
            message: error.message,
            retry_after: error
                .retry_after_ms
                .map(|wait| std::time::Duration::from_millis(u64::from(wait))),
            min_version: error.min_version,
            max_version: error.max_version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_proto::v1;

    #[test]
    fn server_errors_keep_the_wait_and_the_version_range() {
        let Error::Server {
            code,
            retry_after,
            min_version,
            max_version,
            ..
        } = Error::from(v1::Error {
            code: v1::ErrorCode::RateLimited.into(),
            retry_after_ms: Some(1500),
            min_version: Some(1),
            max_version: Some(2),
            ..Default::default()
        })
        else {
            unreachable!()
        };
        assert_eq!(code, i32::from(v1::ErrorCode::RateLimited));
        assert_eq!(retry_after, Some(std::time::Duration::from_millis(1500)));
        assert_eq!((min_version, max_version), (Some(1), Some(2)));
    }
}
