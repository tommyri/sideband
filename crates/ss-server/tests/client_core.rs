//! Real QUIC client/server integration; no audio devices or external network.
use ss_client_core::{
    AudioConfig, AudioSource, Client, ClientEvent, ConnectionState, Credentials, Error, ServerAddr,
    State, TalkGate, VoiceSession,
    storage::{MemoryStorage, Storage},
    v1,
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
        self.endpoint.close(0, b"forced restart");
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
    assert!(
        client.state().read().messages[&general]
            .ordered()
            .iter()
            .any(|message| message.content == "missed while offline")
    );
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
        getrandom::getrandom(&mut random).unwrap();
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
                    let (socket, _) = accepted.unwrap(); let acceptor = acceptor.clone(); let app = app.clone();
                    peers.spawn(async move {
                        let Ok(mut stream) = acceptor.accept(socket).await else { return; };
                        let mut header = Vec::new(); let mut byte = [0];
                        while header.len() < 16384 && !header.ends_with(b"\r\n\r\n") {
                            if stream.read_exact(&mut byte).await.is_err() { return; } header.push(byte[0]);
                        }
                        let text = String::from_utf8(header).unwrap(); let mut lines = text.split("\r\n");
                        let path = lines.next().unwrap().split_whitespace().nth(1).unwrap();
                        let mut request = axum::http::Request::builder().uri(path);
                        for line in lines { if let Some((key, value)) = line.split_once(':') { request = request.header(key, value.trim()); } }
                        let response = app.oneshot(request.body(axum::body::Body::empty()).unwrap()).await.unwrap();
                        let status = response.status(); let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024).await.unwrap();
                        let header = format!("HTTP/1.1 {} OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n", status.as_u16(), body.len());
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
    task.abort();
    wrong_task.abort();
}
