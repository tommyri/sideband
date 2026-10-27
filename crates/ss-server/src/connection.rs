//! Authenticated control-stream owner.
//!
//! The voice spike in the crate root still accepts any username. This server
//! requires [`auth`] and then answers `Ping`. Every other command is rejected
//! until later tasks register handlers. `AuthedContext` is what those handlers
//! will receive.

use crate::{
    Error, MAX_CONNECTIONS,
    auth::{self, Auth},
};
use prost::Message;
use ss_core::{Id, Permissions, Timestamp};
use ss_net::{Connection, control::ServerStream, server::Endpoint};
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope},
    validate::id_to_bytes,
};
use std::{sync::Arc, time::Duration};
use tokio::{task::JoinSet, time::timeout};

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Identity and permission cache for one accepted control stream.
pub struct AuthedContext {
    /// Account that passed `Hello`.
    pub user_id: Id,
    /// This QUIC connection, distinct from the account.
    pub conn_id: Id,
    /// OR of the user's roles and `@everyone`. Channel resolution is T-022.
    pub perms_cache: Permissions,
}

/// Accept loop that authenticates `Hello` before dispatching.
pub struct Server {
    auth: Auth,
}

impl Server {
    /// Shares `pool` with CLI helpers and repository calls.
    pub fn new(pool: Arc<crate::db::Pool>) -> Self {
        Self {
            auth: Auth::new(pool),
        }
    }

    /// Accepts up to [`MAX_CONNECTIONS`] peers. Dropping this future closes them.
    pub async fn run(self: Arc<Self>, endpoint: Endpoint) {
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    if tasks.len() >= MAX_CONNECTIONS {
                        incoming.refuse();
                        continue;
                    }
                    let server = Arc::clone(&self);
                    tasks.spawn(async move {
                        let result = async {
                            let connection = timeout(OPEN_TIMEOUT, incoming.accept())
                                .await
                                .map_err(|_| Error::Timeout)??;
                            server.session(connection).await
                        }
                        .await;
                        if let Err(error) = result {
                            tracing::debug!(%error, "authenticated session ended");
                        }
                    });
                }
                _ = tasks.join_next(), if !tasks.is_empty() => {}
            }
        }
        tasks.shutdown().await;
    }

    async fn session(&self, mut connection: Connection) -> Result<(), Error> {
        let conn_id = Id::generate();
        let peer = connection.quic().remote_address().ip();
        let _close = CloseOnDrop(connection.quic().clone());
        let mut stream = timeout(OPEN_TIMEOUT, connection.accept_control())
            .await
            .map_err(|_| Error::Timeout)??;
        let envelope = timeout(OPEN_TIMEOUT, stream.recv())
            .await
            .map_err(|_| Error::Timeout)??;
        let request_id = envelope.request_id;
        let Some(Msg::Hello(hello)) = envelope.msg else {
            return reject(&connection, &mut stream, &hello_failure(request_id)).await;
        };
        if request_id == 0 {
            return reject(&connection, &mut stream, &hello_failure(request_id)).await;
        }
        if !ss_proto::is_supported_version(hello.protocol_version) {
            return reject(
                &connection,
                &mut stream,
                &ss_proto::unsupported_version(request_id),
            )
            .await;
        }
        let authed = match self.auth.hello(&hello, peer).await {
            Ok(authed) => authed,
            Err(error) => {
                tracing::debug!(%error, "hello rejected");
                let envelope =
                    v1::ServerEnvelope::error(request_id, error.code(), error.wire_message());
                return reject(&connection, &mut stream, &envelope).await;
            }
        };
        let context = AuthedContext {
            user_id: authed.user.id,
            conn_id,
            perms_cache: authed.permissions,
        };
        let welcome = welcome_envelope(request_id, &authed, &self.auth.server_name().await);
        if welcome.encoded_len() > ss_proto::MAX_FRAME_LEN {
            let error = v1::ServerEnvelope::error(
                request_id,
                v1::ErrorCode::Invalid,
                "welcome response is too large",
            );
            return reject(&connection, &mut stream, &error).await;
        }
        stream.send(&welcome).await?;
        dispatch_loop(context, stream, request_id).await
    }
}

fn welcome_envelope(
    request_id: u32,
    authed: &auth::Authenticated,
    server_name: &str,
) -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id,
        msg: Some(server_envelope::Msg::Welcome(v1::Welcome {
            user: Some(wire_user(&authed.user)),
            server_info: Some(v1::ServerInfo {
                name: server_name.to_owned(),
                version: env!("CARGO_PKG_VERSION").into(),
                features: vec!["auth".into()],
            }),
            resume_token: authed.resume_token.clone(),
            snapshot: Some(v1::Snapshot::default()),
        })),
    }
}

fn wire_user(user: &crate::db::User) -> v1::User {
    let kind = match user.kind {
        ss_core::UserKind::Bot => v1::UserKind::Bot,
        _ => v1::UserKind::User,
    };
    v1::User {
        id: id_to_bytes(user.id),
        username: user.username.clone(),
        display_name: user.display_name.clone(),
        avatar_attachment_id: user.avatar_attachment_id.map(id_to_bytes),
        kind: kind.into(),
    }
}

async fn dispatch_loop(
    context: AuthedContext,
    stream: ServerStream,
    mut last_request: u32,
) -> Result<(), Error> {
    let (mut sender, mut receiver) = stream.split();
    loop {
        let envelope = receiver.recv().await?;
        if envelope.request_id <= last_request {
            return Err(Error::Protocol);
        }
        last_request = envelope.request_id;
        sender.send(&dispatch(&context, &envelope)).await?;
    }
}

fn dispatch(context: &AuthedContext, envelope: &v1::ClientEnvelope) -> v1::ServerEnvelope {
    let request_id = envelope.request_id;
    tracing::trace!(
        user_id = %context.user_id,
        conn_id = %context.conn_id,
        permissions = context.perms_cache.bits(),
        "dispatch"
    );
    match &envelope.msg {
        Some(Msg::Ping(ping)) => v1::ServerEnvelope {
            request_id,
            msg: Some(server_envelope::Msg::Pong(v1::Pong {
                sent_at: ping.sent_at,
                server_time: Timestamp::now().as_millis(),
            })),
        },
        _ => v1::ServerEnvelope::error(request_id, v1::ErrorCode::Invalid, "unsupported command"),
    }
}

fn hello_failure(request_id: u32) -> v1::ServerEnvelope {
    v1::ServerEnvelope::error(
        request_id,
        v1::ErrorCode::Invalid,
        "first request must be Hello with a nonzero id",
    )
}

async fn reject<T>(
    connection: &Connection,
    stream: &mut ServerStream,
    envelope: &v1::ServerEnvelope,
) -> Result<T, Error> {
    stream.send(envelope).await?;
    // Leave the connection open long enough for the error frame to arrive.
    // `CloseOnDrop` closes it when this session returns.
    let _ = timeout(Duration::from_secs(1), connection.quic().closed()).await;
    Err(Error::Protocol)
}

struct CloseOnDrop(quinn::Connection);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close(0_u32.into(), b"session ended");
    }
}
