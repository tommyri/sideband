//! Presence, fan-out, and slow-consumer disconnects on the authenticated server.

use bytes::Bytes;
use ss_core::{Id, Permissions};
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
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(400);

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
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn presence_follows_the_first_and_last_connection() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (_observer, mut observer) = world.connect().await;
    let bea = hello(&mut observer, invite(&world, ada_id, "bea").await).await;
    assert_eq!(status(&bea, "ada"), offline());
    assert_eq!(status(&bea, "bea"), online());

    let (desk, mut desk_stream) = world.connect().await;
    let ada = hello(&mut desk_stream, ada_login).await;
    assert_eq!(status(&ada, "ada"), online());
    assert_eq!(status(&ada, "bea"), online());
    assert_eq!(
        world
            .server
            .registry()
            .client_info(only_conn(&world, ada_id))
            .unwrap()
            .name,
        "desk"
    );
    let arrived = next_presence(&mut observer, ada_id, DEADLINE).await;
    assert_eq!(arrived.status, online());
    assert!(next_presence_if_any(&mut observer, ada_id).await.is_none());

    let idle = command(
        &mut desk_stream,
        2,
        Msg::SetPresence(v1::SetPresence {
            status: i32::from(v1::PresenceStatus::Idle),
            activity: "lunch".into(),
        }),
    )
    .await;
    assert!(matches!(idle.msg, Some(Event::Ack(_))));
    let idle = next_presence(&mut observer, ada_id, DEADLINE).await;
    assert_eq!(idle.status, i32::from(v1::PresenceStatus::Idle));
    assert_eq!(idle.activity, "lunch");
    let rejected = command(
        &mut desk_stream,
        3,
        Msg::SetPresence(v1::SetPresence {
            status: offline(),
            activity: String::new(),
        }),
    )
    .await;
    assert!(matches!(
        rejected.msg,
        Some(Event::Error(ref error)) if error.code == i32::from(v1::ErrorCode::Invalid)
    ));
    assert!(next_presence_if_any(&mut observer, ada_id).await.is_none());

    let pong = command(&mut desk_stream, 4, Msg::Ping(v1::Ping { sent_at: 7 })).await;
    assert!(matches!(pong.msg, Some(Event::Pong(ref pong)) if pong.sent_at == 7));
    assert!(
        world
            .server
            .registry()
            .rtt(only_conn(&world, ada_id))
            .is_some()
    );

    let (laptop, mut laptop_stream) = world.connect().await;
    let second = hello(&mut laptop_stream, login("ada", "laptop")).await;
    assert_eq!(status(&second, "ada"), i32::from(v1::PresenceStatus::Idle));
    assert_eq!(activity(&second, "ada"), "lunch");
    assert_eq!(world.server.registry().connection_ids(ada_id).len(), 2);
    assert!(next_presence_if_any(&mut observer, ada_id).await.is_none());

    desk.close(0, b"bye");
    assert!(next_presence_if_any(&mut observer, ada_id).await.is_none());
    assert_eq!(world.server.registry().connection_ids(ada_id).len(), 1);

    laptop.close(0, b"bye");
    let gone = next_presence(&mut observer, ada_id, DEADLINE).await;
    assert_eq!(gone.status, offline());
    assert_eq!(gone.activity, "");
    assert_eq!(world.server.registry().connection_ids(ada_id), Vec::new());
}

