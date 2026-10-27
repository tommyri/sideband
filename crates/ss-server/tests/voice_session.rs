//! Permission-checked voice membership and datagram fan-out.

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
    db::{self, NewInvite, invites},
};
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

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

#[tokio::test]
async fn join_without_connect_is_forbidden_and_an_admin_still_enters() {
    let world = World::new().await;
    let (admin_id, admin_login) = register_owner(&world, "ada").await;
    let (admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, admin_login).await;
    let voice = voice_named(admin.snapshot.as_ref().unwrap(), "General");
    let (_member_conn, mut member_stream) = world.connect().await;
    hello(&mut member_stream, invite(&world, admin_id, "bea").await).await;
    let denied = command(
        &mut admin_stream,
        2,
        Msg::SetChannelOverride(v1::SetChannelOverride {
            channel_id: voice.clone(),
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::CONNECT.bits(),
        }),
    )
    .await;
    assert!(matches!(denied.msg, Some(Event::Ack(_))));

    let rejected = command(&mut member_stream, 2, join(&voice, false, false)).await;
    assert_eq!(error_code(&rejected), i32::from(v1::ErrorCode::Forbidden));
    assert_eq!(error_message(&rejected), "missing connect");

    let joined = command(&mut admin_stream, 3, join(&voice, false, false)).await;
    assert!(matches!(joined.msg, Some(Event::Ack(_))));
    let track = own_track(&mut admin_stream, admin_id).await;
    assert!(track >= 1);
    admin_conn.close(0, b"bye");
}

