//! Direct messages, group membership, and DM history.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bytes::Bytes;
use serde_json::Value;
use ss_core::{Id, Timestamp};
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
    db::{self, NewBan, NewInvite, bans, invites, users},
    http,
};
use tokio::time::timeout;
use tower::ServiceExt;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);

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
        let port = endpoint.local_addr().unwrap().port();
        pins.set(&ServerId::new("127.0.0.1", port).unwrap(), fingerprint)
            .unwrap();
        let server = Arc::new(
            ss_server::connection::Server::open(Arc::clone(&pool))
                .await
                .unwrap(),
        );
        let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
        Self {
            port,
            endpoint,
            pins,
            task,
            pool,
            code,
            _dir: dir,
        }
    }

    async fn connect(&self) -> ss_net::control::ClientStream {
        let mut connection = timeout(
            DEADLINE,
            client::connect("127.0.0.1", self.port, self.pins.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        connection.open_control().await.unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn direct_message_is_reused_and_hidden() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (bea_id, bea_login) = member(&world, ada_id, "bea").await;
    let (cy_id, cy_login) = member(&world, ada_id, "cyd").await;
    let (dot_id, _) = member(&world, ada_id, "dot").await;
    let (zed_id, _) = member(&world, ada_id, "zed").await;
    disable(&world, dot_id).await;
    ban(&world, ada_id, zed_id).await;

    let mut ada = world.connect().await;
    hello(&mut ada, ada_login).await;
    let mut bea = world.connect().await;
    hello(&mut bea, bea_login).await;
    let mut cy = world.connect().await;
    hello(&mut cy, cy_login).await;

    reject_targets(&mut ada, ada_id, dot_id, zed_id).await;

    let opened = ack_dm(&command(&mut ada, 8, create_dm(&[id_to_bytes(bea_id)])).await);
    let created = next_channel(&mut bea, true).await;
    assert_eq!(created.id, opened.id);
    assert_eq!(created.kind, i32::from(v1::ChannelKind::Dm));
    assert!(has_member(&created, ada_id));
    assert!(has_member(&created, bea_id));

    let again = ack_dm(&command(&mut bea, 2, create_dm(&[id_to_bytes(ada_id)])).await);
    assert_eq!(again.id, opened.id);

    expect_error(
        &mut cy,
        2,
        send(&opened.id, "nope"),
        forbidden(),
        "unknown channel",
    )
    .await;
    let message = ack_message(&command(&mut ada, 9, send(&opened.id, "ping")).await);
    let _ = command(&mut bea, 3, mark(&opened.id, &message)).await;

    let mut bea_again = world.connect().await;
    let welcome = hello(&mut bea_again, login("bea", "again")).await;
    assert_snapshot(&welcome, &opened.id, &message, ada_id, bea_id);

    let bea_token = login_token(&world, "bea").await;
    let page = history(&world, &bea_token, &opened.id).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(contents(&page.body), vec!["ping".to_owned()]);
    let cy_token = login_token(&world, "cyd").await;
    let hidden = history(&world, &cy_token, &opened.id).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);

    let private = ack_dm(&command(&mut bea, 4, create_dm(&[id_to_bytes(cy_id)])).await);
    let invited = next_channel(&mut cy, true).await;
    assert_eq!(invited.id, private.id);
    expect_error(
        &mut ada,
        10,
        send(&private.id, "spy"),
        forbidden(),
        "unknown channel",
    )
    .await;
    let ada_token = login_token(&world, "ada").await;
    let spied = history(&world, &ada_token, &private.id).await;
    assert_eq!(spied.status, StatusCode::NOT_FOUND);
    expect_error(
        &mut bea,
        5,
        Msg::LeaveDm(v1::LeaveDm {
            channel_id: opened.id,
        }),
        invalid(),
        "a direct message cannot be left",
    )
    .await;
}

#[tokio::test]
async fn group_conversation_renames_adds_and_leaves() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (bea_id, bea_login) = member(&world, ada_id, "bea").await;
    let (cy_id, cy_login) = member(&world, ada_id, "cyd").await;
    let (dee_id, dee_login) = member(&world, ada_id, "dee").await;

    let mut ada = world.connect().await;
    hello(&mut ada, ada_login).await;
    let mut bea = world.connect().await;
    hello(&mut bea, bea_login).await;
    let mut cy = world.connect().await;
    hello(&mut cy, cy_login).await;
    let mut dee = world.connect().await;
    hello(&mut dee, dee_login).await;

    let opened = ack_dm(
        &command(
            &mut ada,
            2,
            create_dm(&[id_to_bytes(bea_id), id_to_bytes(cy_id)]),
        )
        .await,
    );
    let created = next_channel(&mut bea, true).await;
    assert_eq!(created.id, opened.id);
    assert_eq!(created.kind, i32::from(v1::ChannelKind::GroupDm));
    assert_eq!(created.member_ids.len(), 3);
    let _ = next_channel(&mut cy, true).await;

    let _ = command(
        &mut ada,
        3,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: opened.id.clone(),
            name: Some("weekend".into()),
            ..Default::default()
        }),
    )
    .await;
    let renamed = next_channel(&mut bea, false).await;
    assert_eq!(renamed.name, "weekend");

    expect_error(
        &mut bea,
        2,
        Msg::AddDmMembers(v1::AddDmMembers {
            channel_id: opened.id.clone(),
            user_ids: vec![id_to_bytes(dee_id)],
        }),
        forbidden(),
        "only the creator can add members",
    )
    .await;
    let _ = command(
        &mut ada,
        4,
        Msg::AddDmMembers(v1::AddDmMembers {
            channel_id: opened.id.clone(),
            user_ids: vec![id_to_bytes(dee_id)],
        }),
    )
    .await;
    // Newly visible conversations are creations in this session's view.
    let added = next_channel(&mut dee, true).await;
    assert!(has_member(&added, dee_id));
    assert_eq!(added.member_ids.len(), 4);
    let _ = next_channel(&mut ada, false).await;

    let _ = command(
        &mut cy,
        2,
        Msg::LeaveDm(v1::LeaveDm {
            channel_id: opened.id.clone(),
        }),
    )
    .await;
    let left = next_channel(&mut ada, false).await;
    assert!(!has_member(&left, cy_id));
    assert_eq!(left.member_ids.len(), 3);

    expect_error(
        &mut cy,
        3,
        send(&opened.id, "gone"),
        forbidden(),
        "unknown channel",
    )
    .await;
    finish_group(&world, &mut ada, &mut bea, &opened.id, bea_id, cy_id).await;
}

