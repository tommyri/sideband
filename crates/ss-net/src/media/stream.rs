//! Bounded byte-level video groups. No codec, permission, or subscription policy.
use super::{MediaError, StreamHeader};
use bytes::{Bytes, BytesMut};

/// Maximum encoded frame payload (4 MiB).
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
/// Application reset/stop code: this group is no longer useful.
pub const RESET_GROUP: u32 = 0x70;
/// Keyframe bit in a video frame's flags byte; other bits are reserved.
pub const KEYFRAME: u8 = 1;

/// Framing or QUIC failure. A failed group must be discarded.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Invalid stream prefix.
    #[error(transparent)]
    Header(#[from] MediaError),
    /// QUIC receive failure.
    #[error(transparent)]
    Read(#[from] quinn::ReadError),
    /// QUIC send failure.
    #[error(transparent)]
    Write(#[from] quinn::WriteError),
    /// Connection failed before opening or accepting a group.
    #[error(transparent)]
    Connection(#[from] quinn::ConnectionError),
    /// Truncated, oversized, or invalid frame; or reuse after cancelled write.
    #[error("invalid or incomplete video frame")]
    Frame,
    /// Cannot finish a closed stream.
    #[error("video group already closed")]
    Closed,
}

/// Read just the 12–14 byte stream prefix, leaving the frame bytes untouched.
/// Cancellation drops the owned receive stream; do not reuse a partial prefix.
pub async fn read_header(
    mut recv: quinn::RecvStream,
) -> Result<(StreamHeader, quinn::RecvStream), Error> {
    let header = read_prefix(&mut recv, &mut GroupParser::default()).await?;
    Ok((header, recv))
}

// The parser wants one byte at a time until the prefix parses, so nothing
// after it is read from the stream.
async fn read_prefix(
    recv: &mut quinn::RecvStream,
    parser: &mut GroupParser,
) -> Result<StreamHeader, Error> {
    loop {
        let mut byte = [0_u8; 1];
        recv.read_exact(&mut byte).await.map_err(|_| Error::Frame)?;
        if let Some(Parsed::Header(header)) = parser.feed(&mut &byte[..])? {
            return Ok(header);
        }
    }
}

/// One opaque encoded video frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Presentation time in the shared 48 kHz clock.
    pub timestamp: u32,
    /// Whether this is a keyframe.
    pub keyframe: bool,
    /// Encoded bytes, never interpreted by this module.
    pub payload: Bytes,
}

/// One outgoing GOP. Dropping an unfinished or cancelled write resets it.
pub struct GroupWriter {
    send: quinn::SendStream,
    first: bool,
    writing: bool,
    finished: bool,
}
impl GroupWriter {
    /// Open a uni stream and immediately write its prefix.
    pub async fn open(connection: &quinn::Connection, header: StreamHeader) -> Result<Self, Error> {
        let mut prefix = BytesMut::new();
        header.encode_into(&mut prefix)?;
        let mut writer = Self {
            send: connection.open_uni().await?,
            first: true,
            writing: true,
            finished: false,
        };
        writer.send.write_all(&prefix).await?;
        writer.writing = false;
        Ok(writer)
    }
    /// Append a frame. The first frame must be a keyframe. If this future is
    /// cancelled, drop/reset this writer: subsequent writes are rejected.
    pub async fn write_frame(&mut self, frame: &Frame) -> Result<(), Error> {
        if self.writing
            || frame.payload.is_empty()
            || frame.payload.len() > MAX_FRAME_BYTES
            || (self.first && !frame.keyframe)
        {
            return Err(Error::Frame);
        }
        let length = u32::try_from(frame.payload.len()).map_err(|_| Error::Frame)?;
        let mut prefix = [0_u8; 9];
        prefix[..4].copy_from_slice(&length.to_be_bytes());
        prefix[4..8].copy_from_slice(&frame.timestamp.to_be_bytes());
        prefix[8] = u8::from(frame.keyframe);
        self.writing = true;
        self.send.write_all(&prefix).await?;
        self.send.write_all(&frame.payload).await?;
        self.writing = false;
        self.first = false;
        Ok(())
    }
    /// Finish a complete, nonempty group. Does not wait for acknowledgement.
    pub fn finish(mut self) -> Result<(), Error> {
        if self.first || self.writing {
            return Err(Error::Frame);
        }
        self.send.finish().map_err(|_| Error::Closed)?;
        self.finished = true;
        Ok(())
    }
    /// Abandon a group when the next keyframe supersedes it.
    pub fn reset(mut self) {
        let _ = self.send.reset(RESET_GROUP.into());
        self.finished = true;
    }
}
impl Drop for GroupWriter {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.send.reset(RESET_GROUP.into());
        }
    }
}

