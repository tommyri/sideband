//! Typed, cancellation-safe framing and bounded client request/event routing.

use std::{collections::HashMap, marker::PhantomData, sync::Arc};

use bytes::{Bytes, BytesMut};
use parking_lot::Mutex;
use prost::Message;
use ss_proto::{
    frame::{self, FrameError},
    v1::{self, ClientEnvelope, ServerEnvelope, client_envelope, server_envelope},
};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
};

/// A control-stream failure. Clonable so every pending request sees shutdown.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ControlError {
    /// Encoding, decoding, or length validation failed.
    #[error("invalid control frame: {0}")]
    Frame(Arc<FrameError>),
    /// QUIC read failed.
    #[error("control read failed: {0}")]
    Read(Arc<quinn::ReadError>),
    /// QUIC write failed.
    #[error("control write failed: {0}")]
    Write(Arc<quinn::WriteError>),
    /// Connection or owning control handle was closed.
    #[error("control stream closed")]
    Closed,
    /// Peer sent inconsistent response metadata.
    #[error("control protocol violation: {0}")]
    Protocol(&'static str),
    /// Bounded request or event queue has reached capacity.
    #[error("control capacity exceeded")]
    Capacity,
    /// A new connection is needed after all nonzero u32 ids are used.
    #[error("request ids exhausted")]
    IdsExhausted,
}

impl From<FrameError> for ControlError {
    fn from(value: FrameError) -> Self {
        Self::Frame(Arc::new(value))
    }
}

/// A protobuf envelope carrying a request id. Tracking it preserves monotonic
/// allocation when a raw control stream is handed over to the client router.
pub trait ControlMessage: Message {
    /// Envelope request id (zero identifies an unsolicited server event).
    fn request_id(&self) -> u32;
}

impl ControlMessage for ClientEnvelope {
    fn request_id(&self) -> u32 {
        self.request_id
    }
}

impl ControlMessage for ServerEnvelope {
    fn request_id(&self) -> u32 {
        self.request_id
    }
}

/// Client writes client envelopes and reads server envelopes.
pub type ClientStream = ControlStream<ClientEnvelope, ServerEnvelope>;
/// Server writes server envelopes and reads client envelopes.
pub type ServerStream = ControlStream<ServerEnvelope, ClientEnvelope>;

/// Exactly one bidirectional control stream, split for independent send/receive.
pub struct ControlStream<Out, In> {
    sender: ControlSender<Out>,
    receiver: ControlReceiver<In>,
}

impl<Out: ControlMessage, In: Message + Default> ControlStream<Out, In> {
    pub(crate) fn new(
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        connection: quinn::Connection,
    ) -> Self {
        Self {
            sender: ControlSender {
                stream: send,
                connection: connection.clone(),
                pending: None,
                last_request_id: 0,
                marker: PhantomData,
            },
            receiver: ControlReceiver {
                stream: recv,
                connection,
                frames: FrameDecoder::default(),
            },
        }
    }

    /// Sends one frame. Cancellation may still deliver the message; a later send
    /// completes its partial frame before writing another, preventing corruption.
    pub async fn send(&mut self, message: &Out) -> Result<(), ControlError> {
        self.sender.send(message).await
    }
    /// Receives one frame, retaining partial input if this future is cancelled.
    pub async fn recv(&mut self) -> Result<In, ControlError> {
        self.receiver.recv().await
    }
    /// Splits the stream for concurrent sending and receiving.
    pub fn split(self) -> (ControlSender<Out>, ControlReceiver<In>) {
        (self.sender, self.receiver)
    }
}

// Quinn's close() replaces even an existing remote close reason with LocalClosed.
// Preserve terminal peer/transport reasons so hosts can decide whether to resume.
fn close_if_open(connection: &quinn::Connection, code: u32, reason: &[u8]) {
    if connection.close_reason().is_none() {
        connection.close(code.into(), reason);
    }
}

/// One typed writer. A partial frame is retained across cancellation.
pub struct ControlSender<M> {
    stream: quinn::SendStream,
    connection: quinn::Connection,
    pending: Option<(Bytes, usize)>,
    last_request_id: u32,
    marker: PhantomData<M>,
}

