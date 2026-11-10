//! Real QUIC against a scripted server: audio keeps flowing while the control
//! stream is slow, and a reconnect resumes audio before the message resync.
use super::*;
use ss_net::{
    pins::{MemoryPinStore, PinStore},
    server::Endpoint,
    tls,
};
use std::{path::PathBuf, time::Instant as StdInstant};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

const FRAME: Duration = Duration::from_millis(20);
const WAIT: Duration = Duration::from_secs(8);

/// One accepted, welcomed connection as the scripted server sees it.
struct Session {
    hello: v1::Hello,
    connection: ss_net::Connection,
    events: UnboundedSender<v1::ServerEnvelope>,
}

/// QUIC server that answers Hello with a fixed snapshot and acknowledges
/// every command after `ack_delay`. Datagrams are left to the test.
struct Peer {
    endpoint: Endpoint,
    address: ServerAddr,
    pins: Arc<MemoryPinStore>,
    channels: Vec<ss_core::Id>,
    sessions: UnboundedReceiver<Session>,
    task: JoinHandle<()>,
    dir: PathBuf,
}

impl Peer {
    async fn new(ack_delay: Duration, text_channels: usize) -> Self {
        let dir = std::env::temp_dir().join(format!("ss-client-core-{}", ss_core::Id::generate()));
        let identity = tls::generate_self_signed(&dir).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let address = ServerAddr::new("127.0.0.1", endpoint.local_addr().unwrap().port()).unwrap();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(&address.id().unwrap(), fingerprint).unwrap();
        let channels: Vec<_> = (0..text_channels)
            .map(|_| ss_core::Id::generate())
            .collect();
        let snapshot = v1::Snapshot {
            channels: channels
                .iter()
                .enumerate()
                .map(|(position, id)| v1::Channel {
                    id: id.as_bytes().to_vec().into(),
                    kind: v1::ChannelKind::Text.into(),
                    name: format!("text-{position}"),
                    position: i32::try_from(position).unwrap(),
                    effective_permissions: Some(ss_core::Permissions::EVERYONE.bits()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let (found, sessions) = unbounded_channel();
        let task = tokio::spawn(serve(endpoint.clone(), ack_delay, snapshot, found));
        Self {
            endpoint,
            address,
            pins,
            channels,
            sessions,
            task,
            dir,
        }
    }

    async fn client(&self) -> Client {
        Client::connect(
            self.address.clone(),
            Credentials::Session("scripted-session".into()),
            self.pins.clone(),
        )
        .await
        .unwrap()
    }

    async fn session(&mut self) -> Session {
        timeout(WAIT, self.sessions.recv())
            .await
            .expect("client did not connect")
            .unwrap()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test done");
        self.task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn serve(
    endpoint: Endpoint,
    ack_delay: Duration,
    snapshot: v1::Snapshot,
    found: UnboundedSender<Session>,
) {
    let user = ss_core::Id::generate();
    while let Some(incoming) = endpoint.accept().await {
        let Ok(mut connection) = incoming.accept().await else {
            continue;
        };
        let Ok(stream) = connection.accept_control().await else {
            continue;
        };
        let (mut sender, mut receiver) = stream.split();
        let Ok(first) = receiver.recv().await else {
            continue;
        };
        let Some(Msg::Hello(hello)) = first.msg else {
            continue;
        };
        let (events, mut outbound) = unbounded_channel::<v1::ServerEnvelope>();
        let _ = events.send(v1::ServerEnvelope {
            request_id: first.request_id,
            msg: Some(Event::Welcome(v1::Welcome {
                user: Some(v1::User {
                    id: user.as_bytes().to_vec().into(),
                    username: "scripted".into(),
                    display_name: "Scripted".into(),
                    ..Default::default()
                }),
                server_info: None,
                resume_token: "scripted-resume".into(),
                snapshot: Some(snapshot.clone()),
            })),
        });
        tokio::spawn(async move {
            while let Some(envelope) = outbound.recv().await {
                if sender.send(&envelope).await.is_err() {
                    break;
                }
            }
        });
        let acks = events.clone();
        tokio::spawn(async move {
            while let Ok(envelope) = receiver.recv().await {
                if matches!(envelope.msg, Some(Msg::Ping(_))) {
                    continue;
                }
                let acks = acks.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(ack_delay).await;
                    let _ = acks.send(v1::ServerEnvelope::ack(
                        envelope.request_id,
                        v1::ack::Result::Empty(v1::Empty {}),
                    ));
                });
            }
        });
        if found
            .send(Session {
                hello,
                connection,
                events,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Per-packet delay in each direction, measured against the send instant.
struct Exchange {
    upstream: Vec<Duration>,
    downstream: Vec<Duration>,
}

impl Exchange {
    /// (95th percentile, maximum) one-way delay.
    fn spread(delays: &[Duration]) -> (Duration, Duration) {
        let mut sorted = delays.to_vec();
        sorted.sort();
        let at = |q: usize| sorted.get(sorted.len() * q / 100).copied();
        (
            at(95).unwrap_or(Duration::MAX),
            sorted.last().copied().unwrap_or(Duration::MAX),
        )
    }
}

/// Sends one datagram per frame interval in both directions for `frames`
/// frames and runs `during` from the tenth frame on. Downstream delay uses the
/// arrival stamp the voice worker sees, not the time the test read it.
async fn exchange<T>(
    client: &Client,
    session: &Session,
    frames: u32,
    during: impl std::future::Future<Output = T>,
) -> (Exchange, T) {
    // Receivers stop at this deadline: a datagram may be lost even on loopback.
    let end = Instant::now() + FRAME * frames + Duration::from_secs(2);
    let up_sent = Arc::new(parking_lot::Mutex::new(BTreeMap::<u32, StdInstant>::new()));
    let down_sent = Arc::new(parking_lot::Mutex::new(BTreeMap::<u32, StdInstant>::new()));
    let mut arrivals = client.media.subscribe();
    let server = session.connection.quic().clone();

    let upstream = {
        let sent = up_sent.clone();
        let server = server.clone();
        tokio::spawn(async move {
            let mut delays = Vec::new();
            while delays.len() < frames as usize {
                let Ok(Ok(packet)) = tokio::time::timeout_at(end, server.read_datagram()).await
                else {
                    break;
                };
                let now = StdInstant::now();
                let seq = u32::from_be_bytes(packet[..4].try_into().unwrap());
                if let Some(at) = sent.lock().get(&seq) {
                    delays.push(now.duration_since(*at));
                }
            }
            delays
        })
    };
    let downstream = {
        let sent = down_sent.clone();
        tokio::spawn(async move {
            let mut delays = Vec::new();
            while delays.len() < frames as usize {
                let Ok(Ok(packet)) = tokio::time::timeout_at(end, arrivals.recv()).await else {
                    break;
                };
                let seq = u32::from_be_bytes(packet.bytes[..4].try_into().unwrap());
                if let Some(at) = sent.lock().get(&seq) {
                    delays.push(packet.at.saturating_duration_since(*at));
                }
            }
            delays
        })
    };
    let media = client.media.clone();
    let senders = tokio::spawn(async move {
        let mut tick = tokio::time::interval(FRAME);
        for seq in 0..frames {
            tick.tick().await;
            let packet = Bytes::copy_from_slice(&seq.to_be_bytes());
            let now = StdInstant::now();
            up_sent.lock().insert(seq, now);
            down_sent.lock().insert(seq, now);
            media.send(packet.clone());
            let _ = server.send_datagram(packet);
        }
    });
    tokio::time::sleep(FRAME * 10).await;
    let output = during.await;
    senders.await.unwrap();
    let upstream = timeout(WAIT, upstream).await.unwrap().unwrap();
    let downstream = timeout(WAIT, downstream).await.unwrap().unwrap();
    (
        Exchange {
            upstream,
            downstream,
        },
        output,
    )
}

/// Datagrams normally cross loopback within one frame interval. The maximum
/// allows scheduler noise from tests running in parallel in a debug build;
/// a paused pump holds 25 frames back by up to the 500 ms Ack delay. Loopback
/// UDP can still drop a datagram under load, so two may go missing.
fn assert_no_gap(exchange: &Exchange, frames: u32) {
    for (direction, delays) in [
        ("upstream", &exchange.upstream),
        ("downstream", &exchange.downstream),
    ] {
        assert!(
            delays.len() + 2 >= frames as usize,
            "{direction} lost {} of {frames} datagrams",
            frames as usize - delays.len()
        );
        let (p95, max) = Exchange::spread(delays);
        assert!(
            p95 < FRAME && max < FRAME * 3,
            "{direction} datagrams were held back: p95 {p95:?}, max {max:?}"
        );
    }
}

async fn until_connected(client: &Client, events: &mut broadcast::Receiver<ClientEvent>) {
    timeout(WAIT, async {
        while client.state().read().connection != ConnectionState::Connected {
            let _ = events.recv().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("state: {:?}", client.state().read().connection));
}

async fn reconnecting_within(
    events: &mut broadcast::Receiver<ClientEvent>,
    window: Duration,
) -> bool {
    timeout(window, async {
        loop {
            match events.recv().await {
                Ok(ClientEvent::Connection(ConnectionState::Reconnecting { .. })) => break,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => std::future::pending().await,
            }
        }
    })
    .await
    .is_ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_ack_does_not_pause_datagrams_in_either_direction() {
    let mut peer = Peer::new(Duration::from_millis(500), 1).await;
    let client = peer.client().await;
    let session = peer.session().await;
    let channel = peer.channels[0];
    let frames = 60;
    let (exchange, waited) = exchange(&client, &session, frames, async {
        let started = StdInstant::now();
        client
            .command(Msg::SendMessage(v1::SendMessage {
                channel_id: channel.as_bytes().to_vec().into(),
                content: "while talking".into(),
                ..Default::default()
            }))
            .await
            .unwrap();
        started.elapsed()
    })
    .await;
    assert!(waited >= Duration::from_millis(450), "Ack was not delayed");
    assert_no_gap(&exchange, frames);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_ack_does_not_reconnect_a_healthy_connection() {
    let mut peer = Peer::new(Duration::from_secs(4), 1).await;
    let client = peer.client().await;
    let _session = peer.session().await;
    let mut events = client.subscribe();
    client
        .command(Msg::Typing(v1::Typing {
            channel_id: peer.channels[0].as_bytes().to_vec().into(),
        }))
        .await
        .unwrap();
    assert!(
        !reconnecting_within(&mut events, Duration::from_millis(1500)).await,
        "a 4 s Ack made the heartbeat reconnect"
    );
    assert_eq!(client.state().read().connection, ConnectionState::Connected);
    assert!(peer.sessions.try_recv().is_err(), "client reconnected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_resumes_audio_before_a_slow_resync_and_drains_events() {
    // History requests are accepted and never answered.
    let history = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let history_port = history.local_addr().unwrap().port();
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let stalled = {
        let attempts = attempts.clone();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = history.accept().await {
                attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                held.push(socket);
            }
        })
    };
    let mut peer = Peer::new(Duration::ZERO, 10).await;
    peer.address.https_port = history_port;
    let client = peer.client().await;
    let first = peer.session().await;
    let mut events = client.subscribe();
    for channel in &peer.channels {
        let _ = first.events.send(v1::ServerEnvelope {
            request_id: 0,
            msg: Some(Event::MessageCreated(v1::MessageCreated {
                message: Some(v1::Message {
                    id: ss_core::Id::generate().as_bytes().to_vec().into(),
                    channel_id: channel.as_bytes().to_vec().into(),
                    content: "cached".into(),
                    ..Default::default()
                }),
                nonce: None,
            })),
        });
    }
    timeout(WAIT, async {
        while client.state().message_cursors().len() < 10 {
            let _ = events.recv().await;
        }
    })
    .await
    .expect("messages were not cached");

    first.connection.close(0, b"scripted restart");
    let second = peer.session().await;
    assert!(matches!(
        second.hello.auth,
        Some(v1::hello::Auth::ResumeToken(_))
    ));
    // More events than the control router buffers (256), trickling in over
    // about a second while the resync is stalled. A single burst of that size
    // can outrun any reader; that is the router's own limit.
    let flood = {
        let events = second.events.clone();
        let channel = peer.channels[0].as_bytes().to_vec();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_micros(2500));
            for _ in 0..400 {
                tick.tick().await;
                let _ = events.send(v1::ServerEnvelope {
                    request_id: 0,
                    msg: Some(Event::TypingStarted(v1::TypingStarted {
                        channel_id: channel.clone().into(),
                        user_id: ss_core::Id::generate().as_bytes().to_vec().into(),
                    })),
                });
            }
        })
    };
    until_connected(&client, &mut events).await;
    let frames = 30;
    let (exchange, ()) = exchange(&client, &second, frames, async {}).await;
    flood.await.unwrap();
    assert_no_gap(&exchange, frames);
    assert!(
        attempts.load(std::sync::atomic::Ordering::SeqCst) >= 1,
        "resync did not start"
    );
    assert!(
        second.connection.quic().close_reason().is_none(),
        "connection closed: {:?}",
        second.connection.quic().close_reason()
    );
    assert!(
        peer.sessions.try_recv().is_err(),
        "client reconnected again"
    );
    assert_eq!(client.state().read().connection, ConnectionState::Connected);
    client.close().await;
    stalled.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wall_clock_jump_reconnects_with_the_resume_token() {
    let mut peer = Peer::new(Duration::ZERO, 1).await;
    let client = peer.client().await;
    let _first = peer.session().await;
    let mut events = client.subscribe();
    // Let the heartbeat take its baseline sample first.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // A machine that slept: `Instant` saw a second, the wall clock a minute.
    client
        .clock
        .wall_skew
        .store(60, std::sync::atomic::Ordering::Relaxed);
    assert!(
        reconnecting_within(&mut events, Duration::from_secs(3)).await,
        "the heartbeat missed the wall-clock jump"
    );
    let second = peer.session().await;
    assert!(matches!(
        second.hello.auth,
        Some(v1::hello::Auth::ResumeToken(_))
    ));
    until_connected(&client, &mut events).await;
}

#[test]
fn resync_pages_yield_to_live_edits_and_deletes() {
    let channel = ss_core::Id::generate();
    let message = |content: &str| v1::Message {
        id: ss_core::Id::generate().as_bytes().to_vec().into(),
        channel_id: channel.as_bytes().to_vec().into(),
        content: content.into(),
        ..Default::default()
    };
    let (kept, edited, deleted) = (message("kept"), message("old"), message("deleted"));
    let mut resync = Resync {
        generation: 1,
        pages: mpsc::channel(1).1,
        task: tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .spawn(async {}),
        live: HashMap::new(),
    };
    let mut newer = edited.clone();
    newer.content = "new".into();
    resync.observe(&v1::ServerEnvelope {
        request_id: 0,
        msg: Some(Event::MessageUpdated(v1::MessageUpdated {
            message: Some(newer),
            nonce: None,
        })),
    });
    resync.observe(&v1::ServerEnvelope {
        request_id: 0,
        msg: Some(Event::MessageDeleted(v1::MessageDeleted {
            message: Some(deleted.clone()),
            nonce: None,
        })),
    });
    let page = resync.reconcile(vec![kept.clone(), edited, deleted]);
    assert_eq!(
        page.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(),
        ["kept", "new"]
    );
}