/// What [`GroupParser::feed`] completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// The stream prefix, always first.
    Header(StreamHeader),
    /// The next frame; the first one is a keyframe.
    Frame(Frame),
}

const PREFIX_MAX: usize = 14;
const FRAME_PREFIX: usize = 9;
// Payload memory grows with the bytes that arrive, not the declared length.
const PAYLOAD_RESERVE: usize = 64 * 1024;

/// Incremental parser for one incoming video group: the 12–14 byte stream
/// prefix, then length-prefixed frames. Takes byte chunks of any size, does
/// no I/O, and never consumes past the item it is completing, so the prefix
/// can be read without touching frame bytes. The `video_group` fuzz target
/// drives it with arbitrary chunk boundaries. Errors are sticky.
#[derive(Debug, Default)]
pub struct GroupParser {
    header: Option<StreamHeader>,
    head: [u8; PREFIX_MAX],
    head_len: usize,
    prefix: [u8; FRAME_PREFIX],
    prefix_read: usize,
    payload: Vec<u8>,
    frames: u64,
    failed: bool,
}

impl GroupParser {
    /// The stream prefix, once parsed.
    pub fn header(&self) -> Option<StreamHeader> {
        self.header
    }

    /// Most bytes the current item can still take: one at a time for the
    /// stream prefix, then the rest of a frame prefix or payload. Reading no
    /// more than this keeps a stream positioned at an item boundary.
    pub fn wanted(&self) -> usize {
        if self.header.is_none() {
            1
        } else if self.prefix_read < FRAME_PREFIX {
            FRAME_PREFIX - self.prefix_read
        } else {
            self.length() - self.payload.len()
        }
    }

    fn length(&self) -> usize {
        u32::from_be_bytes([
            self.prefix[0],
            self.prefix[1],
            self.prefix[2],
            self.prefix[3],
        ]) as usize
    }

    /// Takes bytes from the front of `input` up to the end of the next
    /// prefix or frame and returns it once complete. Leftover input belongs
    /// to later items; an empty `input` is allowed.
    ///
    /// # Errors
    ///
    /// An invalid stream prefix, or a zero, oversize, flag-reserved, or
    /// leading non-key frame. Every later call fails with [`Error::Frame`].
    pub fn feed(&mut self, input: &mut &[u8]) -> Result<Option<Parsed>, Error> {
        if self.failed {
            return Err(Error::Frame);
        }
        let result = self.advance(input);
        self.failed = result.is_err();
        result
    }

