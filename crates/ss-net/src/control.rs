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
                buffer: BytesMut::new(),
                marker: PhantomData,
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

/// One typed reader with a bounded, resumable frame buffer.
pub struct ControlReceiver<M> {
    stream: quinn::RecvStream,
    connection: quinn::Connection,
    buffer: BytesMut,
    marker: PhantomData<M>,
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

    async fn read_frame(&mut self) -> Result<M, ControlError> {
        loop {
            let target = if self.buffer.len() < 4 {
                4
            } else {
                let len = u32::from_be_bytes([
                    self.buffer[0],
                    self.buffer[1],
                    self.buffer[2],
                    self.buffer[3],
                ]) as usize;
                if len > frame::MAX_FRAME_LEN {
                    return Err(FrameError::TooLarge { len }.into());
                }
                len + 4
            };
            if self.buffer.len() == target && target >= 4 {
                // Four bytes alone can be a legitimate zero-length protobuf.
                if target != 4 || self.buffer[..4] == [0, 0, 0, 0] {
                    let message = frame::decode(&self.buffer)?;
                    self.buffer.clear();
                    return Ok(message);
                }
                continue;
            }
            let chunk = self
                .stream
                .read_chunk(target - self.buffer.len(), true)
                .await
                .map_err(|error| ControlError::Read(Arc::new(error)))?
                .ok_or_else(|| ControlError::from(FrameError::Incomplete))?;
            self.buffer.extend_from_slice(&chunk.bytes);
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
        if envelope.msg.is_none() {
            break ControlError::Protocol("missing server message");
        }
        let payload_id = match &envelope.msg {
            Some(server_envelope::Msg::Ack(ack)) => Some(ack.request_id),
            Some(server_envelope::Msg::Error(error)) => Some(error.request_id),
            _ => None,
        };
        if payload_id.is_some_and(|id| id == 0 || id != envelope.request_id) {
            break ControlError::Protocol("response ids disagree");
        }
        if envelope.request_id == 0 {
            if events.try_send(envelope).is_err() {
                break ControlError::Capacity;
            }
        } else if let Some(reply) = pending.lock().requests.remove(&envelope.request_id) {
            let _ = reply.send(Ok(envelope));
        }
    };
    pending.lock().fail(&error);
    close_if_open(&receiver.connection, 2, b"control reader stopped");
}
