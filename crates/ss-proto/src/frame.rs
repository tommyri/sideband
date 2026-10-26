//! Length-prefixed control-stream frames.
//!
//! Layout from [docs/plan/02-protocol.md](../../../docs/plan/02-protocol.md):
//! a big-endian `u32` byte count, then one protobuf envelope. The count is
//! the protobuf size and must be at most [`MAX_FRAME_LEN`] (1 MiB).

use std::io::Error as IoError;

use bytes::Bytes;
use prost::{DecodeError, EncodeError, Message};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Size of the big-endian length prefix, in bytes.
pub const HEADER_LEN: usize = 4;

/// Largest accepted protobuf payload, in bytes (1 MiB).
///
/// The length prefix is not included. A frame whose prefix is greater than
/// this is [`FrameError::TooLarge`] and is not read further.
pub const MAX_FRAME_LEN: usize = 1024 * 1024;

/// Framing failure.
///
/// After [`FrameError::TooLarge`], the payload bytes are still sitting on the
/// stream. Drop the connection; do not try to skip a multi-gigabyte prefix.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameError {
    /// The length prefix is larger than [`MAX_FRAME_LEN`].
    #[error("frame of {len} bytes exceeds the {MAX_FRAME_LEN} byte limit")]
    TooLarge {
        /// Payload length from the prefix, in bytes.
        len: usize,
    },

    /// The buffer ended before a whole frame was available.
    #[error("frame ended before the length prefix and payload were complete")]
    Incomplete,

    /// `frame` contained bytes after the single encoded envelope.
    #[error("frame has {extra} trailing bytes")]
    TrailingBytes {
        /// Number of bytes after the declared payload.
        extra: usize,
    },

    /// Protobuf decode failed.
    #[error("failed to decode frame")]
    Decode(#[from] DecodeError),

    /// Protobuf encode failed.
    #[error("failed to encode frame")]
    Encode(#[from] EncodeError),

    /// The underlying reader or writer failed.
    #[error("frame i/o failed")]
    Io(#[from] IoError),
}

/// Encodes `message` as one length-prefixed frame.
///
/// # Errors
///
/// Returns [`FrameError::TooLarge`] when the protobuf encoding is larger than
/// [`MAX_FRAME_LEN`]. Returns [`FrameError::Encode`] when `prost` refuses the
/// message.
pub fn encode(message: &impl Message) -> Result<Bytes, FrameError> {
    let len = message.encoded_len();
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge { len });
    }
    let Ok(len_u32) = u32::try_from(len) else {
        return Err(FrameError::TooLarge { len });
    };

    let mut buf = Vec::with_capacity(HEADER_LEN + len);
    buf.extend_from_slice(&len_u32.to_be_bytes());
    message.encode(&mut buf)?;
    Ok(Bytes::from(buf))
}

/// Decodes one length-prefixed frame from a complete buffer.
///
/// # Errors
///
/// Returns [`FrameError::TooLarge`] when the prefix exceeds [`MAX_FRAME_LEN`],
/// [`FrameError::Incomplete`] when `frame` is shorter than the prefix claims,
/// [`FrameError::TrailingBytes`] when bytes follow that payload, and
/// [`FrameError::Decode`] when the payload is not a valid `M`.
pub fn decode<M>(frame: &[u8]) -> Result<M, FrameError>
where
    M: Message + Default,
{
    let header = header_bytes(frame)?;
    let len = payload_len(header)?;
    let rest = frame.get(HEADER_LEN..).ok_or(FrameError::Incomplete)?;
    if rest.len() < len {
        return Err(FrameError::Incomplete);
    }
    if rest.len() != len {
        return Err(FrameError::TrailingBytes {
            extra: rest.len() - len,
        });
    }
    let payload = rest.get(..len).ok_or(FrameError::Incomplete)?;
    Ok(M::decode(payload)?)
}