#[tokio::test]
async fn hidden_channel_events_are_not_delivered() {
    let world = World::new().await;
    let (_admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, register(&world.code, "ada")).await;
    let general = channel_named(admin.snapshot.as_ref().unwrap(), "general");

    let (_member_conn, mut member_stream) = world.connect().await;
    hello(
        &mut member_stream,
        invite(&world, user_id(&admin), "bea").await,
    )
    .await;

    let created = command(
        &mut admin_stream,
        2,
        Msg::CreateChannel(v1::CreateChannel {
            kind: i32::from(v1::ChannelKind::Text),
            name: "secret".into(),
            position: 1,
            ..Default::default()
        }),
    )
    .await;
    let secret = ack_channel(&created);
    assert_eq!(
        next_channel_name(&mut member_stream, false, DEADLINE).await,
        "secret"
    );

    let override_ack = command(
        &mut admin_stream,
        3,
        Msg::SetChannelOverride(v1::SetChannelOverride {
            channel_id: secret.clone(),
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::VIEW_CHANNEL.bits(),
        }),
    )
    .await;
    assert!(matches!(override_ack.msg, Some(Event::Ack(_))));
    let removed = timeout(DEADLINE, async {
        loop {
            if let Some(Event::ChannelDeleted(event)) = member_stream.recv().await.unwrap().msg {
                break event.channel.unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(removed.id, secret);
    assert_eq!(removed.name, "secret");

    let renamed = command(
        &mut admin_stream,
        4,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: secret,
            name: Some("renamed".into()),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(renamed.msg, Some(Event::Ack(_))));
    assert!(timeout(QUIET, member_stream.recv()).await.is_err());

    let _ = command(
        &mut admin_stream,
        5,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: general,
            name: Some("lounge".into()),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        next_channel_name(&mut member_stream, true, DEADLINE).await,
        "lounge"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn a_full_outbound_queue_disconnects_only_the_slow_peer() {
    let world = World::new().await;
    let (slow_conn, mut slow_stream) = world.connect().await;
    let slow = hello(&mut slow_stream, register(&world.code, "ada")).await;
    let slow_id = user_id(&slow);
    let (_healthy_conn, mut healthy_stream) = world.connect().await;
    let healthy = hello(&mut healthy_stream, invite(&world, slow_id, "bea").await).await;
    let healthy_id = user_id(&healthy);
    let slow_conn_id = only_conn(&world, slow_id);
    world
        .server
        .registry()
        .set_slow_after(Duration::from_millis(200));

    // The writer shares this current-thread runtime. Filling the queue without
    // awaiting leaves it unpolled, which is the wedged consumer.
    for _ in 0..(registry::OUTBOUND_CAPACITY + 2) {
        world.server.registry().enqueue(slow_conn_id, filler());
    }
    std::thread::sleep(Duration::from_millis(250));
    world.server.registry().enqueue(slow_conn_id, filler());

    let error = timeout(DEADLINE, slow_conn.quic().closed()).await.unwrap();
    match error {
        quinn::ConnectionError::ApplicationClosed(close) => {
            assert_eq!(
                close.error_code,
                quinn::VarInt::from_u32(registry::CLOSE_SLOW)
            );
        }
        other => panic!("slow peer stayed up: {other:?}"),
    }

    let pong = command(&mut healthy_stream, 2, Msg::Ping(v1::Ping { sent_at: 42 })).await;
    assert!(matches!(
        pong.msg,
        Some(Event::Pong(ref pong)) if pong.sent_at == 42 && pong.server_time > 0
    ));
    assert!(
        world
            .server
            .registry()
            .rtt(only_conn(&world, healthy_id))
            .is_some()
    );
    assert_eq!(world.server.registry().connection_count(), 1);
}

#[tokio::test]
async fn shutdown_notifies_then_closes_with_a_distinct_code() {
    let world = World::new().await;
    let (connection, mut stream) = world.connect().await;
    hello(&mut stream, register(&world.code, "ada")).await;
    world.server.shutdown();
    let notice = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
    match notice.msg {
        Some(Event::ServerNotice(notice)) => assert_eq!(notice.text, "server shutting down"),
        other => panic!("expected a shutdown notice, got {other:?}"),
    }
    let error = timeout(DEADLINE, connection.quic().closed()).await.unwrap();
    match error {
        quinn::ConnectionError::ApplicationClosed(close) => {
            assert_eq!(
                close.error_code,
                quinn::VarInt::from_u32(registry::CLOSE_SHUTDOWN)
            );
            assert_eq!(&close.reason[..], b"server shutting down");
        }
        other => panic!("expected the shutdown close, got {other:?}"),
    }
}

async fn register_owner(world: &World, username: &str) -> (Id, v1::Hello) {
    let registered = auth::Auth::new(Arc::clone(&world.pool))
        .hello(
            &register(&world.code, username),
            "203.0.113.9".parse().unwrap(),
        )
        .await
        .unwrap();
    (registered.user.id, login(username, "desk"))
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

fn login(username: &str, client_name: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: client_name.into(),
        auth: Some(v1::hello::Auth::Login(v1::Login {
            username: username.into(),
            password: PASSWORD.into(),
        })),
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
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if frame.request_id == request_id {
            return frame;
        }
    }
}

async fn next_presence(
    stream: &mut ss_net::control::ClientStream,
    user: Id,
    limit: Duration,
) -> v1::PresenceUpdated {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = timeout(remaining, stream.recv()).await.unwrap().unwrap();
        if let Some(event) = presence_for(frame, user) {
            return event;
        }
    }
}

async fn next_presence_if_any(
    stream: &mut ss_net::control::ClientStream,
    user: Id,
) -> Option<v1::PresenceUpdated> {
    let deadline = tokio::time::Instant::now() + QUIET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let frame = match timeout(remaining, stream.recv()).await {
            Ok(frame) => frame.unwrap(),
            Err(_) => return None,
        };
        if let Some(event) = presence_for(frame, user) {
            return Some(event);
        }
    }
}

fn presence_for(frame: v1::ServerEnvelope, user: Id) -> Option<v1::PresenceUpdated> {
    match frame.msg {
        Some(Event::PresenceUpdated(event))
            if event.user_id.as_ref() == id_to_bytes(user).as_ref() =>
        {
            Some(event)
        }
        _ => None,
    }
}

async fn next_channel_name(
    stream: &mut ss_net::control::ClientStream,
    updated: bool,
    limit: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let frame = timeout(remaining, stream.recv()).await.unwrap().unwrap();
        let name = match frame.msg {
            Some(Event::ChannelCreated(event)) if !updated => {
                event.channel.map(|channel| channel.name)
            }
            Some(Event::ChannelUpdated(event)) if updated => {
                event.channel.map(|channel| channel.name)
            }
            _ => None,
        };
        if let Some(name) = name {
            return name;
        }
    }
}

fn user_id(welcome: &v1::Welcome) -> Id {
    id_from_bytes(&welcome.user.as_ref().unwrap().id).unwrap()
}

fn status(welcome: &v1::Welcome, username: &str) -> i32 {
    snapshot_member(welcome, username)
        .presence
        .as_ref()
        .unwrap()
        .status
}

fn activity(welcome: &v1::Welcome, username: &str) -> String {
    snapshot_member(welcome, username)
        .presence
        .as_ref()
        .unwrap()
        .activity
        .clone()
}

fn snapshot_member<'a>(welcome: &'a v1::Welcome, username: &str) -> &'a v1::Member {
    welcome
        .snapshot
        .as_ref()
        .unwrap()
        .members
        .iter()
        .find(|member| member.user.as_ref().unwrap().username == username)
        .unwrap_or_else(|| panic!("missing member {username}"))
}

fn channel_named(snapshot: &v1::Snapshot, name: &str) -> Bytes {
    snapshot
        .channels
        .iter()
        .find(|channel| channel.name == name)
        .unwrap_or_else(|| panic!("missing channel {name}"))
        .id
        .clone()
}

fn ack_channel(envelope: &v1::ServerEnvelope) -> Bytes {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::ChannelId(id)) => id.clone(),
            other => panic!("expected a channel id, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
    }
}

fn only_conn(world: &World, user: Id) -> Id {
    let ids = world.server.registry().connection_ids(user);
    assert_eq!(ids.len(), 1);
    ids[0]
}

fn filler() -> v1::ServerEnvelope {
    v1::ServerEnvelope {
        request_id: 0,
        msg: Some(Event::ServerNotice(v1::ServerNotice { text: "x".into() })),
    }
}

fn online() -> i32 {
    i32::from(v1::PresenceStatus::Online)
}

fn offline() -> i32 {
    i32::from(v1::PresenceStatus::Offline)
}
