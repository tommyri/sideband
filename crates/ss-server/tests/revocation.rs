//! What a session loses when access changes, and what it never learns about
//! channels it cannot see: voice seats, voice state, invites, and DM audit rows.

use std::{sync::Arc, time::Duration};

use axum::{body::Body, http::Request};
use bytes::{Bytes, BytesMut};
use ss_core::{Id, Permissions};
use ss_net::{
    client,
    media::DatagramHeader,
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
    db::{self, NewInvite, channels, invites},
    http, perms,
};
use tokio::time::timeout;
use tower::ServiceExt;

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

/// A seated member and a listener, both in the default General channel.
struct Seated {
    world: World,
    ada_id: Id,
    ada_conn: ss_net::Connection,
    ada: ss_net::control::ClientStream,
    ada_track: u32,
    bea_id: Id,
    bea_conn: ss_net::Connection,
    bea: ss_net::control::ClientStream,
    bea_token: String,
    general: Bytes,
}

async fn seated() -> Seated {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (ada_conn, mut ada) = world.connect().await;
    let welcome = hello(&mut ada, ada_login).await;
    let general = voice_named(welcome.snapshot.as_ref().unwrap(), "General");
    let (bea_conn, mut bea) = world.connect().await;
    let bea_welcome = hello(&mut bea, invite(&world, ada_id, "bea").await).await;
    let bea_id = user_id(&bea_welcome);
    assert!(is_ack(&command(&mut ada, 2, join(&general)).await));
    let ada_track = next_track_for(&mut ada, ada_id).await.track_id;
    assert!(is_ack(&command(&mut bea, 2, join(&general)).await));
    next_track_for(&mut bea, bea_id).await;
    drain(&mut ada).await;
    drain(&mut bea).await;
    ada_conn.send_datagram(packet(ada_track)).unwrap();
    let heard = timeout(DEADLINE, bea_conn.read_datagram())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(heard, packet(ada_track));
    Seated {
        world,
        ada_id,
        ada_conn,
        ada,
        ada_track,
        bea_id,
        bea_conn,
        bea,
        bea_token: bea_welcome.resume_token,
        general,
    }
}

async fn evicted_when_everyone_is_denied(bit: Permissions) {
    let mut seats = seated().await;
    assert_eq!(seats.world.server.voice_members(), 2);
    let denied = command(
        &mut seats.ada,
        3,
        Msg::SetChannelOverride(v1::SetChannelOverride {
            channel_id: seats.general.clone(),
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: bit.bits(),
        }),
    )
    .await;
    assert!(is_ack(&denied));
    let own = voice_states_for(&mut seats.bea, seats.bea_id).await;
    assert_eq!(own.len(), 1, "{own:?}");
    assert!(own[0].channel_id.is_none());
    let seen = voice_states_for(&mut seats.ada, seats.bea_id).await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].channel_id.is_none());
    assert_eq!(seats.world.server.voice_members(), 1);

    seats
        .ada_conn
        .send_datagram(packet(seats.ada_track))
        .unwrap();
    assert!(
        timeout(QUIET, seats.bea_conn.read_datagram())
            .await
            .is_err(),
        "an evicted member still receives audio"
    );
}

#[tokio::test]
async fn losing_view_channel_evicts_a_seated_member_and_stops_their_audio() {
    evicted_when_everyone_is_denied(Permissions::VIEW_CHANNEL).await;
}

#[tokio::test]
async fn losing_connect_evicts_a_seated_member() {
    evicted_when_everyone_is_denied(Permissions::CONNECT).await;
}

