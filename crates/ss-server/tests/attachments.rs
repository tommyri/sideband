//! Upload, download, dedupe, and orphan cleanup.

use std::{path::Path, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use bytes::Bytes;
use serde_json::Value;
use ss_core::{ChannelKind, Id, Permissions, Timestamp};
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
    db::{
        self, NewChannel, NewInvite, NewMessage, UserProfile, channels, invites, messages, users,
    },
    http, perms,
    storage::{Store, UploadPolicy},
};
use tokio::time::timeout;
use tower::ServiceExt;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);
const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xC9, 0xFE, 0x92, 0xEF, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

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

struct Rig {
    pool: Arc<db::Pool>,
    store: Store,
    perms: Arc<perms::PermissionService>,
    auth: Arc<auth::Auth>,
    channel: Id,
    owner: Id,
    owner_token: String,
    _dir: tempfile::TempDir,
}

impl Rig {
    async fn open(policy: UploadPolicy) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            db::Pool::open(dir.path().join("socialspeak.db"))
                .await
                .unwrap(),
        );
        let code = auth::ensure_owner_invite(&pool).await.unwrap().unwrap();
        let owner = auth::Auth::new(Arc::clone(&pool))
            .hello(&register(&code, "ada"), "203.0.113.9".parse().unwrap())
            .await
            .unwrap();
        let channel = Id::generate();
        let mut conn = pool.write().await;
        channels::insert(
            &mut conn,
            NewChannel {
                id: channel,
                kind: ChannelKind::Text,
                name: "general".into(),
                topic: None,
                parent_id: None,
                position: 0,
                voice_profile: None,
                voice_bitrate: None,
                user_limit: None,
                created_by: owner.user.id,
            },
        )
        .await
        .unwrap();
        drop(conn);
        let perms = Arc::new(
            perms::PermissionService::open(Arc::clone(&pool))
                .await
                .unwrap(),
        );
        let store = Store::open_in(Arc::clone(&pool), dir.path().to_path_buf(), policy)
            .await
            .unwrap();
        Self {
            auth: Arc::new(auth::Auth::new(Arc::clone(&pool))),
            owner: owner.user.id,
            owner_token: owner.resume_token,
            pool,
            store,
            perms,
            channel,
            _dir: dir,
        }
    }

    fn router(&self) -> axum::Router {
        http::attachments::router(
            Arc::clone(&self.auth),
            Arc::clone(&self.perms),
            self.store.clone(),
        )
    }
}

