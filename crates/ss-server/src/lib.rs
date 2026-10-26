//! Unauthenticated voice spike: QUIC control, connection membership, and opaque
//! Opus forwarding. Accounts, persistence, and permissions arrive in phase 2.

pub mod media;
pub mod voice;

use prost::Message;
use ss_core::{Id, Timestamp};
use ss_net::{
    Connection,
    control::{ControlError, ServerStream},
    server::Endpoint,
};
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope},
    validate::{id_from_bytes, id_to_bytes},
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet, time::timeout};

/// Maximum pending and active peer sessions accepted by this spike.
pub const MAX_CONNECTIONS: usize = 256;
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Failures confined to one peer session.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Transport setup failed.
    #[error(transparent)]
    Transport(#[from] ss_net::Error),
    /// Malformed or closed control stream.
    #[error(transparent)]
    Control(#[from] ControlError),
    /// Peer stopped sending media.
    #[error(transparent)]
    Connection(#[from] quinn::ConnectionError),
    /// The peer did not open its session within the deadline.
    #[error("session open timed out")]
    Timeout,
    /// Hello or the request-id sequence violated the control protocol.
    #[error("control protocol rejected")]
    Protocol,
}

/// Server state shared by bounded peer tasks. Closing the endpoint ends `run`.
pub struct Server {
    registry: Arc<voice::Registry>,
    router: Arc<media::Router>,
}

impl Default for Server {
    fn default() -> Self {
        let registry = Arc::new(voice::Registry::default());
        Self {
            router: Arc::new(media::Router::new(Arc::clone(&registry))),
            registry,
        }
    }
}

impl Server {
    /// Exposes membership diagnostics.
    pub fn registry(&self) -> &voice::Registry {
        &self.registry
    }

    /// Exposes media counters and the synchronous forwarding path.
    pub fn router(&self) -> &media::Router {
        &self.router
    }

    /// Accepts up to 256 concurrent sessions, including pending handshakes.
    /// Each handshake and Hello has its own deadline; stalled peers never block
    /// the accept loop. Cancellation drops all sessions and their memberships.
    pub async fn run(self: Arc<Self>, endpoint: Endpoint) {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    if tasks.len() >= MAX_CONNECTIONS { incoming.refuse(); continue; }
                    let server = Arc::clone(&self);
                    tasks.spawn(async move {
                        let result = async {
                            let connection = timeout(OPEN_TIMEOUT, incoming.accept()).await.map_err(|_| Error::Timeout)??;
                            server.session(connection).await
                        }.await;
                        if let Err(error) = result { tracing::debug!(%error, "peer session ended"); }
                    });
                }
                _ = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
        tasks.shutdown().await;
    }

    async fn session(&self, mut connection: Connection) -> Result<(), Error> {
        let conn_id = Id::generate();
        let _guard = SessionGuard {
            registry: Arc::clone(&self.registry),
            conn_id,
            connection: connection.quic().clone(),
        };
        let (mut stream, request_id, user) = timeout(OPEN_TIMEOUT, open(&mut connection))
            .await
            .map_err(|_| Error::Timeout)??;
        let user_id = id_from_bytes(&user.id).map_err(|_| Error::Protocol)?;
        let welcome = v1::ServerEnvelope {
            request_id,
            msg: Some(server_envelope::Msg::Welcome(v1::Welcome {
                user: Some(user),
                server_info: Some(v1::ServerInfo {
                    name: "SocialSpeak voice spike".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                    features: vec!["unauthenticated-voice-spike".into()],
                }),
                resume_token: String::new(),
                snapshot: Some(v1::Snapshot::default()),
            })),
        };
        // Hello fits one frame, but its username appears twice in Welcome.
        // Reject before registration so every accepted first frame gets a
        // protocol response, even when the expanded response would be too big.
        if welcome.encoded_len() > ss_proto::MAX_FRAME_LEN {
            let error = v1::ServerEnvelope::error(
                request_id,
                v1::ErrorCode::Invalid,
                "username is too large for a Welcome response",
            );
            return reject(&connection, &mut stream, &error).await;
        }
        let (mut sender, mut receiver) = stream.split();
        let (events, mut outgoing) = mpsc::channel(voice::CONTROL_CAPACITY);
        self.registry
            .register(conn_id, user_id, connection.quic().clone(), events);
        self.registry.reply(conn_id, welcome);
        let read = async {
            let mut last_request = request_id;
            loop {
                let envelope = receiver.recv().await?;
                if envelope.request_id <= last_request {
                    return Err(Error::Protocol);
                }
                last_request = envelope.request_id;
                let reply = self.dispatch(conn_id, envelope);
                self.registry.reply(conn_id, reply);
            }
        };
        let write = async {
            while let Some(envelope) = outgoing.recv().await {
                sender.send(&envelope).await?;
            }
            Ok::<(), Error>(())
        };
        let media = async {
            loop {
                match connection.read_datagram().await {
                    Ok(bytes) => {
                        // Let other ready peers contribute to Quinn's next
                        // transmit batch without adding a timer or packet copy.
                        tokio::task::yield_now().await;
                        self.router.forward(conn_id, &bytes);
                    }
                    Err(error) => return Err(Error::Connection(error)),
                }
            }
        };
        tokio::select! { result = read => result, result = write => result, result = media => result }
    }

    fn dispatch(&self, conn: Id, envelope: v1::ClientEnvelope) -> v1::ServerEnvelope {
        let request_id = envelope.request_id;
        match envelope.msg {
            Some(Msg::Ping(ping)) => {
                return v1::ServerEnvelope {
                    request_id,
                    msg: Some(server_envelope::Msg::Pong(v1::Pong {
                        sent_at: ping.sent_at,
                        server_time: Timestamp::now().as_millis(),
                    })),
                };
            }
            Some(Msg::JoinVoice(join)) => {
                let Ok(channel) = id_from_bytes(&join.channel_id) else {
                    return v1::ServerEnvelope::error(
                        request_id,
                        v1::ErrorCode::Invalid,
                        "channel id must be 16 bytes",
                    );
                };
                if let Err(error) =
                    self.registry
                        .join(conn, channel, join.self_mute, join.self_deaf)
                {
                    return v1::ServerEnvelope::error(
                        request_id,
                        v1::ErrorCode::Internal,
                        error.to_string(),
                    );
                }
            }
            Some(Msg::LeaveVoice(_)) => self.registry.leave(conn),
            Some(Msg::SetVoiceState(flags)) => {
                self.registry
                    .set_flags(conn, flags.self_mute, flags.self_deaf);
            }
            _ => {
                return v1::ServerEnvelope::error(
                    request_id,
                    v1::ErrorCode::Invalid,
                    "unsupported command in voice spike",
                );
            }
        }
        v1::ServerEnvelope::ack(request_id, v1::ack::Result::Empty(v1::Empty {}))
    }
}

struct SessionGuard {
    registry: Arc<voice::Registry>,
    conn_id: Id,
    connection: quinn::Connection,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.registry.unregister(self.conn_id);
        self.connection.close(0_u32.into(), b"session ended");
    }
}