    fn advance(&mut self, input: &mut &[u8]) -> Result<Option<Parsed>, Error> {
        while self.header.is_none() {
            let Some((&byte, rest)) = input.split_first() else {
                return Ok(None);
            };
            *input = rest;
            self.head[self.head_len] = byte;
            self.head_len += 1;
            match StreamHeader::parse(&self.head[..self.head_len]) {
                Ok((header, _)) => {
                    self.header = Some(header);
                    return Ok(Some(Parsed::Header(header)));
                }
                // The longest prefix parses or fails; it is never truncated.
                Err(MediaError::Truncated) if self.head_len < PREFIX_MAX => {}
                Err(MediaError::Truncated) => return Err(Error::Frame),
                Err(error) => return Err(error.into()),
            }
        }
        while self.prefix_read < FRAME_PREFIX {
            if input.is_empty() {
                return Ok(None);
            }
            let take = (FRAME_PREFIX - self.prefix_read).min(input.len());
            let (head, rest) = input.split_at(take);
            self.prefix[self.prefix_read..self.prefix_read + take].copy_from_slice(head);
            self.prefix_read += take;
            *input = rest;
            if self.prefix_read == FRAME_PREFIX {
                self.check_prefix()?;
            }
        }
        let length = self.length();
        let take = (length - self.payload.len()).min(input.len());
        if self.payload.len() + take > self.payload.capacity() {
            let target = (self.payload.capacity() * 2)
                .max(self.payload.len() + take)
                .clamp(PAYLOAD_RESERVE.min(length), length);
            self.payload.reserve_exact(target - self.payload.len());
        }
        let (head, rest) = input.split_at(take);
        self.payload.extend_from_slice(head);
        *input = rest;
        if self.payload.len() < length {
            return Ok(None);
        }
        let frame = Frame {
            timestamp: u32::from_be_bytes([
                self.prefix[4],
                self.prefix[5],
                self.prefix[6],
                self.prefix[7],
            ]),
            keyframe: self.prefix[8] & KEYFRAME != 0,
            payload: Bytes::from(std::mem::take(&mut self.payload)),
        };
        self.prefix_read = 0;
        self.frames += 1;
        Ok(Some(Parsed::Frame(frame)))
    }

    fn check_prefix(&self) -> Result<(), Error> {
        let length = self.length();
        let flags = self.prefix[8];
        if length == 0
            || length > MAX_FRAME_BYTES
            || flags & !KEYFRAME != 0
            || (self.frames == 0 && flags & KEYFRAME == 0)
        {
            return Err(Error::Frame);
        }
        Ok(())
    }

    /// The stream ended. A group ends cleanly only between frames, after at
    /// least one.
    ///
    /// # Errors
    ///
    /// [`Error::Frame`] for a truncated prefix or frame, an empty group, or
    /// an already failed parser.
    pub fn finish(&mut self) -> Result<(), Error> {
        if self.failed || self.header.is_none() || self.prefix_read != 0 || self.frames == 0 {
            self.failed = true;
            return Err(Error::Frame);
        }
        Ok(())
    }
}