#[tokio::test]
async fn user_limit_rejects_the_next_member() {
    let world = World::new().await;
    let (admin_id, admin_login) = register_owner(&world, "ada").await;
    let (_admin_conn, mut admin_stream) = world.connect().await;
    let admin = hello(&mut admin_stream, admin_login).await;
    let voice = voice_named(admin.snapshot.as_ref().unwrap(), "General");
    let (_bea_conn, mut bea_stream) = world.connect().await;
    let bea = hello(&mut bea_stream, invite(&world, admin_id, "bea").await).await;
    let (_cy_conn, mut cy_stream) = world.connect().await;
    hello(&mut cy_stream, invite(&world, admin_id, "cyd").await).await;

    let limited = command(
        &mut admin_stream,
        2,
        Msg::UpdateChannel(v1::UpdateChannel {
            channel_id: voice.clone(),
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

    let first = command(&mut bea_stream, 2, join(&voice, false, false)).await;
    assert!(matches!(first.msg, Some(Event::Ack(_))));
    let _ = own_track(&mut bea_stream, user_id(&bea)).await;

    let second = command(&mut cy_stream, 2, join(&voice, false, false)).await;
    assert_eq!(error_code(&second), i32::from(v1::ErrorCode::Forbidden));
    assert_eq!(error_message(&second), "voice channel is full");
}

#[tokio::test]
async fn switching_channels_emits_exactly_two_voice_states() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (_ada_conn, mut ada_stream) = world.connect().await;
    let ada = hello(&mut ada_stream, ada_login).await;
    let general = voice_named(ada.snapshot.as_ref().unwrap(), "General");
    let (_bea_conn, mut bea_stream) = world.connect().await;
    hello(&mut bea_stream, invite(&world, ada_id, "bea").await).await;

    let created = command(
        &mut ada_stream,
        2,
        Msg::CreateChannel(v1::CreateChannel {
            kind: i32::from(v1::ChannelKind::Voice),
            name: "stage".into(),
            position: 2,
            voice: Some(v1::VoiceSettings {
                profile: i32::from(v1::VoiceProfile::Music),
                ..Default::default()
            }),
            ..Default::default()
        }),
    )
    .await;
    let stage = ack_channel(&created);
    drain(&mut bea_stream).await;

    let joined = command(&mut ada_stream, 3, join(&general, false, false)).await;
    assert!(matches!(joined.msg, Some(Event::Ack(_))));
    drain(&mut bea_stream).await;
    drain(&mut ada_stream).await;

    let switched = command(&mut ada_stream, 4, join(&stage, false, false)).await;
    assert!(matches!(switched.msg, Some(Event::Ack(_))));
    let states = voice_states_for(&mut bea_stream, ada_id).await;
    assert_eq!(states.len(), 2);
    assert!(states[0].channel_id.is_none());
    assert_eq!(states[1].channel_id.as_deref(), Some(stage.as_ref()));
    let added = next_track_for(&mut ada_stream, ada_id).await;
    assert_eq!(added.codec.as_ref().map(|codec| codec.channels), Some(2));
}

#[tokio::test]
async fn server_mute_drops_datagrams_and_deaf_skips_the_recipient() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (ada_conn, mut ada_stream) = world.connect().await;
    let ada = hello(&mut ada_stream, ada_login).await;
    let voice = voice_named(ada.snapshot.as_ref().unwrap(), "General");
    let (bea_conn, mut bea_stream) = world.connect().await;
    let bea = hello(&mut bea_stream, invite(&world, ada_id, "bea").await).await;
    let bea_id = user_id(&bea);

    let ada_join = command(&mut ada_stream, 2, join(&voice, false, false)).await;
    assert!(matches!(ada_join.msg, Some(Event::Ack(_))));
    let ada_track = own_track(&mut ada_stream, ada_id).await;
    let bea_join = command(&mut bea_stream, 2, join(&voice, false, false)).await;
    assert!(matches!(bea_join.msg, Some(Event::Ack(_))));
    let bea_track = own_track(&mut bea_stream, bea_id).await;

    let before = world.server.media_stats();
    ada_conn.send_datagram(packet(ada_track)).unwrap();
    let heard = timeout(DEADLINE, bea_conn.read_datagram())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(heard, packet(ada_track));
    let forwarded = world.server.media_stats();
    assert_eq!(forwarded.forwarded, before.forwarded + 1);

    let muted = command(
        &mut ada_stream,
        3,
        Msg::ServerMuteUser(v1::ServerMuteUser {
            user_id: id_to_bytes(bea_id),
            muted: true,
        }),
    )
    .await;
    assert!(matches!(muted.msg, Some(Event::Ack(_))));
    let muted_stats = world.server.media_stats();
    bea_conn.send_datagram(packet(bea_track)).unwrap();
    assert!(timeout(QUIET, ada_conn.read_datagram()).await.is_err());
    let rejected = world.server.media_stats();
    assert_eq!(rejected.forwarded, muted_stats.forwarded);
    assert_eq!(rejected.rejected, muted_stats.rejected + 1);

    let deaf = command(
        &mut bea_stream,
        3,
        Msg::SetVoiceState(v1::SetVoiceState {
            self_mute: false,
            self_deaf: true,
        }),
    )
    .await;
    assert!(matches!(deaf.msg, Some(Event::Ack(_))));
    let deaf_stats = world.server.media_stats();
    ada_conn.send_datagram(packet(ada_track)).unwrap();
    assert!(timeout(QUIET, bea_conn.read_datagram()).await.is_err());
    let after = world.server.media_stats();
    assert_eq!(after.received, deaf_stats.received + 1);
    assert_eq!(after.forwarded, deaf_stats.forwarded);
    assert_eq!(after.rejected, deaf_stats.rejected);
}

#[tokio::test]
async fn welcome_includes_voice_and_a_second_connection_moves_the_user() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (desk, mut desk_stream) = world.connect().await;
    let ada = hello(&mut desk_stream, ada_login).await;
    assert!(
        ada.server_info
            .as_ref()
            .unwrap()
            .features
            .iter()
            .any(|feature| feature == "voice")
    );
    let voice = voice_named(ada.snapshot.as_ref().unwrap(), "General");
    let joined = command(&mut desk_stream, 2, join(&voice, true, false)).await;
    assert!(matches!(joined.msg, Some(Event::Ack(_))));

    let (_bea_conn, mut bea_stream) = world.connect().await;
    let bea = hello(&mut bea_stream, invite(&world, ada_id, "bea").await).await;
    let state = bea
        .snapshot
        .as_ref()
        .unwrap()
        .voice_states
        .iter()
        .find(|state| state.user_id.as_ref() == id_to_bytes(ada_id).as_ref())
        .unwrap();
    assert_eq!(state.channel_id.as_deref(), Some(voice.as_ref()));
    assert!(state.self_mute);
    assert_eq!(bea.snapshot.as_ref().unwrap().tracks.len(), 1);

    let (laptop, mut laptop_stream) = world.connect().await;
    hello(&mut laptop_stream, login("ada", "laptop")).await;
    drain(&mut bea_stream).await;
    let moved = command(&mut laptop_stream, 2, join(&voice, false, false)).await;
    assert!(matches!(moved.msg, Some(Event::Ack(_))));
    let states = voice_states_for(&mut bea_stream, ada_id).await;
    assert_eq!(states.len(), 2);
    assert!(states[0].channel_id.is_none());
    assert_eq!(states[1].channel_id.as_deref(), Some(voice.as_ref()));
    assert_ne!(states[0].conn_id, states[1].conn_id);

    desk.close(0, b"bye");
    laptop.close(0, b"bye");
    let left = voice_states_for(&mut bea_stream, ada_id).await;
    assert_eq!(left.len(), 1);
    assert!(left[0].channel_id.is_none());
}

