//! Allocation-free streaming Opus at 48 kHz. Construct codecs off the audio thread.
//!
//! PCM is interleaved, normalized floating point. Returned slices borrow reusable
//! storage and must be consumed before the next call. Decoder state belongs to
//! one ordered stream; recover a lost frame before decoding the following packet.

#![deny(missing_docs)]

mod decoder;
mod encoder;
// opusic-c does not expose decoder complexity outside its DRED feature, nor
// arbitrary DRED offsets. Keep the missing bindings in one audited boundary.
#[allow(unsafe_code)]
mod native;
mod packet;

pub use decoder::Decoder;
pub use encoder::Encoder;
pub use packet::OpusPacket;

/// Codec sample rate for all voice profiles.
pub const SAMPLE_RATE: u32 = 48_000;
/// Maximum Opus payload in SocialSpeak's media envelope (protocol §4).
/// Kept here without importing the networking layer.
pub const MAX_PACKET_BYTES: usize = 1150;

/// Invalid configuration, input, or native codec failure. Errors do not allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Unsupported channel count, application, or frame duration.
    #[error("unsupported codec configuration")]
    Configuration,
    /// PCM must contain exactly one profile frame of finite samples.
    #[error("invalid PCM frame")]
    Pcm,
    /// Bitrate is outside the profile's inclusive limits.
    #[error("bitrate outside profile limits")]
    Bitrate,
    /// Expected packet loss must be between zero and 100 percent.
    #[error("packet loss outside 0..=100")]
    PacketLoss,
    /// Empty, oversized, or malformed packet.
    #[error("invalid Opus packet")]
    Packet,
    /// Packet duration differs from the configured stream duration.
    #[error("packet duration differs from stream configuration")]
    Duration,
    /// DRED duration must be a multiple of 10 ms, at most 1000 ms.
    #[cfg(feature = "dred")]
    #[error("invalid DRED duration or lost frame count")]
    DredDuration,
    /// Packet has insufficient redundancy; the caller can use PLC instead.
    #[cfg(feature = "dred")]
    #[error("insufficient DRED history")]
    DredUnavailable,
    /// Error returned by libopus.
    #[error("Opus error: {0:?}")]
    Opus(opusic_c::ErrorCode),
}

impl From<opusic_c::ErrorCode> for Error {
    fn from(value: opusic_c::ErrorCode) -> Self {
        Self::Opus(value)
    }
}

fn channels(value: u8) -> Result<opusic_c::Channels, Error> {
    match value {
        1 => Ok(opusic_c::Channels::Mono),
        2 => Ok(opusic_c::Channels::Stereo),
        _ => Err(Error::Configuration),
    }
}

fn frame_samples(duration: std::time::Duration) -> Result<usize, Error> {
    match duration {
        d if d == std::time::Duration::from_millis(10) => Ok(480),
        d if d == std::time::Duration::from_millis(20) => Ok(960),
        _ => Err(Error::Configuration),
    }
}

#[cfg(test)]
mod tests;
