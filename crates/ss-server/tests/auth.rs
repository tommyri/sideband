//! Registration, login, sessions, and the authenticated control stream.
//!
//! `ss-client-core` does not speak `Hello` yet, so these tests drive `ss-net`
//! the same way the voice spike does.

use ss_core::{Id, Permissions, Timestamp};
use ss_net::{
    client,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::v1::{self, client_envelope::Msg, server_envelope::Msg as Event};
use ss_server::{
    auth::{self, Auth},
    connection,
    db::{self, NewBan, NewInvite, bans, invites, sessions, users},
};
use std::{
    net::{IpAddr, Ipv4Addr},
    process::Output,
    sync::Arc,
    time::Duration,
};
use tokio::{task::JoinHandle, time::timeout};

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);
const SESSION_TTL_MS: i64 = 90 * 24 * 60 * 60 * 1000;

struct Accounts {
    _dir: tempfile::TempDir,
    pool: Arc<db::Pool>,
    auth: Auth,
    code: String,
}

impl Accounts {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            db::Pool::open(dir.path().join("socialspeak.db"))
                .await
                .unwrap(),
        );
        let code = auth::ensure_owner_invite(&pool).await.unwrap().unwrap();
        let auth = Auth::new(Arc::clone(&pool));
        Self {
            _dir: dir,
            pool,
            auth,
            code,
        }
    }

    async fn register(&self, code: &str, username: &str) -> auth::Authenticated {
        self.auth
            .hello(&register(code, username, PASSWORD), peer(1))
            .await
            .unwrap()
    }
}

fn peer(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
}

fn register(code: &str, username: &str, password: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::Register(v1::Register {
            invite_code: code.into(),
            username: username.into(),
            password: password.into(),
        })),
        ..Default::default()
    }
}

fn login(username: &str, password: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "test".into(),
        auth: Some(v1::hello::Auth::Login(v1::Login {
            username: username.into(),
            password: password.into(),
        })),
        ..Default::default()
    }
}

fn session_token(token: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        auth: Some(v1::hello::Auth::SessionToken(token.into())),
        ..Default::default()
    }
}

fn resume_token(token: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        auth: Some(v1::hello::Auth::ResumeToken(token.into())),
        ..Default::default()
    }
}

#[tokio::test]
async fn register_accepts_the_owner_code_and_rejects_bad_invites() {
    let accounts = Accounts::new().await;
    let again = auth::ensure_owner_invite(&accounts.pool)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again, accounts.code);
    let rotated = auth::create_owner_invite(&accounts.pool).await.unwrap();
    assert_ne!(rotated, accounts.code);

    let stale = accounts
        .auth
        .hello(&register(&accounts.code, "ada", PASSWORD), peer(1))
        .await
        .unwrap_err();
    assert!(matches!(stale, auth::Error::InvalidInvite));

    let owner = accounts.register(&rotated, "ada").await;
    assert_eq!(owner.user.username, "ada");
    assert_eq!(owner.resume_token.len(), 43);
    assert!(owner.permissions.contains(Permissions::ADMINISTRATOR));
    assert!(owner.permissions.contains(Permissions::VIEW_CHANNEL));
    assert!(matches!(
        auth::create_owner_invite(&accounts.pool).await,
        Err(auth::Error::OwnerExists)
    ));

    let open = insert_invite(&accounts, owner.user.id, "once", Some(1), None).await;
    let member = accounts.register(&open.code, "bea").await;
    assert!(!member.permissions.contains(Permissions::ADMINISTRATOR));
    assert!(member.permissions.contains(Permissions::SPEAK));
    let used = accounts
        .auth
        .hello(&register(&open.code, "cyr", PASSWORD), peer(1))
        .await
        .unwrap_err();
    assert!(matches!(used, auth::Error::InvalidInvite));

    let expired = insert_invite(
        &accounts,
        owner.user.id,
        "old",
        None,
        Some(Timestamp::from_millis(1)),
    )
    .await;
    let expired = accounts
        .auth
        .hello(&register(&expired.code, "dee", PASSWORD), peer(4))
        .await
        .unwrap_err();
    assert!(matches!(expired, auth::Error::InvalidInvite));

    let revoked = insert_invite(&accounts, owner.user.id, "gone", None, None).await;
    {
        let mut conn = accounts.pool.write().await;
        invites::revoke(&mut conn, revoked.id, Timestamp::now())
            .await
            .unwrap();
    }
    let revoked = accounts
        .auth
        .hello(&register(&revoked.code, "eve", PASSWORD), peer(5))
        .await
        .unwrap_err();
    assert!(matches!(revoked, auth::Error::InvalidInvite));

    let spare = insert_invite(&accounts, owner.user.id, "spare", None, None).await;
    let duplicate = accounts
        .auth
        .hello(&register(&spare.code, "ada", PASSWORD), peer(6))
        .await
        .unwrap_err();
    assert!(matches!(duplicate, auth::Error::Conflict));
    assert_eq!(user_count(&accounts).await, 2);
}