impl<M: ControlMessage> ControlSender<M> {
    /// Sends a frame; local oversize errors leave the stream usable.
    pub async fn send(&mut self, message: &M) -> Result<(), ControlError> {
        let bytes = frame::encode(message)?;
        self.flush_pending().await?;
        self.last_request_id = self.last_request_id.max(message.request_id());
        self.pending = Some((bytes, 0));
        self.flush_pending().await
    }

    async fn flush_pending(&mut self) -> Result<(), ControlError> {
        while let Some((bytes, offset)) = self.pending.as_mut() {
            if *offset == bytes.len() {
                self.pending = None;
                break;
            }
            match self.stream.write(&bytes[*offset..]).await {
                Ok(written) => *offset += written,
                Err(error) => {
                    close_if_open(&self.connection, 2, b"control write failed");
                    return Err(ControlError::Write(Arc::new(error)));
                }
            }
        }
        Ok(())
    }
}

/// Incremental control-frame parser over byte chunks of any size, with no I/O.
///
/// Both peers' readers use it; the `control_stream` fuzz target drives it with
/// arbitrary chunk boundaries. It holds at most one frame
/// (`MAX_FRAME_LEN + 4` bytes) and never consumes past the end of a frame.
/// Errors are sticky: after one, every call returns an error.
pub struct FrameDecoder<M> {
    buffer: BytesMut,
    marker: PhantomData<M>,
}

impl<M> Default for FrameDecoder<M> {
    fn default() -> Self {
        Self {
            buffer: BytesMut::new(),
            marker: PhantomData,
        }
    }
}

impl<M: Message + Default> FrameDecoder<M> {
    /// Bytes still missing from the current length prefix or frame body.
    /// Nonzero whenever [`Self::feed`] last returned `Ok(None)`.
    ///
    /// # Errors
    ///
    /// [`FrameError::TooLarge`] once a prefix exceeds `MAX_FRAME_LEN`.
    pub fn wanted(&self) -> Result<usize, FrameError> {
        let Some(prefix) = self.buffer.get(..4) else {
            return Ok(4 - self.buffer.len());
        };
        let len = u32::from_be_bytes([prefix[0], prefix[1], prefix[2], prefix[3]]) as usize;
        if len > frame::MAX_FRAME_LEN {
            return Err(FrameError::TooLarge { len });
        }
        Ok(len + 4 - self.buffer.len())
    }

    /// Takes bytes from the front of `input` up to the end of the next frame
    /// and returns that frame once it is complete. Leftover input belongs to
    /// later frames; an empty `input` is allowed.
    ///
    /// # Errors
    ///
    /// An oversize length prefix or a body that is not a valid protobuf.
    pub fn feed(&mut self, input: &mut &[u8]) -> Result<Option<M>, FrameError> {
        loop {
            let wanted = self.wanted()?;
            if wanted == 0 {
                // A complete frame; four zero bytes alone are an empty message.
                let message = frame::decode(&self.buffer)?;
                self.buffer.clear();
                return Ok(Some(message));
            }
            if input.is_empty() {
                return Ok(None);
            }
            let (head, tail) = input.split_at(wanted.min(input.len()));
            self.buffer.extend_from_slice(head);
            *input = tail;
        }
    }
}

/// One typed reader with a bounded, resumable frame buffer.
pub struct ControlReceiver<M> {
    stream: quinn::RecvStream,
    connection: quinn::Connection,
    frames: FrameDecoder<M>,
}

impl<M: Message + Default> ControlReceiver<M> {
    /// Reads one frame. Oversize, malformed, and truncated frames close the connection.
    pub async fn recv(&mut self) -> Result<M, ControlError> {
        let result = self.read_frame().await;
        if result.is_err() {
            close_if_open(&self.connection, 2, b"invalid control frame");
        }
        result
    }

