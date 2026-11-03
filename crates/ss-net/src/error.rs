//! Transport setup failures, including actionable certificate trust decisions.

use crate::tls::Fingerprint;

/// Failure to configure or establish a Sideband connection.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The caller must explicitly approve this first-use certificate.
    #[error("untrusted server certificate: {fingerprint}")]
    Untrusted {
        /// SHA-256 of the leaf certificate DER.
        fingerprint: Fingerprint,
    },
    /// A stored pin differs, even if the replacement has a valid public chain.
    #[error("server fingerprint changed from {expected} to {received}")]
    FingerprintChanged {
        /// Previously accepted certificate.
        expected: Fingerprint,
        /// Certificate presented now.
        received: Fingerprint,
    },
    /// Filesystem or socket error.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Invalid persisted JSON; never silently reset pins or identities.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// TLS configuration error.
    #[error(transparent)]
    Tls(#[from] rustls::Error),
    /// Certificate generation failed.
    #[error(transparent)]
    Certificate(#[from] rcgen::Error),
    /// QUIC could not start connecting.
    #[error(transparent)]
    Connect(#[from] quinn::ConnectError),
    /// QUIC handshake or connection failed.
    #[error(transparent)]
    Connection(#[from] quinn::ConnectionError),
    /// Blocking persistence task failed.
    #[error(transparent)]
    Task(#[from] tokio::task::JoinError),
    /// Unsupported or invalid local configuration.
    #[error("transport configuration: {0}")]
    Config(&'static str),
    /// The peer did not negotiate QUIC datagrams.
    #[error("peer does not support datagrams")]
    DatagramsRequired,
    /// Only one control stream may be opened per connection.
    #[error("control stream has already been taken")]
    ControlAlreadyTaken,
}
