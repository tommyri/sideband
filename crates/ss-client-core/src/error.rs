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
}

impl From<ss_proto::v1::Error> for Error {
    fn from(error: ss_proto::v1::Error) -> Self {
        Self::Server {
            code: error.code,
            message: error.message,
        }
    }
}