async fn reject_targets(stream: &mut ss_net::control::ClientStream, ada: Id, dot: Id, zed: Id) {
    expect_error(
        stream,
        2,
        create_dm(&[]),
        invalid(),
        "a conversation needs another person",
    )
    .await;
    expect_error(
        stream,
        3,
        create_dm(&[id_to_bytes(ada)]),
        invalid(),
        "a conversation needs another person",
    )
    .await;
    expect_error(
        stream,
        4,
        create_dm(&ten_ids()),
        invalid(),
        "a group conversation can include at most 9 other people",
    )
    .await;
    expect_error(
        stream,
        5,
        create_dm(&[id_to_bytes(Id::generate())]),
        not_found(),
        "unknown user",
    )
    .await;
    expect_error(
        stream,
        6,
        create_dm(&[id_to_bytes(dot)]),
        forbidden(),
        "user cannot be messaged",
    )
    .await;
    expect_error(
        stream,
        7,
        create_dm(&[id_to_bytes(zed)]),
        forbidden(),
        "user cannot be messaged",
    )
    .await;
}

async fn finish_group(
    world: &World,
    ada: &mut ss_net::control::ClientStream,
    bea: &mut ss_net::control::ClientStream,
    channel: &Bytes,
    bea_id: Id,
    cy_id: Id,
) {
    let _ = command(bea, 3, send(channel, "here")).await;
    let bea_token = login_token(world, "bea").await;
    let page = history(world, &bea_token, channel).await;
    assert_eq!(page.status, StatusCode::OK);
    assert_eq!(contents(&page.body), vec!["here".to_owned()]);
    let cy_token = login_token(world, "cyd").await;
    let hidden = history(world, &cy_token, channel).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
    let another = ack_dm(
        &command(
            ada,
            5,
            create_dm(&[id_to_bytes(bea_id), id_to_bytes(cy_id)]),
        )
        .await,
    );
    assert_ne!(another.id, *channel);
    expect_error(
        ada,
        6,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: channel.clone(),
            topic: Some("notes".into()),
            ..Default::default()
        }),
        invalid(),
        "only the name of a group conversation can change",
    )
    .await;
}

