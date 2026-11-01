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
    let mut bytes = [0_u8; 14];
    for len in 1..=bytes.len() {
        recv.read_exact(&mut bytes[len - 1..len])
            .await
            .map_err(|_| Error::Frame)?;
        match StreamHeader::parse(&bytes[..len]) {
            Ok((header, _)) => return Ok((header, recv)),
            Err(MediaError::Truncated) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(Error::Frame)
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

/// Incoming GOP. Frame reads retain partial data across cancellation.
pub struct GroupReader {
    /// Validated prefix identifying the track and group.
    pub header: StreamHeader,
    recv: quinn::RecvStream,
    prefix: [u8; 9],
    prefix_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
    first: bool,
    failed: bool,
}
impl GroupReader {
    /// Accept one uni stream and validate its prefix.
    pub async fn accept(connection: &quinn::Connection) -> Result<Self, Error> {
        Self::from_stream(connection.accept_uni().await?).await
    }
    /// Parse an already accepted stream.
    pub async fn from_stream(recv: quinn::RecvStream) -> Result<Self, Error> {
        let (header, recv) = read_header(recv).await?;
        Ok(Self {
            header,
            recv,
            prefix: [0; 9],
            prefix_read: 0,
            payload: Vec::new(),
            payload_read: 0,
            first: true,
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
    async fn read_frame(&mut self) -> Result<Option<Frame>, Error> {
        while self.prefix_read < 9 {
            match self.recv.read(&mut self.prefix[self.prefix_read..]).await? {
                Some(count) => self.prefix_read += count,
                None if self.prefix_read == 0 && !self.first => return Ok(None),
                None => return Err(Error::Frame),
            }
        }
        let length = u32::from_be_bytes(self.prefix[..4].try_into().map_err(|_| Error::Frame)?);
        let length = usize::try_from(length).map_err(|_| Error::Frame)?;
        let flags = self.prefix[8];
        if length == 0
            || length > MAX_FRAME_BYTES
            || flags & !KEYFRAME != 0
            || (self.first && flags & KEYFRAME == 0)
        {
            return Err(Error::Frame);
        }
        if self.payload.is_empty() {
            self.payload.resize(length, 0);
        }
        while self.payload_read < length {
            self.payload_read += self
                .recv
                .read(&mut self.payload[self.payload_read..])
                .await?
                .ok_or(Error::Frame)?;
        }
        let frame = Frame {
            timestamp: u32::from_be_bytes(self.prefix[4..8].try_into().map_err(|_| Error::Frame)?),
            keyframe: flags & KEYFRAME != 0,
            payload: Bytes::from(std::mem::take(&mut self.payload)),
        };
        self.first = false;
        self.prefix_read = 0;
        self.payload_read = 0;
        Ok(Some(frame))
    }
}
