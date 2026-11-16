//! The current Sideband backend, unchanged: the real `ss-server` relay on
//! loopback, `ss-net` GOP streams, the same VideoToolbox/OpenH264 encoder
//! settings as `ss_video::Publisher`, and `ss_video::decode::VideoDecoder`.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ss_core::Id;
use ss_net::{
    client,
    control::ClientControl,
    media::{
        DatagramHeader, StreamHeader,
        stream::{Frame, GroupReader, GroupWriter},
    },
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use ss_proto::v1::{self, client_envelope::Msg as Cmd, server_envelope::Msg as Event};
use ss_server::{
    auth,
    connection::Server,
    db::{self, NewInvite, invites},
};
use tokio::sync::mpsc;

use crate::{
    metrics::{self, Window},
    source::{self, Content, FrameSource},
};

const PASSWORD: &str = "a-valid-password";
const WAIT: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize)]
struct ServerInfo {
    port: u16,
    fingerprint: String,
    /// Resume tokens per user. Password logins are rate-limited per IP; resume is not.
    tokens: std::collections::HashMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct TrackInfo {
    track: u32,
    codec: String,
    description: Vec<u8>,
}

/// Run the relay and pre-register `publisher` plus `viewer1..viewer3`
/// (the per-IP registration limit allows five attempts in a window).
pub async fn serve(run_dir: &Path) -> Result<()> {
    let data = run_dir.join("baseline-server");
    std::fs::create_dir_all(&data)?;
    let pool = Arc::new(db::Pool::open(data.join("db")).await?);
    let code = auth::ensure_owner_invite(&pool)
        .await?
        .context("owner invite")?;
    let identity = tls::generate_self_signed(&data).await?;
    let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
    let endpoint = Endpoint::bind("127.0.0.1:0".parse()?, identity)?;
    let port = endpoint.local_addr()?.port();
    let server = Arc::new(Server::open(Arc::clone(&pool)).await?);
    let task = tokio::spawn(Arc::clone(&server).run(endpoint.clone()));

    let pins: Arc<dyn PinStore> = {
        let pins = MemoryPinStore::default();
        pins.set(&ServerId::new("127.0.0.1", port)?, fingerprint)?;
        Arc::new(pins)
    };
    let mut tokens = std::collections::HashMap::new();
    let (owner, token) = register(port, &pins, "publisher", code).await?;
    tokens.insert("publisher".to_owned(), token);
    for i in 1..=3 {
        let name = format!("viewer{i}");
        let mut db = pool.write().await;
        invites::insert(
            &mut db,
            NewInvite {
                id: Id::generate(),
                code: name.clone(),
                created_by: owner,
                max_uses: Some(1),
                expires_at: None,
            },
        )
        .await?;
        drop(db);
        let (_, token) = register(port, &pins, &name, name.clone()).await?;
        tokens.insert(name, token);
    }
    let info = ServerInfo {
        port,
        fingerprint: fingerprint.to_string(),
        tokens,
    };
    std::fs::write(run_dir.join("baseline.json"), serde_json::to_vec(&info)?)?;
    metrics::event("server", "ready", json!({"port": port}));

    let mut last_cpu = metrics::cpu_seconds();
    let mut last = Instant::now();
    let mut last_bytes = 0;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let stats = server.stream_stats();
        let cpu = metrics::cpu_seconds();
        let dt = last.elapsed().as_secs_f64();
        println!(
            "{}",
            json!({
                "t_ms": crate::stamp::now_micros() / 1000, "role": "server",
                "active": stats.active, "resets": stats.resets, "rejected": stats.rejected,
                "fwd_kbps": ((stats.bytes_forwarded - last_bytes) as f64 * 8.0 / dt / 1000.0).round(),
                "cpu_pct": ((cpu - last_cpu) / dt * 1000.0).round() / 10.0,
                "mem_mib": (metrics::footprint_mib() * 10.0).round() / 10.0,
            })
        );
        last_cpu = cpu;
        last = Instant::now();
        last_bytes = stats.bytes_forwarded;
        if task.is_finished() {
            bail!("server task ended");
        }
    }
}

