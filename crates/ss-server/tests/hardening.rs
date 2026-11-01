//! T-061 limits: control-stream quotas with `retry_after_ms`, per-address
//! session caps, input limits, authorization fixes, and HTTP hardening.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
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
    auth,
    db::{self, NewInvite, invites},
    http::hardening,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::timeout,
};
use tower::ServiceExt;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);

struct World {
    endpoint: Endpoint,
    port: u16,
    pins: Arc<MemoryPinStore>,
    task: tokio::task::JoinHandle<()>,
    server: Arc<ss_server::connection::Server>,
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

    /// The owner, logged in over QUIC, plus the `general` text channel.
    async fn owner(&self) -> (Id, Session, Bytes) {
        let registered = auth::Auth::new(Arc::clone(&self.pool))
            .hello(&register(&self.code, "ada"), "203.0.113.9".parse().unwrap())
            .await
            .unwrap();
        let mut session = self.session().await;
        let welcome = session.hello(login("ada")).await;
        let general = text_channel(welcome.snapshot.as_ref().unwrap(), "general");
        (registered.user.id, session, general)
    }

    /// A second member invited by `owner`, logged in over QUIC.
    async fn member(&self, owner: Id, username: &str) -> (Id, Session) {
        let mut conn = self.pool.write().await;
        invites::insert(
            &mut conn,
            NewInvite {
                id: Id::generate(),
                code: username.into(),
                created_by: owner,
                max_uses: Some(1),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        drop(conn);
        let mut session = self.session().await;
        let welcome = session.hello(register(username, username)).await;
        let id = id_from_bytes(&welcome.user.unwrap().id).unwrap();
        (id, session)
    }

    async fn session(&self) -> Session {
        let (connection, stream) = self.connect().await;
        Session {
            _connection: connection,
            stream,
            next: 1,
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

struct Session {
    _connection: ss_net::Connection,
    stream: ss_net::control::ClientStream,
    next: u32,
}

impl Session {
    async fn hello(&mut self, hello: v1::Hello) -> v1::Welcome {
        match self.call(Msg::Hello(hello)).await.msg {
            Some(Event::Welcome(welcome)) => welcome,
            other => panic!("expected Welcome, got {other:?}"),
        }
    }

    async fn call(&mut self, msg: Msg) -> v1::ServerEnvelope {
        let request_id = self.send(msg).await;
        self.reply(request_id).await
    }

    async fn send(&mut self, msg: Msg) -> u32 {
        let request_id = self.next;
        self.next += 1;
        self.stream
            .send(&v1::ClientEnvelope {
                request_id,
                msg: Some(msg),
            })
            .await
            .unwrap();
        request_id
    }

    async fn reply(&mut self, request_id: u32) -> v1::ServerEnvelope {
        loop {
            let frame = timeout(DEADLINE, self.stream.recv())
                .await
                .unwrap()
                .unwrap();
            if frame.request_id == request_id {
                return frame;
            }
        }
    }
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

fn login(username: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::Login(v1::Login {
            username: username.into(),
            password: PASSWORD.into(),
        })),
        ..Default::default()
    }
}

fn token_hello(token: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::SessionToken(token.into())),
        ..Default::default()
    }
}

fn text_channel(snapshot: &v1::Snapshot, name: &str) -> Bytes {
    snapshot
        .channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(v1::ChannelKind::Text))
        .unwrap()
        .id
        .clone()
}

fn error_of(envelope: &v1::ServerEnvelope) -> &v1::Error {
    match &envelope.msg {
        Some(Event::Error(error)) => error,
        other => panic!("expected Error, got {other:?}"),
    }
}

fn is_ack(envelope: &v1::ServerEnvelope) -> bool {
    matches!(envelope.msg, Some(Event::Ack(_)))
}

fn send(channel: &Bytes, content: &str, attachments: Vec<Bytes>) -> Msg {
    Msg::SendMessage(v1::SendMessage {
        channel_id: channel.clone(),
        content: content.into(),
        attachment_ids: attachments,
        ..Default::default()
    })
}

fn presence() -> Msg {
    Msg::SetPresence(v1::SetPresence {
        status: i32::from(v1::PresenceStatus::Online),
        activity: String::new(),
    })
}

#[tokio::test]
async fn a_command_flood_is_rate_limited_with_retry_after_but_ping_still_works() {
    let world = World::new().await;
    let (_owner, mut ada, _general) = world.owner().await;
    let mut ids = Vec::new();
    for _ in 0..160 {
        ids.push(ada.send(presence()).await);
    }
    let mut limited = Vec::new();
    for id in ids {
        let reply = ada.reply(id).await;
        if !is_ack(&reply) {
            limited.push(error_of(&reply).clone());
        }
    }
    assert!(!limited.is_empty(), "160 pipelined commands all passed");
    for error in &limited {
        assert_eq!(error.code, i32::from(v1::ErrorCode::RateLimited));
        assert_eq!(error.message, "too many commands");
        let wait = error.retry_after_ms.unwrap();
        assert!((1..=50).contains(&wait), "{wait}");
    }
    let pong = ada.call(Msg::Ping(v1::Ping { sent_at: 7 })).await;
    assert!(matches!(pong.msg, Some(Event::Pong(_))), "{pong:?}");
}

#[tokio::test]
async fn message_rate_limits_say_when_to_retry() {
    let world = World::new().await;
    let (_owner, mut ada, general) = world.owner().await;
    for _ in 0..5 {
        assert!(is_ack(&ada.call(send(&general, "burst", Vec::new())).await));
    }
    let blocked = ada.call(send(&general, "burst", Vec::new())).await;
    let error = error_of(&blocked);
    assert_eq!(error.code, i32::from(v1::ErrorCode::RateLimited));
    let wait = error.retry_after_ms.unwrap();
    assert!((1..=5000).contains(&wait), "{wait}");
}

#[tokio::test]
async fn hello_attempts_are_limited_per_address() {
    let world = World::new().await;
    let mut rejected = None;
    for attempt in 0..40 {
        let mut session = world.session().await;
        let reply = session.call(Msg::Hello(token_hello("bogus"))).await;
        let error = error_of(&reply).clone();
        if error.code == i32::from(v1::ErrorCode::RateLimited) {
            rejected = Some((attempt, error));
            break;
        }
        assert_eq!(error.code, i32::from(v1::ErrorCode::Unauthenticated));
    }
    let (attempt, error) = rejected.expect("40 Hellos from one address were all answered");
    assert!(attempt >= 30, "limited after {attempt}");
    assert!((1..=2000).contains(&error.retry_after_ms.unwrap()));
}

#[tokio::test]
async fn sessions_are_capped_per_address() {
    let world = World::new().await;
    world.server.limit_sessions_per_ip(2);
    let (_owner, _ada, _general) = world.owner().await;
    let _second = world.session().await;
    let third = timeout(
        DEADLINE,
        client::connect("127.0.0.1", world.port, world.pins.clone()),
    )
    .await
    .unwrap();
    assert!(
        third.is_err(),
        "a third session from one address was accepted"
    );
}

#[tokio::test]
async fn attachments_per_message_are_capped_and_unique() {
    let world = World::new().await;
    let (owner, mut ada, general) = world.owner().await;
    let eleven = (0..11).map(|_| id_to_bytes(Id::generate())).collect();
    let reply = ada.call(send(&general, "files", eleven)).await;
    let error = error_of(&reply);
    assert_eq!(error.code, i32::from(v1::ErrorCode::Invalid));
    assert_eq!(error.message, "a message can carry at most 10 attachments");
    let saved = world
        .server
        .files()
        .save(owner, "note.txt", b"hello")
        .await
        .unwrap();
    let twice = vec![id_to_bytes(saved.id), id_to_bytes(saved.id)];
    let reply = ada.call(send(&general, "files", twice)).await;
    assert_eq!(error_of(&reply).message, "attachment is listed twice");
    let once = vec![id_to_bytes(saved.id)];
    assert!(is_ack(&ada.call(send(&general, "files", once)).await));
}

#[tokio::test]
async fn display_names_need_change_nickname_and_are_normalized() {
    let world = World::new().await;
    let (owner, mut ada, _general) = world.owner().await;
    let (_bea_id, mut bea) = world.member(owner, "bea").await;
    let rename = |name: &str| {
        Msg::UpdateProfile(v1::UpdateProfile {
            display_name: name.into(),
            avatar_attachment_id: None,
        })
    };
    let reply = bea.call(rename("  Be\u{301}a  ")).await;
    let Some(Event::Ack(v1::Ack {
        result: Some(v1::ack::Result::User(user)),
        ..
    })) = reply.msg
    else {
        panic!("expected a user ack, got {reply:?}");
    };
    assert_eq!(user.display_name, "B\u{e9}a");
    let hidden = bea.call(rename("ad\u{202e}nimda")).await;
    assert_eq!(error_of(&hidden).code, i32::from(v1::ErrorCode::Invalid));

    let everyone = Permissions::EVERYONE.difference(Permissions::CHANGE_NICKNAME);
    let update = ada
        .call(Msg::UpdateRole(v1::UpdateRole {
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            permissions: Some(everyone.bits()),
            ..Default::default()
        }))
        .await;
    assert!(is_ack(&update), "{update:?}");
    let denied = bea.call(rename("Someone Else")).await;
    let error = error_of(&denied);
    assert_eq!(error.code, i32::from(v1::ErrorCode::Forbidden));
    assert_eq!(error.message, "missing change nickname");
    // Re-sending the current name is not a change.
    assert!(is_ack(&bea.call(rename("B\u{e9}a")).await));
}

#[tokio::test]
async fn server_mute_follows_the_role_hierarchy() {
    let world = World::new().await;
    let (owner, mut ada, _general) = world.owner().await;
    let (bea_id, mut bea) = world.member(owner, "bea").await;
    let role = ada
        .call(Msg::CreateRole(v1::CreateRole {
            name: "Mods".into(),
            permissions: Permissions::MUTE_MEMBERS.bits(),
            position: 1,
            ..Default::default()
        }))
        .await;
    let Some(Event::Ack(v1::Ack {
        result: Some(v1::ack::Result::Role(role)),
        ..
    })) = role.msg
    else {
        panic!("expected a role ack, got {role:?}");
    };
    let grant = ada
        .call(Msg::SetUserRoles(v1::SetUserRoles {
            user_id: id_to_bytes(bea_id),
            role_ids: vec![role.id],
        }))
        .await;
    assert!(is_ack(&grant), "{grant:?}");
    let mute = bea
        .call(Msg::ServerMuteUser(v1::ServerMuteUser {
            user_id: id_to_bytes(owner),
            muted: true,
        }))
        .await;
    let error = error_of(&mute);
    assert_eq!(error.code, i32::from(v1::ErrorCode::Forbidden));
    assert_eq!(error.message, "cannot act on that member");
    let admin_mute = ada
        .call(Msg::ServerMuteUser(v1::ServerMuteUser {
            user_id: id_to_bytes(bea_id),
            muted: true,
        }))
        .await;
    assert!(is_ack(&admin_mute), "{admin_mute:?}");
}

#[tokio::test]
async fn channel_names_are_normalized_and_topics_capped() {
    let world = World::new().await;
    let (_owner, mut ada, _general) = world.owner().await;
    let create = |name: &str, topic: Option<String>| {
        Msg::CreateChannel(v1::CreateChannel {
            kind: i32::from(v1::ChannelKind::Text),
            name: name.into(),
            topic,
            ..Default::default()
        })
    };
    let made = ada.call(create("  cafe\u{301}  ", None)).await;
    assert!(is_ack(&made), "{made:?}");
    let created = loop {
        let frame = timeout(DEADLINE, ada.stream.recv()).await.unwrap().unwrap();
        if let Some(Event::ChannelCreated(created)) = frame.msg {
            break created.channel.unwrap();
        }
    };
    assert_eq!(created.name, "caf\u{e9}");
    for name in ["re\u{202e}verse", "zero\u{200b}width", "   "] {
        let reply = ada.call(create(name, None)).await;
        assert_eq!(
            error_of(&reply).code,
            i32::from(v1::ErrorCode::Invalid),
            "{name:?}"
        );
    }
    let long = ada.call(create("topic", Some("t".repeat(1025)))).await;
    assert_eq!(
        error_of(&long).message,
        "topic must be at most 1024 characters"
    );
    let fits = ada.call(create("topic", Some("t".repeat(1024)))).await;
    assert!(is_ack(&fits), "{fits:?}");
}

fn hardened(world: &World) -> axum::Router {
    hardening::harden(world.server.public_routes())
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn http_responses_carry_strict_headers_and_never_list_directories() {
    let world = World::new().await;
    let router = hardened(&world);
    for path in [
        "/",
        "/api/v1/attachments/",
        "/attachments/",
        "/attachments/26/01/",
        "/api/v1/channels/",
    ] {
        let response = router
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(
            response.status() == StatusCode::NOT_FOUND
                || response.status() == StatusCode::METHOD_NOT_ALLOWED,
            "{path}: {}",
            response.status()
        );
        let headers = response.headers().clone();
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{path}");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer", "{path}");
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], hardening::CSP);
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert!(!body_text(response).await.contains("Index of"), "{path}");
    }
    let unauthorized = router
        .clone()
        .oneshot(
            Request::get(format!("/api/v1/channels/{}/messages", Id::generate()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized.headers()[header::X_FRAME_OPTIONS], "DENY");
}

#[tokio::test]
async fn webhook_posts_need_json_and_a_small_body() {
    let world = World::new().await;
    let router = hardened(&world);
    let path = format!("/api/v1/webhooks/{}/token", Id::generate());
    let plain = router
        .clone()
        .oneshot(
            Request::post(&path)
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from(r#"{"content":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(plain.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let missing = router
        .clone()
        .oneshot(
            Request::post(&path)
                .body(Body::from(r#"{"content":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let huge = format!(r#"{{"content":"{}"}}"#, "x".repeat(hardening::MAX_BODY));
    let large = router
        .clone()
        .oneshot(
            Request::post(&path)
                .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
                .body(Body::from(huge))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let unknown = router
        .clone()
        .oneshot(
            Request::post(&path)
                .header(header::CONTENT_TYPE, "Application/JSON")
                .body(Body::from(r#"{"content":"hi"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uploads_keep_their_own_body_limit_behind_the_default_cap() {
    let world = World::new().await;
    let (owner, _ada, general) = world.owner().await;
    let token = auth::Auth::new(Arc::clone(&world.pool))
        .hello(&login("ada"), "203.0.113.9".parse().unwrap())
        .await
        .unwrap()
        .resume_token;
    let _ = owner;
    let channel = id_from_bytes(&general).unwrap();
    let file = vec![b'a'; 3 * hardening::MAX_BODY];
    let boundary = "ssbound";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"channel_id\"\r\n\r\n{channel}\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.txt\"\r\n\
         Content-Type: text/plain\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(&file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let response = hardened(&world)
        .oneshot(
            Request::post("/api/v1/attachments")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text_upload = hardened(&world)
        .oneshot(
            Request::post("/api/v1/attachments")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "text/plain")
                .body(Body::from("not a form"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        text_upload.status().is_client_error(),
        "{}",
        text_upload.status()
    );
}

#[tokio::test]
async fn http_requests_are_limited_per_address_with_retry_after() {
    let world = World::new().await;
    let router = hardened(&world);
    let mut limited = None;
    for _ in 0..300 {
        let response = router
            .clone()
            .oneshot(Request::get("/nothing").body(Body::empty()).unwrap())
            .await
            .unwrap();
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(response);
            break;
        }
    }
    let response = limited.expect("300 requests from one address all passed");
    let retry = response.headers()[header::RETRY_AFTER].to_str().unwrap();
    assert_eq!(retry, "1");
    assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
}

#[tokio::test]
async fn a_client_that_never_finishes_its_headers_is_dropped() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let app =
        hardening::harden(axum::Router::new().route("/ok", axum::routing::get(|| async { "ok" })));
    let served = tokio::spawn(hardening::serve_with(
        listener,
        app,
        async move {
            let _ = stop_rx.await;
        },
        hardening::ServeLimits {
            header_timeout: Duration::from_millis(200),
            ..hardening::ServeLimits::default()
        },
    ));
    let mut slow = tokio::net::TcpStream::connect(addr).await.unwrap();
    slow.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let closed = timeout(Duration::from_secs(3), slow.read_to_end(&mut buf)).await;
    assert!(closed.is_ok(), "the half-sent request was never dropped");
    let text = String::from_utf8_lossy(&buf);
    assert!(!text.contains("200 OK"), "{text}");

    let mut quick = tokio::net::TcpStream::connect(addr).await.unwrap();
    quick
        .write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    timeout(DEADLINE, quick.read_to_end(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buf).to_ascii_lowercase();
    assert!(text.starts_with("http/1.1 200"), "{text}");
    assert!(text.contains("x-content-type-options: nosniff"), "{text}");
    let _ = stop_tx.send(());
    timeout(DEADLINE, served).await.unwrap().unwrap();
}

struct Site {
    addr: std::net::SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Site {
    async fn start(limits: hardening::ServeLimits) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let app = hardening::harden(
            axum::Router::new().route("/ok", axum::routing::get(|| async { "ok" })),
        );
        let task = tokio::spawn(hardening::serve_with(
            listener,
            app,
            async move {
                let _ = stop_rx.await;
            },
            limits,
        ));
        Self {
            addr,
            stop: Some(stop_tx),
            task,
        }
    }

    /// A keep-alive connection that has completed one request.
    async fn open(&self) -> tokio::net::TcpStream {
        let mut stream = tokio::net::TcpStream::connect(self.addr).await.unwrap();
        assert!(
            keep_alive_get(&mut stream).await,
            "the first request failed"
        );
        stream
    }

    async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        timeout(DEADLINE, &mut self.task).await.unwrap().unwrap();
    }
}

/// Sends `GET /ok` on a kept-alive connection. `false` when the server
/// closed it instead of answering.
async fn keep_alive_get(stream: &mut tokio::net::TcpStream) -> bool {
    if stream
        .write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\n\r\n")
        .await
        .is_err()
    {
        return false;
    }
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !buf.ends_with(b"\r\n\r\nok") {
        match timeout(DEADLINE, stream.read(&mut chunk)).await.unwrap() {
            Ok(0) | Err(_) => return false,
            Ok(read) => buf.extend_from_slice(&chunk[..read]),
        }
    }
    String::from_utf8_lossy(&buf).starts_with("HTTP/1.1 200")
}

/// Whether the server closes `stream` within `limit`.
async fn closes_within(stream: &mut tokio::net::TcpStream, limit: Duration) -> bool {
    // EOF and a reset both mean the server closed it; a timeout means not.
    let mut rest = Vec::new();
    timeout(limit, stream.read_to_end(&mut rest)).await.is_ok()
}

#[tokio::test]
async fn one_address_cannot_hold_every_http_connection() {
    let site = Site::start(hardening::ServeLimits {
        per_address: 2,
        ..hardening::ServeLimits::default()
    })
    .await;
    let first = site.open().await;
    let _second = site.open().await;
    let mut third = tokio::net::TcpStream::connect(site.addr).await.unwrap();
    assert!(
        !keep_alive_get(&mut third).await,
        "a third connection from one address was served"
    );
    drop(first);
    // The slot frees once the server sees the close.
    let mut served = false;
    for _ in 0..50 {
        let mut retry = tokio::net::TcpStream::connect(site.addr).await.unwrap();
        if keep_alive_get(&mut retry).await {
            served = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(served, "the freed slot was never reused");
    site.stop().await;
}

#[tokio::test]
async fn idle_keep_alive_connections_are_closed() {
    let site = Site::start(hardening::ServeLimits {
        idle_timeout: Duration::from_millis(300),
        ..hardening::ServeLimits::default()
    })
    .await;
    let mut idle = site.open().await;
    assert!(
        closes_within(&mut idle, Duration::from_secs(3)).await,
        "an idle keep-alive connection stayed open"
    );
    site.stop().await;
}

#[tokio::test]
async fn busy_connections_are_closed_at_their_maximum_age() {
    let site = Site::start(hardening::ServeLimits {
        max_age: Duration::from_millis(500),
        ..hardening::ServeLimits::default()
    })
    .await;
    let mut busy = site.open().await;
    let started = std::time::Instant::now();
    let mut answered = 1;
    while keep_alive_get(&mut busy).await {
        answered += 1;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "still open after {answered} requests"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(answered > 1, "closed before it was busy");
    site.stop().await;
}