fn assert_snapshot(welcome: &v1::Welcome, channel: &Bytes, message: &Bytes, ada: Id, bea: Id) {
    let snap = welcome.snapshot.as_ref().expect("snapshot");
    let dm = snap
        .direct_messages
        .iter()
        .find(|item| item.id == *channel)
        .expect("dm in snapshot");
    assert_eq!(dm.last_message_id.as_ref(), Some(message));
    assert!(has_member(dm, ada));
    assert!(has_member(dm, bea));
    let read = snap
        .read_states
        .iter()
        .find(|state| state.channel_id == *channel)
        .expect("read state");
    assert_eq!(read.last_read_message_id, *message);
    assert_eq!(read.mention_count, 0);
}

async fn member(world: &World, created_by: Id, username: &str) -> (Id, v1::Hello) {
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
    let registered = auth::Auth::new(Arc::clone(&world.pool))
        .hello(
            &register(username, username),
            "203.0.113.9".parse().unwrap(),
        )
        .await
        .unwrap();
    (registered.user.id, login(username, "desk"))
}

async fn disable(world: &World, id: Id) {
    let mut conn = world.pool.write().await;
    users::set_disabled(&mut conn, id, Some(Timestamp::now()))
        .await
        .unwrap();
}

async fn ban(world: &World, banned_by: Id, user_id: Id) {
    let mut conn = world.pool.write().await;
    bans::insert(
        &mut conn,
        NewBan {
            id: Id::generate(),
            user_id,
            banned_by,
            reason: None,
            expires_at: None,
        },
    )
    .await
    .unwrap();
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

async fn expect_error(
    stream: &mut ss_net::control::ClientStream,
    request_id: u32,
    msg: Msg,
    code: i32,
    text: &str,
) {
    let envelope = command(stream, request_id, msg).await;
    assert_eq!(error_of(&envelope), (code, text));
}

async fn next_channel(stream: &mut ss_net::control::ClientStream, created: bool) -> v1::Channel {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        let frame = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            stream.recv(),
        )
        .await
        .unwrap()
        .unwrap();
        let channel = match frame.msg {
            Some(Event::ChannelCreated(message)) if created => message.channel,
            Some(Event::ChannelUpdated(message)) if !created => message.channel,
            _ => None,
        };
        if let Some(channel) = channel {
            return channel;
        }
    }
}

fn create_dm(user_ids: &[Bytes]) -> Msg {
    Msg::CreateDm(v1::CreateDm {
        user_ids: user_ids.to_vec(),
    })
}

fn send(channel: &Bytes, content: &str) -> Msg {
    Msg::SendMessage(v1::SendMessage {
        channel_id: channel.clone(),
        content: content.into(),
        ..Default::default()
    })
}

fn mark(channel: &Bytes, message: &Bytes) -> Msg {
    Msg::MarkRead(v1::MarkRead {
        channel_id: channel.clone(),
        message_id: message.clone(),
    })
}

fn ten_ids() -> Vec<Bytes> {
    let mut ids = Vec::new();
    for _ in 0..10 {
        ids.push(id_to_bytes(Id::generate()));
    }
    ids
}

fn ack_dm(envelope: &v1::ServerEnvelope) -> v1::Channel {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::DmChannel(channel)) => channel.clone(),
            other => panic!("expected a dm channel, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
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

fn has_member(channel: &v1::Channel, user: Id) -> bool {
    let bytes = id_to_bytes(user);
    channel.member_ids.iter().any(|id| id == &bytes)
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

fn not_found() -> i32 {
    i32::from(v1::ErrorCode::NotFound)
}

struct Page {
    status: StatusCode,
    body: Value,
}

async fn history(world: &World, token: &str, channel: &Bytes) -> Page {
    let channel_id = id_from_bytes(channel).unwrap();
    let path = format!("/api/v1/channels/{channel_id}/messages?limit=50");
    let perms = Arc::new(
        ss_server::perms::PermissionService::open(Arc::clone(&world.pool))
            .await
            .unwrap(),
    );
    let router = http::messages::router(Arc::new(auth::Auth::new(Arc::clone(&world.pool))), perms);
    let request = Request::builder()
        .uri(path)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Page { status, body }
}

async fn login_token(world: &World, username: &str) -> String {
    auth::Auth::new(Arc::clone(&world.pool))
        .hello(&login(username, "http"), "203.0.113.9".parse().unwrap())
        .await
        .unwrap()
        .resume_token
}

fn contents(body: &Value) -> Vec<String> {
    body.as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_str().map(str::to_owned))
        .collect()
}
