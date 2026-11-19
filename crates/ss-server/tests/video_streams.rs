//! Real authenticated QUIC clients exercise opaque video relay policy.
use bytes::{Bytes, BytesMut};
use ss_core::{Id, Permissions};
use ss_net::{
    client,
    control::ClientControl,
    media::{
        DatagramHeader, StreamHeader,
        stream::{Frame, GroupReader, GroupWriter, RESET_GROUP},
    },
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::{
    v1::{self, client_envelope::Msg as Cmd, server_envelope::Msg as Event},
    validate::{id_from_bytes, id_to_bytes},
};
use ss_server::{
    auth,
    connection::Server,
    db::{self, NewInvite, invites},
};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, time::timeout};
const WAIT: Duration = Duration::from_secs(10);
/// Gap between 16 KiB frames in the relay load tests: about 32 Mb/s, four times
/// the 1080p60 preset. A healthy viewer is reset only after falling 32 chunks
/// (about 128 ms) behind, so a busy test runner does not trip it.
const PACE: Duration = Duration::from_millis(4);
struct World {
    endpoint: Endpoint,
    pins: Arc<MemoryPinStore>,
    server: Arc<Server>,
    task: tokio::task::JoinHandle<()>,
    pool: Arc<db::Pool>,
    code: String,
    _dir: tempfile::TempDir,
}
struct Peer {
    conn: ss_net::Connection,
    control: ClientControl,
    events: mpsc::Receiver<v1::ServerEnvelope>,
    welcome: v1::Welcome,
}
impl World {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = Arc::new(db::Pool::open(dir.path().join("db")).await.unwrap());
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
        let server = Arc::new(Server::open(Arc::clone(&pool)).await.unwrap());
        let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));
        Self {
            endpoint,
            pins,
            server,
            task,
            pool,
            code,
            _dir: dir,
        }
    }
    async fn peer(&self, name: &str, inviter: Option<Id>) -> Peer {
        let code = if let Some(inviter) = inviter {
            let mut db = self.pool.write().await;
            invites::insert(
                &mut db,
                NewInvite {
                    id: Id::generate(),
                    code: name.into(),
                    created_by: inviter,
                    max_uses: Some(1),
                    expires_at: None,
                },
            )
            .await
            .unwrap();
            name.to_owned()
        } else {
            self.code.clone()
        };
        let mut conn = client::connect(
            "127.0.0.1",
            self.endpoint.local_addr().unwrap().port(),
            self.pins.clone(),
        )
        .await
        .unwrap();
        let (control, events) = ClientControl::start(conn.open_control().await.unwrap());
        let reply = timeout(
            WAIT,
            control.request(Cmd::Hello(v1::Hello {
                protocol_version: 1,
                client_name: name.into(),
                auth: Some(v1::hello::Auth::Register(v1::Register {
                    invite_code: code,
                    username: name.into(),
                    password: "a-valid-password".into(),
                })),
                ..Default::default()
            })),
        )
        .await
        .unwrap()
        .unwrap();
        let Some(Event::Welcome(welcome)) = reply.msg else {
            panic!("expected welcome")
        };
        Peer {
            conn,
            control,
            events,
            welcome,
        }
    }
}
impl Drop for World {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test finished");
        self.task.abort();
    }
}
impl Peer {
    fn user(&self) -> Id {
        id_from_bytes(&self.welcome.user.as_ref().unwrap().id).unwrap()
    }
    fn channel(&self) -> Bytes {
        self.welcome
            .snapshot
            .as_ref()
            .unwrap()
            .channels
            .iter()
            .find(|c| c.kind == i32::from(v1::ChannelKind::Voice))
            .unwrap()
            .id
            .clone()
    }
    async fn call(&self, msg: Cmd) -> v1::ack::Result {
        let reply = timeout(WAIT, self.control.request(msg))
            .await
            .unwrap()
            .unwrap();
        let Some(Event::Ack(ack)) = reply.msg else {
            panic!("expected ack, got {:?}", reply.msg)
        };
        ack.result.unwrap()
    }
    async fn join(&self) {
        self.call(Cmd::JoinVoice(v1::JoinVoice {
            channel_id: self.channel(),
            self_mute: false,
            self_deaf: false,
        }))
        .await;
    }
    async fn start(&self) -> u32 {
        let v1::ack::Result::TrackId(id) = self.call(start()).await else {
            panic!("expected track id")
        };
        id
    }
    async fn subscribe(&self, id: u32) {
        self.call(Cmd::SubscribeTrack(v1::SubscribeTrack {
            track_id: id,
            renditions: false,
        }))
        .await;
    }
    async fn event(&mut self, predicate: impl Fn(&Event) -> bool) -> Event {
        timeout(WAIT, async {
            loop {
                let event = self.events.recv().await.unwrap().msg.unwrap();
                if predicate(&event) {
                    return event;
                }
            }
        })
        .await
        .unwrap()
    }
}
fn start() -> Cmd {
    Cmd::StartStream(v1::StartStream {
        kind: v1::TrackKind::Screen.into(),
        codec: Some(v1::Codec {
            name: "avc1.64001f".into(),
            description: Bytes::from_static(b"opaque codec config"),
            ..Default::default()
        }),
        renditions: Vec::new(),
    })
}
fn header(track: u32, group_id: u32) -> StreamHeader {
    StreamHeader {
        media: DatagramHeader {
            version: 0,
            kind: 1,
            flags: 0,
            track_id: track,
            seq: 0,
            timestamp: 48000,
        },
        group_id,
    }
}
fn frame() -> Frame {
    Frame {
        timestamp: 48000,
        keyframe: true,
        payload: Bytes::from_static(b"synthetic frame bytes"),
    }
}