/// Incoming GOP. Frame reads retain partial data across cancellation.
pub struct GroupReader {
    /// Validated prefix identifying the track and group.
    pub header: StreamHeader,
    recv: quinn::RecvStream,
    parser: GroupParser,
    failed: bool,
}
impl GroupReader {
    /// Accept one uni stream and validate its prefix.
    pub async fn accept(connection: &quinn::Connection) -> Result<Self, Error> {
        Self::from_stream(connection.accept_uni().await?).await
    }
    /// Parse an already accepted stream.
    pub async fn from_stream(mut recv: quinn::RecvStream) -> Result<Self, Error> {
        let mut parser = GroupParser::default();
        let header = read_prefix(&mut recv, &mut parser).await?;
        Ok(Self {
            header,
            recv,
            parser,
            failed: false,
        })
    }
    /// Read one bounded frame, or EOF. On error, reset/drop the whole group.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>, Error> {
        if self.failed {
            return Err(Error::Frame);
        }
        let result = self.read_frame().await;
        if result.is_err() {
            self.failed = true;
            let _ = self.recv.stop(RESET_GROUP.into());
        }
        result
    }
    // Cancellation-safe: a chunk is never larger than the parser wants, and
    // it is fed before the next await, so no read byte is ever dropped.
    async fn read_frame(&mut self) -> Result<Option<Frame>, Error> {
        let mut chunk = Bytes::new();
        loop {
            if let Some(Parsed::Frame(frame)) = self.parser.feed(&mut chunk.as_ref())? {
                return Ok(Some(frame));
            }
            match self.recv.read_chunk(self.parser.wanted(), true).await? {
                Some(next) => chunk = next.bytes,
                None => return self.parser.finish().map(|()| None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::DatagramHeader;

    fn header() -> StreamHeader {
        StreamHeader {
            media: DatagramHeader {
                version: 0,
                kind: 1,
                flags: 0,
                track_id: 300,
                seq: 1,
                timestamp: 2,
            },
            group_id: 7,
        }
    }

    fn group(frames: &[(u32, bool, &[u8])]) -> Vec<u8> {
        let mut bytes = BytesMut::new();
        header().encode_into(&mut bytes).unwrap();
        for (timestamp, keyframe, payload) in frames {
            bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
            bytes.extend_from_slice(&timestamp.to_be_bytes());
            bytes.extend_from_slice(&[u8::from(*keyframe)]);
            bytes.extend_from_slice(payload);
        }
        bytes.to_vec()
    }

    fn parse(bytes: &[u8], size: usize) -> Result<Vec<Parsed>, Error> {
        let mut parser = GroupParser::default();
        let mut items = Vec::new();
        for mut chunk in bytes.chunks(size) {
            while let Some(item) = parser.feed(&mut chunk)? {
                items.push(item);
            }
        }
        parser.finish()?;
        Ok(items)
    }

    #[test]
    fn groups_parse_identically_at_every_chunk_size() {
        let bytes = group(&[
            (10, true, b"key"),
            (20, false, b"delta frame"),
            (30, true, b"k"),
        ]);
        let expected = parse(&bytes, bytes.len()).unwrap();
        assert_eq!(expected.len(), 4);
        assert_eq!(expected[0], Parsed::Header(header()));
        assert!(
            matches!(&expected[2], Parsed::Frame(f) if f.timestamp == 20 && !f.keyframe && f.payload == b"delta frame"[..])
        );
        for size in 1..bytes.len() {
            assert_eq!(parse(&bytes, size).unwrap(), expected, "chunk size {size}");
        }
    }

    #[test]
    fn the_prefix_is_parsed_without_consuming_frame_bytes() {
        let bytes = group(&[(1, true, b"x")]);
        let mut parser = GroupParser::default();
        let mut input = &bytes[..];
        assert_eq!(parser.wanted(), 1);
        assert_eq!(
            parser.feed(&mut input).unwrap(),
            Some(Parsed::Header(header()))
        );
        // Track 300 needs two LEB128 bytes: a 13-byte prefix.
        assert_eq!(input.len(), bytes.len() - 13);
        assert_eq!(parser.wanted(), 9);
    }

    #[test]
    fn invalid_or_truncated_groups_fail_and_stay_failed() {
        let ok = group(&[(1, true, b"x")]);
        // A group ends only between frames, after at least one.
        for cut in [0, 5, 13, 16, ok.len() - 1] {
            assert!(parse(&ok[..cut], 3).is_err(), "cut at {cut}");
        }
        assert!(
            parse(&group(&[(1, false, b"x")]), 4).is_err(),
            "leading delta"
        );
        assert!(parse(&group(&[(1, true, b"")]), 4).is_err(), "empty frame");
        let mut reserved = group(&[(1, true, b"x")]);
        reserved[13 + 8] = 3;
        assert!(parse(&reserved, 4).is_err(), "reserved flag");
        let mut huge = group(&[(1, true, b"x")]);
        huge[13..17].copy_from_slice(&(u32::try_from(MAX_FRAME_BYTES).unwrap() + 1).to_be_bytes());
        let mut parser = GroupParser::default();
        let mut input = &huge[..];
        assert!(parser.feed(&mut input).unwrap().is_some());
        assert!(parser.feed(&mut input).is_err(), "oversize frame");
        assert!(parser.feed(&mut &b"more"[..]).is_err());
        assert!(parser.finish().is_err());
        let mut audio = group(&[]);
        audio[0] = 0;
        assert!(matches!(
            GroupParser::default().feed(&mut &audio[..]),
            Err(Error::Header(MediaError::Kind))
        ));
    }
}
