//! Real QUIC client/server integration; no audio devices or external network.
use ss_client_core::{
    AudioConfig, AudioSource, Client, ClientEvent, ConnectionState, Credentials, Error, ServerAddr,
    State, TalkGate, VoiceSession,
    storage::{MemoryStorage, Storage},
    v1,
    voice::Membership,
};
use ss_core::{Id, Permissions};
use ss_net::{
    pins::{MemoryPinStore, PinStore},
    server::Endpoint,
    tls,
};
use ss_proto::validate::{id_from_bytes, id_to_bytes as wire};
use ss_server::{auth, connection::Server, db};
use std::{sync::Arc, time::Duration};
use tokio::{sync::broadcast, task::JoinHandle, time::timeout};

const WAIT: Duration = Duration::from_secs(12);
struct World {
    dir: tempfile::TempDir,
    pool: Arc<db::Pool>,
    endpoint: Endpoint,
    server: Arc<Server>,
    worker: JoinHandle<()>,
    address: ServerAddr,
    pins: Arc<MemoryPinStore>,
    storage: Arc<MemoryStorage>,
    invite: String,
}
impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("test.db")).await.unwrap());
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
            storage: Arc::default(),
            invite,
        }
    }
    async fn owner(&self) -> Client {
        Client::connect_with_storage(
            self.address.clone(),
            Credentials::Register(v1::Register {
                invite_code: self.invite.clone(),
                username: "owner".into(),
                password: "test-password".into(),
            }),
            self.pins.clone(),
            self.storage.clone(),
        )
        .await
        .unwrap()
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
                password: "test-password".into(),
            }),
            self.pins.clone(),
        )
        .await
        .unwrap()
    }
    async fn restart(&mut self) {
        self.restart_after(Duration::ZERO).await;
    }
    async fn restart_after(&mut self, outage: Duration) {
        let addr = self.endpoint.local_addr().unwrap();
        // As `serve` does on shutdown, so clients start their backoff over.
        self.endpoint
            .close(ss_server::registry::CLOSE_SHUTDOWN, b"forced restart");
        self.worker.abort();
        let _ = (&mut self.worker).await;
        self.endpoint.wait_idle().await;
        let identity = tls::generate_self_signed(self.dir.path()).await.unwrap();
        // Drop the old socket before rebinding the same UDP address.
        let replacement = Endpoint::bind(
            "127.0.0.1:0".parse().unwrap(),
            tls::generate_self_signed(self.dir.path()).await.unwrap(),
        )
        .unwrap();
        let old = std::mem::replace(&mut self.endpoint, replacement);
        drop(old);
        tokio::time::sleep(outage).await;
        self.server = Arc::new(Server::open(self.pool.clone()).await.unwrap());
        self.endpoint = timeout(WAIT, async {
            loop {
                match Endpoint::bind(
                    addr,
                    tls::TlsConfig {
                        certificates: identity.certificates.clone(),
                        private_key: identity.private_key.clone_key(),
                    },
                ) {
                    Ok(endpoint) => break endpoint,
                    Err(ss_net::Error::Io(error))
                        if error.kind() == std::io::ErrorKind::AddrInUse =>
                    {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("restart bind: {error}"),
                }
            }
        })
        .await
        .unwrap();
        self.worker = tokio::spawn(self.server.clone().run(self.endpoint.clone()));
    }
}
impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test done");
        self.worker.abort();
    }
}
async fn until(
    client: &Client,
    events: &mut broadcast::Receiver<ClientEvent>,
    predicate: impl Fn(&State) -> bool,
) {
    timeout(WAIT, async {
        loop {
            if predicate(&client.state().read()) {
                break;
            }
            let _ = events.recv().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| panic!("state: {:?}", client.state().snapshot()));
}
fn named(state: &State, name: &str, kind: v1::ChannelKind) -> Id {
    *state
        .channels
        .iter()
        .find(|(_, c)| c.name == name && c.kind == i32::from(kind))
        .unwrap()
        .0
}
fn post(channel: Id, content: &str) -> v1::SendMessage {
    v1::SendMessage {
        channel_id: wire(channel),
        content: content.into(),
        nonce: Some(content.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn snapshot_commands_events_persisted_session_and_drop() {
    let world = World::new().await;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let mut owner_events = owner.subscribe();
    let mut guest_events = guest.subscribe();
    let general = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    assert_eq!(
        owner.state().read().permissions(general),
        Some(Permissions::all())
    );
    assert_eq!(
        guest.state().read().permissions(general),
        Some(Permissions::EVERYONE)
    );
    let sent = owner
        .send_message(post(general, "hello core"))
        .await
        .unwrap();
    until(&guest, &mut guest_events, |s| {
        s.messages
            .get(&general)
            .is_some_and(|c| c.ordered().iter().any(|m| m.id == wire(sent)))
    })
    .await;
    until(&owner, &mut owner_events, |s| {
        s.messages
            .get(&general)
            .is_some_and(|c| c.ordered().len() == 1)
    })
    .await;
    owner
        .edit_message(v1::EditMessage {
            message_id: wire(sent),
            content: "edited".into(),
        })
        .await
        .unwrap();
    until(&guest, &mut guest_events, |s| {
        s.messages[&general].ordered()[0].content == "edited"
    })
    .await;
    let created = owner
        .create_channel(v1::CreateChannel {
            name: "new channel".into(),
            kind: v1::ChannelKind::Text.into(),
            ..Default::default()
        })
        .await
        .unwrap();
    until(&guest, &mut guest_events, |s| {
        s.channels.contains_key(&created)
    })
    .await;
    owner
        .delete_channel(v1::DeleteChannel {
            channel_id: wire(created),
        })
        .await
        .unwrap();
    until(&guest, &mut guest_events, |s| {
        !s.channels.contains_key(&created)
    })
    .await;
    assert_eq!(
        world.storage.last_server().unwrap(),
        Some(world.address.clone())
    );
    let saved = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert_eq!(saved.state().read().own_user, owner.state().read().own_user);
    let user = id_from_bytes(&owner.state().read().own_user.as_ref().unwrap().id).unwrap();
    drop(saved);
    owner.close().await;
    drop(guest);
    timeout(WAIT, async {
        while !world.server.registry().connection_ids(user).is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn voice_session_routes_test_tone_and_releases_workers() {
    let world = World::new().await;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let voice = named(&owner.state().read(), "General", v1::ChannelKind::Voice);
    let sender = VoiceSession::start(
        &owner,
        voice,
        AudioConfig {
            source: AudioSource::Test,
            gate: TalkGate::OpenMic,
            ..AudioConfig::default()
        },
    )
    .await
    .unwrap();
    let receiver = VoiceSession::start(
        &guest,
        voice,
        AudioConfig {
            source: AudioSource::Silent,
            ..AudioConfig::default()
        },
    )
    .await
    .unwrap();

    timeout(WAIT, async {
        loop {
            if receiver.stats().output_rms > 0.01 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();

    sender.set_self_mute(true).await.unwrap();
    timeout(WAIT, async {
        loop {
            let stats = receiver.stats();
            if stats.output_rms < 0.001 && stats.speakers.iter().all(|speaker| !speaker.speaking) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    sender.set_self_mute(false).await.unwrap();
    receiver.set_self_deaf(true).await.unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(receiver.stats().output_rms < 0.001);
    sender.leave().await.unwrap();
    receiver.leave().await.unwrap();

    for _ in 0..50 {
        let session = VoiceSession::start(
            &owner,
            voice,
            AudioConfig {
                source: AudioSource::Silent,
                ..AudioConfig::default()
            },
        )
        .await
        .unwrap();
        session.leave().await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

/// Waits for the open-mic tone to stop, then requires a full second with no
/// encoded frame. Published stats lag the worker by up to 500 ms.
async fn assert_transmission_stopped(session: &VoiceSession, own: Id) {
    timeout(Duration::from_millis(200), async {
        while session.speaking_users().contains(&own) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the talk gate stays open after mute returned");
    tokio::time::sleep(Duration::from_millis(600)).await;
    let before = session.stats().sent_packets;
    for _ in 0..50 {
        assert!(!session.speaking_users().contains(&own), "gate reopened");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        session.stats().sent_packets,
        before,
        "frames sent while muted"
    );
}

#[tokio::test]
async fn mute_closes_the_gate_before_the_server_and_reconciles_after_reconnect() {
    let mut world = World::new().await;
    let owner = world.owner().await;
    let mut events = owner.subscribe();
    let own_bytes = owner.state().read().own_user.as_ref().unwrap().id.clone();
    let own = id_from_bytes(&own_bytes).unwrap();
    let voice = named(&owner.state().read(), "General", v1::ChannelKind::Voice);
    let session = VoiceSession::start(
        &owner,
        voice,
        AudioConfig {
            source: AudioSource::Test,
            gate: TalkGate::OpenMic,
            ..AudioConfig::default()
        },
    )
    .await
    .unwrap();
    let talking = || async {
        timeout(WAIT, async {
            while !session.speaking_users().contains(&own) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    };
    talking().await;

    // The gate closes when mute is requested, before the server round trip.
    let pending = session.set_self_mute(true);
    assert!(session.self_muted());
    assert_transmission_stopped(&session, own).await;
    pending.await.unwrap();
    session.set_self_mute(false).await.unwrap();
    talking().await;

    // While reconnecting the server cannot be told, but the microphone stops.
    world.restart().await;
    until(&owner, &mut events, |s| {
        matches!(s.connection, ConnectionState::Reconnecting { .. })
    })
    .await;
    assert!(matches!(
        session.set_self_mute(true).await,
        Err(Error::Disconnected)
    ));
    assert!(session.self_muted());
    // An unmute the server never confirmed leaves the gate closed.
    assert!(session.set_self_mute(false).await.is_err());
    assert!(session.self_muted());
    assert_transmission_stopped(&session, own).await;
    until(&owner, &mut events, |s| {
        s.connection == ConnectionState::Connected
            && s.voice_states
                .values()
                .any(|v| v.user_id == own_bytes && v.channel_id == Some(wire(voice)) && v.self_mute)
    })
    .await;
    assert_transmission_stopped(&session, own).await;

    // Unmuting still waits for the server, then reopens the gate.
    session.set_self_mute(false).await.unwrap();
    talking().await;
    session.leave().await.unwrap();
}

fn tone() -> AudioConfig {
    AudioConfig {
        source: AudioSource::Test,
        gate: TalkGate::OpenMic,
        ..AudioConfig::default()
    }
}
fn silent() -> AudioConfig {
    AudioConfig {
        source: AudioSource::Silent,
        ..AudioConfig::default()
    }
}
fn own_id(client: &Client) -> Id {
    id_from_bytes(&client.state().read().own_user.as_ref().unwrap().id).unwrap()
}
/// The owner's account signed in on another device: its own session and
/// connection.
async fn second_device(world: &World) -> Client {
    Client::connect(
        world.address.clone(),
        Credentials::Login(v1::Login {
            username: "owner".into(),
            password: "test-password".into(),
        }),
        world.pins.clone(),
    )
    .await
    .unwrap()
}
/// `(track, conn)` for each audio track `user` publishes in `channel`.
fn audio_tracks(state: &State, user: Id, channel: Id) -> Vec<(u32, bytes::Bytes)> {
    state
        .tracks
        .values()
        .filter(|t| {
            t.kind == i32::from(v1::TrackKind::Audio)
                && t.user_id == wire(user)
                && t.channel_id == wire(channel)
        })
        .map(|t| (t.track_id, t.conn_id.clone()))
        .collect()
}
/// Waits until `session` plays `track` as speech from `user`.
async fn hears(session: &VoiceSession, track: u32, user: Id, within: Duration) {
    timeout(within, async {
        while !session
            .stats()
            .speakers
            .iter()
            .any(|s| s.track == track && s.user == user && s.speaking)
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "track {track} from {user} not heard: {:?}",
            session.stats().speakers
        )
    });
}
/// Closes `conn` from the server side as a slow consumer. The server drops
/// its voice seat at once, without a resume lease: the same state as a
/// reconnect after the 60 s lease expired.
fn drop_without_lease(world: &World, conn: Id) {
    let registry = world.server.registry();
    let grace = registry.slow_after();
    registry.set_slow_after(Duration::ZERO);
    // The server's writer shares this current-thread runtime, so the queue
    // fills without being drained.
    for _ in 0..=ss_server::registry::OUTBOUND_CAPACITY {
        registry.enqueue(
            conn,
            v1::ServerEnvelope {
                request_id: 0,
                msg: Some(v1::server_envelope::Msg::ServerNotice(v1::ServerNotice {
                    text: "filler".into(),
                })),
            },
        );
    }
    registry.set_slow_after(grace);
}

/// `(conn, channel)` for each voice seat `user` holds.
fn seats(state: &State, user: Id) -> Vec<(bytes::Bytes, bytes::Bytes)> {
    state
        .voice_states
        .values()
        .filter(|v| v.user_id == wire(user))
        .filter_map(|v| Some((v.conn_id.clone(), v.channel_id.clone()?)))
        .collect()
}
async fn voice_channel(owner: &Client, name: &str) -> Id {
    owner
        .create_channel(v1::CreateChannel {
            name: name.into(),
            kind: v1::ChannelKind::Voice.into(),
            ..Default::default()
        })
        .await
        .unwrap()
}

async fn reconnected(client: &Client, events: &mut broadcast::Receiver<ClientEvent>) {
    until(client, events, |s| {
        matches!(s.connection, ConnectionState::Reconnecting { .. })
    })
    .await;
    until(client, events, |s| {
        s.connection == ConnectionState::Connected
    })
    .await;
}

/// The server keeps one voice seat per user: joining on a second device
/// takes the seat from the first. A device that lost the seat, while away or
/// while connected, must not take it back when it reconnects. The device
/// holding it gets it back after losing it with no lease to resume.
#[tokio::test]
async fn one_accounts_devices_share_its_voice_seat_across_reconnects() {
    let world = World::new().await;
    let phone = world.owner().await;
    let listener = world.guest(&phone).await;
    let laptop = second_device(&world).await;
    let own = own_id(&phone);
    let general = named(&phone.state().read(), "General", v1::ChannelKind::Voice);
    let mut listener_events = listener.subscribe();
    let other = voice_channel(&phone, "Other").await;
    let mut phone_events = phone.subscribe();
    let mut laptop_events = laptop.subscribe();
    until(&laptop, &mut laptop_events, |s| {
        s.channels.contains_key(&other)
    })
    .await;
    // Guild channel diffs are debounced per session, so each client learns of
    // the new channel on its own schedule.
    until(&listener, &mut listener_events, |s| {
        s.channels.contains_key(&other)
    })
    .await;
    let listening = VoiceSession::start(&listener, other, silent())
        .await
        .unwrap();
    let phone_voice = VoiceSession::start(&phone, general, silent())
        .await
        .unwrap();
    let (phone_conn, _) = seats(&phone.state().read(), own)[0].clone();

    // The laptop joins while the phone is away; the phone stays out.
    drop_without_lease(&world, id_from_bytes(&phone_conn).unwrap());
    until(&phone, &mut phone_events, |s| {
        matches!(s.connection, ConnectionState::Reconnecting { .. })
    })
    .await;
    let laptop_voice = VoiceSession::start(&laptop, other, tone()).await.unwrap();
    let (track, laptop_conn) = audio_tracks(&laptop.state().read(), own, other)[0].clone();
    until(&phone, &mut phone_events, |s| {
        s.connection == ConnectionState::Connected
    })
    .await;
    assert_eq!(
        seats(&phone.state().read(), own),
        [(laptop_conn.clone(), wire(other))],
        "the phone took the seat back after it reconnected"
    );
    hears(&listening, track, own, WAIT).await;

    // The phone joins again and takes the seat while the laptop is
    // connected; the laptop then reconnects and stays out.
    drop(phone_voice);
    let _phone_voice = VoiceSession::start(&phone, other, tone()).await.unwrap();
    until(&laptop, &mut laptop_events, |s| {
        let seats = seats(s, own);
        seats.len() == 1 && seats[0].0 != laptop_conn
    })
    .await;
    let (phone_track, phone_conn) = audio_tracks(&phone.state().read(), own, other)[0].clone();
    drop(laptop_voice);
    drop_without_lease(&world, id_from_bytes(&laptop_conn).unwrap());
    reconnected(&laptop, &mut laptop_events).await;
    assert_eq!(
        seats(&laptop.state().read(), own),
        [(phone_conn.clone(), wire(other))],
        "the laptop took the seat back after it reconnected"
    );
    hears(&listening, phone_track, own, WAIT).await;

    // The phone loses its seat with no lease to resume, as after the 60 s
    // lease, and is back in voice on its new connection with its own track.
    drop_without_lease(&world, id_from_bytes(&phone_conn).unwrap());
    reconnected(&phone, &mut phone_events).await;
    let seats = seats(&phone.state().read(), own);
    assert_eq!(seats.len(), 1);
    assert_ne!(seats[0].0, phone_conn);
    assert_eq!(seats[0].1, wire(other));
    let (track, conn) = audio_tracks(&phone.state().read(), own, other)[0].clone();
    assert_eq!(conn, seats[0].0);
    hears(&listening, track, own, WAIT).await;
}

#[tokio::test]
async fn a_moved_member_rejoins_the_channel_it_was_moved_to_after_a_restart() {
    let mut world = World::new().await;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let user = own_id(&guest);
    let general = named(&owner.state().read(), "General", v1::ChannelKind::Voice);
    let other = voice_channel(&owner, "Other").await;
    let mut events = guest.subscribe();
    guest
        .join_voice(v1::JoinVoice {
            channel_id: wire(general),
            self_mute: true,
            self_deaf: false,
        })
        .await
        .unwrap();
    owner
        .move_user(v1::MoveUser {
            user_id: wire(user),
            channel_id: wire(other),
        })
        .await
        .unwrap();
    until(&guest, &mut events, |s| {
        seats(s, user)
            .iter()
            .any(|(_, channel)| *channel == wire(other))
    })
    .await;
    world.restart().await;
    until(&guest, &mut events, |s| {
        matches!(s.connection, ConnectionState::Reconnecting { .. })
    })
    .await;
    until(&guest, &mut events, |s| {
        s.connection == ConnectionState::Connected && !seats(s, user).is_empty()
    })
    .await;
    assert_eq!(seats(&guest.state().read(), user)[0].1, wire(other));
}

/// Polls the call's reconcile step until it reports something other than
/// [`Membership::Seated`].
async fn changed(session: &VoiceSession) -> Membership {
    timeout(WAIT, async {
        loop {
            match session.reconcile() {
                Membership::Seated => tokio::time::sleep(Duration::from_millis(10)).await,
                other => break other,
            }
        }
    })
    .await
    .expect("the call stayed seated")
}

/// A moderator moves a member. The call reports the move, and following it
/// restarts the call muted in the destination with that channel's profile,
/// as every host does.
#[tokio::test]
async fn a_moved_member_follows_muted_into_the_new_channel_and_its_profile() {
    let world = World::new().await;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let user = own_id(&guest);
    let general = named(&owner.state().read(), "General", v1::ChannelKind::Voice);
    let mut events = guest.subscribe();
    let stage = owner
        .create_channel(v1::CreateChannel {
            name: "Stage".into(),
            kind: v1::ChannelKind::Voice.into(),
            voice: Some(v1::VoiceSettings {
                profile: v1::VoiceProfile::Music.into(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    // Channel diffs are debounced: the guest must know the destination.
    until(&guest, &mut events, |s| s.channels.contains_key(&stage)).await;
    let session = Arc::new(VoiceSession::start(&guest, general, tone()).await.unwrap());
    assert_eq!(session.reconcile(), Membership::Seated);
    assert!(!session.self_muted());

    owner
        .move_user(v1::MoveUser {
            user_id: wire(user),
            channel_id: wire(stage),
        })
        .await
        .unwrap();
    assert_eq!(
        changed(&session).await,
        Membership::Follow { channel: stage }
    );
    let session = session.follow(&guest, stage, tone()).await.unwrap();
    assert_eq!(session.channel(), stage);
    assert!(session.self_muted(), "a moved member follows muted");
    assert_eq!(session.reconcile(), Membership::Seated);
    until(&guest, &mut events, |s| {
        s.voice_states
            .values()
            .any(|v| v.user_id == wire(user) && v.channel_id == Some(wire(stage)) && v.self_mute)
    })
    .await;
    // The music profile's default bitrate, not the voice profile's 80 kb/s.
    timeout(WAIT, async {
        while session.stats().encoder_bitrate != 128_000 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("encoder at {}", session.stats().encoder_bitrate));
    assert_transmission_stopped(&session, user).await;
    session.leave().await.unwrap();
}

/// The call ends when its seat goes while the connection stays up (the
/// account joined on another device), and when its client stops.
#[tokio::test]
async fn a_call_ends_when_its_seat_or_its_client_is_gone() {
    let world = World::new().await;
    let phone = world.owner().await;
    let laptop = second_device(&world).await;
    let general = named(&phone.state().read(), "General", v1::ChannelKind::Voice);
    let phone_voice = VoiceSession::start(&phone, general, silent())
        .await
        .unwrap();
    assert_eq!(phone_voice.reconcile(), Membership::Seated);
    let laptop_voice = VoiceSession::start(&laptop, general, silent())
        .await
        .unwrap();
    assert_eq!(changed(&phone_voice).await, Membership::Ended);
    assert_eq!(phone.state().read().connection, ConnectionState::Connected);

    // A host that stops a client may still share it, so its store can still
    // say connected. The call ends anyway.
    assert_eq!(laptop_voice.reconcile(), Membership::Seated);
    laptop.abort();
    assert_eq!(changed(&laptop_voice).await, Membership::Ended);
}

/// While the connection retries, the call releases push-to-talk, so a key
/// held through the outage does not transmit after it. A reconnect that lost
/// its lease has a new connection id; the call follows it and stays seated.
#[tokio::test]
async fn retrying_releases_ptt_and_a_new_connection_id_keeps_the_call() {
    let world = World::new().await;
    let owner = world.owner().await;
    let own = own_id(&owner);
    let mut events = owner.subscribe();
    let general = named(&owner.state().read(), "General", v1::ChannelKind::Voice);
    let session = VoiceSession::start(
        &owner,
        general,
        AudioConfig {
            source: AudioSource::Test,
            ..AudioConfig::default()
        },
    )
    .await
    .unwrap();
    let first = session.connection();
    assert_eq!(owner.state().read().own_conn, Some(first));
    let talking = |expected: bool, within: Duration| {
        let session = &session;
        async move {
            timeout(within, async {
                while session.speaking_users().contains(&own) != expected {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("talking never became {expected}"));
        }
    };
    session.set_ptt(true);
    talking(true, WAIT).await;

    drop_without_lease(&world, first);
    until(&owner, &mut events, |s| {
        matches!(s.connection, ConnectionState::Reconnecting { .. })
    })
    .await;
    assert_eq!(session.reconcile(), Membership::Retrying);
    // Released at once, without the release tail.
    talking(false, Duration::from_millis(100)).await;

    until(&owner, &mut events, |s| {
        s.connection == ConnectionState::Connected
    })
    .await;
    assert_eq!(session.reconcile(), Membership::Seated);
    let second = session.connection();
    assert_ne!(second, first, "the lease was gone, so the id is new");
    assert_eq!(owner.state().read().own_conn, Some(second));
    assert_eq!(
        seats(&owner.state().read(), own),
        [(wire(second), wire(general))]
    );
    for _ in 0..25 {
        assert!(
            !session.speaking_users().contains(&own),
            "the key held through the outage transmits"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    session.leave().await.unwrap();
}

#[tokio::test]
async fn after_a_restart_a_reused_track_id_plays_as_its_new_speaker() {
    let mut world = World::new().await;
    let observer = world.owner().await;
    let first = world.guest(&observer).await;
    let invite = observer
        .create_invite(v1::CreateInvite {
            max_uses: Some(1),
            expires_at: None,
        })
        .await
        .unwrap();
    let third_storage = Arc::new(MemoryStorage::default());
    let third = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Register(v1::Register {
            invite_code: invite.code,
            username: "third".into(),
            password: "test-password".into(),
        }),
        world.pins.clone(),
        third_storage.clone(),
    )
    .await
    .unwrap();
    let third_user = own_id(&third);
    let third_token = third_storage
        .session(&world.address.id().unwrap())
        .unwrap()
        .unwrap();
    third.close().await;
    let first_user = own_id(&first);
    let voice = named(&observer.state().read(), "General", v1::ChannelKind::Voice);

    // Before the restart the first member holds track 1.
    let first_voice = VoiceSession::start(&first, voice, tone()).await.unwrap();
    let observing = VoiceSession::start(&observer, voice, silent())
        .await
        .unwrap();
    let (reused, _) = audio_tracks(&observer.state().read(), first_user, voice)[0].clone();
    hears(&observing, reused, first_user, WAIT).await;
    // Let the first member's sequence numbers run well ahead.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // The first member goes away while the server is down, so the observer
    // never hears it leave.
    let mut events = observer.subscribe();
    let gone = async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(first_voice);
        first.close().await;
    };
    tokio::join!(world.restart_after(Duration::from_millis(300)), gone);

    // The third member is back before the observer's reconnect backoff ends.
    let third = Client::connect(
        world.address.clone(),
        Credentials::Session(third_token),
        world.pins.clone(),
    )
    .await
    .unwrap();
    let _third_voice = VoiceSession::start(&third, voice, tone()).await.unwrap();
    until(&observer, &mut events, |s| {
        s.connection == ConnectionState::Connected && !audio_tracks(s, third_user, voice).is_empty()
    })
    .await;
    let (track, _) = audio_tracks(&observer.state().read(), third_user, voice)[0].clone();
    assert_eq!(track, reused, "the third member did not rejoin first");
    // At once: well before the new sequence catches up with the old one.
    hears(&observing, track, third_user, Duration::from_millis(1500)).await;
    assert!(
        observing
            .stats()
            .speakers
            .iter()
            .all(|speaker| speaker.user != first_user)
    );
}

#[tokio::test]
async fn permission_changes_refresh_existing_and_newly_visible_channels() {
    let world = World::new().await;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let mut events = guest.subscribe();
    let general = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    let _ = guest
        .send_message(post(general, "cached private message"))
        .await
        .unwrap();
    until(&guest, &mut events, |s| s.messages.contains_key(&general)).await;
    let override_command = |deny: Permissions| v1::SetChannelOverride {
        channel_id: wire(general),
        role_id: wire(db::EVERYONE_ROLE_ID),
        allow: 0,
        deny: deny.bits(),
    };
    owner
        .set_channel_override(override_command(Permissions::SEND_MESSAGES))
        .await
        .unwrap();
    until(&guest, &mut events, |s| {
        s.permissions(general)
            .is_some_and(|p| !p.contains(Permissions::SEND_MESSAGES))
    })
    .await;
    assert!(
        matches!(guest.send_message(post(general, "denied")).await, Err(Error::Server { code, .. }) if code == i32::from(v1::ErrorCode::Forbidden))
    );
    owner
        .set_channel_override(override_command(Permissions::VIEW_CHANNEL))
        .await
        .unwrap();
    until(&guest, &mut events, |s| !s.channels.contains_key(&general)).await;
    assert!(!guest.state().read().messages.contains_key(&general));
    owner
        .set_channel_override(override_command(Permissions::empty()))
        .await
        .unwrap();
    until(&guest, &mut events, |s| s.channels.contains_key(&general)).await;
    assert_eq!(
        guest.state().read().permissions(general),
        Some(Permissions::EVERYONE)
    );
}

#[tokio::test]
async fn forced_restart_resumes_snapshot_and_voice_without_duplicate_commands() {
    let mut world = World::new().await;
    let (https_port, https_task) = https(&world, false).await;
    world.address.https_port = https_port;
    let client = world.owner().await;
    let mut events = client.subscribe();
    let voice = named(&client.state().read(), "General", v1::ChannelKind::Voice);
    let general = named(&client.state().read(), "general", v1::ChannelKind::Text);
    client
        .join_voice(v1::JoinVoice {
            channel_id: wire(voice),
            self_mute: true,
            self_deaf: false,
        })
        .await
        .unwrap();
    until(&client, &mut events, |s| !s.voice_states.is_empty()).await;
    let _ = client.send_message(post(general, "once")).await.unwrap();
    world.restart().await;
    until(&client, &mut events, |s| {
        matches!(s.connection, ConnectionState::Reconnecting { .. })
    })
    .await;
    let poster = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    let _ = poster
        .send_message(post(general, "missed while offline"))
        .await
        .unwrap();
    let queued = client.send_message(post(general, "during restart"));
    let queued_id = timeout(WAIT, queued).await.unwrap().unwrap();
    timeout(WAIT, async {
        loop {
            if matches!(events.recv().await.unwrap(), ClientEvent::Resynced) {
                break;
            }
        }
    })
    .await
    .unwrap();
    until(&client, &mut events, |s| {
        s.connection == ConnectionState::Connected
            && s.voice_states
                .values()
                .any(|v| v.channel_id == Some(wire(voice)) && v.self_mute)
    })
    .await;
    assert!(
        client.state().read().messages[&general]
            .ordered()
            .iter()
            .any(|message| message.content == "during restart")
    );
    // Missed history is fetched after `Connected`, alongside live traffic.
    until(&client, &mut events, |s| {
        s.messages[&general]
            .ordered()
            .iter()
            .any(|message| message.content == "missed while offline")
    })
    .await;
    assert!(!client.state().read().channels.is_empty());
    let mut conn = world.pool.read().await.unwrap();
    let stored: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE id = ? AND content = 'during restart'",
    )
    .bind(queued_id)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(stored, 1);
    assert!(
        client
            .send_message(post(general, "after restart"))
            .await
            .is_ok()
    );
    https_task.abort();
}

#[tokio::test]
async fn edits_and_deletes_made_during_an_outage_show_after_resume() {
    let mut world = World::new().await;
    let (https_port, https_task) = https(&world, false).await;
    world.address.https_port = https_port;
    let client = world.owner().await;
    let mut events = client.subscribe();
    let general = named(&client.state().read(), "general", v1::ChannelKind::Text);
    let doomed = client.send_message(post(general, "doomed")).await.unwrap();
    let edited = client
        .send_message(post(general, "first draft"))
        .await
        .unwrap();
    let kept = client.send_message(post(general, "kept")).await.unwrap();
    until(&client, &mut events, |s| {
        s.messages
            .get(&general)
            .is_some_and(|cache| cache.ordered().len() == 3)
    })
    .await;
    // While the client is away its server changes two cached messages. It
    // never sees the events, as in an outage.
    let pool = world.pool.clone();
    let offline = async {
        until(&client, &mut events, |s| {
            matches!(s.connection, ConnectionState::Reconnecting { .. })
        })
        .await;
        let mut conn = pool.write().await;
        db::messages::soft_delete(&mut conn, doomed, ss_core::Timestamp::now())
            .await
            .unwrap();
        db::messages::edit(
            &mut conn,
            edited,
            "final text",
            &[],
            false,
            ss_core::Timestamp::now(),
        )
        .await
        .unwrap();
    };
    tokio::join!(world.restart_after(Duration::from_millis(300)), offline);
    until(&client, &mut events, |s| {
        s.connection == ConnectionState::Connected
            && s.messages.get(&general).is_some_and(|cache| {
                let ordered = cache.ordered();
                ordered.iter().all(|message| message.id != wire(doomed))
                    && ordered.iter().any(|message| {
                        message.id == wire(edited) && message.content == "final text"
                    })
            })
    })
    .await;
    assert!(
        client.state().read().messages[&general]
            .ordered()
            .iter()
            .any(|message| message.id == wire(kept))
    );
    https_task.abort();
}

/// The resync after a resume is refused once with `429` and
/// `Retry-After: 1`. It waits the stated second, plus up to a quarter more,
/// and then reads the page: the edit made while away shows, and no
/// "resync deferred" notice is raised.
#[tokio::test]
async fn a_rate_limited_resync_waits_as_told_and_then_catches_up() {
    let mut world = World::new().await;
    let throttle = Arc::new(Throttle::default());
    let (https_port, https_task) = https_with(&world, false, Some(throttle.clone())).await;
    world.address.https_port = https_port;
    let client = world.owner().await;
    let mut events = client.subscribe();
    let mut notices = client.subscribe();
    let general = named(&client.state().read(), "general", v1::ChannelKind::Text);
    let kept = client
        .send_message(post(general, "first draft"))
        .await
        .unwrap();
    until(&client, &mut events, |s| {
        s.messages
            .get(&general)
            .is_some_and(|cache| cache.ordered().len() == 1)
    })
    .await;
    let pool = world.pool.clone();
    let offline = async {
        until(&client, &mut events, |s| {
            matches!(s.connection, ConnectionState::Reconnecting { .. })
        })
        .await;
        throttle
            .refuse
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let mut conn = pool.write().await;
        db::messages::edit(
            &mut conn,
            kept,
            "edited while away",
            &[],
            false,
            ss_core::Timestamp::now(),
        )
        .await
        .unwrap();
    };
    tokio::join!(world.restart_after(Duration::from_millis(300)), offline);
    until(&client, &mut events, |s| {
        s.connection == ConnectionState::Connected
            && s.messages.get(&general).is_some_and(|cache| {
                cache
                    .ordered()
                    .iter()
                    .any(|message| message.content == "edited while away")
            })
    })
    .await;
    let log = throttle.log.lock().clone();
    assert_eq!(
        log.iter().map(|(_, status)| *status).collect::<Vec<_>>(),
        [429, 200]
    );
    let waited = log[1].0 - log[0].0;
    assert!(
        waited >= Duration::from_secs(1) && waited < Duration::from_secs(3),
        "retried after {waited:?}"
    );
    loop {
        match notices.try_recv() {
            Ok(ClientEvent::Notice(text)) => {
                assert!(!text.contains("deferred"), "notice: {text}");
            }
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => {}
            Err(_) => break,
        }
    }
    https_task.abort();
}

/// Twelve sessions sign in while four accounts send twenty mentions. Each
/// mention must be counted once: in the snapshot's `mention_count` or as a
/// live `MessageCreated`, never both. Before T-113 the server subscribed to
/// the bus before it built the snapshot, and every session ended at 21 to 24
/// (T-107 notes). The client cannot tell such events apart: the snapshot
/// carries no message horizon for guild channels.
#[tokio::test]
async fn mentions_stored_while_a_session_signs_in_are_counted_once() {
    const SENDERS: usize = 4;
    // Each account may send five messages per five seconds.
    const EACH: usize = 5;
    const SESSIONS: u64 = 12;
    let world = World::new().await;
    let owner = world.owner().await;
    let general = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    let mut senders = Vec::new();
    for index in 0..SENDERS {
        let invite = owner
            .create_invite(v1::CreateInvite {
                max_uses: Some(1),
                expires_at: None,
            })
            .await
            .unwrap();
        let credentials = Credentials::Register(v1::Register {
            invite_code: invite.code,
            username: format!("sender{index}"),
            password: "test-password".into(),
        });
        senders.push(
            Client::connect(world.address.clone(), credentials, world.pins.clone())
                .await
                .unwrap(),
        );
    }
    let read = owner.send_message(post(general, "read")).await.unwrap();
    owner
        .mark_read(v1::MarkRead {
            channel_id: wire(general),
            message_id: wire(read),
        })
        .await
        .unwrap();
    let mention = format!("<@{}>", own_id(&owner));
    let saved = || {
        Client::connect_with_storage(
            world.address.clone(),
            Credentials::Saved,
            world.pins.clone(),
            world.storage.clone(),
        )
    };
    let flood = futures::future::join_all(senders.iter().enumerate().map(|(who, sender)| {
        let mention = &mention;
        async move {
            for index in 0..EACH {
                let content = format!("{mention} ping {who}.{index}");
                let _ = sender.send_message(post(general, &content)).await.unwrap();
                tokio::time::sleep(Duration::from_millis(3)).await;
            }
        }
    }));
    let sign_ins = futures::future::join_all((0..SESSIONS).map(|index| async move {
        tokio::time::sleep(Duration::from_millis(4 * index)).await;
        saved().await.unwrap()
    }));
    let (_, sessions) = tokio::join!(flood, sign_ins);
    // Late sessions may know every mention from their snapshot. Send a
    // marker after sign-in so every session has a live event to wait for.
    let marker = wire(
        owner
            .send_message(post(general, "after sign-in"))
            .await
            .unwrap(),
    );
    for session in &sessions {
        let mut events = session.subscribe();
        until(session, &mut events, |s| {
            s.messages
                .get(&general)
                .is_some_and(|cache| cache.ordered().iter().any(|m| m.id == marker))
        })
        .await;
    }
    let count = |client: &Client| {
        client
            .state()
            .read()
            .read_states
            .get(&general)
            .map(|read| read.mention_count)
    };
    let truth = count(&saved().await.unwrap());
    assert_eq!(truth, Some(u32::try_from(SENDERS * EACH).unwrap()));
    let counts: Vec<_> = sessions.iter().map(count).collect();
    assert!(
        counts.iter().all(|count| *count == truth),
        "{counts:?}, the database says {truth:?}"
    );
}

/// The connection of `user` that `before` did not list, once registered.
async fn registered(world: &World, user: Id, before: &[Id]) -> Id {
    timeout(WAIT, async {
        loop {
            let now = world.server.registry().connection_ids(user);
            if let Some(conn) = now.into_iter().find(|conn| !before.contains(conn)) {
                break conn;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the signing-in connection was never registered")
}

/// Frames other tasks address to a connection while it signs in (here
/// read states, the realistic one: the user's other session marking a
/// channel read) must not stand between the client and its `Welcome`. The
/// client's `establish` reads nothing but the `Hello` reply, so more early
/// frames than its event queue holds would keep that reply from it.
#[tokio::test]
async fn a_sign_in_behind_hundreds_of_addressed_frames_still_succeeds() {
    const EARLY: usize = 300;
    let world = World::new().await;
    let owner = world.owner().await;
    let general = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    let first = owner.send_message(post(general, "first")).await.unwrap();
    let second = owner.send_message(post(general, "second")).await.unwrap();
    let user = own_id(&owner);
    let before = world.server.registry().connection_ids(user);
    let hold = world.server.hold_sign_ins().await;
    let (address, pins, storage) = (
        world.address.clone(),
        world.pins.clone(),
        world.storage.clone(),
    );
    let signing_in = tokio::spawn(Client::connect_with_storage(
        address,
        Credentials::Saved,
        pins,
        storage,
    ));
    let conn = registered(&world, user, &before).await;
    for _ in 0..EARLY {
        world.server.registry().enqueue(
            conn,
            v1::ServerEnvelope {
                request_id: 0,
                msg: Some(v1::server_envelope::Msg::ReadStateUpdated(
                    v1::ReadStateUpdated {
                        channel_id: wire(general),
                        last_read_message_id: wire(first),
                        mention_count: 0,
                    },
                )),
            },
        );
    }
    owner
        .mark_read(v1::MarkRead {
            channel_id: wire(general),
            message_id: wire(second),
        })
        .await
        .unwrap();
    drop(hold);
    let session = timeout(WAIT, signing_in)
        .await
        .expect("sign-in hung")
        .unwrap()
        .expect("sign-in failed");
    let mut events = session.subscribe();
    // The newest read state, sent last, wins over the snapshot and the
    // earlier frames.
    until(&session, &mut events, |state| {
        state
            .read_states
            .get(&general)
            .is_some_and(|read| read.last_read_message_id == wire(second))
    })
    .await;
    assert_eq!(
        session.state().read().connection,
        ConnectionState::Connected
    );
}

/// A kick aimed at a connection that is still signing in refuses its
/// `Hello` with `FORBIDDEN`: a first sign-in fails with that error.
#[tokio::test]
async fn a_member_kicked_while_signing_in_is_refused() {
    let world = World::new().await;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let user = own_id(&guest);
    drop(guest);
    let before = world.server.registry().connection_ids(user);
    let hold = world.server.hold_sign_ins().await;
    let signing_in = tokio::spawn(Client::connect(
        world.address.clone(),
        Credentials::Login(v1::Login {
            username: "guest".into(),
            password: "test-password".into(),
        }),
        world.pins.clone(),
    ));
    let _ = registered(&world, user, &before).await;
    owner
        .kick_user(v1::KickUser {
            user_id: wire(user),
            reason: "out".into(),
        })
        .await
        .unwrap();
    drop(hold);
    let refused = timeout(WAIT, signing_in)
        .await
        .expect("sign-in hung")
        .unwrap();
    assert!(
        matches!(
            refused,
            Err(Error::Server { code, .. }) if code == i32::from(v1::ErrorCode::Forbidden)
        ),
        "{:?}",
        refused.map(|_| ())
    );
}

/// The same kick while the client reconnects ends its session: the refused
/// `Hello` is final, so it stops and does not try again.
#[tokio::test]
async fn a_member_kicked_while_reconnecting_stops_reconnecting() {
    let world = World::new().await;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let mut events = guest.subscribe();
    let user = own_id(&guest);
    let before = world.server.registry().connection_ids(user);
    let hold = world.server.hold_sign_ins().await;
    drop_without_lease(&world, before[0]);
    let _ = registered(&world, user, &before).await;
    owner
        .kick_user(v1::KickUser {
            user_id: wire(user),
            reason: "out".into(),
        })
        .await
        .unwrap();
    drop(hold);
    until(&guest, &mut events, |state| {
        matches!(state.connection, ConnectionState::Failed(_))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1400)).await;
    assert!(
        matches!(guest.state().read().connection, ConnectionState::Failed(_)),
        "{:?}",
        guest.state().read().connection
    );
    assert_eq!(
        world.server.registry().connection_ids(user),
        Vec::<Id>::new()
    );
}

#[tokio::test]
#[ignore = "two-minute CI chaos test"]
async fn two_minute_random_outages_leave_no_stuck_client_or_voice_tracks() {
    let mut world = World::new().await;
    let client = world.owner().await;
    let mut events = client.subscribe();
    let voice = named(&client.state().read(), "General", v1::ChannelKind::Voice);
    let general = named(&client.state().read(), "general", v1::ChannelKind::Text);
    client
        .join_voice(v1::JoinVoice {
            channel_id: wire(voice),
            self_mute: false,
            self_deaf: false,
        })
        .await
        .unwrap();
    until(&client, &mut events, |state| !state.tracks.is_empty()).await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut sent = 0_i64;
    while tokio::time::Instant::now() < deadline {
        let mut random = [0_u8; 1];
        getrandom::fill(&mut random).unwrap();
        let outage = Duration::from_secs(1 + u64::from(random[0] % 10));
        let content = format!("chaos-{sent}");
        let restart = world.restart_after(outage);
        let exercise = async {
            until(&client, &mut events, |state| {
                matches!(state.connection, ConnectionState::Reconnecting { .. })
            })
            .await;
            let _ = client.send_message(post(general, &content)).await.unwrap();
        };
        tokio::join!(restart, exercise);
        until(&client, &mut events, |state| {
            state.connection == ConnectionState::Connected
                && state
                    .voice_states
                    .values()
                    .any(|voice_state| voice_state.channel_id == Some(wire(voice)))
        })
        .await;
        assert_eq!(world.server.voice_members(), 1);
        sent += 1;
    }

    assert!(sent >= 8, "only completed {sent} outage cycles");
    let mut conn = world.pool.read().await.unwrap();
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE content LIKE 'chaos-%'")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(stored, sent);
    assert_eq!(world.server.voice_members(), 1);
}

#[tokio::test]
async fn ban_stops_automatic_reconnect_and_new_pins_require_approval() {
    let world = World::new().await;
    let untrusted = Client::connect(
        world.address.clone(),
        Credentials::Login(v1::Login {
            username: "unused".into(),
            password: "secret".into(),
        }),
        Arc::new(MemoryPinStore::default()),
    )
    .await;
    assert!(matches!(
        untrusted,
        Err(Error::Network(ss_net::Error::Untrusted { .. }))
    ));
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let mut events = guest.subscribe();
    let user = guest.state().read().own_user.as_ref().unwrap().id.clone();
    owner
        .ban_user(v1::BanUser {
            user_id: user,
            reason: "test ban".into(),
            expires_at: None,
        })
        .await
        .unwrap();
    until(&guest, &mut events, |s| {
        matches!(
            s.connection,
            ConnectionState::Disconnected | ConnectionState::Failed(_)
        )
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1400)).await;
    assert!(!matches!(
        guest.state().read().connection,
        ConnectionState::Connected | ConnectionState::Reconnecting { .. }
    ));
}

// A small HTTPS adapter for the actual production Axum history router. The
// deployed server uses a TLS reverse proxy; this fixture supplies that boundary.
async fn https(world: &World, wrong_certificate: bool) -> (u16, JoinHandle<()>) {
    https_with(world, wrong_certificate, None).await
}
/// Refuses the next `refuse` history requests with `429` and `Retry-After: 1`,
/// as `ss-server`'s per-address limiter answers, and logs every history
/// request's status with the time it arrived.
#[derive(Default)]
struct Throttle {
    refuse: std::sync::atomic::AtomicUsize,
    log: parking_lot::Mutex<Vec<(std::time::Instant, u16)>>,
}
impl Throttle {
    fn refused(&self) -> bool {
        self.refuse
            .try_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |left| left.checked_sub(1),
            )
            .is_ok()
    }
}
async fn https_with(
    world: &World,
    wrong_certificate: bool,
    throttle: Option<Arc<Throttle>>,
) -> (u16, JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tower::ServiceExt;
    let other = tempfile::tempdir().unwrap();
    let identity = tls::generate_self_signed(if wrong_certificate {
        other.path()
    } else {
        world.dir.path()
    })
    .await
    .unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(identity.certificates, identity.private_key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = world.server.public_routes();
    let task = tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (socket, _) = accepted.unwrap(); let acceptor = acceptor.clone(); let app = app.clone(); let throttle = throttle.clone();
                    peers.spawn(async move {
                        let Ok(mut stream) = acceptor.accept(socket).await else { return; };
                        let mut header = Vec::new(); let mut byte = [0];
                        while header.len() < 16384 && !header.ends_with(b"\r\n\r\n") {
                            if stream.read_exact(&mut byte).await.is_err() { return; } header.push(byte[0]);
                        }
                        let text = String::from_utf8(header).unwrap(); let mut lines = text.split("\r\n");
                        let mut start = lines.next().unwrap().split_whitespace();
                        let method = start.next().unwrap(); let path = start.next().unwrap();
                        let mut request = axum::http::Request::builder().method(method).uri(path);
                        let mut length = 0;
                        for line in lines { if let Some((key, value)) = line.split_once(':') {
                            if key.eq_ignore_ascii_case("content-length") { length = value.trim().parse::<usize>().unwrap(); }
                            request = request.header(key, value.trim());
                        } }
                        assert!(length <= 4 * 1024 * 1024);
                        let mut body = vec![0; length]; stream.read_exact(&mut body).await.unwrap();
                        let history = path.contains("/messages");
                        let (status, retry, body) = if history && throttle.as_ref().is_some_and(|t| t.refused()) {
                            (429, "Retry-After: 1\r\n".to_owned(), axum::body::Bytes::from_static(b"rate limited"))
                        } else {
                            let response = app.oneshot(request.body(axum::body::Body::from(body)).unwrap()).await.unwrap();
                            let retry = response.headers().get(axum::http::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).map(|v| format!("Retry-After: {v}\r\n")).unwrap_or_default();
                            (response.status().as_u16(), retry, axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024).await.unwrap())
                        };
                        if let Some(throttle) = throttle.as_ref().filter(|_| history) { throttle.log.lock().push((std::time::Instant::now(), status)); }
                        let header = format!("HTTP/1.1 {status} OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n{retry}Connection: close\r\n\r\n", body.len());
                        let _ = stream.write_all(header.as_bytes()).await; let _ = stream.write_all(&body).await; let _ = stream.shutdown().await;
                    });
                }
                _ = peers.join_next(), if !peers.is_empty() => {}
            }
        }
    });
    (port, task)
}

#[tokio::test]
async fn https_history_uses_session_bearer_and_exact_quic_pin() {
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let mut events = owner.subscribe();
    let channel = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    let first = owner
        .send_message(post(channel, "history one"))
        .await
        .unwrap();
    let second = owner
        .send_message(post(channel, "history two"))
        .await
        .unwrap();
    until(&owner, &mut events, |s| {
        s.messages
            .get(&channel)
            .is_some_and(|c| c.ordered().len() == 2)
    })
    .await;
    // A fresh session starts without cached messages, forcing the REST path.
    let fresh = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    let page = fresh.load_older(channel, Some(second)).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, wire(first));
    assert_eq!(page[0].content, "history one");
    assert_eq!(fresh.state().read().messages[&channel].history_len(), 1);
    let around = fresh.load_around(channel, second).await.unwrap();
    assert!(around.iter().any(|m| m.id == wire(second)));
    assert_eq!(fresh.state().read().messages[&channel].history_len(), 2);
    let file = attachment_roundtrip(&world, &fresh, &owner, channel).await;
    let (wrong_port, wrong_task) = https(&world, true).await;
    let mut address = world.address.clone();
    address.https_port = wrong_port;
    let wrong = Client::connect_with_storage(
        address,
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        wrong.load_older(channel, None).await,
        Err(Error::Http(_))
    ));
    assert!(matches!(
        wrong.load_around(channel, second).await,
        Err(Error::Http(_))
    ));
    // An in-flight authenticated session must not adopt a subsequent store edit.
    world
        .pins
        .set(
            &world.address.id().unwrap(),
            ss_net::tls::fingerprint(b"unrelated approval"),
        )
        .unwrap();
    assert!(fresh.load_older(channel, None).await.is_ok());
    let wrong_files = wrong.attachment_session().await.unwrap();
    assert!(matches!(
        wrong_files.download(file, "notes with spaces.txt").await,
        Err(Error::Http(_))
    ));
    assert!(matches!(
        wrong_files
            .upload(channel, "notes.txt".into(), b"test".to_vec())
            .await,
        Err(Error::Http(_))
    ));
    task.abort();
    wrong_task.abort();
}

async fn attachment_roundtrip(world: &World, fresh: &Client, owner: &Client, channel: Id) -> Id {
    let files = fresh.attachment_session().await.unwrap();
    let contents = b"attachment integration test".to_vec();
    let file = files
        .upload(channel, "notes with spaces.txt".into(), contents.clone())
        .await
        .unwrap();
    assert_eq!(
        files.download(file, "notes with spaces.txt").await.unwrap(),
        contents
    );
    let guest = world.guest(owner).await;
    let guest_files = guest.attachment_session().await.unwrap();
    assert!(
        guest_files
            .download(file, "notes with spaces.txt")
            .await
            .is_err()
    );
    assert!(files.download(file, "wrong.txt").await.is_err());
    assert!(
        files
            .upload(Id::generate(), "no-access.txt".into(), b"test".to_vec())
            .await
            .is_err()
    );
    assert!(
        files
            .upload(channel, "empty.txt".into(), Vec::new())
            .await
            .is_err()
    );
    // Attachment-only posts are real messages, while entirely empty posts remain invalid.
    assert!(fresh.send_message(post(channel, "")).await.is_err());
    let mut attachment_post = post(channel, "");
    attachment_post.attachment_ids = vec![wire(file)];
    let attachment_message = fresh.send_message(attachment_post).await.unwrap();
    let loaded = fresh
        .load_around(channel, attachment_message)
        .await
        .unwrap();
    let sent_file = loaded
        .iter()
        .find(|m| m.id == wire(attachment_message))
        .unwrap();
    assert_eq!(sent_file.content, "");
    assert_eq!(sent_file.attachments.len(), 1);
    assert_eq!(
        guest_files
            .download(file, "notes with spaces.txt")
            .await
            .unwrap(),
        contents
    );
    file
}

#[tokio::test]
async fn invite_metadata_is_scoped_paginated_and_never_returns_codes() {
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let server_invite = owner
        .create_invite(v1::CreateInvite {
            max_uses: Some(3),
            ..Default::default()
        })
        .await
        .unwrap();
    let guest_invite = guest
        .create_invite(v1::CreateInvite::default())
        .await
        .unwrap();
    assert_invite_rest_metadata(&world).await;
    let page = guest.list_invites(None).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, guest_invite.id);
    assert_eq!(page[0].code, "");
    let all = owner.list_invites(None).await.unwrap();
    assert!(all.iter().any(|i| i.id == guest_invite.id));
    assert!(
        all.iter()
            .any(|i| i.id == server_invite.id && i.max_uses == Some(3))
    );
    assert!(all.iter().all(|i| i.code.is_empty()));
    let before = id_from_bytes(&all[1].id).unwrap();
    let older = owner.list_invites(Some(before)).await.unwrap();
    assert!(older.iter().all(|i| id_from_bytes(&i.id).unwrap() < before));
    assert_eq!(older.len(), all.len() - 2);
    assert!(
        guest
            .revoke_invite(v1::RevokeInvite {
                invite_id: server_invite.id.clone()
            })
            .await
            .is_err()
    );
    guest
        .revoke_invite(v1::RevokeInvite {
            invite_id: guest_invite.id.clone(),
        })
        .await
        .unwrap();
    assert!(
        guest.list_invites(None).await.unwrap()[0]
            .revoked_at
            .is_some()
    );
    owner
        .update_role(v1::UpdateRole {
            role_id: wire(db::EVERYONE_ROLE_ID),
            permissions: Some(
                Permissions::EVERYONE
                    .difference(Permissions::CREATE_INVITES)
                    .bits(),
            ),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(guest.list_invites(None).await.is_err());
    assert!(
        guest
            .create_invite(v1::CreateInvite::default())
            .await
            .is_err()
    );
    let (wrong_port, wrong_task) = https(&world, true).await;
    let mut address = world.address.clone();
    address.https_port = wrong_port;
    let wrong = Client::connect_with_storage(
        address,
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        wrong.list_invites(None).await,
        Err(Error::Http(_))
    ));
    task.abort();
    wrong_task.abort();
}

async fn assert_invite_rest_metadata(world: &World) {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let unauthenticated = world
        .server
        .public_routes()
        .oneshot(
            Request::builder()
                .uri("/api/v1/invites")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    let token = world
        .storage
        .session(&world.address.id().unwrap())
        .unwrap()
        .unwrap();
    for (query, expected) in [
        ("limit=1", StatusCode::OK),
        ("limit=0", StatusCode::BAD_REQUEST),
        ("limit=101", StatusCode::BAD_REQUEST),
        ("before=invalid", StatusCode::BAD_REQUEST),
    ] {
        let response = world
            .server
            .public_routes()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/invites?{query}"))
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(rows.len(), 1);
            assert!(rows[0].get("code").is_none());
            assert!(rows[0].get("codeHash").is_none());
            assert!(rows[0].get("code_hash").is_none());
        }
    }
}

#[tokio::test]
async fn audit_pages_filter_over_pinned_https_and_enforce_permissions() {
    use ss_client_core::AuditFilter;
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let all = owner
        .audit_page(None, &AuditFilter::default())
        .await
        .unwrap();
    assert!(!all.is_empty());
    let selected = &all[0];
    let filter = AuditFilter {
        actor: selected.actor_id,
        action: Some(selected.action.clone()),
        since: Some(selected.created_at),
        until: Some(selected.created_at + 1),
    };
    let rows = owner.audit_page(None, &filter).await.unwrap();
    assert!(rows.iter().any(|row| row.id == selected.id));
    assert!(rows.iter().all(|row| row.action == selected.action
        && row.actor_id == selected.actor_id
        && row.created_at == selected.created_at));
    let older = owner.audit_page(Some(rows[0].id), &filter).await.unwrap();
    assert_eq!(older.len(), rows.len() - 1);
    assert!(
        guest
            .audit_page(None, &AuditFilter::default())
            .await
            .is_err()
    );
    owner
        .update_role(v1::UpdateRole {
            role_id: wire(db::EVERYONE_ROLE_ID),
            permissions: Some((Permissions::EVERYONE | Permissions::VIEW_AUDIT_LOG).bits()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(!guest.audit_page(None, &filter).await.unwrap().is_empty());
    owner
        .update_role(v1::UpdateRole {
            role_id: wire(db::EVERYONE_ROLE_ID),
            permissions: Some(Permissions::EVERYONE.bits()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(guest.audit_page(None, &filter).await.is_err());
    assert_audit_query_validation(&world).await;
    let (wrong_port, wrong_task) = https(&world, true).await;
    let mut address = world.address.clone();
    address.https_port = wrong_port;
    let wrong = Client::connect_with_storage(
        address,
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        wrong.audit_page(None, &filter).await,
        Err(Error::Http(_))
    ));
    task.abort();
    wrong_task.abort();
}

async fn assert_audit_query_validation(world: &World) {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let token = world
        .storage
        .session(&world.address.id().unwrap())
        .unwrap()
        .unwrap();
    for (query, authenticated, expected) in [
        ("", false, StatusCode::UNAUTHORIZED),
        ("limit=1", true, StatusCode::OK),
        ("actor=invalid", true, StatusCode::BAD_REQUEST),
        ("action=", true, StatusCode::BAD_REQUEST),
        ("since=invalid", true, StatusCode::BAD_REQUEST),
        ("since=20&until=10", true, StatusCode::BAD_REQUEST),
        ("since=20&until=20", true, StatusCode::BAD_REQUEST),
        ("limit=101", true, StatusCode::BAD_REQUEST),
        ("before=invalid", true, StatusCode::BAD_REQUEST),
    ] {
        let mut request = Request::builder().uri(format!("/api/v1/audit-log?{query}"));
        if authenticated {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = world
            .server
            .public_routes()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{query}");
    }
}

#[tokio::test]
async fn webhook_management_lists_metadata_and_revokes_the_created_url() {
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let channel = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    let command = v1::CreateWebhook {
        channel_id: wire(channel),
        name: "Builds".into(),
    };
    assert!(guest.create_webhook_url(command.clone()).await.is_err());
    assert!(guest.list_webhooks(None).await.is_err());
    let created = owner.create_webhook_url(command.clone()).await.unwrap();
    let url = reqwest::Url::parse(&created.url).unwrap();
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert_eq!(url.port(), Some(port));
    assert!(
        url.path()
            .starts_with(&format!("/api/v1/webhooks/{}/", created.id))
    );
    let second = owner.create_webhook_url(command).await.unwrap();
    // The list is newest id first. Two webhooks made in the same millisecond
    // get ULIDs whose random part sorts either way, so compare the page as a
    // set and page from whichever came first.
    let rows = owner.list_webhooks(None).await.unwrap();
    assert_eq!(rows.len(), 2);
    let mut listed: Vec<_> = rows.iter().map(|row| row.id).collect();
    listed.sort_unstable();
    let mut made = vec![created.id, second.id];
    made.sort_unstable();
    assert_eq!(listed, made);
    assert!(rows[0].id > rows[1].id, "the list is not newest id first");
    for row in &rows {
        assert_eq!(row.name, "Builds");
        assert_eq!(row.channel_id, channel);
        assert!(row.revoked_at.is_none());
    }
    let older = owner.list_webhooks(Some(rows[0].id)).await.unwrap();
    assert_eq!(older.len(), 1);
    assert_eq!(older[0].id, rows[1].id);
    assert_webhook_metadata(&world, &url).await;
    assert_webhook_post(&world, url.path(), axum::http::StatusCode::OK).await;
    assert!(
        guest
            .delete_webhook(v1::DeleteWebhook {
                webhook_id: wire(created.id)
            })
            .await
            .is_err()
    );
    owner
        .delete_webhook(v1::DeleteWebhook {
            webhook_id: wire(created.id),
        })
        .await
        .unwrap();
    assert_webhook_post(&world, url.path(), axum::http::StatusCode::NOT_FOUND).await;
    let rows = owner.list_webhooks(None).await.unwrap();
    assert!(
        rows.iter()
            .find(|row| row.id == created.id)
            .unwrap()
            .revoked_at
            .is_some()
    );
    assert_webhook_permission_loss(&owner, &guest).await;
    let (wrong_port, wrong_task) = https(&world, true).await;
    let mut address = world.address.clone();
    address.https_port = wrong_port;
    let wrong = Client::connect_with_storage(
        address,
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        wrong.list_webhooks(None).await,
        Err(Error::Http(_))
    ));
    task.abort();
    wrong_task.abort();
}
async fn assert_webhook_permission_loss(owner: &Client, guest: &Client) {
    for allowed in [true, false] {
        owner
            .update_role(v1::UpdateRole {
                role_id: wire(db::EVERYONE_ROLE_ID),
                permissions: Some(
                    if allowed {
                        Permissions::EVERYONE | Permissions::MANAGE_SERVER
                    } else {
                        Permissions::EVERYONE
                    }
                    .bits(),
                ),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(guest.list_webhooks(None).await.is_ok(), allowed);
    }
}
async fn assert_webhook_post(world: &World, path: &str, expected: axum::http::StatusCode) {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    let response = world
        .server
        .public_routes()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"content":"build passed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), expected);
}
async fn assert_webhook_metadata(world: &World, url: &reqwest::Url) {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let token = world
        .storage
        .session(&world.address.id().unwrap())
        .unwrap()
        .unwrap();
    for (query, authenticated, expected) in [
        ("", false, StatusCode::UNAUTHORIZED),
        ("limit=1", true, StatusCode::OK),
        ("limit=0", true, StatusCode::BAD_REQUEST),
        ("limit=101", true, StatusCode::BAD_REQUEST),
        ("before=invalid", true, StatusCode::BAD_REQUEST),
    ] {
        let mut request = Request::builder().uri(format!("/api/v1/webhooks?{query}"));
        if authenticated {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let response = world
            .server
            .public_routes()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(rows.len(), 1);
            for field in ["token", "tokenHash", "token_hash", "url"] {
                assert!(rows[0].get(field).is_none());
            }
            let credential = url.path_segments().unwrap().next_back().unwrap();
            assert!(
                !String::from_utf8(bytes.to_vec())
                    .unwrap()
                    .contains(credential)
            );
        }
    }
}

#[tokio::test]
async fn channel_override_reads_require_visible_guild_channels_and_management() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let channel = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    assert!(guest.channel_overrides(channel).await.is_err());
    owner
        .set_channel_override(v1::SetChannelOverride {
            channel_id: wire(channel),
            role_id: wire(db::EVERYONE_ROLE_ID),
            allow: Permissions::ATTACH_FILES.bits(),
            deny: Permissions::SEND_MESSAGES.bits(),
        })
        .await
        .unwrap();
    let rows = owner.channel_overrides(channel).await.unwrap();
    let row = rows
        .iter()
        .find(|r| r.role_id == db::EVERYONE_ROLE_ID.to_string())
        .unwrap();
    assert_eq!(row.allow, Permissions::ATTACH_FILES.bits().to_string());
    assert_eq!(row.deny, Permissions::SEND_MESSAGES.bits().to_string());
    owner
        .update_role(v1::UpdateRole {
            role_id: wire(db::EVERYONE_ROLE_ID),
            permissions: Some((Permissions::EVERYONE | Permissions::MANAGE_CHANNELS).bits()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(guest.channel_overrides(channel).await.is_ok());
    owner
        .set_channel_override(v1::SetChannelOverride {
            channel_id: wire(channel),
            role_id: wire(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::VIEW_CHANNEL.bits(),
        })
        .await
        .unwrap();
    assert!(guest.channel_overrides(channel).await.is_err());
    let guest_id = id_from_bytes(&guest.state().read().own_user.as_ref().unwrap().id).unwrap();
    let dm = owner
        .create_dm(v1::CreateDm {
            user_ids: vec![wire(guest_id)],
        })
        .await
        .unwrap();
    assert!(
        owner
            .channel_overrides(id_from_bytes(&dm.id).unwrap())
            .await
            .is_err()
    );
    assert!(owner.channel_overrides(Id::generate()).await.is_err());
    let response = world
        .server
        .public_routes()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/channels/{channel}/overrides"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let (wrong_port, wrong_task) = https(&world, true).await;
    let mut address = world.address.clone();
    address.https_port = wrong_port;
    let wrong = Client::connect_with_storage(
        address,
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(
        wrong.channel_overrides(channel).await,
        Err(Error::Http(_))
    ));
    wrong.close().await;
    guest.close().await;
    owner.close().await;
    wrong_task.abort();
    task.abort();
}

fn settings_icon() -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([40, 180, 160, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}
#[tokio::test]
async fn server_settings_are_authorized_atomic_audited_and_pin_checked() {
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    assert!(guest.server_settings().await.is_err());
    assert!(guest.upload_server_icon(settings_icon()).await.is_err());
    let mut original = owner.server_settings().await.unwrap().settings;
    assert!(guest.save_server_settings(&original).await.is_err());
    original.name = "Community".into();
    original.description = "A place for friends".into();
    assert!(
        owner
            .upload_server_icon(b"<svg>not a raster</svg>".to_vec())
            .await
            .is_err()
    );
    let icon = owner.upload_server_icon(settings_icon()).await.unwrap();
    original.icon_id = Some(icon.clone());
    let saved = owner.save_server_settings(&original).await.unwrap();
    assert_eq!(saved.settings.name, "Community");
    assert_eq!(saved.settings.icon_id, Some(icon));
    assert_eq!(saved.icon_mime.as_deref(), Some("image/png"));
    assert!(
        matches!(owner.save_server_settings(&original).await,Err(Error::Protocol(message)) if message.contains("Reload"))
    );
    let mut bad = saved.settings.clone();
    bad.registration_mode = "open".into();
    assert!(owner.save_server_settings(&bad).await.is_err());
    bad = saved.settings.clone();
    bad.name = " ".into();
    assert!(owner.save_server_settings(&bad).await.is_err());
    bad = saved.settings.clone();
    bad.description = "x".repeat(2049);
    assert!(owner.save_server_settings(&bad).await.is_err());
    assert_eq!(
        owner.server_settings().await.unwrap().settings.revision,
        saved.settings.revision
    );
    let mut clear = saved.settings;
    clear.icon_id = None;
    clear.description.clear();
    let cleared = owner.save_server_settings(&clear).await.unwrap();
    assert!(cleared.settings.icon_id.is_none());
    assert_eq!(cleared.settings.description, "");
    let rows = owner
        .audit_page(
            None,
            &ss_client_core::AuditFilter {
                action: Some("server.update".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let (wrong_port, wrong_task) = https(&world, true).await;
    let mut address = world.address.clone();
    address.https_port = wrong_port;
    let wrong = Client::connect_with_storage(
        address,
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert!(matches!(wrong.server_settings().await, Err(Error::Http(_))));
    wrong.close().await;
    guest.close().await;
    owner.close().await;
    wrong_task.abort();
    task.abort();
}
#[tokio::test]
async fn closed_registration_rejects_invites_but_preserves_existing_accounts() {
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let invite = owner
        .create_invite(v1::CreateInvite::default())
        .await
        .unwrap();
    let mut profile = owner.server_settings().await.unwrap().settings;
    profile.registration_mode = "closed".into();
    let saved = owner.save_server_settings(&profile).await.unwrap();
    let rejected = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Register(v1::Register {
            invite_code: invite.code.clone(),
            username: "newcomer".into(),
            password: "test-password".into(),
        }),
        world.pins.clone(),
        Arc::new(MemoryStorage::default()),
    )
    .await;
    assert!(rejected.is_err());
    let login = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Saved,
        world.pins.clone(),
        world.storage.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        login.state().read().server.as_ref().unwrap().name,
        saved.settings.name
    );
    let mut profile = saved.settings;
    profile.registration_mode = "invite".into();
    owner.save_server_settings(&profile).await.unwrap();
    let newcomer = Client::connect_with_storage(
        world.address.clone(),
        Credentials::Register(v1::Register {
            invite_code: invite.code,
            username: "newcomer".into(),
            password: "test-password".into(),
        }),
        world.pins.clone(),
        Arc::new(MemoryStorage::default()),
    )
    .await
    .unwrap();
    newcomer.close().await;
    login.close().await;
    owner.close().await;
    task.abort();
}

#[tokio::test]
async fn moderation_admin_reads_are_scoped_and_mutations_remain_authoritative() {
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let guest_id = id_from_bytes(&guest.state().read().own_user.as_ref().unwrap().id).unwrap();
    assert!(guest.moderation_page(false, None, "").await.is_err());
    assert!(guest.moderation_page(true, None, "").await.is_err());
    moderation_kick_only_scope(&owner, &guest).await;
    let page = owner.moderation_page(false, None, "").await.unwrap();
    assert_eq!(page.members.len(), 2);
    let own = page.members.iter().find(|m| m.username == "owner").unwrap();
    assert!(!own.can_kick && !own.can_ban);
    let other = page.members.iter().find(|m| m.username == "guest").unwrap();
    assert!(other.can_kick && other.can_ban);
    assert!(other.last_session_at.is_some());
    assert!(other.roles.iter().any(|role| role == "@everyone"));
    assert_eq!(
        owner
            .moderation_page(false, None, "%")
            .await
            .unwrap()
            .members
            .len(),
        0
    );
    assert_eq!(
        owner
            .moderation_page(false, None, "GUEST")
            .await
            .unwrap()
            .members
            .len(),
        1
    );
    let serialized = serde_json::to_string(&page).unwrap();
    assert!(!serialized.contains("password") && !serialized.contains("token"));
    let expiry = ss_core::Timestamp::now().as_millis() + 3_600_000;
    owner
        .ban_user(v1::BanUser {
            user_id: wire(guest_id),
            reason: "test reason <script>".into(),
            expires_at: Some(expiry),
        })
        .await
        .unwrap();
    let bans = owner.moderation_page(true, None, "guest").await.unwrap();
    assert_eq!(bans.bans.len(), 1);
    assert_eq!(bans.bans[0].reason, "test reason <script>");
    assert_eq!(bans.bans[0].expires_at, Some(expiry.to_string()));
    assert!(bans.bans[0].can_unban);
    let banned = owner.moderation_page(false, None, "guest").await.unwrap();
    assert!(banned.members[0].banned);
    assert!(!banned.members[0].can_kick && !banned.members[0].can_ban);
    owner
        .unban_user(v1::UnbanUser {
            user_id: wire(guest_id),
        })
        .await
        .unwrap();
    assert_eq!(
        owner
            .moderation_page(true, None, "")
            .await
            .unwrap()
            .bans
            .len(),
        0
    );
    assert!(
        owner
            .unban_user(v1::UnbanUser {
                user_id: wire(guest_id)
            })
            .await
            .is_err()
    );
    guest.close().await;
    owner.close().await;
    task.abort();
}

#[tokio::test]
async fn moderation_admin_pages_are_bounded_and_expired_bans_are_omitted() {
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let owner_id = id_from_bytes(&owner.state().read().own_user.as_ref().unwrap().id).unwrap();
    {
        let mut conn = world.pool.write().await;
        for index in 0..55 {
            let user = db::users::insert(
                &mut conn,
                db::users::NewUser {
                    id: Id::generate(),
                    username: format!("paged{index:02}"),
                    display_name: "Paged member".into(),
                    password_hash: "not exposed".into(),
                    kind: ss_core::UserKind::User,
                },
            )
            .await
            .unwrap();
            db::bans::insert(
                &mut conn,
                db::bans::NewBan {
                    id: Id::generate(),
                    user_id: user.id,
                    banned_by: owner_id,
                    reason: None,
                    expires_at: None,
                },
            )
            .await
            .unwrap();
        }
    }
    for bans in [false, true] {
        let first = owner.moderation_page(bans, None, "paged").await.unwrap();
        let cursor = first.next_cursor.as_ref().unwrap().parse().unwrap();
        let second = owner
            .moderation_page(bans, Some(cursor), "paged")
            .await
            .unwrap();
        assert!(second.next_cursor.is_none());
        if bans {
            assert_eq!(first.bans.len(), 50);
            assert_eq!(second.bans.len(), 5);
            assert!(
                first
                    .bans
                    .iter()
                    .all(|a| second.bans.iter().all(|b| a.id != b.id))
            );
        } else {
            assert_eq!(first.members.len(), 50);
            assert_eq!(second.members.len(), 5);
            assert!(
                first
                    .members
                    .iter()
                    .all(|a| second.members.iter().all(|b| a.id != b.id))
            );
        }
    }
    {
        let mut conn = world.pool.write().await;
        sqlx::query("UPDATE bans SET expires_at = 1")
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    assert_eq!(
        owner
            .moderation_page(true, None, "")
            .await
            .unwrap()
            .bans
            .len(),
        0
    );
    assert!(
        !owner
            .moderation_page(false, None, "paged")
            .await
            .unwrap()
            .members[0]
            .banned
    );
    assert!(
        owner
            .moderation_page(false, None, &"a".repeat(101))
            .await
            .is_err()
    );
    owner.close().await;
    task.abort();
}

async fn moderation_kick_only_scope(owner: &Client, guest: &Client) {
    owner
        .update_role(v1::UpdateRole {
            role_id: wire(db::EVERYONE_ROLE_ID),
            permissions: Some((Permissions::EVERYONE | Permissions::KICK_MEMBERS).bits()),
            ..Default::default()
        })
        .await
        .unwrap();
    let limited = guest.moderation_page(false, None, "").await.unwrap();
    assert!(!limited.can_ban);
    assert!(limited.members.iter().all(|m| !m.can_ban && !m.can_kick));
    assert!(guest.moderation_page(true, None, "").await.is_err());
    let owner_id = id_from_bytes(&owner.state().read().own_user.as_ref().unwrap().id).unwrap();
    assert!(
        guest
            .kick_user(v1::KickUser {
                user_id: wire(owner_id),
                reason: String::new()
            })
            .await
            .is_err()
    );
    owner
        .update_role(v1::UpdateRole {
            role_id: wire(db::EVERYONE_ROLE_ID),
            permissions: Some(Permissions::EVERYONE.bits()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(guest.moderation_page(false, None, "").await.is_err());
}

// Keep the authenticated visibility transitions in one real connection scenario.
#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn search_https_scopes_permissions_and_private_messages() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use ss_client_core::search::SearchRequest;
    use tower::ServiceExt;
    let mut world = World::new().await;
    let (port, task) = https(&world, false).await;
    world.address.https_port = port;
    let owner = world.owner().await;
    let guest = world.guest(&owner).await;
    let mut events = guest.subscribe();
    let channel = named(&owner.state().read(), "general", v1::ChannelKind::Text);
    let message = owner
        .send_message(post(channel, "launch searchable café"))
        .await
        .unwrap();
    let query = SearchRequest {
        q: "launch cafe".into(),
        ..Default::default()
    };
    let page = guest.search_messages(&query).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, message.to_string());
    let detached = guest.search_session().await.unwrap();
    owner
        .set_channel_override(v1::SetChannelOverride {
            channel_id: wire(channel),
            role_id: wire(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::VIEW_CHANNEL.bits(),
        })
        .await
        .unwrap();
    until(&guest, &mut events, |s| !s.channels.contains_key(&channel)).await;
    assert!(detached.search_messages(&query).await.unwrap().is_empty());
    assert_eq!(owner.search_messages(&query).await.unwrap().len(), 1);
    let guest_id = id_from_bytes(&guest.state().read().own_user.as_ref().unwrap().id).unwrap();
    let dm = owner
        .create_dm(v1::CreateDm {
            user_ids: vec![wire(guest_id)],
        })
        .await
        .unwrap();
    let dm = id_from_bytes(&dm.id).unwrap();
    until(&guest, &mut events, |s| s.direct_messages.contains_key(&dm)).await;
    let _ = owner
        .send_message(post(dm, "launch private café"))
        .await
        .unwrap();
    let mut dm_query = query.clone();
    dm_query.channel = Some(dm.to_string());
    assert_eq!(guest.search_messages(&dm_query).await.unwrap().len(), 1);
    let invite = owner
        .create_invite(v1::CreateInvite {
            max_uses: Some(1),
            expires_at: None,
        })
        .await
        .unwrap();
    let third = Client::connect(
        world.address.clone(),
        Credentials::Register(v1::Register {
            invite_code: invite.code,
            username: "third".into(),
            password: "test-password".into(),
        }),
        world.pins.clone(),
    )
    .await
    .unwrap();
    let third_id = id_from_bytes(&third.state().read().own_user.as_ref().unwrap().id).unwrap();
    let self_dm = guest
        .create_dm(v1::CreateDm {
            user_ids: vec![wire(third_id)],
        })
        .await
        .unwrap();
    let self_dm = id_from_bytes(&self_dm.id).unwrap();
    let _ = guest
        .send_message(post(self_dm, "launch secret café"))
        .await
        .unwrap();
    dm_query.channel = Some(self_dm.to_string());
    assert!(owner.search_messages(&dm_query).await.unwrap().is_empty());
    assert!(
        guest
            .search_messages(&SearchRequest {
                q: "\"***".into(),
                ..Default::default()
            })
            .await
            .is_err()
    );
    let response = world
        .server
        .public_routes()
        .oneshot(
            Request::builder()
                .uri("/api/v1/search?q=launch")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    task.abort();
}