#[tokio::test]
async fn login_tokens_bans_and_disabled_accounts() {
    let accounts = Accounts::new().await;
    let owner = accounts.register(&accounts.code, "ada").await;
    let wrong = accounts
        .auth
        .hello(&login("ada", "not-the-password"), peer(1))
        .await
        .unwrap_err();
    assert!(matches!(wrong, auth::Error::Unauthenticated));
    assert_eq!(session_count(&accounts, owner.user.id).await, 1);

    let logged_in = accounts
        .auth
        .hello(&login("ada", PASSWORD), peer(1))
        .await
        .unwrap();
    assert_eq!(logged_in.user.id, owner.user.id);
    assert_ne!(logged_in.resume_token, owner.resume_token);
    let resumed = accounts
        .auth
        .hello(&session_token(&logged_in.resume_token), peer(1))
        .await
        .unwrap();
    assert_eq!(resumed.user.id, owner.user.id);
    assert_eq!(resumed.resume_token, logged_in.resume_token);
    {
        let mut conn = accounts.pool.write().await;
        let stored = sessions::list_for_user(&mut conn, owner.user.id)
            .await
            .unwrap();
        assert!(stored.iter().all(|session| {
            session.token_hash != logged_in.resume_token && session.token_hash.len() == 64
        }));
    }
    let via_resume = accounts
        .auth
        .hello(&resume_token(&logged_in.resume_token), peer(1))
        .await
        .unwrap();
    assert_eq!(via_resume.user.id, owner.user.id);
    assert!(session_expiry_is_sliding(&accounts, owner.user.id).await);

    expire_sessions(&accounts).await;
    let expired = accounts
        .auth
        .hello(&session_token(&logged_in.resume_token), peer(1))
        .await
        .unwrap_err();
    assert!(matches!(expired, auth::Error::Unauthenticated));

    let fresh = accounts
        .auth
        .hello(&login("ada", PASSWORD), peer(2))
        .await
        .unwrap();
    let expired_ban = Timestamp::from_millis(1);
    insert_ban(&accounts, owner.user.id, Some(expired_ban)).await;
    accounts
        .auth
        .hello(&session_token(&fresh.resume_token), peer(1))
        .await
        .unwrap();
    let until = Timestamp::from_millis(Timestamp::now().as_millis() + 60_000);
    let ban = insert_ban(&accounts, owner.user.id, Some(until)).await;
    let banned = accounts
        .auth
        .hello(&session_token(&fresh.resume_token), peer(1))
        .await
        .unwrap_err();
    match banned {
        auth::Error::Banned { message } => {
            assert!(
                message.contains(&until.as_millis().to_string()),
                "{message}"
            );
        }
        other => panic!("expected a ban, got {other}"),
    }
    {
        let mut conn = accounts.pool.write().await;
        bans::lift(&mut conn, ban.id, Timestamp::now())
            .await
            .unwrap();
        users::set_disabled(&mut conn, owner.user.id, Some(Timestamp::now()))
            .await
            .unwrap();
    }
    let disabled = accounts
        .auth
        .hello(&login("ada", PASSWORD), peer(3))
        .await
        .unwrap_err();
    assert!(matches!(disabled, auth::Error::Disabled));
}

#[tokio::test]
async fn login_attempts_are_limited_per_address() {
    let accounts = Accounts::new().await;
    for _ in 0..5 {
        let error = accounts
            .auth
            .hello(&login("ghost", PASSWORD), peer(8))
            .await
            .unwrap_err();
        assert!(matches!(error, auth::Error::Unauthenticated));
    }
    let limited = accounts
        .auth
        .hello(&login("ghost", PASSWORD), peer(8))
        .await
        .unwrap_err();
    assert!(matches!(limited, auth::Error::RateLimited));
    let other = accounts
        .auth
        .hello(&login("ghost", PASSWORD), peer(9))
        .await
        .unwrap_err();
    assert!(matches!(other, auth::Error::Unauthenticated));
}