#[tokio::test]
async fn pipes_before_fin_relays_hints_and_keyframes_and_unsubscribes() {
    let world = World::new().await;
    let mut sender = world.peer("publisher", None).await;
    let receiver = world.peer("viewer", Some(sender.user())).await;
    let outsider = world.peer("outsider", Some(sender.user())).await;
    sender.join().await;
    receiver.join().await;
    outsider.join().await;
    let track = sender.start().await;
    receiver.subscribe(track).await;
    receiver
        .call(Cmd::SetPreferredQuality(v1::SetPreferredQuality {
            track_id: track,
            max_height: 720,
            max_fps: 30,
        }))
        .await;
    let hint = sender
        .event(|e| matches!(e, Event::SubscriberHint(h) if h.max_height == 720))
        .await;
    assert!(matches!(hint, Event::SubscriberHint(h) if h.viewer_count == 1 && h.max_fps == 30));
    for _ in 0..4 {
        receiver
            .call(Cmd::RequestKeyframe(v1::RequestKeyframe {
                track_id: track,
                rendition: 0,
            }))
            .await;
    }
    sender
        .event(|e| matches!(e, Event::RequestKeyframe(k) if k.track_id == track))
        .await;
    assert!(
        timeout(Duration::from_millis(100), async {
            loop {
                if matches!(
                    sender.events.recv().await.unwrap().msg,
                    Some(Event::RequestKeyframe(_))
                ) {
                    break;
                }
            }
        })
        .await
        .is_err()
    );
    let mut outgoing = GroupWriter::open(sender.conn.quic(), header(track, 1))
        .await
        .unwrap();
    outgoing.write_frame(&frame()).await.unwrap();
    let mut incoming = timeout(WAIT, GroupReader::accept(receiver.conn.quic()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(incoming.header, header(track, 1));
    assert_eq!(
        timeout(WAIT, incoming.next_frame()).await.unwrap().unwrap(),
        Some(frame())
    );
    // No FIN yet: delivery proves the relay pipes rather than buffering a GOP.
    assert!(
        timeout(
            Duration::from_millis(100),
            outsider.conn.quic().accept_uni()
        )
        .await
        .is_err()
    );
    receiver
        .call(Cmd::UnsubscribeTrack(v1::UnsubscribeTrack {
            track_id: track,
        }))
        .await;
    assert!(timeout(WAIT, incoming.next_frame()).await.unwrap().is_err());
    outgoing.reset();
    sender.call(Cmd::StopStream(v1::StopStream {})).await;
    assert_eq!(world.server.stream_stats().active, 0);
}

#[tokio::test]
async fn slow_viewer_resets_while_two_healthy_viewers_receive_identical_frames() {
    let world = World::new().await;
    let sender = world.peer("publisher", None).await;
    let fast1 = world.peer("fastone", Some(sender.user())).await;
    let fast2 = world.peer("fasttwo", Some(sender.user())).await;
    let slow = world.peer("slowview", Some(sender.user())).await;
    for peer in [&sender, &fast1, &fast2, &slow] {
        peer.join().await;
    }
    let track = sender.start().await;
    for peer in [&fast1, &fast2, &slow] {
        peer.subscribe(track).await;
    }
    let receive = |connection: quinn::Connection| async move {
        let mut reader = GroupReader::accept(&connection).await.unwrap();
        let mut count = 0;
        while let Some(frame) = reader.next_frame().await.unwrap() {
            assert!(frame.payload.iter().all(|byte| *byte == 37));
            count += 1;
        }
        count
    };
    let read1 = tokio::spawn(receive(fast1.conn.quic().clone()));
    let read2 = tokio::spawn(receive(fast2.conn.quic().clone()));
    let mut writer = GroupWriter::open(sender.conn.quic(), header(track, 1))
        .await
        .unwrap();
    let mut data = Frame {
        timestamp: 0,
        keyframe: true,
        payload: Bytes::from(vec![37; 16384]),
    };
    for tick in 0..512 {
        data.timestamp = tick * 480;
        writer.write_frame(&data).await.unwrap();
        data.keyframe = false;
        tokio::time::sleep(PACE).await;
    }
    writer.finish().unwrap();
    assert_eq!(timeout(WAIT, read1).await.unwrap().unwrap(), 512);
    assert_eq!(timeout(WAIT, read2).await.unwrap().unwrap(), 512);
    // This subscriber did not read its stream, exhausting its stream window.
    let mut incoming = timeout(WAIT, slow.conn.quic().accept_uni())
        .await
        .unwrap()
        .unwrap();
    assert!(
        timeout(WAIT, incoming.read_to_end(16 * 1024 * 1024))
            .await
            .unwrap()
            .is_err()
    );
    assert!(world.server.stream_stats().resets >= 1);
}

#[tokio::test]
async fn forged_track_and_sender_reset_do_not_escape_ownership() {
    let world = World::new().await;
    let sender = world.peer("publisher", None).await;
    let viewer = world.peer("viewer", Some(sender.user())).await;
    sender.join().await;
    viewer.join().await;
    let track = sender.start().await;
    viewer.subscribe(track).await;
    let mut forged = viewer.conn.quic().open_uni().await.unwrap();
    let mut bytes = BytesMut::new();
    header(track, 1).encode_into(&mut bytes).unwrap();
    forged.write_all(&bytes).await.unwrap();
    assert_eq!(
        timeout(WAIT, forged.stopped()).await.unwrap().unwrap(),
        Some(RESET_GROUP.into())
    );
    let mut writer = GroupWriter::open(sender.conn.quic(), header(track, 2))
        .await
        .unwrap();
    writer.write_frame(&frame()).await.unwrap();
    let mut reader = timeout(WAIT, GroupReader::accept(viewer.conn.quic()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reader.next_frame().await.unwrap(), Some(frame()));
    writer.reset();
    assert!(timeout(WAIT, reader.next_frame()).await.unwrap().is_err());
    sender.call(Cmd::LeaveVoice(v1::LeaveVoice {})).await;
    assert_eq!(world.server.stream_stats().active, 0);
}

#[tokio::test]
async fn permission_revocation_stops_publication_and_active_groups() {
    let world = World::new().await;
    let admin = world.peer("admin", None).await;
    let publisher = world.peer("publisher", Some(admin.user())).await;
    admin.join().await;
    publisher.join().await;
    let track = publisher.start().await;
    admin.subscribe(track).await;
    let mut writer = GroupWriter::open(publisher.conn.quic(), header(track, 1))
        .await
        .unwrap();
    writer.write_frame(&frame()).await.unwrap();
    let mut reader = timeout(WAIT, GroupReader::accept(admin.conn.quic()))
        .await
        .unwrap()
        .unwrap();
    assert!(reader.next_frame().await.unwrap().is_some());
    admin
        .call(Cmd::SetChannelOverride(v1::SetChannelOverride {
            channel_id: admin.channel(),
            role_id: id_to_bytes(db::EVERYONE_ROLE_ID),
            allow: 0,
            deny: Permissions::STREAM.bits(),
        }))
        .await;
    assert_eq!(world.server.stream_stats().active, 0);
    assert!(timeout(WAIT, reader.next_frame()).await.unwrap().is_err());
    let denied = publisher.control.request(start()).await.unwrap();
    assert!(
        matches!(denied.msg, Some(Event::Error(e)) if e.code == i32::from(v1::ErrorCode::Forbidden))
    );
}

#[tokio::test]
async fn group_budget_drops_oldest_and_membership_cleanup_preserves_newer_groups() {
    let world = World::new().await;
    let sender = world.peer("publisher", None).await;
    let viewer = world.peer("viewer", Some(sender.user())).await;
    sender.join().await;
    let track = sender.start().await;
    let denied = viewer
        .control
        .request(Cmd::SubscribeTrack(v1::SubscribeTrack {
            track_id: track,
            renditions: false,
        }))
        .await
        .unwrap();
    assert!(matches!(denied.msg, Some(Event::Error(_))));
    viewer.join().await;
    viewer.subscribe(track).await;
    let duplicate = sender.control.request(start()).await.unwrap();
    assert!(matches!(duplicate.msg, Some(Event::Error(_))));
    let late = world.peer("lateview", Some(sender.user())).await;
    assert!(
        late.welcome
            .snapshot
            .unwrap()
            .tracks
            .iter()
            .any(|t| t.track_id == track)
    );
    let mut writers = Vec::new();
    let mut readers = Vec::new();
    for group in 0..3 {
        let mut writer = GroupWriter::open(sender.conn.quic(), header(track, group))
            .await
            .unwrap();
        writer.write_frame(&frame()).await.unwrap();
        let mut reader = timeout(WAIT, GroupReader::accept(viewer.conn.quic()))
            .await
            .unwrap()
            .unwrap();
        assert!(
            timeout(WAIT, reader.next_frame())
                .await
                .unwrap()
                .unwrap()
                .is_some()
        );
        writers.push(writer);
        readers.push(reader);
    }
    assert!(
        timeout(WAIT, readers[0].next_frame())
            .await
            .unwrap()
            .is_err()
    );
    let newest = writers.pop().unwrap();
    newest.finish().unwrap();
    assert!(
        timeout(WAIT, readers[2].next_frame())
            .await
            .unwrap()
            .unwrap()
            .is_none()
    );
    viewer.call(Cmd::LeaveVoice(v1::LeaveVoice {})).await;
    assert!(
        timeout(WAIT, readers[1].next_frame())
            .await
            .unwrap()
            .is_err()
    );
    sender.call(Cmd::StopStream(v1::StopStream {})).await;
    let replacement = sender.start().await;
    assert!(replacement > track);
    sender.conn.close(0, b"publisher left");
    timeout(WAIT, async {
        while world.server.stream_stats().active != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn client_viewers_demux_two_tracks_and_release() {
    let world = World::new().await;
    let publisher = world.peer("publisher", None).await;
    let second = world.peer("second", Some(publisher.user())).await;
    let viewer_invite = {
        let mut db = world.pool.write().await;
        invites::insert(
            &mut db,
            NewInvite {
                id: Id::generate(),
                code: "viewer-code".into(),
                created_by: publisher.user(),
                max_uses: Some(1),
                expires_at: None,
            },
        )
        .await
        .unwrap();
        "viewer-code"
    };
    let viewer = ss_client_core::Client::connect(
        ss_client_core::ServerAddr::new("127.0.0.1", world.endpoint.local_addr().unwrap().port())
            .unwrap(),
        ss_client_core::Credentials::Register(v1::Register {
            invite_code: viewer_invite.into(),
            username: "viewer".into(),
            password: "test-password".into(),
        }),
        world.pins.clone(),
    )
    .await
    .unwrap();
    publisher.join().await;
    second.join().await;
    viewer
        .join_voice(v1::JoinVoice {
            channel_id: publisher.channel(),
            self_mute: true,
            self_deaf: false,
        })
        .await
        .unwrap();
    let a = publisher.start().await;
    let b = second.start().await;
    timeout(WAIT, async {
        while !viewer.state().read().tracks.contains_key(&b) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let first = viewer.prepare_video(a).unwrap();
    first.subscribe().await.unwrap();
    let other = viewer.prepare_video(b).unwrap();
    other.subscribe().await.unwrap();
    assert!(viewer.prepare_video(a).is_err());
    let mut send_a = ss_client_core::media::VideoTrackSender::new(&publisher.conn, a).unwrap();
    let mut send_b = ss_client_core::media::VideoTrackSender::new(&second.conn, b).unwrap();
    for (sender, value) in [(&mut send_a, 1_u8), (&mut send_b, 2_u8)] {
        let mut group = sender.begin_group(0).await.unwrap();
        group
            .write_frame(&Frame {
                timestamp: 0,
                keyframe: true,
                payload: Bytes::from(vec![value; 8]),
            })
            .await
            .unwrap();
        group.finish().unwrap();
    }
    let mut group = timeout(WAIT, first.next_group()).await.unwrap().unwrap();
    assert_eq!(group.next_frame().await.unwrap().unwrap().payload[0], 1);
    let mut group = timeout(WAIT, other.next_group()).await.unwrap().unwrap();
    assert_eq!(group.next_frame().await.unwrap().unwrap().payload[0], 2);
    drop(first);
    // Reopening a closed viewer is allowed without starting another accept loop.
    let reopened = viewer.prepare_video(a).unwrap();
    reopened.subscribe().await.unwrap();
    viewer.leave_voice(v1::LeaveVoice {}).await.unwrap();
    timeout(WAIT, async {
        while reopened.valid() || other.valid() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(reopened.next_group().await.is_none());
    drop(reopened);
    drop(other);
    viewer.close().await;
}

fn rendition(index: u32, width: u32, height: u32, fps: u32) -> v1::Rendition {
    v1::Rendition {
        index,
        width,
        height,
        fps,
        bitrate: 0,
        codec: None,
    }
}
fn start_two() -> Cmd {
    let Cmd::StartStream(mut start) = start() else {
        unreachable!()
    };
    start.renditions = vec![rendition(0, 1920, 1080, 30), rendition(1, 640, 360, 15)];
    Cmd::StartStream(start)
}
fn low_header(track: u32, group_id: u32) -> StreamHeader {
    let mut header = header(track, group_id);
    header.media.flags = ss_net::media::LOW_RENDITION;
    header
}
fn tagged(value: u8, keyframe: bool) -> Frame {
    Frame {
        timestamp: 48000,
        keyframe,
        payload: Bytes::from(vec![value; 64]),
    }
}
/// Next group on `connection` within `wait`, or `None`.
async fn next_group(connection: &quinn::Connection, wait: Duration) -> Option<GroupReader> {
    timeout(wait, GroupReader::accept(connection))
        .await
        .ok()
        .map(Result::unwrap)
}

#[tokio::test]
async fn stalled_subscriber_is_reset_every_group_while_healthy_one_gets_each_group() {
    let world = World::new().await;
    let sender = world.peer("publisher", None).await;
    let healthy = world.peer("healthy", Some(sender.user())).await;
    let stalled = world.peer("stalled", Some(sender.user())).await;
    for peer in [&sender, &healthy, &stalled] {
        peer.join().await;
    }
    let track = sender.start().await;
    healthy.subscribe(track).await;
    stalled.subscribe(track).await;
    // The healthy viewer reads each group to its end and records its id.
    let quic = healthy.conn.quic().clone();
    let reader = tokio::spawn(async move {
        let mut seen = Vec::new();
        while seen.len() < 12 {
            let mut group = GroupReader::accept(&quic).await.unwrap();
            let mut frames = 0;
            while let Some(frame) = group.next_frame().await.unwrap() {
                assert_eq!(frame.payload.len(), 16 * 1024);
                frames += 1;
            }
            assert_eq!(frames, 96, "group {} incomplete", group.header.group_id);
            seen.push(group.header.group_id);
        }
        seen
    });
    // The stalled viewer accepts its streams but never reads them.
    let quic = stalled.conn.quic().clone();
    let held = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok(Ok(stream)) = timeout(Duration::from_secs(3), quic.accept_uni()).await {
            held.push(stream);
        }
        held
    });
    let payload = Bytes::from(vec![9; 16 * 1024]);
    // Each 1.5 MiB group is larger than the stalled viewer's stream window.
    for group in 0..12 {
        let mut writer = GroupWriter::open(sender.conn.quic(), header(track, group))
            .await
            .unwrap();
        for index in 0..96 {
            writer
                .write_frame(&Frame {
                    timestamp: group * 96 + index,
                    keyframe: index == 0,
                    payload: payload.clone(),
                })
                .await
                .unwrap();
            tokio::time::sleep(PACE).await;
        }
        writer.finish().unwrap();
    }
    let seen = timeout(Duration::from_secs(30), reader)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen, (0..12).collect::<Vec<_>>());
    let held = held.await.unwrap();
    assert!(!held.is_empty());
    for mut stream in held {
        // Every group the stalled viewer got was reset, none delivered whole.
        assert!(
            timeout(WAIT, stream.read_to_end(32 * 1024 * 1024))
                .await
                .unwrap()
                .is_err()
        );
    }
    assert!(world.server.stream_stats().resets >= 12);
}

#[tokio::test]
#[allow(clippy::too_many_lines, reason = "one switch sequence, step by step")]
async fn rendition_switches_happen_only_at_a_new_group() {
    let world = World::new().await;
    let mut sender = world.peer("publisher", None).await;
    let viewer = world.peer("viewer", Some(sender.user())).await;
    let legacy = world.peer("legacy", Some(sender.user())).await;
    for peer in [&sender, &viewer, &legacy] {
        peer.join().await;
    }
    let v1::ack::Result::TrackId(track) = sender.call(start_two()).await else {
        panic!("expected track id")
    };
    viewer
        .call(Cmd::SubscribeTrack(v1::SubscribeTrack {
            track_id: track,
            renditions: true,
        }))
        .await;
    legacy.subscribe(track).await;
    let quic = sender.conn.quic().clone();
    let open = |header: StreamHeader| GroupWriter::open(&quic, header);
    // Rendition 0 group 0 and rendition 1 group 0 run side by side.
    let mut full = open(header(track, 0)).await.unwrap();
    full.write_frame(&tagged(10, true)).await.unwrap();
    let mut low = open(low_header(track, 0)).await.unwrap();
    low.write_frame(&tagged(20, true)).await.unwrap();
    let mut on_full = next_group(viewer.conn.quic(), WAIT).await.unwrap();
    assert_eq!(on_full.header.rendition(), 0);
    let mut legacy_full = next_group(legacy.conn.quic(), WAIT).await.unwrap();
    assert_eq!(legacy_full.header, header(track, 0));
    assert!(
        next_group(viewer.conn.quic(), Duration::from_millis(200))
            .await
            .is_none()
    );

    // Capping the viewer to 360p asks for a low keyframe and reports the low
    // subscriber, but the groups in flight do not change mid-group.
    viewer
        .call(Cmd::SetPreferredQuality(v1::SetPreferredQuality {
            track_id: track,
            max_height: 360,
            max_fps: 0,
        }))
        .await;
    sender
        .event(|e| matches!(e, Event::RequestKeyframe(k) if k.rendition == 1))
        .await;
    sender
        .event(|e| {
            matches!(e, Event::SubscriberStats(s)
                if s.renditions.len() == 2 && s.renditions[1].subscribers == 1)
        })
        .await;
    full.write_frame(&tagged(11, false)).await.unwrap();
    low.write_frame(&tagged(21, false)).await.unwrap();
    assert_eq!(on_full.next_frame().await.unwrap().unwrap().payload[0], 10);
    assert_eq!(on_full.next_frame().await.unwrap().unwrap().payload[0], 11);
    assert!(
        next_group(viewer.conn.quic(), Duration::from_millis(200))
            .await
            .is_none()
    );

    // The next low group starts with a keyframe: that is where the viewer moves.
    low.finish().unwrap();
    let mut low = open(low_header(track, 1)).await.unwrap();
    low.write_frame(&tagged(22, true)).await.unwrap();
    let mut on_low = next_group(viewer.conn.quic(), WAIT).await.unwrap();
    assert_eq!(on_low.header, low_header(track, 1));
    assert_eq!(on_low.next_frame().await.unwrap().unwrap().payload[0], 22);
    full.finish().unwrap();
    assert!(on_full.next_frame().await.unwrap().is_none());
    let mut full = open(header(track, 1)).await.unwrap();
    full.write_frame(&tagged(12, true)).await.unwrap();
    assert!(
        next_group(viewer.conn.quic(), Duration::from_millis(200))
            .await
            .is_none()
    );
    // A viewer that never declared renditions keeps the full rendition only.
    assert_eq!(
        legacy_full.next_frame().await.unwrap().unwrap().payload[0],
        10
    );
    assert_eq!(
        legacy_full.next_frame().await.unwrap().unwrap().payload[0],
        11
    );
    assert!(legacy_full.next_frame().await.unwrap().is_none());
    let mut legacy_next = next_group(legacy.conn.quic(), WAIT).await.unwrap();
    assert_eq!(legacy_next.header, header(track, 1));
    assert_eq!(
        legacy_next.next_frame().await.unwrap().unwrap().payload[0],
        12
    );

    // Lifting the cap probes the full rendition at its next group, while the
    // low rendition keeps flowing; a full group a second later confirms it
    // and the low rendition's group for this viewer is reset.
    viewer
        .call(Cmd::SetPreferredQuality(v1::SetPreferredQuality {
            track_id: track,
            max_height: 0,
            max_fps: 0,
        }))
        .await;
    full.finish().unwrap();
    let mut full = open(header(track, 2)).await.unwrap();
    full.write_frame(&tagged(13, true)).await.unwrap();
    let mut probe = next_group(viewer.conn.quic(), WAIT).await.unwrap();
    assert_eq!(probe.header, header(track, 2));
    assert_eq!(probe.next_frame().await.unwrap().unwrap().payload[0], 13);
    low.write_frame(&tagged(23, false)).await.unwrap();
    assert_eq!(on_low.next_frame().await.unwrap().unwrap().payload[0], 23);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    full.finish().unwrap();
    let mut full = open(header(track, 3)).await.unwrap();
    full.write_frame(&tagged(14, true)).await.unwrap();
    let mut confirmed = next_group(viewer.conn.quic(), WAIT).await.unwrap();
    assert_eq!(confirmed.header, header(track, 3));
    assert_eq!(
        confirmed.next_frame().await.unwrap().unwrap().payload[0],
        14
    );
    assert!(timeout(WAIT, on_low.next_frame()).await.unwrap().is_err());
    assert!(world.server.stream_stats().switches_up >= 1);
    full.reset();
    low.reset();
}