#[tokio::test]
async fn a_member_moved_in_without_connect_keeps_the_seat() {
    let mut seats = seated().await;
    let stage = ack_channel(
        &command(
            &mut seats.ada,
            3,
            Msg::CreateChannel(v1::CreateChannel {
                kind: i32::from(v1::ChannelKind::Voice),
                name: "stage".into(),
                position: 2,
                ..Default::default()
            }),
        )
        .await,
    );
    let denied = command(
        &mut seats.ada,
        4,
        Msg::SetChannelOverride(v1::SetChannelOverride {
            channel_id: stage.clone(),
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::CONNECT.bits(),
        }),
    )
    .await;
    assert!(is_ack(&denied));
    let moved = command(
        &mut seats.ada,
        5,
        Msg::MoveUser(v1::MoveUser {
            user_id: id_to_bytes(seats.bea_id),
            channel_id: stage.clone(),
        }),
    )
    .await;
    assert!(is_ack(&moved), "{moved:?}");
    drain(&mut seats.bea).await;
    // Any later command runs the seat refresh; the moved seat survives it.
    let renamed = command(
        &mut seats.ada,
        6,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: stage.clone(),
            name: Some("podium".into()),
            ..Default::default()
        }),
    )
    .await;
    assert!(is_ack(&renamed));
    let states = voice_states_for(&mut seats.bea, seats.bea_id).await;
    assert!(states.is_empty(), "{states:?}");
    assert_eq!(seats.world.server.voice_members(), 2);
}

#[tokio::test]
async fn resume_rechecks_the_seat_it_restores() {
    let mut seats = seated().await;
    seats.bea_conn.close(73, b"test reconnect");
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    drain(&mut seats.ada).await;
    // Change permissions behind the running server's back, then let another
    // Hello reload the snapshot. No command runs, so only the resume check
    // can notice.
    let general = id_from_bytes(&seats.general).unwrap();
    {
        let mut conn = seats.world.pool.write().await;
        channels::set_override(
            &mut conn,
            general,
            db::EVERYONE_ROLE_ID,
            Permissions::empty(),
            Permissions::VIEW_CHANNEL,
        )
        .await
        .unwrap();
    }
    let (_cyd_conn, mut cyd) = seats.world.connect().await;
    hello(&mut cyd, invite(&seats.world, seats.ada_id, "cyd").await).await;

    let (_resumed_conn, mut resumed) = seats.world.connect().await;
    let welcome = hello(
        &mut resumed,
        v1::Hello {
            protocol_version: 1,
            client_name: "resumed".into(),
            auth: Some(v1::hello::Auth::ResumeToken(seats.bea_token.clone())),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        seats.world.server.voice_members(),
        1,
        "resume restored a seat the member can no longer hold"
    );
    let snapshot = welcome.snapshot.unwrap();
    let bea = id_to_bytes(seats.bea_id);
    assert!(
        !snapshot
            .voice_states
            .iter()
            .any(|state| state.user_id.as_ref() == bea.as_ref()),
        "resume restored a seat the member can no longer hold"
    );
    let left = voice_states_for(&mut seats.ada, seats.bea_id).await;
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].channel_id.is_none());
}

/// Ada (owner), Bea (an insider who can see `staff`), Cyd (a moderator with
/// mute and move rights who cannot), and Dan (a plain member).
struct Hidden {
    _world: World,
    ada: ss_net::control::ClientStream,
    bea_id: Id,
    bea: ss_net::control::ClientStream,
    cyd: ss_net::control::ClientStream,
    dan: ss_net::control::ClientStream,
    general: Bytes,
    staff: Bytes,
    next: u32,
    _conns: Vec<ss_net::Connection>,
}

impl Hidden {
    fn request(&mut self) -> u32 {
        self.next += 1;
        self.next
    }
}