#[tokio::test]
async fn usernames_and_passwords_follow_the_rules() {
    let accounts = Accounts::new().await;
    for (username, password) in [
        ("ab", PASSWORD),
        ("Ada", PASSWORD),
        ("has space", PASSWORD),
        ("ok_name", "short"),
        (&"n".repeat(33), PASSWORD),
    ] {
        let error = accounts
            .auth
            .hello(&register(&accounts.code, username, password), peer(1))
            .await
            .unwrap_err();
        assert!(
            matches!(error, auth::Error::Invalid(_)),
            "{username}: {error}"
        );
    }
    assert_eq!(user_count(&accounts).await, 0);
    let boundary = "a".repeat(32);
    accounts.register(&accounts.code, &boundary).await;
}

#[tokio::test]
async fn authenticated_hello_answers_ping_and_rejects_other_commands() {
    let quic = Quic::new().await;
    let (connection, mut stream) = quic.connect().await;
    let rejected = command(
        &mut stream,
        1,
        Msg::Hello(v1::Hello {
            protocol_version: 99,
            auth: Some(v1::hello::Auth::Login(v1::Login {
                username: "ada".into(),
                password: PASSWORD.into(),
            })),
            ..Default::default()
        }),
    )
    .await;
    assert_eq!(
        error_code(&rejected),
        i32::from(v1::ErrorCode::ProtocolVersion)
    );
    drop(connection);

    let (_connection, mut stream) = quic.connect().await;
    let welcome = command(
        &mut stream,
        1,
        Msg::Hello(register(&quic.code, "ada", PASSWORD)),
    )
    .await;
    let Event::Welcome(welcome) = welcome.msg.unwrap() else {
        panic!("expected Welcome");
    };
    let user = welcome.user.unwrap();
    assert_eq!(user.username, "ada");
    assert_eq!(user.id.len(), 16);
    assert_eq!(welcome.resume_token.len(), 43);
    assert_eq!(welcome.snapshot, Some(v1::Snapshot::default()));
    assert_eq!(welcome.server_info.unwrap().name, "SocialSpeak");

    let pong = command(&mut stream, 2, Msg::Ping(v1::Ping { sent_at: 42 })).await;
    let Event::Pong(pong) = pong.msg.unwrap() else {
        panic!("expected Pong");
    };
    assert_eq!(pong.sent_at, 42);

    let unsupported = command(&mut stream, 3, Msg::JoinVoice(v1::JoinVoice::default())).await;
    assert_eq!(error_code(&unsupported), i32::from(v1::ErrorCode::Invalid));
    match unsupported.msg.unwrap() {
        Event::Error(error) => assert_eq!(error.message, "unsupported command"),
        other => panic!("expected an error, got {other:?}"),
    }

    let (_again, mut again) = quic.connect().await;
    let resumed = command(
        &mut again,
        1,
        Msg::Hello(session_token(&welcome.resume_token)),
    )
    .await;
    let Event::Welcome(resumed) = resumed.msg.unwrap() else {
        panic!("expected Welcome");
    };
    assert_eq!(resumed.user.unwrap().id, user.id);
}

#[tokio::test]
async fn a_banned_hello_is_rejected_and_the_connection_closes() {
    let quic = Quic::new().await;
    let owner = Auth::new(Arc::clone(&quic.pool))
        .hello(&register(&quic.code, "ada", PASSWORD), peer(1))
        .await
        .unwrap();
    let until = Timestamp::from_millis(Timestamp::now().as_millis() + 60_000);
    insert_ban_on(&quic.pool, owner.user.id, Some(until)).await;
    let (connection, mut stream) = quic.connect().await;
    let banned = command(&mut stream, 1, Msg::Hello(login("ada", PASSWORD))).await;
    assert_eq!(error_code(&banned), i32::from(v1::ErrorCode::Banned));
    match banned.msg.unwrap() {
        Event::Error(error) => assert!(error.message.contains(&until.as_millis().to_string())),
        other => panic!("expected an error, got {other:?}"),
    }
    timeout(DEADLINE, connection.quic().closed())
        .await
        .expect("connection closed");
}

