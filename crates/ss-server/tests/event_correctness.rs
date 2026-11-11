//! What the server tells connected clients matches what it committed:
//! registrations are announced, and a command whose connection closes after
//! its commit still reloads permissions and publishes its events.

use std::{sync::Arc, time::Duration};

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
    db::{self, NewInvite, invites, roles},
};
use tokio::time::timeout;

const PASSWORD: &str = "password-ok";
const DEADLINE: Duration = Duration::from_secs(5);
const QUIET: Duration = Duration::from_millis(800);

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

    /// Ada, the owner, and Bea, a member, both connected.
    async fn pair(&self) -> Pair {
        let (ada_conn, mut ada) = self.connect().await;
        let ada_welcome = hello(&mut ada, register(&self.code, "ada")).await;
        let ada_id = user_id(&ada_welcome);
        let (bea_conn, mut bea) = self.connect().await;
        let bea_welcome = hello(&mut bea, invite(self, ada_id, "bea").await).await;
        Pair {
            ada_id,
            ada_conn,
            ada,
            bea_id: user_id(&bea_welcome),
            bea_token: bea_welcome.resume_token,
            _bea_conn: bea_conn,
            bea,
        }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

struct Pair {
    ada_id: Id,
    ada_conn: ss_net::Connection,
    ada: ss_net::control::ClientStream,
    bea_id: Id,
    bea_token: String,
    _bea_conn: ss_net::Connection,
    bea: ss_net::control::ClientStream,
}

#[tokio::test]
async fn a_registration_is_announced_to_connected_members() {
    let world = World::new().await;
    let (_ada_conn, mut ada) = world.connect().await;
    let ada_id = user_id(&hello(&mut ada, register(&world.code, "ada")).await);

    let (_bea_conn, mut bea) = world.connect().await;
    let bea_welcome = hello(&mut bea, invite(&world, ada_id, "bea").await).await;
    let bea_id = id_to_bytes(user_id(&bea_welcome));

    // The first frame Ada gets about Bea is the new member, before presence.
    let frame = first_naming(&mut ada, &bea_id).await;
    let Some(Event::MemberJoined(joined)) = frame.msg else {
        panic!("expected MemberJoined first, got {frame:?}");
    };
    let member = joined.member.unwrap();
    let user = member.user.unwrap();
    assert_eq!(user.username, "bea");
    assert_eq!(member.roles, vec![id_to_bytes(db::EVERYONE_ROLE_ID)]);
    // Bea's own Welcome already lists her, and she is not told twice.
    assert!(
        bea_welcome
            .snapshot
            .unwrap()
            .members
            .iter()
            .any(|member| member.user.as_ref().is_some_and(|user| user.id == bea_id))
    );
    assert!(
        !until_event(&mut bea, QUIET, |event| matches!(
            event,
            Event::MemberJoined(_)
        ))
        .await,
        "the new member was sent their own MemberJoined"
    );

    // A login is not a registration.
    drop(bea);
    let (_again, mut again) = world.connect().await;
    hello(&mut again, login("bea")).await;
    assert!(
        !until_event(&mut ada, QUIET, |event| matches!(
            event,
            Event::MemberJoined(_)
        ))
        .await,
        "a login was announced as a new member"
    );
}

/// Holds every read connection so the command blocks in its permission
/// reload, after its commit; closes the actor's connection there; then
/// releases the reads. The reload and the event must still happen.
#[tokio::test]
async fn closing_the_connection_after_commit_still_reloads_and_publishes() {
    let world = World::new().await;
    let mut pair = world.pair().await;
    drain(&mut pair.bea).await;

    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(world.pool.read().await.unwrap());
    }
    pair.ada
        .send(&v1::ClientEnvelope {
            request_id: 2,
            msg: Some(Msg::SetUserRoles(v1::SetUserRoles {
                user_id: id_to_bytes(pair.bea_id),
                role_ids: vec![id_to_bytes(db::ADMIN_ROLE_ID)],
            })),
        })
        .await
        .unwrap();
    // Wait for the commit, on a connection the reload cannot have.
    let committed = timeout(DEADLINE, async {
        loop {
            let granted = roles::roles_for_user(&mut held[0], pair.bea_id)
                .await
                .unwrap();
            if granted.iter().any(|role| role.id == db::ADMIN_ROLE_ID) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(committed.is_ok(), "the grant never committed");
    pair.ada_conn.close(0, b"gone");
    let closed = timeout(DEADLINE, async {
        while !world
            .server
            .registry()
            .connection_ids(pair.ada_id)
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "the actor's session did not end");
    drop(held);

    assert!(
        until_event(&mut pair.bea, DEADLINE, |event| matches!(
            event,
            Event::MemberRolesUpdated(updated) if updated.user_id == id_to_bytes(pair.bea_id)
        ))
        .await,
        "the committed role change was never published"
    );
    // The snapshot was reloaded: Bea now passes an administrator check.
    let created = command(
        &mut pair.bea,
        10,
        Msg::CreateRole(v1::CreateRole {
            name: "Helpers".into(),
            color: None,
            permissions: 0,
            position: 1,
        }),
    )
    .await;
    assert!(
        matches!(created.msg, Some(Event::Ack(_))),
        "permissions were not reloaded after the commit: {created:?}"
    );
}

/// Bea reconnects (every Hello reloads permissions) while Ada demotes her.
/// However the two interleave, Bea ends up without the role's permission.
#[tokio::test]
async fn concurrent_hello_and_set_user_roles_end_with_the_new_permissions() {
    let world = World::new().await;
    let mut pair = world.pair().await;
    let mods = create_role(
        &mut pair.ada,
        2,
        "Mods",
        Permissions::MANAGE_CHANNELS.bits(),
    )
    .await;
    let mut request = 3;
    for round in 0..20 {
        request += 1;
        let granted = command(
            &mut pair.ada,
            request,
            Msg::SetUserRoles(v1::SetUserRoles {
                user_id: id_to_bytes(pair.bea_id),
                role_ids: vec![mods.clone()],
            }),
        )
        .await;
        assert!(matches!(granted.msg, Some(Event::Ack(_))), "{granted:?}");
        request += 1;
        let demote = command(
            &mut pair.ada,
            request,
            Msg::SetUserRoles(v1::SetUserRoles {
                user_id: id_to_bytes(pair.bea_id),
                role_ids: Vec::new(),
            }),
        );
        let reconnect = async {
            let (connection, mut stream) = world.connect().await;
            hello(&mut stream, token(&pair.bea_token)).await;
            (connection, stream)
        };
        let (demoted, _bea) = tokio::join!(demote, reconnect);
        assert!(matches!(demoted.msg, Some(Event::Ack(_))), "{demoted:?}");
        let forbidden = command(
            &mut pair.bea,
            100 + round,
            Msg::CreateChannel(v1::CreateChannel {
                name: format!("round-{round}"),
                kind: v1::ChannelKind::Text.into(),
                ..Default::default()
            }),
        )
        .await;
        assert!(
            matches!(forbidden.msg, Some(Event::Error(_))),
            "round {round}: a demoted member kept the role's permission: {forbidden:?}"
        );
    }
}

async fn create_role(
    stream: &mut ss_net::control::ClientStream,
    request_id: u32,
    name: &str,
    permissions: u64,
) -> Bytes {
    let created = command(
        stream,
        request_id,
        Msg::CreateRole(v1::CreateRole {
            name: name.into(),
            color: None,
            permissions,
            position: 1,
        }),
    )
    .await;
    match created.msg {
        Some(Event::Ack(ack)) => match ack.result {
            Some(v1::ack::Result::Role(role)) => role.id,
            other => panic!("expected a role, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
    }
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
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if frame.request_id == request_id {
            return frame;
        }
    }
}

/// The first event frame that names `user`.
async fn first_naming(
    stream: &mut ss_net::control::ClientStream,
    user: &Bytes,
) -> v1::ServerEnvelope {
    loop {
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        let names = match &frame.msg {
            Some(Event::MemberJoined(v1::MemberJoined {
                member: Some(member),
            })) => member.user.as_ref().is_some_and(|named| named.id == *user),
            Some(Event::PresenceUpdated(presence)) => presence.user_id == *user,
            _ => false,
        };
        if names {
            return frame;
        }
    }
}

/// Whether an event matching `pred` arrives `within`.
async fn until_event(
    stream: &mut ss_net::control::ClientStream,
    within: Duration,
    pred: impl Fn(&Event) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let Some(frame) = timeout(remaining, stream.recv())
            .await
            .ok()
            .and_then(Result::ok)
        else {
            return false;
        };
        if frame.msg.as_ref().is_some_and(&pred) {
            return true;
        }
    }
}

async fn drain(stream: &mut ss_net::control::ClientStream) {
    while timeout(Duration::from_millis(300), stream.recv())
        .await
        .is_ok()
    {}
}

fn user_id(welcome: &v1::Welcome) -> Id {
    id_from_bytes(&welcome.user.as_ref().unwrap().id).unwrap()
}