async fn register(
    port: u16,
    pins: &Arc<dyn PinStore>,
    name: &str,
    code: String,
) -> Result<(Id, String)> {
    let mut conn = client::connect("127.0.0.1", port, Arc::clone(pins)).await?;
    let (control, _events) = ClientControl::start(conn.open_control().await?);
    let reply = control
        .request(Cmd::Hello(v1::Hello {
            protocol_version: 1,
            client_name: name.into(),
            auth: Some(v1::hello::Auth::Register(v1::Register {
                invite_code: code,
                username: name.into(),
                password: PASSWORD.into(),
            })),
            ..Default::default()
        }))
        .await?;
    let Some(Event::Welcome(welcome)) = reply.msg else {
        bail!("register {name}: {:?}", reply.msg)
    };
    let id = ss_proto::validate::id_from_bytes(&welcome.user.context("user")?.id)?;
    // Keep the connection open briefly so the session is persisted, then drop it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    conn.close(0, b"registered");
    Ok((id, welcome.resume_token))
}

struct Peer {
    conn: ss_net::Connection,
    control: ClientControl,
    events: Option<mpsc::Receiver<v1::ServerEnvelope>>,
    channel: Bytes,
}

impl Peer {
    /// Log in. `via` connects through another local port (the impairment proxy).
    async fn login(run_dir: &Path, name: &str, via: Option<u16>) -> Result<Self> {
        let info: ServerInfo =
            serde_json::from_slice(&wait_file(&run_dir.join("baseline.json")).await?)?;
        let port = via.unwrap_or(info.port);
        let pins = MemoryPinStore::default();
        pins.set(
            &ServerId::new("127.0.0.1", port)?,
            tls::Fingerprint::try_from(info.fingerprint).map_err(anyhow::Error::msg)?,
        )?;
        let mut conn = client::connect("127.0.0.1", port, Arc::new(pins)).await?;
        let (control, events) = ClientControl::start(conn.open_control().await?);
        let reply = tokio::time::timeout(
            WAIT,
            control.request(Cmd::Hello(v1::Hello {
                protocol_version: 1,
                client_name: name.into(),
                auth: Some(v1::hello::Auth::ResumeToken(
                    info.tokens.get(name).context("unknown user")?.clone(),
                )),
                ..Default::default()
            })),
        )
        .await??;
        let Some(Event::Welcome(welcome)) = reply.msg else {
            bail!("login {name}: {:?}", reply.msg)
        };
        let channel = welcome
            .snapshot
            .context("snapshot")?
            .channels
            .iter()
            .find(|c| c.kind == i32::from(v1::ChannelKind::Voice))
            .context("voice channel")?
            .id
            .clone();
        Ok(Self {
            conn,
            control,
            events: Some(events),
            channel,
        })
    }

    async fn call(&self, msg: Cmd) -> Result<v1::ack::Result> {
        let reply = tokio::time::timeout(WAIT, self.control.request(msg)).await??;
        match reply.msg {
            Some(Event::Ack(ack)) => ack.result.context("empty ack"),
            other => bail!("expected ack, got {other:?}"),
        }
    }

    async fn join(&self) -> Result<()> {
        self.call(Cmd::JoinVoice(v1::JoinVoice {
            channel_id: self.channel.clone(),
            self_mute: true,
            self_deaf: false,
        }))
        .await?;
        Ok(())
    }
}