#[tokio::test]
async fn upload_attaches_and_downloads_with_bearer_or_token() {
    let world = World::new().await;
    let (ada_id, ada_login, ada_token) = register_owner(&world, "ada").await;
    let mut ada = world.connect().await;
    let welcome = hello(&mut ada, ada_login).await;
    let general = channel_named(welcome.snapshot.as_ref().unwrap(), "general");
    let (bea_login, bea_token) = invite(&world, ada_id, "bea").await;
    let mut bea = world.connect().await;
    hello(&mut bea, bea_login).await;

    let router = world_router(&world).await;
    let uploaded = post_file(
        &router,
        &ada_token,
        &id_from_bytes(&general).unwrap(),
        "../../pixel.png",
        PNG_1X1,
    )
    .await;
    assert_eq!(uploaded.status, StatusCode::OK);
    assert_eq!(uploaded.body["mime"], "image/png");
    assert_eq!(uploaded.body["width"], 1);
    assert_eq!(uploaded.body["height"], 1);
    assert_eq!(uploaded.body["size"], PNG_1X1.len().to_string());
    let url = uploaded.body["url"].as_str().unwrap();
    assert!(url.contains("/pixel.png?token="), "{url}");
    let file_id = id_from_url(url);

    let hidden = get_bytes(&router, Some(&bea_token), path_of(url)).await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);

    let sent = command(
        &mut ada,
        2,
        Msg::SendMessage(v1::SendMessage {
            channel_id: general,
            content: "with file".into(),
            attachment_ids: vec![id_to_bytes(file_id)],
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(sent.msg, Some(Event::Ack(_))));
    let created = next_created(&mut bea).await;
    let attachment = created
        .message
        .unwrap()
        .attachments
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(attachment.width, Some(1));
    assert_eq!(attachment.height, Some(1));

    let by_token = get_bytes(&router, None, url).await;
    assert_eq!(by_token.status, StatusCode::OK);
    assert_eq!(by_token.body.as_ref(), PNG_1X1);
    let disposition = header(&by_token.headers, "content-disposition");
    assert!(disposition.contains("inline"), "{disposition}");
    assert!(disposition.contains("pixel.png"), "{disposition}");

    let by_bearer = get_bytes(&router, Some(&bea_token), path_of(url)).await;
    assert_eq!(by_bearer.status, StatusCode::OK);
    assert_eq!(by_bearer.body.as_ref(), PNG_1X1);
    let etag = header(&by_bearer.headers, "etag");
    let cached = get_bytes_match(&router, Some(&bea_token), path_of(url), &etag).await;
    assert_eq!(cached.status, StatusCode::NOT_MODIFIED);
    assert!(cached.body.is_empty());

    let anonymous = get_bytes(&router, None, path_of(url)).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

    let profile = command(
        &mut ada,
        3,
        Msg::UpdateProfile(v1::UpdateProfile {
            display_name: String::new(),
            avatar_attachment_id: Some(id_to_bytes(file_id)),
        }),
    )
    .await;
    let user = ack_user(&profile);
    assert_eq!(
        user.avatar_attachment_id.as_deref(),
        Some(id_to_bytes(file_id).as_ref())
    );
    let updated = next_member(&mut bea).await;
    let member = updated.member.unwrap().user.unwrap();
    assert_eq!(member.avatar_attachment_id, user.avatar_attachment_id);
}

#[tokio::test]
async fn rejects_oversize_executables_quota_and_duplicate_blobs() {
    let rig = Rig::open(UploadPolicy {
        max_bytes: 8,
        daily_bytes: 16,
        ..UploadPolicy::default()
    })
    .await;
    let router = rig.router();
    expect_error(
        &router,
        &rig,
        "big.txt",
        b"123456789",
        StatusCode::PAYLOAD_TOO_LARGE,
        "file is too large",
    )
    .await;
    expect_error(
        &router,
        &rig,
        "run.exe",
        b"MZ\x90\x00no",
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "file type is not allowed",
    )
    .await;
    assert_duplicate_blob(&rig, &router).await;
    let quota = expect_error(
        &router,
        &rig,
        "more.txt",
        b"bbbbbbbb",
        StatusCode::TOO_MANY_REQUESTS,
        "daily upload quota exceeded",
    )
    .await;
    assert_eq!(header(&quota.headers, "retry-after"), "86400");
    deny_attach(&rig).await;
    assert_allowlisted_executable(&rig).await;
}

#[tokio::test]
async fn orphan_collection_deletes_unlinked_files() {
    let rig = Rig::open(UploadPolicy::default()).await;
    let router = rig.router();
    let loose = upload_id(&router, &rig, "loose.txt", b"loose").await;
    let avatar = upload_id(&router, &rig, "face.png", b"face-bytes").await;
    let shared = upload_id(&router, &rig, "same.txt", b"shared!!").await;
    let linked = upload_id(&router, &rig, "same.txt", b"shared!!").await;
    let fresh = upload_id(&router, &rig, "fresh.txt", b"fresh").await;
    keep_avatar_and_message(&rig, avatar, linked).await;
    age_attachments(&rig, &[loose, avatar, shared, linked]).await;

    let removed = rig.store.collect_orphans().await.unwrap();
    assert_eq!(removed, 2);
    assert!(row_gone(&rig, loose).await);
    assert!(row_gone(&rig, shared).await);
    assert!(row_kept(&rig, avatar).await.is_some());
    let kept = row_kept(&rig, linked).await.unwrap();
    let recent = row_kept(&rig, fresh).await.unwrap();
    assert!(rig.store.data_dir().join(&kept.storage_path).is_file());
    assert!(rig.store.data_dir().join(&recent.storage_path).is_file());
    assert_eq!(blob_count(rig.store.data_dir()), 3);
}

async fn expect_error(
    router: &axum::Router,
    rig: &Rig,
    filename: &str,
    bytes: &[u8],
    status: StatusCode,
    message: &str,
) -> Upload {
    let uploaded = post_file(router, &rig.owner_token, &rig.channel, filename, bytes).await;
    assert_eq!(uploaded.status, status);
    assert_eq!(uploaded.body["error"], message);
    uploaded
}

async fn assert_duplicate_blob(rig: &Rig, router: &axum::Router) {
    let first = post_file(
        router,
        &rig.owner_token,
        &rig.channel,
        "note.txt",
        b"aaaaaaaa",
    )
    .await;
    let second = post_file(
        router,
        &rig.owner_token,
        &rig.channel,
        "copy.txt",
        b"aaaaaaaa",
    )
    .await;
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(second.status, StatusCode::OK);
    let first_id = id_from_b64(first.body["attachmentId"].as_str().unwrap());
    let second_id = id_from_b64(second.body["attachmentId"].as_str().unwrap());
    assert_ne!(first_id, second_id);
    let mut conn = rig.pool.write().await;
    let left = db::attachments::get(&mut conn, first_id)
        .await
        .unwrap()
        .unwrap();
    let right = db::attachments::get(&mut conn, second_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(left.storage_path, right.storage_path);
    drop(conn);
    assert_eq!(blob_count(rig.store.data_dir()), 1);
    let downloaded = get_bytes(
        router,
        Some(&rig.owner_token),
        path_of(first.body["url"].as_str().unwrap()),
    )
    .await;
    assert_eq!(downloaded.status, StatusCode::OK);
    let disposition = header(&downloaded.headers, "content-disposition");
    assert!(disposition.contains("attachment"), "{disposition}");
}

async fn deny_attach(rig: &Rig) {
    let member_token = member(rig, "bea").await;
    let mut conn = rig.pool.write().await;
    channels::set_override(
        &mut conn,
        rig.channel,
        db::EVERYONE_ROLE_ID,
        Permissions::empty(),
        Permissions::ATTACH_FILES,
    )
    .await
    .unwrap();
    drop(conn);
    rig.perms.reload().await.unwrap();
    let denied = post_file(
        &rig.router(),
        &member_token,
        &rig.channel,
        "no.txt",
        b"abcd",
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.body["error"], "missing attach files");
}

async fn assert_allowlisted_executable(rig: &Rig) {
    let allowed = Store::open_in(
        Arc::clone(&rig.pool),
        rig.store.data_dir().to_path_buf(),
        UploadPolicy {
            executable_allow: vec!["exe".into()],
            ..UploadPolicy::default()
        },
    )
    .await
    .unwrap();
    let router = http::attachments::router(Arc::clone(&rig.auth), Arc::clone(&rig.perms), allowed);
    let exe = post_file(
        &router,
        &rig.owner_token,
        &rig.channel,
        "run.exe",
        b"MZ\x90\x00ok",
    )
    .await;
    assert_eq!(exe.status, StatusCode::OK, "{}", exe.body);
}

async fn upload_id(router: &axum::Router, rig: &Rig, filename: &str, bytes: &[u8]) -> Id {
    let uploaded = post_file(router, &rig.owner_token, &rig.channel, filename, bytes).await;
    assert_eq!(uploaded.status, StatusCode::OK, "{}", uploaded.body);
    id_from_b64(uploaded.body["attachmentId"].as_str().unwrap())
}

async fn keep_avatar_and_message(rig: &Rig, avatar: Id, linked: Id) {
    let mut conn = rig.pool.write().await;
    let owner = users::get(&mut conn, rig.owner).await.unwrap().unwrap();
    users::set_profile(
        &mut conn,
        rig.owner,
        UserProfile {
            display_name: owner.display_name,
            avatar_attachment_id: Some(avatar),
        },
    )
    .await
    .unwrap();
    let message = messages::insert(
        &mut conn,
        NewMessage {
            id: Id::generate(),
            channel_id: rig.channel,
            author_id: Some(rig.owner),
            webhook_id: None,
            webhook_name: None,
            content: "kept".into(),
            reply_to: None,
            mentions: Vec::new(),
            mentions_everyone: false,
        },
    )
    .await
    .unwrap();
    db::attachments::set_message(&mut conn, linked, Some(message.id))
        .await
        .unwrap();
}

async fn age_attachments(rig: &Rig, ids: &[Id]) {
    let old = Timestamp::now().as_millis() - 2 * 60 * 60 * 1000;
    let mut conn = rig.pool.write().await;
    for id in ids {
        sqlx::query("UPDATE attachments SET created_at = ? WHERE id = ?")
            .bind(old)
            .bind(id)
            .execute(&mut *conn)
            .await
            .unwrap();
    }
}

async fn row_gone(rig: &Rig, id: Id) -> bool {
    let mut conn = rig.pool.write().await;
    db::attachments::get(&mut conn, id).await.unwrap().is_none()
}

async fn row_kept(rig: &Rig, id: Id) -> Option<db::Attachment> {
    let mut conn = rig.pool.write().await;
    db::attachments::get(&mut conn, id).await.unwrap()
}

async fn world_router(world: &World) -> axum::Router {
    let perms = Arc::new(
        perms::PermissionService::open(Arc::clone(&world.pool))
            .await
            .unwrap(),
    );
    http::attachments::router(
        Arc::new(auth::Auth::new(Arc::clone(&world.pool))),
        perms,
        world.server.files(),
    )
}

struct Upload {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Value,
}

struct Download {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Bytes,
}

async fn post_file(
    router: &axum::Router,
    token: &str,
    channel: &Id,
    filename: &str,
    bytes: &[u8],
) -> Upload {
    let (content_type, body) = multipart(*channel, filename, bytes);
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/attachments")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Upload {
        status,
        headers,
        body,
    }
}

async fn get_bytes(router: &axum::Router, token: Option<&str>, uri: &str) -> Download {
    get_bytes_match(router, token, uri, "").await
}

async fn get_bytes_match(
    router: &axum::Router,
    token: Option<&str>,
    uri: &str,
    etag: &str,
) -> Download {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    if !etag.is_empty() {
        builder = builder.header("if-none-match", etag);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    Download {
        status,
        headers,
        body,
    }
}

fn multipart(channel: Id, filename: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "ssbound";
    let mut body = Vec::new();
    let header = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"channel_id\"\r\n\r\n{channel}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    body.extend(header.as_bytes());
    body.extend(bytes);
    body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

fn path_of(url: &str) -> &str {
    url.split_once('?').map_or(url, |(path, _)| path)
}

fn id_from_url(url: &str) -> Id {
    path_of(url).split('/').nth(4).unwrap().parse().unwrap()
}

fn id_from_b64(value: &str) -> Id {
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, value).unwrap();
    let mut id = [0; 16];
    id.copy_from_slice(&bytes);
    Id::from_bytes(id)
}

fn header(headers: &axum::http::HeaderMap, name: &str) -> String {
    headers.get(name).unwrap().to_str().unwrap().to_owned()
}

fn blob_count(data_dir: &Path) -> usize {
    fn walk(dir: &Path) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut count = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().and_then(|name| name.to_str()) == Some(".incoming") {
                continue;
            }
            if path.is_dir() {
                count += walk(&path);
            } else {
                count += 1;
            }
        }
        count
    }
    walk(&data_dir.join("attachments"))
}

