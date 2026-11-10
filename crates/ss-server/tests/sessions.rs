//! Session revocation, logout, and expiry: a token stops working on QUIC and
//! HTTPS after a password reset, a `Logout`, a self-service revoke, or the
//! operator's revoke, and connections that used it are closed.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use ss_client_core::{
    Client, ClientEvent, ConnectionState, Credentials, Error, ServerAddr,
    storage::{MemoryStorage, Storage},
    v1,
};
use ss_core::{Id, Timestamp};
use ss_net::{
    pins::{MemoryPinStore, PinStore},
    server::Endpoint,
    tls,
};
use ss_server::{
    auth::{self, Auth},
    connection::Server,
    db::{self, sessions},
};
use std::{net::IpAddr, sync::Arc, time::Duration};
use tokio::{sync::broadcast, task::JoinHandle, time::timeout};
use tower::ServiceExt;

const WAIT: Duration = Duration::from_secs(12);
const PASSWORD: &str = "test-password";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

struct World {
    dir: tempfile::TempDir,
    pool: Arc<db::Pool>,
    endpoint: Endpoint,
    server: Arc<Server>,
    worker: JoinHandle<()>,
    address: ServerAddr,
    pins: Arc<MemoryPinStore>,
    invite: String,
}

impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(
            db::Pool::open(dir.path().join("socialspeak.db"))
                .await
                .unwrap(),
        );
        let invite = auth::ensure_owner_invite(&pool).await.unwrap().unwrap();
        let identity = tls::generate_self_signed(dir.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let address = ServerAddr::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(&address.id().unwrap(), fingerprint).unwrap();
        let server = Arc::new(Server::open(pool.clone()).await.unwrap());
        let worker = tokio::spawn(server.clone().run(endpoint.clone()));
        Self {
            dir,
            pool,
            endpoint,
            server,
            worker,
            address,
            pins,
            invite,
        }
    }

    /// Registers `owner` and returns the client and the token it saved.
    async fn owner(&self) -> (Client, String) {
        let storage = Arc::new(MemoryStorage::default());
        let client = Client::connect_with_storage(
            self.address.clone(),
            Credentials::Register(v1::Register {
                invite_code: self.invite.clone(),
                username: "owner".into(),
                password: PASSWORD.into(),
            }),
            self.pins.clone(),
            storage.clone(),
        )
        .await
        .unwrap();
        let token = saved(&storage, &self.address);
        (client, token)
    }

    /// A password login: a new session of an existing account.
    async fn login(&self, username: &str, password: &str) -> Result<(Client, String), Error> {
        let storage = Arc::new(MemoryStorage::default());
        let client = Client::connect_with_storage(
            self.address.clone(),
            Credentials::Login(v1::Login {
                username: username.into(),
                password: password.into(),
            }),
            self.pins.clone(),
            storage.clone(),
        )
        .await?;
        Ok((client, saved(&storage, &self.address)))
    }

    async fn guest(&self, owner: &Client) -> Client {
        let invite = owner
            .create_invite(v1::CreateInvite {
                max_uses: Some(1),
                expires_at: None,
            })
            .await
            .unwrap();
        Client::connect(
            self.address.clone(),
            Credentials::Register(v1::Register {
                invite_code: invite.code,
                username: "guest".into(),
                password: PASSWORD.into(),
            }),
            self.pins.clone(),
        )
        .await
        .unwrap()
    }

    async fn with_token(&self, token: &str) -> Result<Client, Error> {
        Client::connect(
            self.address.clone(),
            Credentials::Session(token.to_owned()),
            self.pins.clone(),
        )
        .await
    }

    /// Status of an authenticated HTTPS request with `token` as the bearer.
    async fn https(&self, token: &str) -> StatusCode {
        self.server
            .public_routes()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/invites")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    /// Runs the `ss-server` binary against this world's data directory.
    fn cli(&self, args: &[&str]) -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_ss-server"))
            .args(args)
            .arg("--data-dir")
            .arg(self.dir.path())
            .output()
            .unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test done");
        self.worker.abort();
    }
}