async fn open(connection: &mut Connection) -> Result<(ServerStream, u32, v1::User), Error> {
    let mut stream = connection.accept_control().await?;
    let envelope = stream.recv().await?;
    let request_id = envelope.request_id;
    let failure = v1::ServerEnvelope::error(
        request_id,
        v1::ErrorCode::Invalid,
        "first request must be Hello with a nonzero id and a login or register username",
    );
    let username = match envelope.msg {
        Some(Msg::Hello(hello)) if request_id != 0 => {
            if !ss_proto::is_supported_version(hello.protocol_version) {
                return reject(
                    connection,
                    &mut stream,
                    &ss_proto::unsupported_version(request_id),
                )
                .await;
            }
            match hello.auth {
                Some(v1::hello::Auth::Login(login)) => login.username,
                Some(v1::hello::Auth::Register(register)) => register.username,
                _ => return reject(connection, &mut stream, &failure).await,
            }
        }
        _ => return reject(connection, &mut stream, &failure).await,
    };
    let user = v1::User {
        id: id_to_bytes(Id::generate()),
        display_name: username.clone(),
        username,
        avatar_attachment_id: None,
        kind: v1::UserKind::User.into(),
    };
    Ok((stream, request_id, user))
}

async fn reject<T>(
    connection: &Connection,
    stream: &mut ServerStream,
    envelope: &v1::ServerEnvelope,
) -> Result<T, Error> {
    stream.send(envelope).await?;
    // Give the reliable error a chance to arrive before the guard closes QUIC.
    let _ = timeout(Duration::from_secs(1), connection.quic().closed()).await;
    Err(Error::Protocol)
}