#[tokio::test]
async fn cli_prints_an_owner_invite_and_resets_the_password() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_owned();
    let created = succeed(&["invite", "create", "--owner", "--data-dir", path.as_str()]);
    let code = created
        .lines()
        .next()
        .unwrap()
        .trim()
        .strip_prefix("owner invite: ")
        .unwrap()
        .to_owned();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let auth = Auth::new(Arc::clone(&pool));
    auth.hello(&register(&code, "ada", PASSWORD), peer(1))
        .await
        .unwrap();
    let denied = CommandOutput::run(&["invite", "create", "--owner", "--data-dir", path.as_str()]);
    assert!(!denied.output.status.success());
    let stderr = String::from_utf8(denied.output.stderr).unwrap();
    assert!(
        stderr.contains("an owner is already registered"),
        "{stderr}"
    );

    let updated = succeed(&[
        "user",
        "set-password",
        "--data-dir",
        path.as_str(),
        "--username",
        "ada",
        "--password",
        "replacement-ok",
    ]);
    assert_eq!(updated.trim(), "password updated");
    let old = auth
        .hello(&login("ada", PASSWORD), peer(2))
        .await
        .unwrap_err();
    assert!(matches!(old, auth::Error::Unauthenticated));
    auth.hello(&login("ada", "replacement-ok"), peer(2))
        .await
        .unwrap();
}

async fn insert_invite(
    accounts: &Accounts,
    created_by: Id,
    code: &str,
    max_uses: Option<i64>,
    expires_at: Option<Timestamp>,
) -> db::Invite {
    let mut conn = accounts.pool.write().await;
    invites::insert(
        &mut conn,
        NewInvite {
            id: Id::generate(),
            code: code.into(),
            created_by,
            max_uses,
            expires_at,
        },
    )
    .await
    .unwrap()
}

async fn insert_ban(accounts: &Accounts, user_id: Id, expires_at: Option<Timestamp>) -> db::Ban {
    insert_ban_on(&accounts.pool, user_id, expires_at).await
}

async fn insert_ban_on(pool: &db::Pool, user_id: Id, expires_at: Option<Timestamp>) -> db::Ban {
    let mut conn = pool.write().await;
    bans::insert(
        &mut conn,
        NewBan {
            id: Id::generate(),
            user_id,
            banned_by: user_id,
            reason: None,
            expires_at,
        },
    )
    .await
    .unwrap()
}

async fn user_count(accounts: &Accounts) -> i64 {
    let mut conn = accounts.pool.write().await;
    users::count(&mut conn).await.unwrap()
}

async fn session_count(accounts: &Accounts, user_id: Id) -> usize {
    let mut conn = accounts.pool.write().await;
    sessions::list_for_user(&mut conn, user_id)
        .await
        .unwrap()
        .len()
}

async fn session_expiry_is_sliding(accounts: &Accounts, user_id: Id) -> bool {
    let mut conn = accounts.pool.write().await;
    let stored = sessions::list_for_user(&mut conn, user_id).await.unwrap();
    let now = Timestamp::now().as_millis();
    stored.into_iter().any(|session| {
        let Some(expires) = session.expires_at else {
            return false;
        };
        let expires = expires.as_millis();
        expires >= now + SESSION_TTL_MS - 60_000 && expires <= now + SESSION_TTL_MS + 60_000
    })
}

async fn expire_sessions(accounts: &Accounts) {
    let mut conn = accounts.pool.write().await;
    sqlx::query("UPDATE sessions SET expires_at = 1")
        .execute(&mut *conn)
        .await
        .unwrap();
}

struct Quic {
    endpoint: Endpoint,
    port: u16,
    pins: Arc<MemoryPinStore>,
    task: JoinHandle<()>,
    pool: Arc<db::Pool>,
    code: String,
    _dir: tempfile::TempDir,
}

impl Quic {
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

impl Drop for Quic {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
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
    timeout(DEADLINE, stream.recv()).await.unwrap().unwrap()
}

fn error_code(envelope: &v1::ServerEnvelope) -> i32 {
    match &envelope.msg {
        Some(Event::Error(error)) => error.code,
        other => panic!("expected an error, got {other:?}"),
    }
}

struct CommandOutput {
    output: Output,
}

impl CommandOutput {
    fn run(args: &[&str]) -> Self {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_ss-server"))
            .args(args)
            .output()
            .unwrap();
        Self { output }
    }
}

fn succeed(args: &[&str]) -> String {
    let command = CommandOutput::run(args);
    let stderr = String::from_utf8_lossy(&command.output.stderr);
    assert!(
        command.output.status.success(),
        "status {:?}\n{stderr}",
        command.output.status
    );
    String::from_utf8(command.output.stdout).unwrap()
}
