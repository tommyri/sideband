//! Kick, ban, move, invites, and the audit log.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
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
    auth, connection,
    db::{self, NewInvite, audit_log, invites},
    http, perms,
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

impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn equal_roles_cannot_kick_and_a_kick_leaves_voice() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (_ada_conn, mut ada) = world.connect().await;
    let welcome = hello(&mut ada, ada_login).await;
    let voice = voice_named(welcome.snapshot.as_ref().unwrap(), "General");
    let (_bea_conn, mut bea) = world.connect().await;
    hello(&mut bea, invite(&world, ada_id, "bea").await).await;
    let (_cyd_conn, mut cyd) = world.connect().await;
    let cyd_welcome = hello(&mut cyd, invite(&world, ada_id, "cyd").await).await;
    let cyd_id = user_id(&cyd_welcome);

    let role = ack_role(
        &command(
            &mut ada,
            2,
            Msg::CreateRole(v1::CreateRole {
                name: "mods".into(),
                permissions: Permissions::KICK_MEMBERS.bits(),
                position: 2,
                ..Default::default()
            }),
        )
        .await,
    );
    for (request_id, member) in [(3_u32, user_id_of("bea", &world).await), (4, cyd_id)] {
        let set = command(
            &mut ada,
            request_id,
            Msg::SetUserRoles(v1::SetUserRoles {
                user_id: id_to_bytes(member),
                role_ids: vec![role.clone()],
            }),
        )
        .await;
        assert!(matches!(set.msg, Some(Event::Ack(_))));
    }

    let denied = command(
        &mut bea,
        2,
        Msg::KickUser(v1::KickUser {
            user_id: id_to_bytes(cyd_id),
            reason: "no".into(),
        }),
    )
    .await;
    assert_eq!(error_message(&denied), "cannot act on that member");

    let joined = command(&mut cyd, 2, join(&voice)).await;
    assert!(matches!(joined.msg, Some(Event::Ack(_))));
    // Observe the join before testing the leave. A command Ack on another
    // connection does not guarantee its earlier bus events have been drained.
    let entered = next_voice_state(&mut ada, cyd_id).await;
    assert_eq!(entered.channel_id.as_ref(), Some(&voice));
    let kicked = command(
        &mut ada,
        5,
        Msg::KickUser(v1::KickUser {
            user_id: id_to_bytes(cyd_id),
            reason: "spam".into(),
        }),
    )
    .await;
    assert!(matches!(kicked.msg, Some(Event::Ack(_))));
    assert!(until_event(&mut cyd, |event| matches!(event, Event::Kicked(_))).await);
    let left = next_voice_state(&mut ada, cyd_id).await;
    assert!(left.channel_id.is_none());
    assert!(
        audit_actions(&world)
            .await
            .iter()
            .any(|action| action == "user.kick")
    );
}