#[tokio::test]
async fn disconnect_and_channel_delete_clear_membership_quickly() {
    let world = World::new().await;
    let (ada_id, ada_login) = register_owner(&world, "ada").await;
    let (ada_conn, mut ada_stream) = world.connect().await;
    let ada = hello(&mut ada_stream, ada_login).await;
    let voice = voice_named(ada.snapshot.as_ref().unwrap(), "General");
    let (_bea_conn, mut bea_stream) = world.connect().await;
    hello(&mut bea_stream, invite(&world, ada_id, "bea").await).await;
    let joined = command(&mut ada_stream, 2, join(&voice, false, false)).await;
    assert!(matches!(joined.msg, Some(Event::Ack(_))));
    drain(&mut bea_stream).await;

    ada_conn.close(0, b"bye");
    let started = tokio::time::Instant::now();
    let left = voice_states_for(&mut bea_stream, ada_id).await;
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(left.len(), 1);
    assert!(left[0].channel_id.is_none());

    let (ada_conn, mut ada_stream) = world.connect().await;
    let ada = hello(&mut ada_stream, login("ada", "desk")).await;
    let voice = voice_named(ada.snapshot.as_ref().unwrap(), "General");
    let joined = command(&mut ada_stream, 2, join(&voice, false, false)).await;
    assert!(matches!(joined.msg, Some(Event::Ack(_))));
    drain(&mut bea_stream).await;

    let deleted = command(
        &mut ada_stream,
        3,
        Msg::DeleteChannel(v1::DeleteChannel {
            channel_id: voice.clone(),
        }),
    )
    .await;
    assert!(matches!(deleted.msg, Some(Event::Ack(_))));
    let gone = voice_states_for(&mut bea_stream, ada_id).await;
    assert_eq!(gone.len(), 1);
    assert!(gone[0].channel_id.is_none());
    ada_conn.close(0, b"bye");
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

fn join(channel: &Bytes, self_mute: bool, self_deaf: bool) -> Msg {
    Msg::JoinVoice(v1::JoinVoice {
        channel_id: channel.clone(),
        self_mute,
        self_deaf,
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

async fn own_track(stream: &mut ss_net::control::ClientStream, user: Id) -> u32 {
    next_track_for(stream, user).await.track_id
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

fn ack_channel(envelope: &v1::ServerEnvelope) -> Bytes {
    match &envelope.msg {
        Some(Event::Ack(ack)) => match &ack.result {
            Some(v1::ack::Result::ChannelId(id)) => id.clone(),
            other => panic!("expected a channel id, got {other:?}"),
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
    bytes.extend_from_slice(&[0x55; 200]);
    bytes.freeze()
}