/// Reads one length-prefixed frame from `reader`.
///
/// `reader` is generic so tests can use an in-memory duplex. A
/// `quinn::RecvStream` works the same way because it implements
/// [`AsyncRead`].
///
/// # Errors
///
/// See [`decode`]. A short read is [`FrameError::Io`]. An oversized prefix
/// returns [`FrameError::TooLarge`] without reading the payload.
pub async fn read_frame<R, M>(reader: &mut R) -> Result<M, FrameError>
where
    R: AsyncRead + Unpin,
    M: Message + Default,
{
    let mut header = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;
    let len = payload_len(header)?;
    let mut payload = vec![0_u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(M::decode(payload.as_slice())?)
}

/// Writes one length-prefixed frame to `writer`.
///
/// This does not flush. Callers that need the bytes on the wire immediately
/// flush the writer themselves.
///
/// # Errors
///
/// See [`encode`]. A write failure is [`FrameError::Io`].
pub async fn write_frame<W, M>(writer: &mut W, message: &M) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    M: Message,
{
    let frame = encode(message)?;
    writer.write_all(&frame).await?;
    Ok(())
}

/// Reads the 4-byte prefix. `frame` shorter than that is incomplete.
fn header_bytes(frame: &[u8]) -> Result<[u8; HEADER_LEN], FrameError> {
    let Some(src) = frame.get(..HEADER_LEN) else {
        return Err(FrameError::Incomplete);
    };
    let mut header = [0_u8; HEADER_LEN];
    header.copy_from_slice(src);
    Ok(header)
}

/// Checks the big-endian length and converts it to a `usize` byte count.
fn payload_len(header: [u8; HEADER_LEN]) -> Result<usize, FrameError> {
    let declared = u32::from_be_bytes(header);
    let Ok(len) = usize::try_from(declared) else {
        return Err(FrameError::TooLarge {
            len: MAX_FRAME_LEN.saturating_add(1),
        });
    };
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge { len });
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;

    use super::{FrameError, HEADER_LEN, MAX_FRAME_LEN, decode, encode, read_frame, write_frame};
    use crate::v1::{
        ClientEnvelope, Ping, ServerEnvelope, ServerNotice, client_envelope, server_envelope,
    };

    fn ping(request_id: u32, sent_at: i64) -> ClientEnvelope {
        ClientEnvelope {
            request_id,
            msg: Some(client_envelope::Msg::Ping(Ping { sent_at })),
        }
    }

    #[test]
    fn encode_decode_round_trip() {
        let envelope = ping(7, 1_700_000_000_000);
        let frame = encode(&envelope).expect("small frame");
        let mut header = [0_u8; HEADER_LEN];
        header.copy_from_slice(&frame[..HEADER_LEN]);
        let len = usize::try_from(u32::from_be_bytes(header)).expect("len fits");
        assert_eq!(frame.len(), HEADER_LEN + len);
        let decoded: ClientEnvelope = decode(&frame).expect("decode");
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn empty_payload_decodes_as_default() {
        let frame = 0_u32.to_be_bytes();
        let decoded: ClientEnvelope = decode(&frame).expect("empty payload");
        assert_eq!(decoded, ClientEnvelope::default());
    }

    #[test]
    fn truncated_prefix_is_incomplete() {
        let err = decode::<ClientEnvelope>(&[0, 1]).expect_err("short");
        assert!(matches!(err, FrameError::Incomplete));
    }

    #[test]
    fn truncated_payload_is_incomplete() {
        let mut frame = 4_u32.to_be_bytes().to_vec();
        frame.extend_from_slice(&[1, 2]);
        let err = decode::<ClientEnvelope>(&frame).expect_err("short payload");
        assert!(matches!(err, FrameError::Incomplete));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut frame = encode(&ping(1, 2)).expect("encode").to_vec();
        frame.push(0);
        let err = decode::<ClientEnvelope>(&frame).expect_err("trailing");
        assert!(matches!(err, FrameError::TrailingBytes { extra: 1 }));
    }

    #[test]
    fn oversized_prefix_in_a_buffer_is_too_large() {
        let declared = u32::try_from(MAX_FRAME_LEN).expect("max fits u32") + 1;
        let err = decode::<ClientEnvelope>(&declared.to_be_bytes()).expect_err("too large");
        assert!(matches!(err, FrameError::TooLarge { len } if len == MAX_FRAME_LEN + 1));
    }

    #[test]
    fn oversized_message_is_too_large_and_does_not_panic() {
        let envelope = ServerEnvelope {
            request_id: 0,
            msg: Some(server_envelope::Msg::ServerNotice(ServerNotice {
                text: "x".repeat(MAX_FRAME_LEN + 1),
            })),
        };
        let err = encode(&envelope).expect_err("over cap");
        assert!(matches!(err, FrameError::TooLarge { len } if len > MAX_FRAME_LEN));
    }

    #[tokio::test]
    async fn duplex_round_trip_both_directions() {
        let (mut client, mut server) = tokio::io::duplex(64);
        let outbound = ping(3, 9);
        let inbound = ServerEnvelope::ack(
            3,
            crate::v1::ack::Result::Empty(crate::v1::Empty::default()),
        );

        write_frame(&mut client, &outbound)
            .await
            .expect("write client");
        write_frame(&mut server, &inbound)
            .await
            .expect("write server");

        let got_out: ClientEnvelope = read_frame(&mut server).await.expect("read client");
        let got_in: ServerEnvelope = read_frame(&mut client).await.expect("read server");
        assert_eq!(got_out, outbound);
        assert_eq!(got_in, inbound);
    }

    #[tokio::test]
    async fn two_frames_on_one_stream() {
        let (mut writer, mut reader) = tokio::io::duplex(128);
        let first = ping(1, 10);
        let second = ping(2, 11);
        write_frame(&mut writer, &first).await.expect("first");
        write_frame(&mut writer, &second).await.expect("second");
        let got_first: ClientEnvelope = read_frame(&mut reader).await.expect("read first");
        let got_second: ClientEnvelope = read_frame(&mut reader).await.expect("read second");
        assert_eq!(got_first, first);
        assert_eq!(got_second, second);
    }

    #[tokio::test]
    async fn oversized_frame_returns_before_reading_the_body() {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        let declared = u32::try_from(MAX_FRAME_LEN).expect("max fits u32") + 1;
        writer
            .write_all(&declared.to_be_bytes())
            .await
            .expect("prefix");

        let result = tokio::time::timeout(
            Duration::from_millis(200),
            read_frame::<_, ClientEnvelope>(&mut reader),
        )
        .await;

        let err = result
            .expect("length check must not wait for the body")
            .expect_err("too large");
        assert!(matches!(err, FrameError::TooLarge { len } if len == MAX_FRAME_LEN + 1));
    }
}
