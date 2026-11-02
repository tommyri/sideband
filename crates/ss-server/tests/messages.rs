//! Text commands, mention counts, and history pagination.

use std::{fmt::Write, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bytes::Bytes;
use serde_json::Value;
use ss_core::{Id, Permissions, Timestamp};
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
    auth,
    db::{self, NewInvite, invites},
    http,
};
use tokio::time::timeout;
use tower::ServiceExt;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(300);

struct World {
    endpoint: Endpoint,
    port: u16,
    pins: Arc<MemoryPinStore>,
    task: tokio::task::JoinHandle<()>,
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
        let server = Arc::new(
            ss_server::connection::Server::open(Arc::clone(&pool))
                .await
                .unwrap(),
        );
        let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
        Self {
            port: endpoint.local_addr().unwrap().port(),
            endpoint,
            pins,
            task,
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
async fn send_requires_permission_and_replies_stay_in_channel() {
    let world = World::new().await;
    let (admin_id, admin_login) = register_owner(&world, "ada").await;
    let (_admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, admin_login).await;
    let general = channel_named(admin.snapshot.as_ref().unwrap(), "general");
    let (_bea_conn, mut bea_stream) = world.connect().await;
    hello(&mut bea_stream, invite(&world, admin_id, "bea").await).await;

    let denied = command(
        &mut admin_stream,
        2,
        Msg::SetChannelOverride(v1::SetChannelOverride {
            channel_id: general.clone(),
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::SEND_MESSAGES.bits(),
        }),
    )
    .await;
    assert!(matches!(denied.msg, Some(Event::Ack(_))));
    let rejected = command(&mut bea_stream, 2, send(&general, "nope", None, None)).await;
    assert_eq!(error_of(&rejected), (forbidden(), "missing send messages"));

    let sent = command(
        &mut admin_stream,
        3,
        send(&general, "hello", None, Some("nonce-1")),
    )
    .await;
    let message_id = ack_message(&sent);
    let created = next_created(&mut bea_stream, DEADLINE).await;
    assert_eq!(created.message.unwrap().content, "hello");
    assert_eq!(created.nonce.as_deref(), Some("nonce-1"));

    let retried = command(
        &mut admin_stream,
        4,
        send(&general, "hello", None, Some("nonce-1")),
    )
    .await;
    assert_eq!(ack_message(&retried), message_id);
    assert!(
        timeout(QUIET, next_created(&mut bea_stream, DEADLINE))
            .await
            .is_err()
    );

    let other = command(
        &mut admin_stream,
        5,
        Msg::CreateChannel(v1::CreateChannel {
            kind: i32::from(v1::ChannelKind::Text),
            name: "other".into(),
            position: 2,
            ..Default::default()
        }),
    )
    .await;
    let other = ack_channel(&other);
    let reply = command(
        &mut admin_stream,
        6,
        send(&other, "cross", Some(message_id), None),
    )
    .await;
    assert_eq!(error_of(&reply), (invalid(), "reply is in another channel"));
}

#[tokio::test]
async fn edits_close_after_seven_days_and_deletes_leave_history() {
    let world = World::new().await;
    let (admin_id, admin_login) = register_owner(&world, "ada").await;
    let token = login_token(&world, "ada").await;
    let (_admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, admin_login).await;
    let general = channel_named(admin.snapshot.as_ref().unwrap(), "general");
    let (_bea_conn, mut bea_stream) = world.connect().await;
    hello(&mut bea_stream, invite(&world, admin_id, "bea").await).await;

    let first = command(&mut admin_stream, 2, send(&general, "stay", None, None)).await;
    let first_id = ack_message(&first);
    let fresh = command(&mut admin_stream, 3, send(&general, "edit-me", None, None)).await;
    let fresh_id = ack_message(&fresh);
    age_message(&world, &first_id).await;

    let stale = command(&mut admin_stream, 4, edit(&first_id, "too late")).await;
    assert_eq!(error_of(&stale), (forbidden(), "edit window has closed"));
    let edited = command(&mut admin_stream, 5, edit(&fresh_id, "edited")).await;
    assert!(matches!(edited.msg, Some(Event::Ack(_))));
    let updated = next_updated(&mut bea_stream, DEADLINE).await;
    let message = updated.message.unwrap();
    assert_eq!(message.content, "edited");
    assert!(message.edited_at.is_some());

    let forbidden_delete = command(&mut bea_stream, 2, delete(&fresh_id)).await;
    assert_eq!(
        error_of(&forbidden_delete),
        (forbidden(), "missing manage messages")
    );
    let removed = command(&mut admin_stream, 6, delete(&fresh_id)).await;
    assert!(matches!(removed.msg, Some(Event::Ack(_))));

    let history = history(&world, &token, &general, None, Some(50)).await;
    assert_eq!(history.status, StatusCode::OK);
    let bodies = contents(&history.body);
    assert_eq!(bodies, vec!["stay".to_owned()]);
}

#[tokio::test]
async fn mentions_update_the_read_cursor_and_history_pages() {
    let world = World::new().await;
    let (admin_id, admin_login) = register_owner(&world, "ada").await;
    let (_admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, admin_login).await;
    let general = channel_named(admin.snapshot.as_ref().unwrap(), "general");
    let (bea_conn, mut bea_stream) = world.connect().await;
    let bea = hello(&mut bea_stream, invite(&world, admin_id, "bea").await).await;
    let bea_id = user_id(&bea);
    let (_second_conn, mut second_stream) = world.connect().await;
    hello(&mut second_stream, login("bea", "phone")).await;

    let everyone = command(
        &mut bea_stream,
        2,
        send(&general, "hey @everyone", None, None),
    )
    .await;
    assert_eq!(
        error_of(&everyone),
        (forbidden(), "missing mention everyone")
    );

    let mut sent = Vec::new();
    for (request, content) in [(3_u32, "one"), (4, "two"), (5, "three")] {
        let envelope = command(
            &mut admin_stream,
            request,
            send(&general, content, None, None),
        )
        .await;
        sent.push((content, ack_message(&envelope)));
    }
    let ping = format!("hello <@{bea_id}>");
    let mentioned = command(&mut admin_stream, 6, send(&general, &ping, None, None)).await;
    assert!(matches!(mentioned.msg, Some(Event::Ack(_))));
    let broadcast = command(
        &mut admin_stream,
        7,
        send(&general, "hey @everyone", None, None),
    )
    .await;
    assert!(matches!(broadcast.msg, Some(Event::Ack(_))));

    let cursor = &sent[0].1;
    let marked = command(&mut bea_stream, 3, mark(&general, cursor)).await;
    assert!(matches!(marked.msg, Some(Event::Ack(_))));
    let state = next_read(&mut second_stream, DEADLINE).await;
    assert!(state.mention_count >= 1);
    let again = hello_snapshot(&world, login("bea", "web")).await;
    let read = again
        .read_states
        .iter()
        .find(|state| state.channel_id == general)
        .unwrap();
    assert!(read.mention_count >= 1);
    bea_conn.close(0, b"bye");
}

#[tokio::test]
async fn history_pages_stop_at_cursors() {
    let world = World::new().await;
    let token = {
        let _ = register_owner(&world, "ada").await;
        login_token(&world, "ada").await
    };
    let (_admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, login("ada", "desk")).await;
    let general = channel_named(admin.snapshot.as_ref().unwrap(), "general");
    let mut sent = Vec::new();
    for (request, content) in [(2_u32, "one"), (3, "two"), (4, "three")] {
        let envelope = command(
            &mut admin_stream,
            request,
            send(&general, content, None, None),
        )
        .await;
        sent.push((content, id_from_bytes(&ack_message(&envelope)).unwrap()));
    }
    sent.sort_by_key(|(_, id)| *id);
    let page = history(&world, &token, &general, None, Some(2)).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(
        contents(&page.body),
        vec![sent[1].0.to_owned(), sent[2].0.to_owned()]
    );
    let older = history(
        &world,
        &token,
        &general,
        Some(sent[1].1.to_string()),
        Some(2),
    )
    .await;
    assert_eq!(contents(&older.body), vec![sent[0].0.to_owned()]);
    let newer = history_after(&world, &token, &general, &sent[0].1.to_string(), Some(2)).await;
    assert_eq!(
        contents(&newer.body),
        vec![sent[1].0.to_owned(), sent[2].0.to_owned()]
    );
    let empty = history(
        &world,
        &token,
        &general,
        Some(sent[0].1.to_string()),
        Some(2),
    )
    .await;
    assert_eq!(contents(&empty.body), Vec::<String>::new());
    let around = history_around(&world, &token, &general, &sent[1].1.to_string(), Some(1)).await;
    assert_eq!(contents(&around.body), vec![sent[1].0.to_owned()]);
    let too_many = history(&world, &token, &general, None, Some(101)).await;
    assert_eq!(too_many.status, StatusCode::BAD_REQUEST);
    let anonymous = history(&world, "", &general, None, Some(1)).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn typing_and_sends_are_rate_limited() {
    let world = World::new().await;
    let (_admin_id, admin_login) = register_owner(&world, "ada").await;
    let (_admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, admin_login).await;
    let general = channel_named(admin.snapshot.as_ref().unwrap(), "general");
    let (_bea_conn, mut bea_stream) = world.connect().await;
    let (bea_id, _) = {
        let welcome = hello(
            &mut bea_stream,
            invite(&world, user_id(&admin), "bea").await,
        )
        .await;
        (user_id(&welcome), welcome)
    };
    let _ = bea_id;

    let first = command(
        &mut admin_stream,
        2,
        Msg::Typing(v1::Typing {
            channel_id: general.clone(),
        }),
    )
    .await;
    assert!(matches!(first.msg, Some(Event::Ack(_))));
    let started = next_typing(&mut bea_stream, DEADLINE).await;
    assert_eq!(started.channel_id, general);
    let second = command(
        &mut admin_stream,
        3,
        Msg::Typing(v1::Typing {
            channel_id: general.clone(),
        }),
    )
    .await;
    assert_eq!(
        error_of(&second),
        (rate_limited(), "typing is rate limited")
    );

    for request in 4..9 {
        let sent = command(
            &mut admin_stream,
            request,
            send(&general, "burst", None, None),
        )
        .await;
        assert!(matches!(sent.msg, Some(Event::Ack(_))), "{sent:?}");
    }
    let blocked = command(&mut admin_stream, 9, send(&general, "burst", None, None)).await;
    assert_eq!(
        error_of(&blocked),
        (rate_limited(), "sending messages too quickly")
    );
}

async fn hello_snapshot(world: &World, hello_message: v1::Hello) -> v1::Snapshot {
    let (_connection, mut stream) = world.connect().await;
    hello(&mut stream, hello_message).await.snapshot.unwrap()
}

async fn login_token(world: &World, username: &str) -> String {
    auth::Auth::new(Arc::clone(&world.pool))
        .hello(&login(username, "http"), "203.0.113.9".parse().unwrap())
        .await
        .unwrap()
        .resume_token
}

async fn age_message(world: &World, id: &Bytes) {
    let message_id = id_from_bytes(id).unwrap();
    let old = Timestamp::now().as_millis() - 8 * 24 * 60 * 60 * 1000;
    let mut conn = world.pool.write().await;
    sqlx::query("UPDATE messages SET created_at = ? WHERE id = ?")
        .bind(old)
        .bind(message_id)
        .execute(&mut *conn)
        .await
        .unwrap();
}

struct Page {
    status: StatusCode,
    body: Value,
}

async fn history(
    world: &World,
    token: &str,
    channel: &Bytes,
    before: Option<String>,
    limit: Option<u32>,
) -> Page {
    let channel_id = id_from_bytes(channel).unwrap();
    let mut path = format!("/api/v1/channels/{channel_id}/messages?");
    if let Some(before) = before {
        let _ = write!(path, "before={before}&");
    }
    if let Some(limit) = limit {
        let _ = write!(path, "limit={limit}");
    }
    get_json(world, token, &path).await
}

async fn history_around(
    world: &World,
    token: &str,
    channel: &Bytes,
    message: &str,
    limit: Option<u32>,
) -> Page {
    let channel_id = id_from_bytes(channel).unwrap();
    let limit = limit.unwrap_or(50);
    let path = format!("/api/v1/channels/{channel_id}/messages/around/{message}?limit={limit}");
    get_json(world, token, &path).await
}

async fn history_after(
    world: &World,
    token: &str,
    channel: &Bytes,
    after: &str,
    limit: Option<u32>,
) -> Page {
    let channel_id = id_from_bytes(channel).unwrap();
    let limit = limit.unwrap_or(50);
    let path = format!("/api/v1/channels/{channel_id}/messages?after={after}&limit={limit}");
    get_json(world, token, &path).await
}

async fn get_json(world: &World, token: &str, path: &str) -> Page {
    let perms = Arc::new(
        ss_server::perms::PermissionService::open(Arc::clone(&world.pool))
            .await
            .unwrap(),
    );
    let router = http::messages::router(Arc::new(auth::Auth::new(Arc::clone(&world.pool))), perms);
    let mut builder = Request::builder().uri(path);
    if !token.is_empty() {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Page { status, body }
}

fn contents(body: &Value) -> Vec<String> {
    body.as_array()
        .unwrap()
        .iter()
        .map(|message| message["content"].as_str().unwrap().to_owned())
        .collect()
}

fn send(channel: &Bytes, content: &str, reply_to: Option<Bytes>, nonce: Option<&str>) -> Msg {
    Msg::SendMessage(v1::SendMessage {
        channel_id: channel.clone(),
        content: content.into(),
        reply_to,
        nonce: nonce.map(str::to_owned),
        ..Default::default()
    })
}

fn edit(message: &Bytes, content: &str) -> Msg {
    Msg::EditMessage(v1::EditMessage {
        message_id: message.clone(),
        content: content.into(),
    })
}

fn delete(message: &Bytes) -> Msg {
    Msg::DeleteMessage(v1::DeleteMessage {
        message_id: message.clone(),
    })
}

fn mark(channel: &Bytes, message: &Bytes) -> Msg {
    Msg::MarkRead(v1::MarkRead {
        channel_id: channel.clone(),
        message_id: message.clone(),
    })
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

async fn next_created(
    stream: &mut ss_net::control::ClientStream,
    limit: Duration,
) -> v1::MessageCreated {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let frame = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            stream.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        if let Some(Event::MessageCreated(message)) = frame.msg {
            return message;
        }
    }
}

async fn next_updated(
    stream: &mut ss_net::control::ClientStream,
    limit: Duration,
) -> v1::MessageUpdated {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let frame = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            stream.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        if let Some(Event::MessageUpdated(message)) = frame.msg {
            return message;
        }
    }
}

async fn next_read(
    stream: &mut ss_net::control::ClientStream,
    limit: Duration,
) -> v1::ReadStateUpdated {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let frame = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            stream.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        if let Some(Event::ReadStateUpdated(state)) = frame.msg {
            return state;
        }
    }
}

async fn next_typing(
    stream: &mut ss_net::control::ClientStream,
    limit: Duration,
) -> v1::TypingStarted {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        let frame = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            stream.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        if let Some(Event::TypingStarted(typing)) = frame.msg {
            return typing;
        }
    }
}

fn ack_message(envelope: &v1::ServerEnvelope) -> Bytes {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::MessageId(id)) => id.clone(),
            other => panic!("expected a message id, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
    }
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

fn channel_named(snapshot: &v1::Snapshot, name: &str) -> Bytes {
    snapshot
        .channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(v1::ChannelKind::Text))
        .unwrap_or_else(|| panic!("missing text channel {name}"))
        .id
        .clone()
}

fn user_id(welcome: &v1::Welcome) -> Id {
    id_from_bytes(&welcome.user.as_ref().unwrap().id).unwrap()
}

fn error_of(envelope: &v1::ServerEnvelope) -> (i32, &str) {
    match &envelope.msg {
        Some(Event::Error(error)) => (error.code, error.message.as_str()),
        other => panic!("expected Error, got {other:?}"),
    }
}

fn forbidden() -> i32 {
    i32::from(v1::ErrorCode::Forbidden)
}

fn invalid() -> i32 {
    i32::from(v1::ErrorCode::Invalid)
}

fn rate_limited() -> i32 {
    i32::from(v1::ErrorCode::RateLimited)
}
