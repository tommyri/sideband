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
        self.call(Cmd::SubscribeTrack(v1::SubscribeTrack { track_id: id }))
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
        tokio::time::sleep(Duration::from_millis(1)).await;
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
        .request(Cmd::SubscribeTrack(v1::SubscribeTrack { track_id: track }))
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