    // Cancellation-safe: a chunk is never larger than the decoder wants, and
    // it is fed before the next await, so no read byte is ever dropped.
    async fn read_frame(&mut self) -> Result<M, ControlError> {
        let mut chunk = Bytes::new();
        loop {
            if let Some(message) = self.frames.feed(&mut chunk.as_ref())? {
                return Ok(message);
            }
            chunk = self
                .stream
                .read_chunk(self.frames.wanted()?, true)
                .await
                .map_err(|error| ControlError::Read(Arc::new(error)))?
                .ok_or_else(|| ControlError::from(FrameError::Incomplete))?
                .bytes;
        }
    }
}

/// Successful command acknowledgement or an application-level error reply.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "wire Ack embeds resources; keep the public reply API allocation-free"
)]
pub enum Reply {
    /// Server accepted the command.
    Ack(v1::Ack),
    /// Server rejected the command; the connection remains usable.
    Error(v1::Error),
}

type Response = oneshot::Sender<Result<ServerEnvelope, ControlError>>;

#[derive(Default)]
struct Pending {
    requests: HashMap<u32, Response>,
    terminal: Option<ControlError>,
    /// Events with a `msg` variant this build does not know, skipped.
    unknown: u64,
}

impl Pending {
    fn fail(&mut self, error: &ControlError) {
        if self.terminal.is_none() {
            self.terminal = Some(error.clone());
        }
        for (_, reply) in self.requests.drain() {
            let _ = reply.send(Err(error.clone()));
        }
    }
}

struct Command {
    message: client_envelope::Msg,
    reply: Option<Response>,
}

/// Bounded request router. Dropping it closes the control connection and cancels
/// both worker tasks. Application request timeouts belong to the caller.
/// Cancelled requests never reuse ids; any late replies are discarded.
/// After the application sends its initial Hello, sends protocol `Ping` after
/// 15 seconds without an outbound control frame. A fresh router stays silent.
pub struct ClientControl {
    commands: mpsc::Sender<Command>,
    pending: Arc<Mutex<Pending>>,
    connection: quinn::Connection,
    workers: [JoinHandle<()>; 2],
}

impl ClientControl {
    /// Starts independent read/write workers and returns the request-id-zero
    /// event receiver. Overflow closes the connection instead of losing state.
    pub fn start(stream: ClientStream) -> (Self, mpsc::Receiver<ServerEnvelope>) {
        let (sender, receiver) = stream.split();
        let connection = sender.connection.clone();
        let pending = Arc::new(Mutex::new(Pending::default()));
        let (commands, command_rx) = mpsc::channel(64);
        let (events, event_rx) = mpsc::channel(256);
        let writer = tokio::spawn(write_requests(sender, command_rx, pending.clone()));
        let reader = tokio::spawn(read_responses(receiver, events, pending.clone()));
        (
            Self {
                commands,
                pending,
                connection,
                workers: [writer, reader],
            },
            event_rx,
        )
    }

    /// Sends a request with a monotonically allocated nonzero id and waits for
    /// its response. Use for `Hello`/`Welcome`, `Ping`/`Pong`, or custom replies.
    pub async fn request(
        &self,
        message: client_envelope::Msg,
    ) -> Result<ServerEnvelope, ControlError> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command {
                message,
                reply: Some(reply),
            })
            .await
            .map_err(|_| self.closed_error())?;
        response.await.map_err(|_| self.closed_error())?
    }

    /// Sends a command and correlates its `Ack` or `Error` reply.
    pub async fn call(&self, message: client_envelope::Msg) -> Result<Reply, ControlError> {
        match self.request(message).await?.msg {
            Some(server_envelope::Msg::Ack(ack)) => Ok(Reply::Ack(ack)),
            Some(server_envelope::Msg::Error(error)) => Ok(Reply::Error(error)),
            _ => Err(ControlError::Protocol("expected Ack or Error")),
        }
    }

    /// Server events skipped because their message is one this build does not
    /// know: a newer server's addition (`02-protocol.md` §7).
    pub fn unknown_messages(&self) -> u64 {
        self.pending.lock().unknown
    }

    /// Reason the control workers stopped, if available.
    pub fn terminal_error(&self) -> Option<ControlError> {
        self.pending.lock().terminal.clone()
    }

    fn closed_error(&self) -> ControlError {
        self.terminal_error().unwrap_or(ControlError::Closed)
    }
}