async fn register_owner(world: &World, username: &str) -> (Id, v1::Hello, String) {
    let registered = auth::Auth::new(Arc::clone(&world.pool))
        .hello(
            &register(&world.code, username),
            "203.0.113.9".parse().unwrap(),
        )
        .await
        .unwrap();
    (
        registered.user.id,
        login(username, "desk"),
        registered.resume_token,
    )
}

async fn invite(world: &World, created_by: Id, username: &str) -> (v1::Hello, String) {
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
            "203.0.113.10".parse().unwrap(),
        )
        .await
        .unwrap();
    (login(username, "desk"), registered.resume_token)
}

async fn member(rig: &Rig, username: &str) -> String {
    let mut conn = rig.pool.write().await;
    invites::insert(
        &mut conn,
        NewInvite {
            id: Id::generate(),
            code: username.into(),
            created_by: rig.owner,
            max_uses: Some(1),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    drop(conn);
    auth::Auth::new(Arc::clone(&rig.pool))
        .hello(
            &register(username, username),
            "203.0.113.11".parse().unwrap(),
        )
        .await
        .unwrap()
        .resume_token
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

async fn next_created(stream: &mut ss_net::control::ClientStream) -> v1::MessageCreated {
    loop {
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if let Some(Event::MessageCreated(message)) = frame.msg {
            return message;
        }
    }
}

async fn next_member(stream: &mut ss_net::control::ClientStream) -> v1::MemberUpdated {
    loop {
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if let Some(Event::MemberUpdated(member)) = frame.msg {
            return member;
        }
    }
}

fn ack_user(envelope: &v1::ServerEnvelope) -> v1::User {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::User(user)) => user.clone(),
            other => panic!("expected a user, got {other:?}"),
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