async fn hidden_staff_channel() -> Hidden {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (ada_conn, mut ada) = world.connect().await;
    let welcome = hello(&mut ada, ada_login).await;
    let general = voice_named(welcome.snapshot.as_ref().unwrap(), "General");
    let (bea_conn, mut bea) = world.connect().await;
    let bea_id = user_id(&hello(&mut bea, invite(&world, ada_id, "bea").await).await);
    let (cyd_conn, mut cyd) = world.connect().await;
    let cyd_id = user_id(&hello(&mut cyd, invite(&world, ada_id, "cyd").await).await);
    let (dan_conn, mut dan) = world.connect().await;
    hello(&mut dan, invite(&world, ada_id, "dan").await).await;

    let staff = ack_channel(
        &command(
            &mut ada,
            2,
            Msg::CreateChannel(v1::CreateChannel {
                kind: i32::from(v1::ChannelKind::Voice),
                name: "staff".into(),
                position: 2,
                ..Default::default()
            }),
        )
        .await,
    );
    let insiders = create_role(&mut ada, 3, "insiders", 0, 2).await;
    let moderators = create_role(
        &mut ada,
        4,
        "moderators",
        Permissions::MUTE_MEMBERS
            .union(Permissions::MOVE_MEMBERS)
            .bits(),
        3,
    )
    .await;
    for (request, user, role) in [(5, bea_id, &insiders), (6, cyd_id, &moderators)] {
        let granted = command(
            &mut ada,
            request,
            Msg::SetUserRoles(v1::SetUserRoles {
                user_id: id_to_bytes(user),
                role_ids: vec![role.clone()],
            }),
        )
        .await;
        assert!(is_ack(&granted), "{granted:?}");
    }
    for (request, role, allow, deny) in [
        (
            7,
            id_to_bytes(db::EVERYONE_ROLE_ID),
            0,
            Permissions::VIEW_CHANNEL.bits(),
        ),
        (8, insiders.clone(), Permissions::VIEW_CHANNEL.bits(), 0),
    ] {
        let set = command(
            &mut ada,
            request,
            Msg::SetChannelOverride(v1::SetChannelOverride {
                channel_id: staff.clone(),
                role_id: role,
                allow,
                deny,
            }),
        )
        .await;
        assert!(is_ack(&set), "{set:?}");
    }
    drain(&mut ada).await;
    drain(&mut bea).await;
    drain(&mut cyd).await;
    drain(&mut dan).await;
    Hidden {
        _world: world,
        ada,
        bea_id,
        bea,
        cyd,
        dan,
        general,
        staff,
        next: 1,
        _conns: vec![ada_conn, bea_conn, cyd_conn, dan_conn],
    }
}

#[tokio::test]
async fn a_moderator_never_learns_a_hidden_voice_channel() {
    let mut room = hidden_staff_channel().await;
    let request = room.request();
    assert!(is_ack(
        &command(&mut room.bea, request, join(&room.staff)).await
    ));
    drain(&mut room.cyd).await;

    let request = room.request();
    let (muted, frames) = command_frames(
        &mut room.cyd,
        request,
        Msg::ServerMuteUser(v1::ServerMuteUser {
            user_id: id_to_bytes(room.bea_id),
            muted: true,
        }),
    )
    .await;
    assert!(is_ack(&muted), "{muted:?}");
    assert!(
        !frames.iter().any(|frame| names_channel(frame, &room.staff)),
        "the mute reply named the hidden channel: {frames:?}"
    );

    // Moving someone out of, or into, a channel the moderator cannot see is
    // acting on it.
    let request = room.request();
    let (out, frames) = command_frames(
        &mut room.cyd,
        request,
        Msg::MoveUser(v1::MoveUser {
            user_id: id_to_bytes(room.bea_id),
            channel_id: room.general.clone(),
        }),
    )
    .await;
    assert_eq!(error_message(&out), "not in a voice channel");
    assert!(!frames.iter().any(|frame| names_channel(frame, &room.staff)));

    let request = room.request();
    assert!(is_ack(
        &command(&mut room.bea, request, join(&room.general)).await
    ));
    let request = room.request();
    let into = command(
        &mut room.cyd,
        request,
        Msg::MoveUser(v1::MoveUser {
            user_id: id_to_bytes(room.bea_id),
            channel_id: room.staff.clone(),
        }),
    )
    .await;
    assert_eq!(error_code(&into), i32::from(v1::ErrorCode::NotFound));
}

#[tokio::test]
async fn leaving_a_hidden_voice_channel_is_not_announced_to_outsiders() {
    let mut room = hidden_staff_channel().await;
    let request = room.request();
    assert!(is_ack(
        &command(&mut room.bea, request, join(&room.staff)).await
    ));
    drain(&mut room.ada).await;
    drain(&mut room.dan).await;
    let request = room.request();
    assert!(is_ack(
        &command(&mut room.bea, request, Msg::LeaveVoice(v1::LeaveVoice {})).await
    ));
    let outsider = voice_states_for(&mut room.dan, room.bea_id).await;
    assert!(outsider.is_empty(), "{outsider:?}");
    let insider = voice_states_for(&mut room.ada, room.bea_id).await;
    assert_eq!(insider.len(), 1, "{insider:?}");
    assert!(insider[0].channel_id.is_none());

    // A switch from the hidden channel into a visible one shows the outsider
    // only the arrival.
    let request = room.request();
    assert!(is_ack(
        &command(&mut room.bea, request, join(&room.staff)).await
    ));
    drain(&mut room.dan).await;
    let request = room.request();
    assert!(is_ack(
        &command(&mut room.bea, request, join(&room.general)).await
    ));
    let outsider = voice_states_for(&mut room.dan, room.bea_id).await;
    assert_eq!(outsider.len(), 1, "{outsider:?}");
    assert_eq!(
        outsider[0].channel_id.as_deref(),
        Some(room.general.as_ref())
    );
    // And the leave from the visible channel reaches them again.
    let request = room.request();
    assert!(is_ack(
        &command(&mut room.bea, request, Msg::LeaveVoice(v1::LeaveVoice {})).await
    ));
    let outsider = voice_states_for(&mut room.dan, room.bea_id).await;
    assert_eq!(outsider.len(), 1, "{outsider:?}");
    assert!(outsider[0].channel_id.is_none());
}