impl Drop for ClientControl {
    fn drop(&mut self) {
        self.pending.lock().fail(&ControlError::Closed);
        close_if_open(&self.connection, 0, b"control owner dropped");
        for worker in &self.workers {
            worker.abort();
        }
    }
}

async fn write_requests(
    mut sender: ControlSender<ClientEnvelope>,
    mut commands: mpsc::Receiver<Command>,
    pending: Arc<Mutex<Pending>>,
) {
    let mut next_id = sender.last_request_id.checked_add(1);
    let mut last_write = tokio::time::Instant::now();
    loop {
        let command = tokio::select! {
            _ = sender.connection.closed() => break,
            command = commands.recv() => match command { Some(command) => command, None => break },
            () = tokio::time::sleep_until(last_write + std::time::Duration::from_secs(15)),
                if sender.last_request_id != 0 => Command {
                message: client_envelope::Msg::Ping(v1::Ping { sent_at: ss_core::Timestamp::now().as_millis() }),
                reply: None,
            },
        };
        if command
            .reply
            .as_ref()
            .is_some_and(oneshot::Sender::is_closed)
        {
            continue;
        }
        let Some(id) = next_id else {
            if let Some(reply) = command.reply {
                let _ = reply.send(Err(ControlError::IdsExhausted));
            }
            pending.lock().fail(&ControlError::IdsExhausted);
            close_if_open(&sender.connection, 2, b"request ids exhausted");
            break;
        };
        let envelope = ClientEnvelope {
            request_id: id,
            msg: Some(command.message),
        };
        let len = envelope.encoded_len();
        if len > frame::MAX_FRAME_LEN {
            if let Some(reply) = command.reply {
                let _ = reply.send(Err(FrameError::TooLarge { len }.into()));
            }
            continue;
        }
        if let Some(reply) = command.reply {
            let mut state = pending.lock();
            if let Some(error) = &state.terminal {
                let _ = reply.send(Err(error.clone()));
                continue;
            }
            state.requests.retain(|_, reply| !reply.is_closed());
            if state.requests.len() >= 256 {
                let _ = reply.send(Err(ControlError::Capacity));
                continue;
            }
            state.requests.insert(id, reply);
        }
        next_id = id.checked_add(1);
        if let Err(error) = sender.send(&envelope).await {
            pending.lock().fail(&error);
            break;
        }
        last_write = tokio::time::Instant::now();
    }
    pending.lock().fail(&ControlError::Closed);
}

/// Where the reader sends one server envelope.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Request id zero: an event, including an unsolicited `Error` notice.
    Event,
    /// The reply to request `id`, if anyone still waits for it.
    Reply(u32),
    /// An event whose `msg` variant this build does not know.
    Unknown,
}

// Fields are only ever added (`02-protocol.md` §7), and the server cannot
// tell which ones a client knows. An unknown `msg` variant decodes to none.
fn route(envelope: &ServerEnvelope) -> Result<Route, ControlError> {
    let payload_id = match &envelope.msg {
        None if envelope.request_id == 0 => return Ok(Route::Unknown),
        // Only an `Error` can be unsolicited (`envelope.proto`).
        Some(server_envelope::Msg::Ack(_)) if envelope.request_id == 0 => {
            return Err(ControlError::Protocol("unsolicited Ack"));
        }
        Some(server_envelope::Msg::Ack(ack)) => Some(ack.request_id),
        Some(server_envelope::Msg::Error(error)) => Some(error.request_id),
        _ => None,
    };
    if payload_id.is_some_and(|id| id != envelope.request_id) {
        return Err(ControlError::Protocol("response ids disagree"));
    }
    Ok(match envelope.request_id {
        0 => Route::Event,
        id => Route::Reply(id),
    })
}