async fn wait_file(path: &Path) -> Result<Vec<u8>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(bytes) = std::fs::read(path)
            && !bytes.is_empty()
        {
            return Ok(bytes);
        }
        if Instant::now() > deadline {
            bail!("timed out waiting for {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn clock_48k(micros: u128) -> u32 {
    ((micros.saturating_mul(48) / 1_000) % (u128::from(u32::MAX) + 1)) as u32
}

fn is_idr(bytes: &[u8]) -> bool {
    // Same rule as ss-video's private h264::is_idr: any NAL of type 5.
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 1 {
            if bytes[i + 3] & 0x1f == 5 {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Capture/pattern -> encoder (ss_video::Publisher settings) -> one QUIC stream per GOP.
pub async fn publish(
    run_dir: &Path,
    content: &Content,
    preset: ss_video::Preset,
    seconds: u64,
) -> Result<()> {
    let mut peer = Peer::login(run_dir, "publisher", None).await?;
    peer.join().await?;
    let mut source =
        FrameSource::open(content, preset.width(), preset.height(), preset.fps()).await?;
    let rate = moq_video::Rate::new(preset.fps(), 1)?;
    let mut config = moq_video::encode::Config::new(preset.width(), preset.height(), rate);
    config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(preset.bitrate()));
    config.kind = moq_video::encode::Kind::Auto;
    let codec = config.probe().await?;
    let mut encoder = moq_video::encode::Sink::open(&config).await?;
    let backend = encoder.name().to_owned();
    let description = codec.description.unwrap_or_default();
    let v1::ack::Result::TrackId(track) = peer
        .call(Cmd::StartStream(v1::StartStream {
            kind: v1::TrackKind::Screen.into(),
            codec: Some(v1::Codec {
                name: codec.codec.to_string(),
                sample_rate: 0,
                channels: 0,
                description: description.clone(),
            }),
        }))
        .await?
    else {
        bail!("expected a track id");
    };
    let info = TrackInfo {
        track,
        codec: codec.codec.to_string(),
        description: description.to_vec(),
    };
    std::fs::write(
        run_dir.join("baseline-track.json"),
        serde_json::to_vec(&info)?,
    )?;
    metrics::event(
        "publisher",
        "started",
        json!({"track": track, "codec": info.codec, "encoder": backend}),
    );

    let mut events = peer.events.take().context("events")?;
    let mut window = Window::new("publisher");
    let mut writer: Option<GroupWriter> = None;
    let (mut seq, mut group) = (0_u16, 0_u32);
    let mut force = false;
    let mut keyframes_forced = 0_u64;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        let stamped = tokio::select! {
            s = source.next() => s?,
            Some(event) = events.recv() => {
                if let Some(Event::RequestKeyframe(k)) = event.msg && k.track_id == track {
                    force = true;
                }
                continue;
            }
        };
        if force {
            encoder.cut().await?;
            force = false;
            keyframes_forced += 1;
        }
        let began = Instant::now();
        let encoded = encoder.encode(stamped.frame).await?;
        let encode_ms = began.elapsed().as_secs_f64() * 1e3;
        for unit in encoded {
            let keyframe = is_idr(&unit.payload);
            let timestamp = clock_48k(unit.timestamp.as_micros());
            if keyframe {
                if let Some(w) = writer.take() {
                    let _ = w.finish();
                }
                let header = StreamHeader {
                    media: DatagramHeader {
                        version: 0,
                        kind: 1,
                        flags: 0,
                        track_id: track,
                        seq,
                        timestamp,
                    },
                    group_id: group,
                };
                seq = seq.wrapping_add(1);
                group = group.wrapping_add(1);
                writer = Some(GroupWriter::open(peer.conn.quic(), header).await?);
            }
            let Some(w) = writer.as_mut() else { continue };
            window.bytes(unit.payload.len());
            w.write_frame(&Frame {
                timestamp,
                keyframe,
                payload: unit.payload,
            })
            .await?;
        }
        window.frame(
            (preset.width(), preset.height()),
            Some((stamped.micros, stamped.seq)),
        );
        window
            .extra
            .insert("encode_ms".into(), json!((encode_ms * 10.0).round() / 10.0));
        window.extra.insert("encoder".into(), json!(backend));
        window
            .extra
            .insert("keyframes_forced".into(), json!(keyframes_forced));
        let stats = peer.conn.stats();
        window
            .extra
            .insert("rtt_ms".into(), json!(stats.rtt.as_millis()));
        window
            .extra
            .insert("lost_pkts".into(), json!(stats.lost_packets));
        window.tick();
    }
    if let Some(w) = writer.take() {
        let _ = w.finish();
    }
    let _ = peer.call(Cmd::StopStream(v1::StopStream {})).await;
    Ok(())
}

/// Viewer actions for the lifecycle tests.
#[derive(Clone, Debug, Default)]
pub struct Lifecycle {
    /// Leave voice at this second (models a channel move away / losing VIEW_CHANNEL).
    pub leave_at: Option<u64>,
    /// Rejoin and resubscribe at this second.
    pub rejoin_at: Option<u64>,
    /// Drop the QUIC connection and log in again at this second.
    pub reconnect_at: Option<u64>,
}

/// Subscribe, demux GOP streams (newest pending group wins), decode, read barcodes.
pub async fn view(
    run_dir: &Path,
    name: &str,
    via: Option<u16>,
    seconds: u64,
    life: Lifecycle,
) -> Result<()> {
    let started = Instant::now();
    let info: TrackInfo =
        serde_json::from_slice(&wait_file(&run_dir.join("baseline-track.json")).await?)?;
    let mut window = Window::new(name);
    let mut decoder = ss_video::decode::VideoDecoder::open(
        &info.codec,
        Bytes::from(info.description.clone()),
        false,
    )?;
    metrics::event(name, "decoder", json!({"backend": decoder.backend()}));
    let mut session = Session::open(run_dir, name, via, info.track).await?;
    let mut need_key = false;
    let mut resets = 0_u64;
    let mut decode_errors = 0_u64;
    let (mut left, mut rejoined, mut reconnected) = (false, false, false);
    let mut waiting_since: Option<(Instant, &'static str)> = None;
    let deadline = started + Duration::from_secs(seconds);
    loop {
        let now = Instant::now();
        if now > deadline {
            break;
        }
        let at = now.duration_since(started).as_secs();
        if life.leave_at.is_some_and(|s| at >= s) && !left {
            left = true;
            session
                .peer
                .call(Cmd::LeaveVoice(v1::LeaveVoice {}))
                .await?;
            metrics::event(name, "left_voice", json!({}));
            waiting_since = Some((Instant::now(), "last_frame_after_leave"));
        }
        if life.rejoin_at.is_some_and(|s| at >= s) && left && !rejoined {
            rejoined = true;
            let t = Instant::now();
            session.peer.join().await?;
            session
                .peer
                .call(Cmd::SubscribeTrack(v1::SubscribeTrack {
                    track_id: info.track,
                }))
                .await?;
            metrics::event(
                name,
                "rejoined",
                json!({"control_ms": t.elapsed().as_millis()}),
            );
            waiting_since = Some((t, "first_frame_after_rejoin"));
        }
        if life.reconnect_at.is_some_and(|s| at >= s) && !reconnected {
            reconnected = true;
            let t = Instant::now();
            session.peer.conn.close(0, b"reconnect test");
            session = Session::open(run_dir, name, via, info.track).await?;
            decoder.reset()?;
            metrics::event(
                name,
                "reconnected",
                json!({"control_ms": t.elapsed().as_millis()}),
            );
            waiting_since = Some((t, "first_frame_after_reconnect"));
        }
        let frame = tokio::select! {
            f = session.next_frame() => f,
            () = tokio::time::sleep(Duration::from_millis(200)) => { window.tick(); continue; }
        };
        match frame {
            Ok(Some(frame)) => {
                if need_key && !frame.keyframe {
                    continue;
                }
                need_key = false;
                window.bytes(frame.payload.len());
                match decoder.decode(&frame.payload, frame.timestamp, frame.keyframe) {
                    Ok(pictures) => {
                        for picture in pictures {
                            let barcode = source::read_frame(&picture, crate::stamp::now_micros());
                            let size = picture.size();
                            window.frame((size.width, size.height), barcode);
                            if let Some((t, label)) = waiting_since.take() {
                                if label == "last_frame_after_leave" {
                                    waiting_since = Some((t, label));
                                } else {
                                    metrics::event(
                                        name,
                                        label,
                                        json!({"ms": t.elapsed().as_millis()}),
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        decode_errors += 1;
                        metrics::event(name, "decode_error", json!({"error": e.to_string()}));
                        decoder.recover()?;
                        need_key = true;
                        session.request_keyframe().await;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                // A reset group: drop references and wait for the next IDR.
                resets += 1;
                if !left {
                    metrics::event(name, "group_reset", json!({"error": e.to_string()}));
                }
                decoder.reset()?;
                need_key = true;
                session.request_keyframe().await;
            }
        }
        window.extra.insert("resets".into(), json!(resets));
        window
            .extra
            .insert("decode_errors".into(), json!(decode_errors));
        let stats = session.peer.conn.stats();
        window
            .extra
            .insert("rtt_ms".into(), json!(stats.rtt.as_millis()));
        window.tick();
    }
    if let Some((t, "last_frame_after_leave")) = waiting_since {
        metrics::event(
            name,
            "no_frames_since_leave",
            json!({"ms": t.elapsed().as_millis()}),
        );
    }
    Ok(())
}

struct Session {
    peer: Peer,
    track: u32,
    slot: Arc<(std::sync::Mutex<Option<GroupReader>>, tokio::sync::Notify)>,
    current: Option<GroupReader>,
    last_key_request: Option<Instant>,
}

impl Session {
    async fn open(run_dir: &Path, name: &str, via: Option<u16>, track: u32) -> Result<Self> {
        let mut peer = Peer::login(run_dir, name, via).await?;
        peer.join().await?;
        peer.call(Cmd::SubscribeTrack(v1::SubscribeTrack { track_id: track }))
            .await?;
        let mut events = peer.events.take().context("events")?;
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        let slot = Arc::new((std::sync::Mutex::new(None), tokio::sync::Notify::new()));
        let quic = peer.conn.quic().clone();
        let writer = Arc::clone(&slot);
        let role = name.to_owned();
        tokio::spawn(async move {
            // Mirrors the desktop demux (ss-client-core media/viewer.rs): stop only
            // when the connection ends; a stream whose prefix fails or stalls for
            // 1 s is skipped. Only a newer group id replaces (and so resets) the
            // pending group nobody has started reading.
            let mut newest: Option<u32> = None;
            while let Ok(incoming) = quic.accept_uni().await {
                match tokio::time::timeout(
                    Duration::from_secs(1),
                    GroupReader::from_stream(incoming),
                )
                .await
                {
                    Ok(Ok(reader)) => {
                        let id = reader.header.group_id;
                        if newest.is_none_or(|old| id.wrapping_sub(old).cast_signed() > 0) {
                            newest = Some(id);
                            let old = writer.0.lock().expect("slot").replace(reader);
                            drop(old);
                            writer.1.notify_one();
                        }
                    }
                    Ok(Err(e)) => metrics::event(
                        &role,
                        "group_prefix_failed",
                        json!({"error": e.to_string()}),
                    ),
                    Err(_) => metrics::event(&role, "group_prefix_timeout", json!({})),
                }
            }
        });
        Ok(Self {
            peer,
            track,
            slot,
            current: None,
            last_key_request: None,
        })
    }

    async fn next_frame(&mut self) -> Result<Option<Frame>, ss_net::media::stream::Error> {
        while self.current.is_none() {
            let next = self.slot.0.lock().expect("slot").take();
            match next {
                Some(g) => self.current = Some(g),
                None => self.slot.1.notified().await,
            }
        }
        let reader = self.current.as_mut().expect("current group");
        match reader.next_frame().await {
            Ok(Some(frame)) => Ok(Some(frame)),
            Ok(None) => {
                self.current = None;
                Ok(None)
            }
            Err(e) => {
                self.current = None;
                Err(e)
            }
        }
    }

    async fn request_keyframe(&mut self) {
        if self
            .last_key_request
            .is_some_and(|t| t.elapsed() < Duration::from_millis(500))
        {
            return;
        }
        self.last_key_request = Some(Instant::now());
        let _ = self
            .peer
            .call(Cmd::RequestKeyframe(v1::RequestKeyframe {
                track_id: self.track,
            }))
            .await;
    }
}

pub fn run_dir(path: &Option<PathBuf>) -> PathBuf {
    path.clone()
        .unwrap_or_else(|| std::env::temp_dir().join("t100-run"))
}
