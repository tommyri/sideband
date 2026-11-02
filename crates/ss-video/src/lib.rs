//! Native screen capture and H.264 encoding, independent of transport.

mod h264;
mod preset;
#[cfg(feature = "capture")]
mod publisher;
#[cfg(feature = "capture")]
mod source;

pub use preset::{Preset, SubscriberHint};
#[cfg(feature = "capture")]
pub use publisher::{CodecInfo, EncodedFrame, EncoderStats, Publisher, PublisherConfig};
#[cfg(feature = "capture")]
pub use source::{Source, SourceKind, Thumbnail, list_sources, thumbnail};

/// Package name for architecture checks.
pub const NAME: &str = env!("CARGO_PKG_NAME");

/// Screen capture and encoding failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Native capture or encoding failed.
    #[error(transparent)]
    Video(#[from] moq_video::Error),
    /// A frame-rate numerator or denominator is invalid.
    #[error(transparent)]
    Rate(#[from] moq_video::RateError),
    /// The selected source stopped producing frames.
    #[error("capture source ended")]
    SourceEnded,
    /// The encoder emitted a non-IDR access unit before the first group.
    #[error("encoder output did not start with an H.264 IDR")]
    MissingKeyframe,
}
