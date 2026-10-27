use std::time::Duration;

use super::{Error, MAX_PACKET_BYTES, native};

/// Validated Opus framing metadata. This validates framing, not coded audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusPacket {
    samples: usize,
    channels: u8,
    frames: usize,
}

impl OpusPacket {
    /// Parse TOC and framing without allocating. Accepts Opus durations through
    /// 120 ms; the stream decoder separately enforces its 10/20 ms duration.
    pub fn toc(packet: &[u8]) -> Result<Self, Error> {
        if packet.is_empty() || packet.len() > MAX_PACKET_BYTES {
            return Err(Error::Packet);
        }
        let (samples, frames) = native::packet_info(packet)?;
        Ok(Self {
            samples,
            frames,
            channels: if packet[0] & 4 == 0 { 1 } else { 2 },
        })
    }

    /// Encoded channel count. A stereo decoder can also accept mono packets.
    pub fn channels(self) -> u8 {
        self.channels
    }
    /// Total samples per channel at 48 kHz across all frames in this packet.
    pub fn samples_per_channel(self) -> usize {
        self.samples
    }
    /// Number of encoded Opus frames in the packet.
    pub fn frames(self) -> usize {
        self.frames
    }
    /// Total packet duration (including all encoded frames).
    pub fn duration(self) -> Duration {
        // All legal Opus durations are multiples of 2.5 ms (120 samples).
        Duration::from_micros(u64::try_from(self.samples / 120).unwrap_or(0) * 2500)
    }
}