#[tokio::test]
async fn banning_a_member_revokes_their_invites() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (_ada_conn, mut ada) = world.connect().await;
    hello(&mut ada, ada_login).await;
    let (_bea_conn, mut bea) = world.connect().await;
    let bea_id = user_id(&hello(&mut bea, invite(&world, ada_id, "bea").await).await);
    let created = command(
        &mut bea,
        2,
        Msg::CreateInvite(v1::CreateInvite {
            max_uses: None,
            expires_at: None,
        }),
    )
    .await;
    let code = ack_invite(&created).code;
    assert_ne!(code, "");
    drain(&mut ada).await;

    let banned = command(
        &mut ada,
        2,
        Msg::BanUser(v1::BanUser {
            user_id: id_to_bytes(bea_id),
            reason: String::new(),
            expires_at: None,
        }),
    )
    .await;
    assert!(is_ack(&banned), "{banned:?}");
    assert!(
        until_event(&mut ada, |event| matches!(event, Event::InviteRevoked(_))).await,
        "no InviteRevoked for the banned member's invite"
    );
    let (_cyd_conn, mut cyd) = world.connect().await;
    let rejected = command(&mut cyd, 1, Msg::Hello(register(&code, "cyd"))).await;
    assert!(
        matches!(rejected.msg, Some(Event::Error(_))),
        "a banned member's invite still registers people: {rejected:?}"
    );
}

#[tokio::test]
async fn direct_messages_stay_out_of_the_audit_log() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (_ada_conn, mut ada) = world.connect().await;
    let ada_token = hello(&mut ada, ada_login).await.resume_token;
    let (_bea_conn, mut bea) = world.connect().await;
    hello(&mut bea, invite(&world, ada_id, "bea").await).await;
    let (_cyd_conn, mut cyd) = world.connect().await;
    let cyd_id = user_id(&hello(&mut cyd, invite(&world, ada_id, "cyd").await).await);
    let (_dan_conn, mut dan) = world.connect().await;
    let dan_id = user_id(&hello(&mut dan, invite(&world, ada_id, "dan").await).await);

    let created = command(
        &mut bea,
        2,
        Msg::CreateDm(v1::CreateDm {
            user_ids: vec![id_to_bytes(cyd_id), id_to_bytes(dan_id)],
        }),
    )
    .await;
    let Some(Event::Ack(v1::Ack {
        result: Some(v1::ack::Result::DmChannel(group)),
        ..
    })) = created.msg
    else {
        panic!("expected a DM channel, got {created:?}");
    };
    let group = group.id;
    let left = command(&mut bea, 3, Msg::LeaveDm(v1::LeaveDm { channel_id: group })).await;
    assert!(is_ack(&left), "{left:?}");
    let created = command(
        &mut ada,
        2,
        Msg::CreateChannel(v1::CreateChannel {
            kind: i32::from(v1::ChannelKind::Text),
            name: "visible".into(),
            ..Default::default()
        }),
    )
    .await;
    assert!(is_ack(&created));

    let actions = audit_actions_http(&world, &ada_token).await;
    assert!(actions.iter().any(|action| action == "channel.create"));
    assert!(
        !actions.iter().any(|action| action.starts_with("dm.")),
        "{actions:?}"
    );
}