async fn read_responses(
    mut receiver: ControlReceiver<ServerEnvelope>,
    events: mpsc::Sender<ServerEnvelope>,
    pending: Arc<Mutex<Pending>>,
) {
    let error = loop {
        let envelope = match receiver.recv().await {
            Ok(envelope) => envelope,
            Err(error) => break error,
        };
        match route(&envelope) {
            Ok(Route::Event) => {
                if events.try_send(envelope).is_err() {
                    break ControlError::Capacity;
                }
            }
            Ok(Route::Reply(id)) => {
                if let Some(reply) = pending.lock().requests.remove(&id) {
                    let _ = reply.send(Ok(envelope));
                }
            }
            Ok(Route::Unknown) => {
                let mut pending = pending.lock();
                pending.unknown = pending.unknown.saturating_add(1);
            }
            Err(error) => break error,
        }
    };
    pending.lock().fail(&error);
    close_if_open(&receiver.connection, 2, b"control reader stopped");
}

#[cfg(test)]
mod tests {
    use super::*;
    use ss_proto::v1::{ack, server_envelope::Msg};

    fn decode_all(
        decoder: &mut FrameDecoder<ServerEnvelope>,
        mut input: &[u8],
    ) -> Vec<ServerEnvelope> {
        let mut out = Vec::new();
        while let Some(message) = decoder.feed(&mut input).unwrap() {
            out.push(message);
        }
        assert_eq!(input.len(), 0);
        out
    }

    #[test]
    fn frames_decode_identically_at_every_chunk_size() {
        let messages = [
            ServerEnvelope::default(),
            ServerEnvelope::ack(7, ack::Result::Empty(v1::Empty {})),
            ServerEnvelope::rate_limited(0, "slow down", std::time::Duration::from_secs(1)),
        ];
        let mut stream = Vec::new();
        for message in &messages {
            stream.extend_from_slice(&frame::encode(message).unwrap());
        }
        for size in 1..=stream.len() {
            let mut decoder = FrameDecoder::default();
            let mut messages_seen = Vec::new();
            for chunk in stream.chunks(size) {
                messages_seen.extend(decode_all(&mut decoder, chunk));
            }
            assert_eq!(messages_seen, messages, "chunk size {size}");
            assert_eq!(decoder.wanted().unwrap(), 4);
        }
    }

    #[test]
    fn oversize_prefixes_and_bad_bodies_stay_failed() {
        let mut decoder = FrameDecoder::<ServerEnvelope>::default();
        let mut input = &[0xff, 0xff, 0xff, 0xff, 0][..];
        assert!(matches!(
            decoder.feed(&mut input),
            Err(FrameError::TooLarge { .. })
        ));
        assert_eq!(input, [0]);
        assert!(decoder.wanted().is_err());
        let mut decoder = FrameDecoder::<ServerEnvelope>::default();
        assert!(decoder.feed(&mut &[0, 0, 0, 1, 0xff][..]).is_err());
        assert!(decoder.feed(&mut &[][..]).is_err());
    }

    #[test]
    fn unsolicited_errors_are_events_and_unknown_messages_are_skipped() {
        let notice = ServerEnvelope::rate_limited(
            0,
            "media bitrate exceeded",
            std::time::Duration::from_secs(2),
        );
        assert_eq!(route(&notice).unwrap(), Route::Event);
        let unknown = ServerEnvelope {
            request_id: 0,
            msg: None,
        };
        assert_eq!(route(&unknown).unwrap(), Route::Unknown);
        // A reply in a shape this build does not know still ends its request.
        let reply = ServerEnvelope {
            request_id: 9,
            msg: None,
        };
        assert_eq!(route(&reply).unwrap(), Route::Reply(9));
        assert!(route(&ServerEnvelope::ack(0, ack::Result::Empty(v1::Empty {}))).is_err());
        let mut mismatch = ServerEnvelope::ack(3, ack::Result::Empty(v1::Empty {}));
        mismatch.request_id = 4;
        assert!(route(&mismatch).is_err());
        let mut late = ServerEnvelope::error(5, v1::ErrorCode::Invalid, "x");
        if let Some(Msg::Error(error)) = late.msg.as_mut() {
            error.request_id = 0;
        }
        assert!(
            route(&late).is_err(),
            "a reply's id must match its envelope"
        );
    }
}