#[tokio::test]
async fn ban_blocks_hello_until_unban_or_expiry() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (_ada_conn, mut ada) = world.connect().await;
    hello(&mut ada, ada_login).await;
    let (_bea_conn, mut bea) = world.connect().await;
    let bea_id = user_id(&hello(&mut bea, invite(&world, ada_id, "bea").await).await);

    let banned = command(
        &mut ada,
        2,
        Msg::BanUser(v1::BanUser {
            user_id: id_to_bytes(bea_id),
            reason: "spam".into(),
            expires_at: None,
        }),
    )
    .await;
    assert!(matches!(banned.msg, Some(Event::Ack(_))));
    assert!(until_event(&mut bea, |event| matches!(event, Event::Banned(_))).await);

    let (_rejected_conn, mut rejected) = world.connect().await;
    let error = command(&mut rejected, 1, login("bea")).await;
    assert_eq!(error_code(&error), i32::from(v1::ErrorCode::Banned));

    prepare_invite(&world, ada_id, "fresh").await;
    let taken = auth::Auth::new(Arc::clone(&world.pool))
        .hello(&register("fresh", "bea"), "203.0.113.11".parse().unwrap())
        .await;
    assert!(matches!(taken, Err(auth::Error::Conflict)), "{taken:?}");

    let lifted = command(
        &mut ada,
        3,
        Msg::UnbanUser(v1::UnbanUser {
            user_id: id_to_bytes(bea_id),
        }),
    )
    .await;
    assert!(matches!(lifted.msg, Some(Event::Ack(_))));
    let (_back_conn, mut back) = world.connect().await;
    assert!(matches!(
        command(&mut back, 1, login("bea")).await.msg,
        Some(Event::Welcome(_))
    ));

    let until = Timestamp::now().as_millis().saturating_add(3_600_000);
    let timed = command(
        &mut ada,
        4,
        Msg::BanUser(v1::BanUser {
            user_id: id_to_bytes(bea_id),
            reason: String::new(),
            expires_at: Some(until),
        }),
    )
    .await;
    assert!(matches!(timed.msg, Some(Event::Ack(_))));
    expire_bans(&world, bea_id).await;
    let (_later_conn, mut later) = world.connect().await;
    assert!(matches!(
        command(&mut later, 1, login("bea")).await.msg,
        Some(Event::Welcome(_))
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn move_respects_view_and_bypass_a_full_channel() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (_ada_conn, mut ada) = world.connect().await;
    let welcome = hello(&mut ada, ada_login).await;
    let general = voice_named(welcome.snapshot.as_ref().unwrap(), "General");
    let token = welcome.resume_token.clone();
    let (_bea_conn, mut bea) = world.connect().await;
    let bea_welcome = hello(&mut bea, invite(&world, ada_id, "bea").await).await;
    let bea_id = user_id(&bea_welcome);
    let (_cyd_conn, mut cyd) = world.connect().await;
    let cyd_id = user_id(&hello(&mut cyd, invite(&world, ada_id, "cyd").await).await);

    let denied = command(
        &mut bea,
        2,
        Msg::MoveUser(v1::MoveUser {
            user_id: id_to_bytes(cyd_id),
            channel_id: general.clone(),
        }),
    )
    .await;
    assert_eq!(error_message(&denied), "missing move members");

    let stage = ack_channel(
        &command(
            &mut ada,
            2,
            Msg::CreateChannel(v1::CreateChannel {
                kind: i32::from(v1::ChannelKind::Voice),
                name: "stage".into(),
                position: 2,
                voice: Some(v1::VoiceSettings {
                    profile: i32::from(v1::VoiceProfile::Voice),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await,
    );
    let limited = command(
        &mut ada,
        3,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: general.clone(),
            voice: Some(v1::VoiceSettings {
                profile: i32::from(v1::VoiceProfile::Voice),
                user_limit: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(limited.msg, Some(Event::Ack(_))));
    assert!(matches!(
        command(&mut bea, 3, join(&general)).await.msg,
        Some(Event::Ack(_))
    ));
    assert!(matches!(
        command(&mut cyd, 2, join(&stage)).await.msg,
        Some(Event::Ack(_))
    ));
    let moved = command(
        &mut ada,
        4,
        Msg::MoveUser(v1::MoveUser {
            user_id: id_to_bytes(cyd_id),
            channel_id: general.clone(),
        }),
    )
    .await;
    assert!(matches!(moved.msg, Some(Event::Ack(_))));

    let pit = ack_channel(
        &command(
            &mut ada,
            5,
            Msg::CreateChannel(v1::CreateChannel {
                kind: i32::from(v1::ChannelKind::Voice),
                name: "pit".into(),
                position: 3,
                voice: Some(v1::VoiceSettings {
                    profile: i32::from(v1::VoiceProfile::Voice),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        )
        .await,
    );
    let hidden = command(
        &mut ada,
        6,
        Msg::SetChannelOverride(v1::SetChannelOverride {
            channel_id: pit.clone(),
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::VIEW_CHANNEL.bits(),
        }),
    )
    .await;
    assert!(matches!(hidden.msg, Some(Event::Ack(_))));
    let blocked = command(
        &mut ada,
        7,
        Msg::MoveUser(v1::MoveUser {
            user_id: id_to_bytes(cyd_id),
            channel_id: pit,
        }),
    )
    .await;
    assert_eq!(error_message(&blocked), "missing view channel");

    let invite = command(&mut ada, 8, Msg::CreateInvite(v1::CreateInvite::default())).await;
    assert_ne!(ack_invite(&invite).code, "");
    let published = timeout(DEADLINE, async {
        loop {
            if let Event::InviteCreated(event) = next_event(&mut ada).await {
                return event.invite.unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(published.code, "");
    let revoked = command(
        &mut ada,
        9,
        Msg::RevokeInvite(v1::RevokeInvite {
            invite_id: ack_invite(&invite).id,
        }),
    )
    .await;
    assert!(matches!(revoked.msg, Some(Event::Ack(_))));
    let stripped = command(
        &mut ada,
        10,
        Msg::UpdateRole(v1::UpdateRole {
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            permissions: Some(
                Permissions::EVERYONE
                    .difference(Permissions::CREATE_INVITES)
                    .bits(),
            ),
            ..Default::default()
        }),
    )
    .await;
    assert!(matches!(stripped.msg, Some(Event::Ack(_))));
    let member = command(&mut bea, 4, Msg::CreateInvite(v1::CreateInvite::default())).await;
    assert_eq!(error_message(&member), "missing create invites");

    let muted = command(
        &mut ada,
        11,
        Msg::ServerMuteUser(v1::ServerMuteUser {
            user_id: id_to_bytes(bea_id),
            muted: true,
        }),
    )
    .await;
    assert!(matches!(muted.msg, Some(Event::Ack(_))));

    let actions = audit_actions(&world).await;
    for action in ["user.move", "invite.create", "invite.revoke", "user.mute"] {
        assert!(
            actions.iter().any(|row| row == action),
            "{action} in {actions:?}"
        );
    }
    let status = audit_http(&world, Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let hidden_log = audit_http(&world, Some(&bea_welcome.resume_token)).await;
    assert_eq!(hidden_log, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cli_bans_unbans_and_grants_admin() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_str().unwrap().to_owned();
    let created = succeed(&["invite", "create", "--owner", "--data-dir", path.as_str()]);
    let code = created
        .lines()
        .next()
        .unwrap()
        .trim()
        .strip_prefix("owner invite: ")
        .unwrap();
    let pool = Arc::new(
        db::Pool::open(dir.path().join("socialspeak.db"))
            .await
            .unwrap(),
    );
    let auth = auth::Auth::new(Arc::clone(&pool));
    let ip = "203.0.113.20".parse().unwrap();
    auth.hello(&register(code, "ada"), ip).await.unwrap();
    let already = succeed(&[
        "role",
        "grant-admin",
        "--username",
        "ada",
        "--data-dir",
        path.as_str(),
    ]);
    assert!(already.contains("already an admin"), "{already}");

    let mut conn = pool.write().await;
    let ada = db::users::get_by_username(&mut conn, "ada")
        .await
        .unwrap()
        .unwrap()
        .id;
    invites::insert(
        &mut conn,
        NewInvite {
            id: Id::generate(),
            code: "bea".into(),
            created_by: ada,
            max_uses: Some(1),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    drop(conn);
    auth.hello(&register("bea", "bea"), ip).await.unwrap();
    let granted = succeed(&[
        "role",
        "grant-admin",
        "--username",
        "bea",
        "--data-dir",
        path.as_str(),
    ]);
    assert!(granted.contains("admin granted"), "{granted}");

    let banned = succeed(&[
        "user",
        "ban",
        "--username",
        "bea",
        "--reason",
        "spam",
        "--data-dir",
        path.as_str(),
    ]);
    assert!(banned.contains("banned bea"), "{banned}");
    let rejected = auth.hello(&login_hello("bea"), ip).await;
    assert!(
        matches!(rejected, Err(auth::Error::Banned { .. })),
        "{rejected:?}"
    );
    let lifted = succeed(&[
        "user",
        "unban",
        "--username",
        "bea",
        "--data-dir",
        path.as_str(),
    ]);
    assert!(lifted.contains("unbanned bea"), "{lifted}");
    auth.hello(&login_hello("bea"), ip).await.unwrap();
}

async fn register_owner(world: &World, username: &str) -> (Id, v1::Hello) {
    let registered = auth::Auth::new(Arc::clone(&world.pool))
        .hello(
            &register(&world.code, username),
            "203.0.113.9".parse().unwrap(),
        )
        .await
        .unwrap();
    (registered.user.id, login_hello(username))
}

async fn invite(world: &World, created_by: Id, username: &str) -> v1::Hello {
    prepare_invite(world, created_by, username).await;
    register(username, username)
}

async fn prepare_invite(world: &World, created_by: Id, code: &str) {
    let mut conn = world.pool.write().await;
    invites::insert(
        &mut conn,
        NewInvite {
            id: Id::generate(),
            code: code.into(),
            created_by,
            max_uses: Some(1),
            expires_at: None,
        },
    )
    .await
    .unwrap();
}

async fn user_id_of(username: &str, world: &World) -> Id {
    let mut conn = world.pool.write().await;
    db::users::get_by_username(&mut conn, username)
        .await
        .unwrap()
        .unwrap()
        .id
}

async fn expire_bans(world: &World, user: Id) {
    let mut conn = world.pool.write().await;
    sqlx::query("UPDATE bans SET expires_at = 1 WHERE user_id = ? AND lifted_at IS NULL")
        .bind(user)
        .execute(&mut *conn)
        .await
        .unwrap();
}

async fn audit_actions(world: &World) -> Vec<String> {
    let mut conn = world.pool.write().await;
    audit_log::recent(&mut conn, 50)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.action)
        .collect()
}

async fn audit_http(world: &World, token: Option<&str>) -> StatusCode {
    let perms = Arc::new(
        perms::PermissionService::open(Arc::clone(&world.pool))
            .await
            .unwrap(),
    );
    let auth = Arc::new(auth::Auth::new(Arc::clone(&world.pool)));
    let mut builder = Request::builder().uri("/api/v1/audit-log");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    http::audit::router(auth, perms)
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

async fn hello(stream: &mut ss_net::control::ClientStream, hello: v1::Hello) -> v1::Welcome {
    match command(stream, 1, Msg::Hello(hello)).await.msg {
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

async fn until_event(
    stream: &mut ss_net::control::ClientStream,
    pred: impl Fn(&Event) -> bool,
) -> bool {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let frame = timeout(remaining, stream.recv())
            .await
            .ok()
            .and_then(Result::ok);
        let Some(frame) = frame else {
            return false;
        };
        if let Some(event) = frame.msg.as_ref()
            && pred(event)
        {
            return true;
        }
    }
}

async fn next_event(stream: &mut ss_net::control::ClientStream) -> Event {
    loop {
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if let Some(event) = frame.msg {
            return event;
        }
    }
}

async fn next_voice_state(
    stream: &mut ss_net::control::ClientStream,
    user: Id,
) -> v1::VoiceStateUpdated {
    let bytes = id_to_bytes(user);
    loop {
        match next_event(stream).await {
            Event::VoiceStateUpdated(state) if state.user_id.as_ref() == bytes.as_ref() => {
                return state;
            }
            _ => {}
        }
    }
}

fn join(channel: &bytes::Bytes) -> Msg {
    Msg::JoinVoice(v1::JoinVoice {
        channel_id: channel.clone(),
        self_mute: false,
        self_deaf: false,
    })
}

fn login(username: &str) -> Msg {
    Msg::Hello(login_hello(username))
}

fn login_hello(username: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        client_name: "desk".into(),
        auth: Some(v1::hello::Auth::Login(v1::Login {
            username: username.into(),
            password: PASSWORD.into(),
        })),
        ..Default::default()
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

fn voice_named(snapshot: &v1::Snapshot, name: &str) -> bytes::Bytes {
    snapshot
        .channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(v1::ChannelKind::Voice))
        .unwrap_or_else(|| panic!("missing voice channel {name}"))
        .id
        .clone()
}

fn ack_channel(envelope: &v1::ServerEnvelope) -> bytes::Bytes {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::ChannelId(id)) => id.clone(),
            other => panic!("expected a channel id, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
    }
}

fn ack_role(envelope: &v1::ServerEnvelope) -> bytes::Bytes {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::Role(role)) => role.id.clone(),
            other => panic!("expected a role, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
    }
}

fn ack_invite(envelope: &v1::ServerEnvelope) -> v1::Invite {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::Invite(invite)) => invite.clone(),
            other => panic!("expected an invite, got {other:?}"),
        },
        other => panic!("expected Ack, got {other:?}"),
    }
}

fn user_id(welcome: &v1::Welcome) -> Id {
    id_from_bytes(&welcome.user.as_ref().unwrap().id).unwrap()
}

fn error_code(envelope: &v1::ServerEnvelope) -> i32 {
    match &envelope.msg {
        Some(Event::Error(error)) => error.code,
        other => panic!("expected Error, got {other:?}"),
    }
}

fn error_message(envelope: &v1::ServerEnvelope) -> &str {
    match &envelope.msg {
        Some(Event::Error(error)) => &error.message,
        other => panic!("expected Error, got {other:?}"),
    }
}

fn succeed(args: &[&str]) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ss-server"))
        .args(args)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "status {:?}\n{stderr}",
        output.status
    );
    String::from_utf8(output.stdout).unwrap()
}