async fn audit_actions_http(world: &World, token: &str) -> Vec<String> {
    let perms = Arc::new(
        perms::PermissionService::open(Arc::clone(&world.pool))
            .await
            .unwrap(),
    );
    let auth = Arc::new(auth::Auth::new(Arc::clone(&world.pool)));
    let response = http::audit::router(auth, perms)
        .oneshot(
            Request::builder()
                .uri("/api/v1/audit-log?limit=100")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    rows.iter()
        .map(|row| row["action"].as_str().unwrap().to_owned())
        .collect()
}

async fn create_role(
    stream: &mut ss_net::control::ClientStream,
    request_id: u32,
    name: &str,
    permissions: u64,
    position: i32,
) -> Bytes {
    let created = command(
        stream,
        request_id,
        Msg::CreateRole(v1::CreateRole {
            name: name.into(),
            color: None,
            permissions,
            position,
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

fn names_channel(frame: &v1::ServerEnvelope, channel: &Bytes) -> bool {
    match &frame.msg {
        Some(Event::VoiceStateUpdated(state)) => {
            state.channel_id.as_ref().is_some_and(|id| id == channel)
        }
        Some(Event::TrackAdded(track)) => track.channel_id == *channel,
        _ => false,
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
    (registered.user.id, login(username))
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
        client_name: "desk".into(),
        auth: Some(v1::hello::Auth::Login(v1::Login {
            username: username.into(),
            password: PASSWORD.into(),
        })),
        ..Default::default()
    }
}

fn join(channel: &Bytes) -> Msg {
    Msg::JoinVoice(v1::JoinVoice {
        channel_id: channel.clone(),
        self_mute: false,
        self_deaf: false,
    })
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

/// The answer to `msg` plus every frame that arrives in the quiet period after it.
async fn command_frames(
    stream: &mut ss_net::control::ClientStream,
    request_id: u32,
    msg: Msg,
) -> (v1::ServerEnvelope, Vec<v1::ServerEnvelope>) {
    let answer = command(stream, request_id, msg).await;
    let mut frames = Vec::new();
    let deadline = tokio::time::Instant::now() + QUIET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return (answer, frames);
        }
        match timeout(remaining, stream.recv()).await {
            Ok(frame) => frames.push(frame.unwrap()),
            Err(_) => return (answer, frames),
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

async fn next_track_for(stream: &mut ss_net::control::ClientStream, user: Id) -> v1::TrackAdded {
    let user_bytes = id_to_bytes(user);
    loop {
        let frame = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        if let Some(Event::TrackAdded(added)) = frame.msg
            && added.user_id.as_ref() == user_bytes.as_ref()
        {
            return added;
        }
    }
}

async fn voice_states_for(
    stream: &mut ss_net::control::ClientStream,
    user: Id,
) -> Vec<v1::VoiceStateUpdated> {
    let user_bytes = id_to_bytes(user);
    let mut states = Vec::new();
    let deadline = tokio::time::Instant::now() + QUIET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return states;
        }
        let frame = match timeout(remaining, stream.recv()).await {
            Ok(frame) => frame.unwrap(),
            Err(_) => return states,
        };
        if let Some(Event::VoiceStateUpdated(state)) = frame.msg
            && state.user_id.as_ref() == user_bytes.as_ref()
        {
            states.push(state);
        }
    }
}

async fn drain(stream: &mut ss_net::control::ClientStream) {
    let deadline = tokio::time::Instant::now() + QUIET;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() || timeout(remaining, stream.recv()).await.is_err() {
            return;
        }
    }
}

fn voice_named(snapshot: &v1::Snapshot, name: &str) -> Bytes {
    snapshot
        .channels
        .iter()
        .find(|channel| channel.name == name && channel.kind == i32::from(v1::ChannelKind::Voice))
        .unwrap_or_else(|| panic!("missing voice channel {name}"))
        .id
        .clone()
}

fn is_ack(envelope: &v1::ServerEnvelope) -> bool {
    matches!(envelope.msg, Some(Event::Ack(_)))
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

fn packet(track: u32) -> Bytes {
    let mut bytes = BytesMut::with_capacity(208);
    DatagramHeader {
        version: 0,
        kind: 0,
        flags: 1,
        track_id: track,
        seq: 7,
        timestamp: 960,
    }
    .encode_into(&mut bytes)
    .unwrap();
    bytes.resize(bytes.len() + 200, 0x55);
    bytes.freeze()
}
