//! Handshake and forwarding against the in-process spike server. No sound device.

use std::{sync::Arc, time::Duration};

use ss_core::VoiceProfile;
use ss_net::{
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::{
    v1::{self, client_envelope, server_envelope},
    validate::id_to_bytes,
};
use tokio::{task::JoinHandle, time::timeout};

use crate::{
    audio::TrackUpdate,
    call::{Gate, Options, join_client, track_update},
    channel::channel_from_arg,
    graph::VoiceGraph,
};

struct Fixture {
    endpoint: Endpoint,
    port: u16,
    task: JoinHandle<()>,
    pins: Arc<MemoryPinStore>,
    _data: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let data = tempfile::tempdir().unwrap();
        let identity = tls::generate_self_signed(data.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let port = endpoint.local_addr().unwrap().port();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(&ServerId::new("127.0.0.1", port).unwrap(), fingerprint)
            .unwrap();
        let server = Arc::new(ss_server::Server::default());
        let accept = endpoint.clone();
        let task = tokio::spawn(async move { server.run(accept).await });
        Self {
            endpoint,
            port,
            task,
            pins,
            _data: data,
        }
    }

    fn options(&self, name: &str, channel: ss_core::Id) -> Options {
        Options {
            host: "127.0.0.1".to_owned(),
            port: self.port,
            name: name.to_owned(),
            channel,
            profile: VoiceProfile::Voice,
            gate: Gate::Open,
            measure_latency: false,
            impairment: crate::Impairment::default(),
            bots: 0,
            record: None,
            pins: self.pins.clone(),
            duration: None,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn forwarded_voice_is_audible_on_the_other_client() {
    let fixture = Fixture::start().await;
    let channel = channel_from_arg("7").unwrap();
    let left = join_client(&fixture.options("left", channel), "left")
        .await
        .unwrap();
    let right = join_client(&fixture.options("right", channel), "right")
        .await
        .unwrap();
    assert!(right.remote_tracks.contains(&left.track_id));
    let mut sender = VoiceGraph::new(VoiceProfile::Voice, left.track_id).unwrap();
    let mut receiver = VoiceGraph::new(VoiceProfile::Voice, right.track_id).unwrap();
    receiver.add_track(left.track_id);
    let mut frame = vec![0.0; sender.quantum()];
    for index in 0..20_u32 {
        write_tone(&mut frame, index * 480);
        sender.push_capture(&frame, true).unwrap();
        for packet in sender.pop_outgoing() {
            left.connection.send_datagram(packet).unwrap();
        }
    }
    let mut arrival = Duration::from_millis(20);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(50), right.connection.read_datagram()).await {
            Ok(Ok(bytes)) => {
                receiver.push_datagram(&bytes, arrival);
                arrival += Duration::from_millis(20);
            }
            _ => break,
        }
    }
    let mut energy = 0.0_f64;
    for step in 0..80 {
        let mixed = receiver.render(Duration::from_millis(step * 10));
        energy += mixed
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>();
    }
    assert!(energy > 0.5, "forwarded audio energy {energy}");
    assert!(receiver.snapshot().accepted > 0);
    // Keep the control streams alive until the assertions finish.
    drop(left);
    drop(right);
}

/// A voice server with accounts, which the spike does not have: one user id
/// per username, its own connection id per session, and every session of an
/// account in the channel at once. The real server keeps one seat per user,
/// so there the second join takes the first one's seat; either way each
/// session must find its own track, not the account's.
struct Accounts {
    endpoint: Endpoint,
    port: u16,
    task: JoinHandle<()>,
    pins: Arc<MemoryPinStore>,
    _data: tempfile::TempDir,
}

type Members = Arc<
    std::sync::Mutex<
        Vec<(
            v1::TrackAdded,
            tokio::sync::mpsc::UnboundedSender<v1::ServerEnvelope>,
        )>,
    >,
>;

impl Accounts {
    async fn start() -> Self {
        let data = tempfile::tempdir().unwrap();
        let identity = tls::generate_self_signed(data.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let port = endpoint.local_addr().unwrap().port();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(&ServerId::new("127.0.0.1", port).unwrap(), fingerprint)
            .unwrap();
        let accept = endpoint.clone();
        let task = tokio::spawn(async move {
            let user = ss_core::Id::generate();
            let members = Members::default();
            let mut sessions = tokio::task::JoinSet::new();
            while let Some(incoming) = accept.accept().await {
                let Ok(connection) = incoming.accept().await else {
                    continue;
                };
                sessions.spawn(Self::session(connection, user, Arc::clone(&members)));
            }
        });
        Self {
            endpoint,
            port,
            task,
            pins,
            _data: data,
        }
    }

    async fn session(mut connection: ss_net::Connection, user: ss_core::Id, members: Members) {
        let Ok(stream) = connection.accept_control().await else {
            return;
        };
        let (mut sender, mut receiver) = stream.split();
        let Ok(hello) = receiver.recv().await else {
            return;
        };
        let conn = ss_core::Id::generate();
        let (outbound, mut queue) = tokio::sync::mpsc::unbounded_channel();
        let _ = outbound.send(v1::ServerEnvelope {
            request_id: hello.request_id,
            msg: Some(server_envelope::Msg::Welcome(v1::Welcome {
                user: Some(v1::User {
                    id: id_to_bytes(user),
                    username: "same".into(),
                    display_name: "Same".into(),
                    ..Default::default()
                }),
                server_info: None,
                resume_token: String::new(),
                snapshot: Some(v1::Snapshot::default()),
                conn_id: id_to_bytes(conn),
            })),
        });
        let writer = tokio::spawn(async move {
            while let Some(envelope) = queue.recv().await {
                if sender.send(&envelope).await.is_err() {
                    break;
                }
            }
        });
        while let Ok(envelope) = receiver.recv().await {
            if let Some(client_envelope::Msg::JoinVoice(join)) = envelope.msg {
                let mut members = members.lock().unwrap();
                let added = v1::TrackAdded {
                    track_id: u32::try_from(members.len()).unwrap() + 1,
                    user_id: id_to_bytes(user),
                    conn_id: id_to_bytes(conn),
                    kind: v1::TrackKind::Audio.into(),
                    codec: None,
                    channel_id: join.channel_id,
                    renditions: Vec::new(),
                };
                // The joiner hears of its own track first, then the others'.
                let event = |track: &v1::TrackAdded| v1::ServerEnvelope {
                    request_id: 0,
                    msg: Some(server_envelope::Msg::TrackAdded(track.clone())),
                };
                let _ = outbound.send(event(&added));
                for (track, peer) in members.iter() {
                    let _ = outbound.send(event(track));
                    let _ = peer.send(event(&added));
                }
                members.push((added, outbound.clone()));
            }
            let _ = outbound.send(v1::ServerEnvelope::ack(
                envelope.request_id,
                v1::ack::Result::Empty(v1::Empty {}),
            ));
        }
        writer.abort();
    }

    fn options(&self, channel: ss_core::Id) -> Options {
        Options {
            host: "127.0.0.1".to_owned(),
            port: self.port,
            name: "same".to_owned(),
            channel,
            profile: VoiceProfile::Voice,
            gate: Gate::Open,
            measure_latency: false,
            impairment: crate::Impairment::default(),
            bots: 0,
            record: None,
            pins: self.pins.clone(),
            duration: None,
        }
    }
}

impl Drop for Accounts {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn two_sessions_of_one_account_in_one_channel_each_find_their_own_track() {
    let server = Accounts::start().await;
    let channel = channel_from_arg("7").unwrap();
    let options = server.options(channel);
    let mut first = join_client(&options, "same").await.unwrap();
    let second = join_client(&options, "same").await.unwrap();
    assert_eq!(first.own.user_id, second.own.user_id, "one account");
    assert_eq!(first.track_id, 1);
    assert_eq!(first.remote_tracks, Vec::<u32>::new());
    assert_eq!(
        second.track_id, 2,
        "the second session took the first's track"
    );
    assert_eq!(second.remote_tracks, [1]);
    // The first session hears the second as a remote track to decode.
    let added = timeout(Duration::from_secs(5), first.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        track_update(&added, &first.own, 1),
        Some(TrackUpdate::Add(2))
    ));
    drop(first);
    drop(second);
}

fn write_tone(frame: &mut [f32], start: u32) {
    for (index, sample) in frame.iter_mut().enumerate() {
        let time = f64::from(start.saturating_add(u32::try_from(index).unwrap_or(0))) / 48_000.0;
        #[allow(clippy::cast_possible_truncation)]
        {
            *sample = (0.25 * (2.0 * std::f64::consts::PI * 440.0 * time).sin()) as f32;
        }
    }
}
