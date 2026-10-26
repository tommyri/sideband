//! Parse and validation errors. This crate does not return I/O failures.

use thiserror::Error;

/// A value could not be parsed or was outside the domain the schema allows.
///
/// Network, database, and filesystem errors belong to the crate that performs
/// that I/O.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Crockford text was not 26 characters.
    #[error("invalid id length")]
    InvalidIdLength,

    /// Crockford text contained a character outside the alphabet.
    #[error("invalid id character")]
    InvalidIdCharacter,

    /// An id byte buffer was not exactly 16 bytes.
    #[error("id must be 16 bytes, got {length}")]
    InvalidIdByteLength {
        /// Length of the buffer that was offered.
        length: usize,
    },

    /// `channels.kind` was not a known discriminant.
    #[error("unknown channel kind {0}")]
    UnknownChannelKind(i64),

    /// `users.kind` was not a known discriminant.
    #[error("unknown user kind {0}")]
    UnknownUserKind(i64),

    /// `voice_profile` was not `voice`, `music`, or `low_latency`.
    #[error("unknown voice profile `{value}`")]
    UnknownVoiceProfile {
        /// The text that did not match a profile name.
        value: String,
    },
}

#[cfg(test)]
mod tests {
    use crate::CoreError;

    #[test]
    fn is_a_sendable_error() {
        fn assert_error<T: core::error::Error + Send + Sync>() {}
        assert_error::<CoreError>();
    }
}