fn saved(storage: &MemoryStorage, address: &ServerAddr) -> String {
    storage.session(&address.id().unwrap()).unwrap().unwrap()
}

fn peer() -> IpAddr {
    IpAddr::from([203, 0, 113, 7])
}

fn session_hello(token: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        auth: Some(v1::hello::Auth::SessionToken(token.into())),
        ..Default::default()
    }
}

fn unauthenticated(result: &Result<Client, Error>) -> bool {
    matches!(result, Err(Error::Server { code, .. }) if *code == i32::from(v1::ErrorCode::Unauthenticated))
}

async fn current_session(client: &Client) -> Id {
    let listed = client.list_sessions().await.unwrap();
    let current = listed.iter().find(|session| session.current).unwrap();
    Id::try_from(current.id.as_ref()).unwrap()
}

/// Waits for the server to end `client`'s session, then for the client to
/// stop without trying to reconnect.
async fn signed_out(client: &Client, events: &mut broadcast::Receiver<ClientEvent>) {
    timeout(WAIT, async {
        loop {
            match events.recv().await {
                Ok(ClientEvent::Removed { banned, reason }) => {
                    assert!(!banned);
                    assert!(reason.contains("signed out"), "{reason}");
                    break;
                }
                Ok(ClientEvent::Connection(ConnectionState::Reconnecting { .. })) => {
                    panic!("reconnecting with a revoked token")
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => panic!("client dropped"),
            }
        }
    })
    .await
    .expect("SessionRevoked");
    timeout(WAIT, async {
        while client.state().read().connection != ConnectionState::Disconnected {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("client stopped");
}

#[tokio::test]
async fn password_reset_ends_every_session_on_quic_and_https() {
    let world = World::new().await;
    let (client, token) = world.owner().await;
    let (_second, second_token) = world.login("owner", PASSWORD).await.unwrap();
    let mut events = client.subscribe();
    assert_eq!(world.https(&token).await, StatusCode::OK);

    let ended = auth::set_password(&world.pool, "owner", "replacement-ok")
        .await
        .unwrap();
    assert_eq!(ended, 2);
    // The row is gone: new requests fail at once.
    for stale in [&token, &second_token] {
        assert_eq!(world.https(stale).await, StatusCode::UNAUTHORIZED);
        assert!(unauthenticated(&world.with_token(stale).await));
    }
    // The reset ran outside the server (the CLI does); its sweep closes the
    // connections still open with those tokens.
    assert_eq!(world.server.sessions().sweep().await.unwrap(), 2);
    signed_out(&client, &mut events).await;

    let (fresh, _) = world.login("owner", "replacement-ok").await.unwrap();
    assert_eq!(fresh.state().read().connection, ConnectionState::Connected);
}

#[tokio::test]
async fn logout_ends_this_session_and_closes_its_other_connections() {
    let world = World::new().await;
    let (client, token) = world.owner().await;
    // The same token on a second connection, and a separate login.
    let resumed = world.with_token(&token).await.unwrap();
    let mut resumed_events = resumed.subscribe();
    let (other, other_token) = world.login("owner", PASSWORD).await.unwrap();
    let session = current_session(&client).await;
    assert_eq!(world.server.sessions().connections(session), 2);

    client.logout().await.unwrap();
    assert_eq!(
        client.state().read().connection,
        ConnectionState::Disconnected
    );
    signed_out(&resumed, &mut resumed_events).await;
    assert_eq!(world.https(&token).await, StatusCode::UNAUTHORIZED);
    assert!(unauthenticated(&world.with_token(&token).await));
    let replay = Auth::new(world.pool.clone())
        .hello(&session_hello(&token), peer())
        .await;
    assert!(matches!(replay, Err(auth::Error::Unauthenticated)));
    timeout(WAIT, async {
        while world.server.sessions().connections(session) > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("connections detached");

    // The other session is untouched.
    assert_eq!(other.state().read().connection, ConnectionState::Connected);
    assert_eq!(world.https(&other_token).await, StatusCode::OK);
    let left = other.list_sessions().await.unwrap();
    assert_eq!(left.len(), 1);
    assert!(left[0].current);
}

#[tokio::test]
async fn members_list_and_revoke_only_their_own_sessions() {
    let world = World::new().await;
    let (owner, _) = world.owner().await;
    let (laptop, laptop_token) = world.login("owner", PASSWORD).await.unwrap();
    let mut laptop_events = laptop.subscribe();
    let guest = world.guest(&owner).await;
    let listed = owner.list_sessions().await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed.iter().filter(|session| session.current).count(), 1);
    for session in &listed {
        assert_eq!(session.device_name, "Sideband");
        assert!(session.expires_at > session.last_seen_at);
        assert!(session.expires_at <= session.created_at + 180 * DAY_MS);
    }
    let laptop_session = current_session(&laptop).await;

    let foreign = guest.revoke_session(laptop_session).await;
    assert!(
        matches!(foreign, Err(Error::Server { code, .. }) if code == i32::from(v1::ErrorCode::NotFound)),
        "{foreign:?}"
    );
    assert_eq!(world.https(&laptop_token).await, StatusCode::OK);
    let malformed = guest
        .command(v1::client_envelope::Msg::RevokeSession(v1::RevokeSession {
            session_id: vec![1, 2, 3].into(),
        }))
        .await;
    assert!(
        matches!(malformed, Err(Error::Server { code, .. }) if code == i32::from(v1::ErrorCode::Invalid))
    );

    owner.revoke_session(laptop_session).await.unwrap();
    signed_out(&laptop, &mut laptop_events).await;
    assert_eq!(world.https(&laptop_token).await, StatusCode::UNAUTHORIZED);
    assert_eq!(owner.list_sessions().await.unwrap().len(), 1);
    assert_eq!(owner.state().read().connection, ConnectionState::Connected);
}

#[tokio::test]
async fn the_operator_lists_and_revokes_sessions_from_the_cli() {
    let world = World::new().await;
    let (client, token) = world.owner().await;
    let (_second, second_token) = world.login("owner", PASSWORD).await.unwrap();
    let mut events = client.subscribe();
    let session = current_session(&client).await;

    let listed = world.cli(&["user", "sessions", "list", "--username", "owner"]);
    assert!(listed.status.success(), "{listed:?}");
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert_eq!(listed.lines().count(), 2, "{listed}");
    assert!(listed.contains(&session.to_string()), "{listed}");
    assert!(!listed.contains(&token), "the CLI printed a token");
    assert!(listed.contains("UTC"), "{listed}");

    let id = session.to_string();
    let revoked = world.cli(&[
        "user",
        "sessions",
        "revoke",
        "--username",
        "owner",
        "--id",
        &id,
    ]);
    assert!(revoked.status.success(), "{revoked:?}");
    assert_eq!(
        String::from_utf8(revoked.stdout).unwrap().trim(),
        "1 session(s) ended"
    );
    assert_eq!(world.https(&token).await, StatusCode::UNAUTHORIZED);
    assert!(unauthenticated(&world.with_token(&token).await));
    assert_eq!(world.https(&second_token).await, StatusCode::OK);
    assert_eq!(world.server.sessions().sweep().await.unwrap(), 1);
    signed_out(&client, &mut events).await;

    let again = world.cli(&[
        "user",
        "sessions",
        "revoke",
        "--username",
        "owner",
        "--id",
        &id,
    ]);
    assert!(!again.status.success());
    let all = world.cli(&["user", "sessions", "revoke", "--username", "owner", "--all"]);
    assert!(all.status.success(), "{all:?}");
    assert_eq!(
        String::from_utf8(all.stdout).unwrap().trim(),
        "1 session(s) ended"
    );
    assert_eq!(world.https(&second_token).await, StatusCode::UNAUTHORIZED);
    let neither = world.cli(&["user", "sessions", "revoke", "--username", "owner"]);
    assert!(!neither.status.success(), "--id or --all is required");
}

#[tokio::test]
async fn sliding_expiry_stops_at_the_absolute_lifetime() {
    let world = World::new().await;
    let (client, token) = world.owner().await;
    client.close().await;
    let auth = Auth::new(world.pool.clone());
    let now = Timestamp::now().as_millis();
    let backdate = async |created: i64| {
        let mut conn = world.pool.write().await;
        sqlx::query("UPDATE sessions SET created_at = ?, last_seen_at = ?, expires_at = ?")
            .bind(created)
            .bind(now - 89 * DAY_MS)
            .bind(now + DAY_MS)
            .execute(&mut *conn)
            .await
            .unwrap();
    };

    // 170 days old: still valid, but the slide stops at day 180, not now+90.
    backdate(now - 170 * DAY_MS).await;
    let used = auth.hello(&session_hello(&token), peer()).await.unwrap();
    let row = {
        let mut conn = world.pool.write().await;
        sessions::get(&mut conn, used.session_id)
            .await
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        row.expires_at.unwrap().as_millis(),
        row.created_at.as_millis() + 180 * DAY_MS
    );

    // Used every 89 days for over a year: every use pushed expiry out before.
    backdate(now - 400 * DAY_MS).await;
    let reused = auth.hello(&session_hello(&token), peer()).await;
    assert!(
        matches!(reused, Err(auth::Error::Unauthenticated)),
        "a 400-day-old session still works: {reused:?}"
    );
    assert_eq!(world.https(&token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_pruner_deletes_expired_rows_and_keeps_live_ones() {
    let world = World::new().await;
    let (owner, _) = world.owner().await;
    let (_idle, idle_token) = world.login("owner", PASSWORD).await.unwrap();
    let (_old, old_token) = world.login("owner", PASSWORD).await.unwrap();
    let live = current_session(&owner).await;
    let now = Timestamp::now().as_millis();
    {
        let mut conn = world.pool.write().await;
        let idle = sessions::get_by_token_hash(&mut conn, &sha256(&idle_token))
            .await
            .unwrap()
            .unwrap();
        let old = sessions::get_by_token_hash(&mut conn, &sha256(&old_token))
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE sessions SET expires_at = ? WHERE id = ?")
            .bind(now - 1)
            .bind(idle.id)
            .execute(&mut *conn)
            .await
            .unwrap();
        sqlx::query("UPDATE sessions SET created_at = ? WHERE id = ?")
            .bind(now - 181 * DAY_MS)
            .bind(old.id)
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    assert_eq!(world.server.sessions().prune().await.unwrap(), 2);
    let owner_id = {
        let state = owner.state();
        let view = state.read();
        Id::try_from(view.own_user.as_ref().unwrap().id.as_ref()).unwrap()
    };
    let left = {
        let mut conn = world.pool.write().await;
        sessions::list_for_user(&mut conn, owner_id).await.unwrap()
    };
    assert_eq!(
        left.iter().map(|session| session.id).collect::<Vec<_>>(),
        vec![live]
    );
    assert_eq!(world.server.sessions().prune().await.unwrap(), 0);
}

#[tokio::test]
async fn a_kick_is_only_a_disconnect() {
    let world = World::new().await;
    let (owner, _) = world.owner().await;
    let storage = Arc::new(MemoryStorage::default());
    let invite = owner
        .create_invite(v1::CreateInvite {
            max_uses: Some(1),
            expires_at: None,
        })
        .await
        .unwrap();
    let guest = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Register(v1::Register {
            invite_code: invite.code,
            username: "guest".into(),
            password: PASSWORD.into(),
        }),
        world.pins.clone(),
        storage.clone(),
    )
    .await
    .unwrap();
    let token = saved(&storage, &world.address);
    let guest_id = guest.state().read().own_user.as_ref().unwrap().id.clone();
    let mut events = guest.subscribe();
    owner
        .kick_user(v1::KickUser {
            user_id: guest_id,
            reason: "cool off".into(),
        })
        .await
        .unwrap();
    timeout(WAIT, async {
        loop {
            if let Ok(ClientEvent::Removed { banned: false, .. }) = events.recv().await {
                break;
            }
        }
    })
    .await
    .unwrap();
    // The session survives: a kicked member can come straight back.
    assert!(world.with_token(&token).await.is_ok());
    assert_eq!(world.https(&token).await, StatusCode::OK);
}

fn sha256(token: &str) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
