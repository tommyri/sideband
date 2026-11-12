//! The first frame a connection receives after `Hello` is the reply to it.
//! Frames other tasks address to a connection while it signs in (a read
//! state from the user's other session, a kick, a revoked session) follow
//! its `Welcome`.
//!
//! Each test holds sign-ins with [`connection::Server::hold_sign_ins`], so
//! the frame is queued after the connection is registered and before its
//! `Welcome` exists.

use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use ss_core::Id;
use ss_net::{
    client,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope::Msg as Event},
    validate::{id_from_bytes, id_to_bytes},
};
use ss_server::{
    auth, connection,
    db::{self, NewInvite, invites},
    registry,
};
use tokio::time::timeout;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);

struct World {
    endpoint: Endpoint,
    port: u16,
    pins: Arc<MemoryPinStore>,
    task: tokio::task::JoinHandle<()>,
    server: Arc<connection::Server>,
    pool: Arc<db::Pool>,
    code: String,
    _dir: tempfile::TempDir,
}

impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            db::Pool::open(dir.path().join("socialspeak.db"))
                .await
                .unwrap(),
        );
        let code = auth::ensure_owner_invite(&pool).await.unwrap().unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(
            &ServerId::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap(),
            fingerprint,
        )
        .unwrap();
        let server = Arc::new(connection::Server::open(Arc::clone(&pool)).await.unwrap());
        let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
        Self {
            port: endpoint.local_addr().unwrap().port(),
            endpoint,
            pins,
            task,
            server,
            pool,
            code,
            _dir: dir,
        }
    }

    async fn connect(&self) -> (ss_net::Connection, ss_net::control::ClientStream) {
        let mut connection = timeout(
            DEADLINE,
            client::connect("127.0.0.1", self.port, self.pins.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        let stream = connection.open_control().await.unwrap();
        (connection, stream)
    }

    /// Sends `hello` on a new connection without waiting for the reply, and
    /// returns once the server has registered the connection: it is now
    /// held before its snapshot.
    async fn start_sign_in(
        &self,
        user: Id,
        hello: v1::Hello,
    ) -> (ss_net::Connection, ss_net::control::ClientStream, Id) {
        let before = self.server.registry().connection_ids(user);
        let (connection, mut stream) = self.connect().await;
        stream
            .send(&v1::ClientEnvelope {
                request_id: 1,
                msg: Some(Msg::Hello(hello)),
            })
            .await
            .unwrap();
        let conn = timeout(DEADLINE, async {
            loop {
                let now = self.server.registry().connection_ids(user);
                if let Some(conn) = now.into_iter().find(|conn| !before.contains(conn)) {
                    break conn;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the signing-in connection was never registered");
        (connection, stream, conn)
    }

    /// Ada, the owner, connected, with her Welcome.
    async fn ada(
        &self,
    ) -> (
        ss_net::Connection,
        ss_net::control::ClientStream,
        v1::Welcome,
    ) {
        let (conn, mut stream) = self.connect().await;
        let welcome = hello(&mut stream, register(&self.code, "ada")).await;
        (conn, stream, welcome)
    }

    /// Registers Bea through an invite Ada made and returns her token. Her
    /// connection is closed again.
    async fn bea_token(&self, ada: Id) -> (Id, String) {
        let (_conn, mut stream) = self.connect().await;
        let welcome = hello(&mut stream, invite(self, ada, "bea").await).await;
        (user_id(&welcome), welcome.resume_token)
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn a_read_state_from_another_session_reaches_a_signing_in_session_after_its_welcome() {
    let world = World::new().await;
    let (_ada_conn, mut ada, welcome) = world.ada().await;
    let ada_id = user_id(&welcome);
    let general = channel_named(&welcome, "general");
    let message = send(&mut ada, 2, &general, "read up to here").await;

    let hold = world.server.hold_sign_ins().await;
    let (_second_conn, mut second, _) = world
        .start_sign_in(ada_id, token(&welcome.resume_token))
        .await;
    // Ada's first session marks the channel read while the second signs in.
    let ack = command(
        &mut ada,
        3,
        Msg::MarkRead(v1::MarkRead {
            channel_id: general.clone(),
            message_id: message.clone(),
        }),
    )
    .await;
    assert!(matches!(ack.msg, Some(Event::Ack(_))), "{ack:?}");
    drop(hold);

    let first = next_frame(&mut second).await;
    assert!(
        first.request_id == 1 && matches!(first.msg, Some(Event::Welcome(_))),
        "the first frame after Hello must be its Welcome, got {first:?}"
    );
    let read = until(
        &mut second,
        |event| matches!(event, Event::ReadStateUpdated(read) if read.channel_id == general),
    )
    .await;
    let Event::ReadStateUpdated(read) = read else {
        unreachable!()
    };
    assert_eq!(read.last_read_message_id, message);
    // The session works: a command after the sign-in is answered.
    let pong = command(&mut second, 2, Msg::Ping(v1::Ping { sent_at: 7 })).await;
    assert!(matches!(pong.msg, Some(Event::Pong(_))), "{pong:?}");
}

#[tokio::test]
async fn a_kick_aimed_at_a_signing_in_connection_follows_its_welcome() {
    let world = World::new().await;
    let (_ada_conn, mut ada, welcome) = world.ada().await;
    let (bea_id, bea_token) = world.bea_token(user_id(&welcome)).await;

    let hold = world.server.hold_sign_ins().await;
    let (bea_conn, mut bea, _) = world.start_sign_in(bea_id, token(&bea_token)).await;
    let ack = command(
        &mut ada,
        2,
        Msg::KickUser(v1::KickUser {
            user_id: id_to_bytes(bea_id),
            reason: "out".into(),
        }),
    )
    .await;
    assert!(matches!(ack.msg, Some(Event::Ack(_))), "{ack:?}");
    drop(hold);

    let first = next_frame(&mut bea).await;
    assert!(
        first.request_id == 1 && matches!(first.msg, Some(Event::Welcome(_))),
        "the first frame after Hello must be its Welcome, got {first:?}"
    );
    until(
        &mut bea,
        |event| matches!(event, Event::Kicked(kicked) if kicked.reason == "out"),
    )
    .await;
    assert_closed_with(&bea_conn, registry::CLOSE_KICKED).await;
}

#[tokio::test]
async fn a_session_revoked_while_it_signs_in_is_closed_after_its_welcome() {
    let world = World::new().await;
    let (_ada_conn, _ada, welcome) = world.ada().await;
    let (bea_id, bea_token) = world.bea_token(user_id(&welcome)).await;

    let hold = world.server.hold_sign_ins().await;
    let (bea_conn, mut bea, _) = world.start_sign_in(bea_id, token(&bea_token)).await;
    // The operator CLI deletes the row; the sweep closes what used it.
    assert_eq!(
        auth::revoke_sessions(&world.pool, "bea", None)
            .await
            .unwrap(),
        1
    );
    assert_eq!(world.server.sessions().sweep().await.unwrap(), 1);
    drop(hold);

    let first = next_frame(&mut bea).await;
    assert!(
        first.request_id == 1 && matches!(first.msg, Some(Event::Welcome(_))),
        "the first frame after Hello must be its Welcome, got {first:?}"
    );
    until(&mut bea, |event| matches!(event, Event::SessionRevoked(_))).await;
    assert_closed_with(&bea_conn, auth::CLOSE_SIGNED_OUT).await;
}

async fn assert_closed_with(connection: &ss_net::Connection, code: u32) {
    match timeout(DEADLINE, connection.quic().closed()).await.unwrap() {
        quinn::ConnectionError::ApplicationClosed(close) => {
            assert_eq!(close.error_code, quinn::VarInt::from_u32(code));
        }
        other => panic!("expected close code {code}, got {other:?}"),
    }
}

async fn next_frame(stream: &mut ss_net::control::ClientStream) -> v1::ServerEnvelope {
    timeout(DEADLINE, stream.recv()).await.unwrap().unwrap()
}

/// The first event matching `pred`.
async fn until(stream: &mut ss_net::control::ClientStream, pred: impl Fn(&Event) -> bool) -> Event {
    loop {
        let frame = next_frame(stream).await;
        if let Some(event) = frame.msg
            && pred(&event)
        {
            return event;
        }
    }
}

async fn send(
    stream: &mut ss_net::control::ClientStream,
    request_id: u32,
    channel: &Bytes,
    content: &str,
) -> Bytes {
    let ack = command(
        stream,
        request_id,
        Msg::SendMessage(v1::SendMessage {
            channel_id: channel.clone(),
            content: content.into(),
            ..Default::default()
        }),
    )
    .await;
    match ack.msg {
        Some(Event::Ack(v1::Ack {
            result: Some(v1::ack::Result::MessageId(id)),
            ..
        })) => id,
        other => panic!("expected the message id, got {other:?}"),
    }
}

fn channel_named(welcome: &v1::Welcome, name: &str) -> Bytes {
    welcome
        .snapshot
        .as_ref()
        .unwrap()
        .channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(v1::ChannelKind::Text))
        .unwrap()
        .id
        .clone()
}

async fn invite(world: &World, created_by: Id, username: &str) -> v1::Hello {
    let mut conn = world.pool.write().await;
    invites::insert(
        &mut conn,
        NewInvite {
            id: Id::generate(),
            code: username.into(),
            created_by,
            max_uses: Some(1),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    drop(conn);
    register(username, username)
}

fn register(code: &str, username: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::Register(v1::Register {
            invite_code: code.into(),
            username: username.into(),
            password: PASSWORD.into(),
        })),
        ..Default::default()
    }
}

fn token(token: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::SessionToken(token.into())),
        ..Default::default()
    }
}

async fn hello(stream: &mut ss_net::control::ClientStream, hello: v1::Hello) -> v1::Welcome {
    let envelope = command(stream, 1, Msg::Hello(hello)).await;
    match envelope.msg {
        Some(Event::Welcome(welcome)) => welcome,
        other => panic!("expected Welcome, got {other:?}"),
    }
}

async fn command(
    stream: &mut ss_net::control::ClientStream,
    request_id: u32,
    msg: Msg,
) -> v1::ServerEnvelope {
    stream
        .send(&v1::ClientEnvelope {
            request_id,
            msg: Some(msg),
        })
        .await
        .unwrap();
    loop {
        let frame = next_frame(stream).await;
        if frame.request_id == request_id {
            return frame;
        }
    }
}

fn user_id(welcome: &v1::Welcome) -> Id {
    id_from_bytes(&welcome.user.as_ref().unwrap().id).unwrap()
}
