use bytes::{Bytes, BytesMut};
use ss_core::Id;
use ss_net::{
    Connection, client,
    control::ClientStream,
    media::DatagramHeader,
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::{
    v1::{self, client_envelope::Msg, server_envelope::Msg as Event},
    validate::{id_from_bytes, id_to_bytes},
};
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::{task::JoinHandle, time::timeout};

const DEADLINE: Duration = Duration::from_secs(5);

struct Fixture {
    endpoint: Endpoint,
    port: u16,
    server: Arc<ss_server::Server>,
    task: JoinHandle<()>,
    pins: Arc<MemoryPinStore>,
    _data: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let identity = tls::generate_self_signed(data.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(
            &ServerId::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap(),
            fingerprint,
        )
        .unwrap();
        let server = Arc::new(ss_server::Server::default());
        let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
        Self {
            port: endpoint.local_addr().unwrap().port(),
            endpoint,
            server,
            task,
            pins,
            _data: data,
        }
    }

    async fn raw(&self) -> (Connection, ClientStream) {
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

    async fn peer(&self, name: &str) -> Peer {
        let (connection, stream) = self.raw().await;
        let mut peer = Peer {
            connection,
            stream,
            request: 0,
            user: Bytes::new(),
            conn: Id::nil(),
            track: 0,
            tracks: HashSet::new(),
        };
        let response = peer.command(Msg::Hello(hello(name))).await;
        let Event::Welcome(welcome) = response.msg.unwrap() else {
            panic!("expected Welcome")
        };
        assert_eq!(welcome.snapshot, Some(v1::Snapshot::default()));
        peer.user = welcome.user.unwrap().id;
        peer
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

struct Peer {
    connection: Connection,
    stream: ClientStream,
    request: u32,
    user: Bytes,
    conn: Id,
    track: u32,
    tracks: HashSet<u32>,
}
impl Peer {
    async fn command(&mut self, msg: Msg) -> v1::ServerEnvelope {
        self.request += 1;
        self.stream
            .send(&v1::ClientEnvelope {
                request_id: self.request,
                msg: Some(msg),
            })
            .await
            .unwrap();
        loop {
            let response = timeout(DEADLINE, self.stream.recv())
                .await
                .unwrap()
                .unwrap();
            match &response.msg {
                Some(Event::TrackAdded(track)) => {
                    self.tracks.insert(track.track_id);
                    if track.user_id == self.user {
                        self.track = track.track_id;
                        self.conn = id_from_bytes(&track.conn_id).unwrap();
                    }
                }
                Some(Event::TrackRemoved(track)) => {
                    self.tracks.remove(&track.track_id);
                }
                _ => {}
            }
            if response.request_id == self.request {
                return response;
            }
            assert_eq!(response.request_id, 0);
        }
    }
    async fn join(&mut self, channel: Id) {
        assert_ack(
            &self
                .command(Msg::JoinVoice(v1::JoinVoice {
                    channel_id: id_to_bytes(channel),
                    ..Default::default()
                }))
                .await,
        );
        assert_ne!(self.track, 0);
    }
    async fn receive(&self) -> Bytes {
        timeout(DEADLINE, self.connection.read_datagram())
            .await
            .unwrap()
            .unwrap()
    }
    async fn silent(&self) {
        assert!(
            timeout(Duration::from_millis(100), self.connection.read_datagram())
                .await
                .is_err()
        );
    }
}

fn hello(name: &str) -> v1::Hello {
    v1::Hello {
        protocol_version: 1,
        auth: Some(v1::hello::Auth::Login(v1::Login {
            username: name.into(),
            password: String::new(),
        })),
        ..Default::default()
    }
}
fn assert_ack(envelope: &v1::ServerEnvelope) {
    assert!(matches!(envelope.msg, Some(Event::Ack(_))), "{envelope:?}");
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

#[tokio::test]
async fn routes_only_owned_tracks_to_other_members_and_cleans_up() {
    let fixture = Fixture::new().await;
    let channel = Id::generate();
    let other = Id::generate();
    let mut a = fixture.peer("a").await;
    let mut b = fixture.peer("b").await;
    let mut c = fixture.peer("c").await;
    let mut d = fixture.peer("d").await;
    a.join(channel).await;
    b.join(channel).await;
    c.join(channel).await;
    d.join(other).await;
    assert_eq!(a.tracks, HashSet::from([a.track]));
    assert_eq!(b.tracks, HashSet::from([a.track, b.track]));
    assert_eq!(c.tracks, HashSet::from([a.track, b.track, c.track]));
    assert_eq!(d.tracks, HashSet::from([d.track]));
    let data = packet(a.track);
    a.connection.send_datagram(data.clone()).unwrap();
    assert_eq!(b.receive().await, data);
    assert_eq!(c.receive().await, data);
    a.silent().await;
    d.silent().await;
    // A forged track, malformed header, and non-member sender must all fail.
    d.connection.send_datagram(data.clone()).unwrap();
    a.connection.send_datagram(packet(b.track)).unwrap();
    a.connection
        .send_datagram(Bytes::from_static(b"bad"))
        .unwrap();
    b.silent().await;
    c.silent().await;
    assert_eq!(fixture.server.router().stats().rejected, 3);

    let old_track = a.track;
    a.join(other).await;
    a.connection.send_datagram(packet(old_track)).unwrap();
    a.connection.send_datagram(packet(a.track)).unwrap();
    assert_eq!(d.receive().await, packet(a.track));
    b.silent().await;
    c.silent().await;
    // Leaving invalidates the assignment. Repeated leave is harmless.
    assert_ack(&a.command(Msg::LeaveVoice(v1::LeaveVoice {})).await);
    assert_ack(&a.command(Msg::LeaveVoice(v1::LeaveVoice {})).await);
    a.connection.send_datagram(packet(a.track)).unwrap();
    d.silent().await;
    // Explicit disconnect emits removal and channel=None to the remaining peer.
    b.connection.close(0, b"bye");
    let mut removed = false;
    let mut left = false;
    timeout(DEADLINE, async {
        while !removed || !left {
            match c.stream.recv().await.unwrap().msg.unwrap() {
                Event::TrackRemoved(event) if event.track_id == b.track => removed = true,
                Event::VoiceStateUpdated(event)
                    if event.conn_id == id_to_bytes(b.conn) && event.channel_id.is_none() =>
                {
                    left = true;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.server.registry().connection_count(), 3);
}

#[tokio::test]
async fn validates_hello_ids_and_supports_ping_and_flags() {
    let fixture = Fixture::new().await;
    let (connection, mut stream) = fixture.raw().await;
    let mut unsupported = hello("a");
    unsupported.protocol_version = 999;
    stream
        .send(&v1::ClientEnvelope {
            request_id: 1,
            msg: Some(Msg::Hello(unsupported)),
        })
        .await
        .unwrap();
    let response = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
    let Some(Event::Error(error)) = response.msg else {
        panic!("expected version error")
    };
    assert_eq!(error.code, i32::from(v1::ErrorCode::ProtocolVersion));
    assert_eq!(error.min_version, Some(1));
    connection.close(0, b"bye");
    let mut a = fixture.peer("any username!").await;
    let mut b = fixture.peer("any username!").await;
    let invalid = a
        .command(Msg::JoinVoice(v1::JoinVoice {
            channel_id: Bytes::from_static(b"short"),
            ..Default::default()
        }))
        .await;
    assert!(
        matches!(invalid.msg, Some(Event::Error(error)) if error.code == i32::from(v1::ErrorCode::Invalid))
    );
    let channel = Id::generate();
    a.join(channel).await;
    b.join(channel).await;
    let track = a.track;
    a.join(channel).await;
    assert_eq!(a.track, track);
    assert_ack(
        &a.command(Msg::SetVoiceState(v1::SetVoiceState {
            self_mute: true,
            self_deaf: false,
        }))
        .await,
    );
    a.connection.send_datagram(packet(a.track)).unwrap();
    b.silent().await;
    assert_ack(
        &a.command(Msg::SetVoiceState(v1::SetVoiceState::default()))
            .await,
    );
    assert_ack(
        &b.command(Msg::SetVoiceState(v1::SetVoiceState {
            self_mute: false,
            self_deaf: true,
        }))
        .await,
    );
    a.connection.send_datagram(packet(a.track)).unwrap();
    b.silent().await;
    assert_ack(
        &b.command(Msg::SetVoiceState(v1::SetVoiceState::default()))
            .await,
    );
    a.connection.send_datagram(packet(a.track)).unwrap();
    assert_eq!(b.receive().await, packet(a.track));
    let pong = a.command(Msg::Ping(v1::Ping { sent_at: 42 })).await;
    assert!(
        matches!(pong.msg, Some(Event::Pong(pong)) if pong.sent_at == 42 && pong.server_time > 0)
    );
    a.stream
        .send(&v1::ClientEnvelope {
            request_id: a.request,
            msg: Some(Msg::LeaveVoice(v1::LeaveVoice {})),
        })
        .await
        .unwrap();
    timeout(DEADLINE, a.connection.quic().closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn forwarding_reuses_payload_and_counts_send_failures() {
    let fixture = Fixture::new().await;
    let mut a = fixture.peer("a").await;
    let mut b = fixture.peer("b").await;
    let channel = Id::generate();
    a.join(channel).await;
    b.join(channel).await;
    let data = packet(a.track);
    // Warm Quinn's queue, then measure forwarding a freshly received Bytes
    // buffer. This also checks the transport's payload-sharing representation.
    fixture.server.router().forward(a.conn, &data);
    let received = b.receive().await;
    assert_eq!(received, data);
    let allocations =
        allocation_counter::measure(|| fixture.server.router().forward(a.conn, &received));
    assert_eq!(allocations.count_total, 0, "{allocations:?}");
    assert_eq!(b.receive().await, data);
    // Current-thread runtime stays paused while routing: eventually the bounded
    // Quinn send queue fills, and every additional drop is observable.
    let before = fixture.server.router().stats();
    for _ in 0..30_000 {
        fixture.server.router().forward(a.conn, &data);
    }
    let after = fixture.server.router().stats();
    assert!(after.dropped > before.dropped);
    assert_eq!(
        after.forwarded - before.forwarded + after.dropped - before.dropped,
        30_000
    );
}

/// Release-only manual load check. Use an external process CPU timer; this
/// process includes the server AND all 20 clients, so CPU is an upper bound.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual 20-client, 50-pps sustained load check"]
async fn sustained_twenty_clients() {
    let mut fixture = Fixture::new().await;
    // Optional separate server process for measuring server-only CPU. Trust
    // remains explicit: both the loopback port and its pin must be supplied.
    let external = if let Ok(port) = std::env::var("SS_LOAD_PORT") {
        fixture.port = port.parse().unwrap();
        let fingerprint =
            tls::Fingerprint::try_from(std::env::var("SS_LOAD_FINGERPRINT").unwrap()).unwrap();
        fixture
            .pins
            .set(
                &ServerId::new("127.0.0.1", fixture.port).unwrap(),
                fingerprint,
            )
            .unwrap();
        true
    } else {
        false
    };
    let channel = Id::generate();
    let mut peers = Vec::new();
    for i in 0..20 {
        let mut peer = fixture.peer(&format!("peer-{i}")).await;
        peer.join(channel).await;
        peers.push(peer);
    }
    let mut receivers = Vec::new();
    for peer in &peers {
        let connection = peer.connection.quic().clone();
        receivers.push(tokio::spawn(async move {
            let mut count = 0_u64;
            while connection.read_datagram().await.is_ok() {
                count += 1;
            }
            count
        }));
    }
    let packets: Vec<_> = peers.iter().map(|peer| packet(peer.track)).collect();
    let mut interval = tokio::time::interval(Duration::from_millis(20));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    for _ in 0..1500 {
        interval.tick().await;
        for (peer, packet) in peers.iter().zip(&packets) {
            peer.connection.send_datagram(packet.clone()).unwrap();
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let stats = fixture.server.router().stats();
    for peer in &peers {
        peer.connection.close(0, b"load complete");
    }
    let mut received = 0;
    for receiver in receivers {
        received += receiver.await.unwrap();
    }
    eprintln!("20 clients x 50 pps, 200-byte Opus payload, 30 seconds: received={received}");
    if !external {
        eprintln!("server: {stats:?}");
        assert_eq!(stats.received, 30_000);
        assert_eq!(stats.forwarded, 570_000);
        assert_eq!(stats.rejected, 0);
        assert_eq!(stats.dropped, 0);
    }
    assert!(received >= 564_300, "more than 1% end-to-end loss");
}

#[tokio::test]
async fn hello_must_be_first_and_a_stalled_peer_does_not_block_accepts() {
    let mut fixture = Fixture::new().await;
    let (_stalled, _stream) = fixture.raw().await;
    for request in [
        v1::ClientEnvelope {
            request_id: 1,
            msg: Some(Msg::Ping(v1::Ping::default())),
        },
        v1::ClientEnvelope {
            request_id: 0,
            msg: Some(Msg::Hello(hello("zero"))),
        },
        v1::ClientEnvelope {
            request_id: 1,
            msg: Some(Msg::Hello(v1::Hello {
                protocol_version: 1,
                ..Default::default()
            })),
        },
    ] {
        let (connection, mut stream) = fixture.raw().await;
        stream.send(&request).await.unwrap();
        let response = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
        assert_eq!(response.request_id, request.request_id);
        assert!(
            matches!(response.msg, Some(Event::Error(error)) if error.code == i32::from(v1::ErrorCode::Invalid))
        );
        connection.close(0, b"rejected");
    }
    let mut peer = fixture.peer("valid").await;
    peer.join(Id::generate()).await;
    fixture.endpoint.close(0, b"shutdown");
    timeout(DEADLINE, &mut fixture.task).await.unwrap().unwrap();
    assert_eq!(fixture.server.registry().connection_count(), 0);
}

#[tokio::test]
async fn overflowing_control_events_closes_the_peer() {
    let fixture = Fixture::new().await;
    let mut peer = fixture.peer("slow").await;
    peer.join(Id::generate()).await;
    // Pause the writer task on this current-thread runtime and exceed its
    // control queue. Missing even one track/state event must close the peer.
    for index in 0..(2 * ss_server::MAX_CONNECTIONS + 20) {
        fixture
            .server
            .registry()
            .set_flags(peer.conn, index % 2 == 0, false);
    }
    timeout(DEADLINE, peer.connection.quic().closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn large_channel_bootstrap_does_not_close_a_healthy_peer() {
    let fixture = Fixture::new().await;
    let channel = Id::generate();
    let mut peers = Vec::new();
    for index in 0..ss_server::MAX_CONNECTIONS {
        let mut peer = fixture.peer(&format!("member-{index}")).await;
        peer.join(channel).await;
        assert_eq!(peer.tracks.len(), index + 1);
        peers.push(peer);
    }
    assert_eq!(fixture.server.registry().connection_count(), peers.len());
}

#[tokio::test]
async fn oversized_welcome_is_rejected_before_registering_a_session() {
    let fixture = Fixture::new().await;
    let (connection, mut stream) = fixture.raw().await;
    stream
        .send(&v1::ClientEnvelope {
            request_id: 1,
            msg: Some(Msg::Hello(hello(&"x".repeat(600_000)))),
        })
        .await
        .unwrap();
    let response = timeout(DEADLINE, stream.recv()).await.unwrap().unwrap();
    assert_eq!(response.request_id, 1);
    assert!(
        matches!(response.msg, Some(Event::Error(error)) if error.code == i32::from(v1::ErrorCode::Invalid))
    );
    assert_eq!(fixture.server.registry().connection_count(), 0);
    connection.close(0, b"rejected");
}
