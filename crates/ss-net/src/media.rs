//! Media headers from protocol §4–§5. Payloads remain opaque and borrowed.

pub mod stream;

use bytes::{BufMut, BytesMut};

/// Largest track id in the canonical three-byte unsigned LEB128 encoding.
pub const MAX_TRACK_ID: u32 = (1 << 21) - 1;
/// Maximum Opus payload length, independent of the negotiated path MTU.
pub const MAX_AUDIO_PAYLOAD: usize = 1150;
/// First packet of a talk burst.
pub const TALK_START: u8 = 1;
/// Last packet of a talk burst.
pub const TALK_END: u8 = 2;

/// Invalid or unsupported media header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    /// Not enough bytes for the complete header.
    #[error("truncated media header")]
    Truncated,
    /// Only version zero is defined.
    #[error("unsupported media version")]
    Version,
    /// Datagram audio is kind zero; stream video is kind one.
    #[error("unsupported media kind")]
    Kind,
    /// Reserved flag bits must be zero.
    #[error("reserved media flags")]
    Flags,
    /// Track ids must use canonical unsigned LEB128 in at most three bytes.
    #[error("invalid track id encoding")]
    TrackId,
    /// Audio payload exceeds the wire limit.
    #[error("audio payload exceeds 1150 bytes")]
    PayloadTooLarge,
}

/// Audio datagram header. Sequence and timestamp wrap at their integer widths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatagramHeader {
    /// Wire version (zero).
    pub version: u8,
    /// Media kind (zero for datagrams).
    pub kind: u8,
    /// `TALK_START` and/or `TALK_END`.
    pub flags: u8,
    /// Server-assigned track id.
    pub track_id: u32,
    /// Packet sequence number, big-endian on the wire.
    pub seq: u16,
    /// Timestamp in 48 kHz samples, big-endian on the wire.
    pub timestamp: u32,
}

impl DatagramHeader {
    fn check(self, kind: u8) -> Result<(), MediaError> {
        if self.version != 0 {
            return Err(MediaError::Version);
        }
        if self.kind != kind {
            return Err(MediaError::Kind);
        }
        if self.flags & !(TALK_START | TALK_END) != 0 {
            return Err(MediaError::Flags);
        }
        if self.track_id > MAX_TRACK_ID {
            return Err(MediaError::TrackId);
        }
        Ok(())
    }

    fn encode(self, output: &mut BytesMut, kind: u8) -> Result<(), MediaError> {
        self.check(kind)?;
        output.put_u8((self.kind << 4) | self.flags);
        let mut track = self.track_id;
        loop {
            // Mask guarantees that the value fits in seven bits.
            let byte = (track & 0x7f).to_le_bytes()[0];
            track >>= 7;
            output.put_u8(byte | if track == 0 { 0 } else { 0x80 });
            if track == 0 {
                break;
            }
        }
        output.put_u16(self.seq);
        output.put_u32(self.timestamp);
        Ok(())
    }

    /// Appends the header, leaving `output` unchanged if validation fails.
    pub fn encode_into(self, output: &mut BytesMut) -> Result<(), MediaError> {
        self.encode(output, 0)
    }

    fn decode(input: &[u8], kind: u8) -> Result<(Self, &[u8]), MediaError> {
        let (&first, mut rest) = input.split_first().ok_or(MediaError::Truncated)?;
        let mut track_id = 0;
        let mut count = 0;
        loop {
            let (&byte, tail) = rest.split_first().ok_or(MediaError::Truncated)?;
            rest = tail;
            track_id |= u32::from(byte & 0x7f) << (count * 7);
            count += 1;
            if byte & 0x80 == 0 {
                if count > 1 && byte == 0 {
                    return Err(MediaError::TrackId);
                }
                break;
            }
            if count == 3 {
                return Err(MediaError::TrackId);
            }
        }
        let fixed = rest.get(..6).ok_or(MediaError::Truncated)?;
        let header = Self {
            version: first >> 6,
            kind: (first >> 4) & 3,
            flags: first & 15,
            track_id,
            seq: u16::from_be_bytes([fixed[0], fixed[1]]),
            timestamp: u32::from_be_bytes([fixed[2], fixed[3], fixed[4], fixed[5]]),
        };
        header.check(kind)?;
        Ok((header, &rest[6..]))
    }

    /// Parses a header and returns the original payload slice without copying.
    pub fn parse(input: &[u8]) -> Result<(Self, &[u8]), MediaError> {
        let (header, payload) = Self::decode(input, 0)?;
        if payload.len() > MAX_AUDIO_PAYLOAD {
            return Err(MediaError::PayloadTooLarge);
        }
        Ok((header, payload))
    }
}

/// One video GOP's stream prefix. Frame bodies are defined in protocol §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamHeader {
    /// Common header with kind one (video).
    pub media: DatagramHeader,
    /// Group id in big-endian byte order.
    pub group_id: u32,
}

impl StreamHeader {
    /// Appends a stream prefix; no video codec or subscription policy is involved.
    pub fn encode_into(self, output: &mut BytesMut) -> Result<(), MediaError> {
        self.media.encode(output, 1)?;
        output.put_u32(self.group_id);
        Ok(())
    }

    /// Returns the prefix and a borrowed slice of the remaining stream bytes.
    pub fn parse(input: &[u8]) -> Result<(Self, &[u8]), MediaError> {
        let (media, rest) = DatagramHeader::decode(input, 1)?;
        let group = rest.get(..4).ok_or(MediaError::Truncated)?;
        let group_id = u32::from_be_bytes([group[0], group[1], group[2], group[3]]);
        Ok((Self { media, group_id }, &rest[4..]))
    }
}
