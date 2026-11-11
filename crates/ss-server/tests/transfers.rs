//! T-092: concurrent transfers per account, the upload permission check
//! before the file body, and attachments of deleted messages.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
    response::Response,
};
use bytes::Bytes;
use futures::StreamExt as _;
use serde_json::Value;
use ss_core::{ChannelKind, Id, Permissions, Timestamp};
use ss_proto::v1;
use ss_server::{
    auth,
    db::{self, NewChannel, NewInvite, NewMessage, channels, invites, messages},
    http, perms,
    storage::{Store, UploadPolicy},
};
use tokio::time::timeout;
use tower::ServiceExt;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);
const BOUNDARY: &str = "ssbound";
/// Several download chunks, so a body nobody reads stays open.
const BIG: usize = 512 * 1024;

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
    async fn open() -> Self {
        Self::open_with(UploadPolicy::default()).await
    }

    async fn open_with(policy: UploadPolicy) -> Self {
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

    async fn member(&self, username: &str) -> String {
        let mut conn = self.pool.write().await;
        invites::insert(
            &mut conn,
            NewInvite {
                id: Id::generate(),
                code: username.into(),
                created_by: self.owner,
                max_uses: Some(1),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        drop(conn);
        auth::Auth::new(Arc::clone(&self.pool))
            .hello(
                &register(username, username),
                "203.0.113.11".parse().unwrap(),
            )
            .await
            .unwrap()
            .resume_token
    }

    /// Stores `bytes` for the owner and attaches it to a new message.
    async fn posted(&self, filename: &str, bytes: &[u8]) -> (Id, Id) {
        let saved = self.store.save(self.owner, filename, bytes).await.unwrap();
        let mut conn = self.pool.write().await;
        let message = messages::insert(
            &mut conn,
            NewMessage {
                id: Id::generate(),
                channel_id: self.channel,
                author_id: Some(self.owner),
                webhook_id: None,
                webhook_name: None,
                content: "with file".into(),
                reply_to: None,
                mentions: Vec::new(),
                mentions_everyone: false,
            },
        )
        .await
        .unwrap();
        db::attachments::set_message(&mut conn, saved.id, Some(message.id))
            .await
            .unwrap();
        (saved.id, message.id)
    }
}

#[tokio::test]
async fn an_upload_without_permission_is_rejected_before_the_file_is_read() {
    let rig = Rig::open().await;
    let member = rig.member("bea").await;
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

    // The body sends the channel id and the start of the file, then stalls
    // for ever. Only a check made before reading the file can answer.
    let denied = stalled_upload(&rig.router(), &member, rig.channel).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_eq!(json(denied).await["error"], "missing attach files");

    let hidden = stalled_upload(&rig.router(), &member, Id::generate()).await;
    assert_eq!(hidden.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_channel_id_must_come_before_the_file() {
    let rig = Rig::open().await;
    let mut body = Vec::new();
    body.extend(file_part("late.txt").as_bytes());
    body.extend(b"late");
    body.extend(
        format!(
            "\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"channel_id\"\r\n\r\n{}\r\n--{BOUNDARY}--\r\n",
            rig.channel
        )
        .as_bytes(),
    );
    let response = post(&rig.router(), &rig.owner_token, Body::from(body)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        json(response).await["error"],
        "channel id must come before the file"
    );
}

#[tokio::test]
async fn concurrent_transfers_are_capped_per_account() {
    let rig = Rig::open().await;
    let cap = UploadPolicy::default().transfers_per_account;
    let member = rig.member("bea").await;
    let big = vec![5_u8; BIG];
    let (file, _) = rig.posted("note.bin", &big).await;
    let uri = format!("/api/v1/attachments/{file}/note.bin");
    let router = rig.router();

    // Response bodies that are never read keep their transfer open.
    let mut open = Vec::new();
    for _ in 0..cap {
        let response = get(&router, Some(&rig.owner_token), &uri).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            BIG.to_string().as_str()
        );
        open.push(response);
    }
    let refused = get(&router, Some(&rig.owner_token), &uri).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(refused.headers().contains_key(header::RETRY_AFTER));
    let upload = post(
        &router,
        &rig.owner_token,
        Body::from(multipart(rig.channel, "more.txt", b"more")),
    )
    .await;
    assert_eq!(upload.status(), StatusCode::TOO_MANY_REQUESTS);

    let other = get(&router, Some(&member), &uri).await;
    assert_eq!(
        other.status(),
        StatusCode::OK,
        "another account is not capped"
    );
    let read = axum::body::to_bytes(other.into_body(), BIG).await.unwrap();
    assert_eq!(read.as_ref(), big.as_slice());

    let finished = open.pop().unwrap();
    let read = axum::body::to_bytes(finished.into_body(), BIG)
        .await
        .unwrap();
    assert_eq!(read.as_ref(), big.as_slice());
    let again = get(&router, Some(&rig.owner_token), &uri).await;
    assert_eq!(
        again.status(),
        StatusCode::OK,
        "a finished body frees its slot"
    );
    drop(again);
    drop(open);
    let fresh = get(&router, Some(&rig.owner_token), &uri).await;
    assert_eq!(fresh.status(), StatusCode::OK);
}

#[tokio::test]
async fn signed_downloads_through_one_proxy_count_against_their_signer() {
    let rig = Rig::open().await;
    let bea = rig.member("bea").await;
    let cy = rig.member("cyd").await;
    let router = rig.router();
    let bea_url = uploaded_url(&router, &bea, rig.channel, "bea.bin", &vec![1_u8; BIG]).await;
    let cy_url = uploaded_url(&router, &cy, rig.channel, "cy.bin", b"cy's file").await;
    // Behind NGINX every client has the proxy's address.
    let proxy: SocketAddr = "10.0.0.2:40000".parse().unwrap();
    let cap = UploadPolicy::default().transfers_per_account;
    let mut stalled = Vec::new();
    for _ in 0..cap {
        let response = get_from(&router, &bea_url, proxy).await;
        assert_eq!(response.status(), StatusCode::OK);
        stalled.push(response);
    }
    assert_eq!(
        get_from(&router, &bea_url, proxy).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the signer's own cap still applies"
    );
    let other = get_from(&router, &cy_url, proxy).await;
    assert_eq!(
        other.status(),
        StatusCode::OK,
        "one member's stalled signed downloads blocked another's"
    );
    drop(stalled);
}

#[tokio::test]
async fn a_leaked_signed_url_cannot_lock_its_signer_out() {
    let rig = Rig::open().await;
    let (file, _) = rig.posted("big.bin", &vec![4_u8; BIG]).await;
    let signed = rig.store.download_url(file, "big.bin", rig.owner);
    let bearer = format!("/api/v1/attachments/{file}/big.bin");
    let router = rig.router();
    let cap = UploadPolicy::default().transfers_per_account;
    let mut stalled = Vec::new();
    for _ in 0..cap {
        let response = get(&router, None, &signed).await;
        assert_eq!(response.status(), StatusCode::OK);
        stalled.push(response);
    }
    assert_eq!(
        get(&router, None, &signed).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "signed downloads keep a per-signer cap"
    );
    assert_eq!(
        get(&router, Some(&rig.owner_token), &bearer).await.status(),
        StatusCode::OK,
        "readers of a signed URL used up the signer's own transfers"
    );
    drop(stalled);
}

#[tokio::test]
async fn a_download_nobody_reads_frees_its_slot_at_the_deadline() {
    let rig = Rig::open_with(UploadPolicy {
        transfers_per_account: 1,
        transfer_timeout: Duration::from_millis(300),
        ..UploadPolicy::default()
    })
    .await;
    let (file, _) = rig.posted("big.bin", &vec![3_u8; BIG]).await;
    let uri = format!("/api/v1/attachments/{file}/big.bin");
    let router = rig.router();
    // Never polled, like an HTTP/2 stream whose client stopped reading.
    let held = get(&router, Some(&rig.owner_token), &uri).await;
    assert_eq!(held.status(), StatusCode::OK);
    assert_eq!(
        get(&router, Some(&rig.owner_token), &uri).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(
        get(&router, Some(&rig.owner_token), &uri).await.status(),
        StatusCode::OK,
        "the unread download still held its slot after the deadline"
    );
    let body = axum::body::to_bytes(held.into_body(), BIG).await;
    assert!(body.is_err(), "a cut-off body must not look complete");
}

#[tokio::test]
async fn a_deleted_messages_attachment_is_gone() {
    let rig = Rig::open().await;
    let member = rig.member("bea").await;
    let (file, message) = rig.posted("gone.txt", b"soon gone").await;
    let path = format!("/api/v1/attachments/{file}/gone.txt");
    let signed = rig.store.download_url(file, "gone.txt", rig.owner);
    let router = rig.router();
    assert_eq!(
        get(&router, Some(&member), &path).await.status(),
        StatusCode::OK
    );
    assert_eq!(get(&router, None, &signed).await.status(), StatusCode::OK);

    let mut conn = rig.pool.write().await;
    messages::soft_delete(&mut conn, message, Timestamp::now())
        .await
        .unwrap();
    let row = db::attachments::get(&mut conn, file)
        .await
        .unwrap()
        .unwrap();
    drop(conn);
    for token in [Some(member.as_str()), Some(rig.owner_token.as_str())] {
        assert_eq!(
            get(&router, token, &path).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        get(&router, None, &signed).await.status(),
        StatusCode::NOT_FOUND,
        "a signed URL issued before the delete no longer works"
    );

    assert_eq!(rig.store.collect_orphans().await.unwrap(), 1);
    let mut conn = rig.pool.write().await;
    assert!(
        db::attachments::get(&mut conn, file)
            .await
            .unwrap()
            .is_none()
    );
    drop(conn);
    assert!(
        !rig.store.data_dir().join(&row.storage_path).exists(),
        "the blob is unlinked"
    );
}

async fn stalled_upload(router: &axum::Router, token: &str, channel: Id) -> Response {
    let mut head = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"channel_id\"\r\n\r\n{channel}\r\n"
    )
    .into_bytes();
    head.extend(file_part("stalled.bin").as_bytes());
    head.extend([7_u8; 1024]);
    let body = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from(head))])
        .chain(futures::stream::pending());
    timeout(DEADLINE, post(router, token, Body::from_stream(body)))
        .await
        .expect("the upload waited for a body that never ends")
}

/// Uploads through HTTP and returns the signed URL from the reply.
async fn uploaded_url(
    router: &axum::Router,
    token: &str,
    channel: Id,
    filename: &str,
    bytes: &[u8],
) -> String {
    let response = post(
        router,
        token,
        Body::from(multipart(channel, filename, bytes)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    json(response).await["url"].as_str().unwrap().to_owned()
}

/// An anonymous GET that arrives from `peer`, as the public listener records it.
async fn get_from(router: &axum::Router, uri: &str, peer: SocketAddr) -> Response {
    let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
    request.extensions_mut().insert(ConnectInfo(peer));
    timeout(DEADLINE, router.clone().oneshot(request))
        .await
        .unwrap()
        .unwrap()
}

fn file_part(filename: &str) -> String {
    format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
}

fn multipart(channel: Id, filename: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"channel_id\"\r\n\r\n{channel}\r\n"
    )
    .into_bytes();
    body.extend(file_part(filename).as_bytes());
    body.extend(bytes);
    body.extend(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    body
}

async fn post(router: &axum::Router, token: &str, body: Body) -> Response {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/attachments")
                .header("authorization", format!("Bearer {token}"))
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={BOUNDARY}"),
                )
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get(router: &axum::Router, token: Option<&str>, uri: &str) -> Response {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    timeout(
        DEADLINE,
        router.clone().oneshot(builder.body(Body::empty()).unwrap()),
    )
    .await
    .unwrap()
    .unwrap()
}

async fn json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
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
