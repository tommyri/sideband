//! The LiveKit raw-frame path: native frames into `NativeVideoSource`
//! (libwebrtc encodes, H.264 via VideoToolbox, simulcast), decoded frames out
//! of `NativeVideoStream`. Authorization is a LiveKit JWT per participant.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use futures::StreamExt;
use livekit::{
    options::{TrackPublishOptions, VideoCodec, VideoEncoding, VideoPreset},
    prelude::*,
    webrtc::{
        peer_connection_factory::{IceServer, IceTransportsType, RtcConfiguration},
        prelude::*,
        rtp_sender::VideoEncoderBackend,
        stats::RtcStats,
        video_frame::native::{NativeBuffer, VideoFrameBufferExt},
        video_source::native::NativeVideoSource,
        video_stream::native::NativeVideoStream,
    },
};
use livekit_api::{
    access_token::{AccessToken, VideoGrants},
    services::room::{RemoveParticipantOptions, RoomClient, UpdateParticipantOptions},
};
use moq_video::Surface;
use serde_json::json;
use tokio::sync::mpsc;

use crate::{
    metrics::{self, Window},
    source::{Content, FrameSource},
    stamp,
};

pub const URL: &str = "ws://127.0.0.1:7880";
const HTTP: &str = "http://127.0.0.1:7880";
const KEY: &str = "devkey";
const SECRET: &str = "secret";
pub const TURN_USER: &str = "spike";
pub const TURN_PASS: &str = "spike-pass";

pub fn token(room: &str, identity: &str, publish: bool, ttl: Duration) -> Result<String> {
    Ok(AccessToken::with_api_key(KEY, SECRET)
        .with_identity(identity)
        .with_ttl(ttl)
        .with_grants(VideoGrants {
            room_join: true,
            room: room.to_owned(),
            can_publish: Some(publish),
            can_subscribe: Some(!publish),
            ..Default::default()
        })
        .to_jwt()?)
}

fn admin() -> RoomClient {
    RoomClient::with_api_key(HTTP, KEY, SECRET)
}

#[derive(Clone, Debug)]
pub struct PublishSettings {
    pub simulcast: bool,
    pub hardware: bool,
    pub bitrate: u64,
}

pub async fn publish(
    room_name: &str,
    content: &Content,
    preset: ss_video::Preset,
    settings: PublishSettings,
    seconds: u64,
) -> Result<()> {
    let token = token(room_name, "publisher", true, Duration::from_secs(3600))?;
    let mut options = RoomOptions::default();
    options.auto_subscribe = false;
    options.dynacast = true;
    let (room, mut events) = Room::connect(URL, &token, options).await?;
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    let (w, h) = (preset.width(), preset.height());
    let mut source = FrameSource::open(content, w, h, preset.fps()).await?;
    let rtc_source = NativeVideoSource::new(
        VideoResolution {
            width: w,
            height: h,
        },
        true,
    );
    let track =
        LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(rtc_source.clone()));
    let publish_options = TrackPublishOptions {
        source: TrackSource::Screenshare,
        video_codec: VideoCodec::H264,
        simulcast: settings.simulcast,
        // Layers chosen so a degraded viewer still gets a usable picture:
        // 360p15 and 720p30 under the 1080p30 top layer.
        simulcast_layers: Some(vec![
            VideoPreset::new(640, 360, 400_000, 15.0),
            VideoPreset::new(1280, 720, 1_200_000, 30.0),
        ]),
        video_encoding: Some(VideoEncoding {
            max_bitrate: settings.bitrate,
            max_framerate: f64::from(preset.fps()),
        }),
        video_encoder: if settings.hardware {
            VideoEncoderBackend::VideoToolbox
        } else {
            VideoEncoderBackend::Auto
        },
        ..Default::default()
    };
    room.local_participant()
        .publish_track(LocalTrack::Video(track.clone()), publish_options)
        .await
        .context("publish track")?;
    metrics::event(
        "publisher",
        "started",
        json!({"room": room_name, "simulcast": settings.simulcast}),
    );

    let mut window = Window::new("publisher");
    let mut last_stats = Instant::now();
    let mut sent: HashMap<String, u64> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        let stamped = source.next().await?;
        let timestamp_us = i64::try_from(stamped.micros).unwrap_or(0);
        match stamped.frame.surface {
            Surface::PixelBuffer(pixels) => {
                // webrtc-sys takes over one reference and releases it.
                let raw = objc2_core_foundation::CFRetained::into_raw(
                    objc2_core_foundation::Type::retain(pixels.buffer()),
                );
                // SAFETY: `raw` is a +1 CVPixelBufferRef that NativeBuffer consumes.
                let buffer = unsafe { NativeBuffer::from_cv_pixel_buffer(raw.as_ptr().cast()) };
                rtc_source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us,
                    frame_metadata: None,
                    buffer,
                });
            }
            other => {
                let i420 = other.into_i420()?;
                let mut buffer = I420Buffer::new(w, h);
                let (sy, su, sv) = buffer.strides();
                let (y, u, v) = buffer.data_mut();
                let data = i420.data();
                let (pw, ph) = (w as usize, h as usize);
                let (cw, ch) = (pw / 2, ph / 2);
                for row in 0..ph {
                    y[row * sy as usize..row * sy as usize + pw]
                        .copy_from_slice(&data[row * pw..(row + 1) * pw]);
                }
                let (uo, vo) = (pw * ph, pw * ph + cw * ch);
                for row in 0..ch {
                    u[row * su as usize..row * su as usize + cw]
                        .copy_from_slice(&data[uo + row * cw..uo + (row + 1) * cw]);
                    v[row * sv as usize..row * sv as usize + cw]
                        .copy_from_slice(&data[vo + row * cw..vo + (row + 1) * cw]);
                }
                rtc_source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us,
                    frame_metadata: None,
                    buffer,
                });
            }
        }
        window.frame((w, h), Some((stamped.micros, stamped.seq)));
        if last_stats.elapsed() >= Duration::from_secs(1) {
            let dt = last_stats.elapsed().as_secs_f64();
            last_stats = Instant::now();
            let mut layers = Vec::new();
            let mut total = 0_u64;
            for stat in track.get_stats().await.unwrap_or_default() {
                if let RtcStats::OutboundRtp(out) = stat {
                    let rid = out.outbound.rid.clone();
                    let bytes = out.sent.bytes_sent;
                    let prev = sent.insert(rid.clone(), bytes).unwrap_or(bytes);
                    let kbps = ((bytes - prev) as f64 * 8.0 / dt / 1000.0).round();
                    total += bytes - prev;
                    layers.push(json!({
                        "rid": rid, "kbps": kbps, "w": out.outbound.frame_width, "h": out.outbound.frame_height,
                        "fps": out.outbound.frames_per_second, "target_kbps": (out.outbound.target_bitrate / 1000.0).round(),
                        "active": out.outbound.active, "encoder": out.outbound.encoder_implementation,
                        "limit": format!("{:?}", out.outbound.quality_limitation_reason),
                        "pli": out.outbound.pli_count, "nack": out.outbound.nack_count,
                    }));
                }
            }
            window.bytes(total as usize);
            window.extra.insert("layers".into(), json!(layers));
        }
        window.tick();
    }
    room.close().await?;
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct Lifecycle {
    pub signal_reconnect_at: Option<u64>,
    pub full_reconnect_at: Option<u64>,
    /// Revoke canSubscribe through the room service (models losing VIEW_CHANNEL).
    pub revoke_at: Option<u64>,
    pub restore_at: Option<u64>,
    /// MoveParticipant to `<room>-other` (a channel move), and back.
    pub move_at: Option<u64>,
    pub move_back_at: Option<u64>,
    /// RemoveParticipant (kick/ban), then try to rejoin with the same unexpired token.
    pub kick_at: Option<u64>,
    /// Token lifetime; a reconnect after expiry shows whether expiry ends a live session.
    pub ttl: Option<u64>,
}

enum Msg {
    Frame((u32, u32), Option<(u64, u16)>),
    Ended,
}

fn read_barcode(frame: &VideoFrame<BoxVideoBuffer>) -> Option<(u64, u16)> {
    let now = stamp::now_micros();
    let buffer = &frame.buffer;
    if let Some(native) = buffer.as_native() {
        let ptr = native.get_cv_pixel_buffer();
        if !ptr.is_null() {
            // SAFETY: a live CVPixelBufferRef owned by the frame for this call.
            let pb = unsafe { &*(ptr as *const objc2_core_video::CVPixelBuffer) };
            return crate::pixels::with_luma(pb, false, |luma, stride, width| {
                stamp::read(luma, stride, width, now)
            })
            .flatten();
        }
        let i420 = native.to_i420();
        let (sy, _, _) = i420.strides();
        return stamp::read(i420.data().0, sy as usize, i420.width() as usize, now);
    }
    if let Some(i420) = buffer.as_i420() {
        let (sy, _, _) = i420.strides();
        return stamp::read(i420.data().0, sy as usize, i420.width() as usize, now);
    }
    None
}

async fn connect_viewer(
    room: &str,
    identity: &str,
    turn: Option<u16>,
    token: &str,
) -> Result<(Room, mpsc::UnboundedReceiver<RoomEvent>)> {
    let mut options = RoomOptions::default();
    options.auto_subscribe = true;
    options.adaptive_stream = false;
    options.dynacast = true;
    if let Some(port) = turn {
        // Relay-only through the embedded TURN server, which sits behind the
        // impairment proxy. Only this viewer's media takes that path.
        let mut rtc = RtcConfiguration::default();
        rtc.ice_servers = vec![IceServer {
            urls: vec![format!("turn:127.0.0.1:{port}?transport=udp")],
            username: TURN_USER.into(),
            password: TURN_PASS.into(),
        }];
        rtc.ice_transport_type = IceTransportsType::Relay;
        options.rtc_config = rtc;
    }
    let _ = identity;
    Room::connect(URL, token, options)
        .await
        .with_context(|| format!("connect {room}"))
}

pub async fn view(
    room_name: &str,
    identity: &str,
    turn: Option<u16>,
    seconds: u64,
    life: Lifecycle,
) -> Result<()> {
    let started = Instant::now();
    let ttl = Duration::from_secs(life.ttl.unwrap_or(3600));
    let mut current_room = room_name.to_owned();
    let mut token_str = token(room_name, identity, false, ttl)?;
    let (mut room, mut events) = connect_viewer(room_name, identity, turn, &token_str).await?;
    metrics::event(
        identity,
        "connected",
        json!({"room": room_name, "turn": turn}),
    );
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let mut window = Window::new(identity);
    let mut remote: Option<RemoteVideoTrack> = None;
    let mut publication: Option<RemoteTrackPublication> = None;
    let mut received: u64 = 0;
    let mut done: HashMap<&'static str, bool> = HashMap::new();
    let mut waiting: Option<(Instant, &'static str)> = None;
    let mut last_frame = Instant::now();
    let mut last_stats = Instant::now();
    let mut stats_prev: Option<serde_json::Value> = None;
    let deadline = started + Duration::from_secs(seconds);
    let api = admin();
    while Instant::now() < deadline {
        let at = started.elapsed().as_secs();
        let mut due = |key: &'static str, when: Option<u64>| -> bool {
            if when.is_some_and(|s| at >= s) && !done.get(key).copied().unwrap_or(false) {
                done.insert(key, true);
                true
            } else {
                false
            }
        };
        if due("signal", life.signal_reconnect_at) {
            room.simulate_scenario(livekit::SimulateScenario::SignalReconnect)
                .await?;
            metrics::event(identity, "signal_reconnect", json!({}));
            waiting = Some((Instant::now(), "first_frame_after_signal_reconnect"));
        }
        if due("full", life.full_reconnect_at) {
            room.simulate_scenario(livekit::SimulateScenario::FullReconnect)
                .await?;
            metrics::event(identity, "full_reconnect", json!({}));
            waiting = Some((Instant::now(), "first_frame_after_full_reconnect"));
        }
        if due("revoke", life.revoke_at) {
            let t = Instant::now();
            let permission = livekit_protocol::ParticipantPermission {
                can_subscribe: false,
                can_publish: false,
                can_publish_data: true,
                ..Default::default()
            };
            api.update_participant(
                &current_room,
                identity,
                UpdateParticipantOptions {
                    permission: Some(permission),
                    ..Default::default()
                },
            )
            .await?;
            metrics::event(
                identity,
                "revoked_subscribe",
                json!({"api_ms": t.elapsed().as_millis()}),
            );
            waiting = Some((t, "frames_stopped_after_revoke"));
        }
        if due("restore", life.restore_at) {
            let t = Instant::now();
            let permission = livekit_protocol::ParticipantPermission {
                can_subscribe: true,
                can_publish: false,
                can_publish_data: true,
                ..Default::default()
            };
            api.update_participant(
                &current_room,
                identity,
                UpdateParticipantOptions {
                    permission: Some(permission),
                    ..Default::default()
                },
            )
            .await?;
            metrics::event(
                identity,
                "restored_subscribe",
                json!({"api_ms": t.elapsed().as_millis()}),
            );
            waiting = Some((t, "first_frame_after_restore"));
        }
        if due("move", life.move_at) {
            let t = Instant::now();
            let other = format!("{room_name}-other");
            let result = api.move_participant(&current_room, identity, &other).await;
            metrics::event(
                identity,
                "moved",
                json!({"to": other, "ok": result.is_ok(), "error": result.err().map(|e| e.to_string()), "api_ms": t.elapsed().as_millis()}),
            );
            current_room = other;
            waiting = Some((t, "frames_stopped_after_move"));
        }
        if due("back", life.move_back_at) {
            let t = Instant::now();
            let result = api
                .move_participant(&current_room, identity, room_name)
                .await;
            metrics::event(
                identity,
                "moved_back",
                json!({"ok": result.is_ok(), "error": result.err().map(|e| e.to_string()), "api_ms": t.elapsed().as_millis()}),
            );
            current_room = room_name.to_owned();
            waiting = Some((t, "first_frame_after_move_back"));
        }
        if due("kick", life.kick_at) {
            let t = Instant::now();
            api.remove_participant_with_options(
                &current_room,
                identity,
                RemoveParticipantOptions::default(),
            )
            .await?;
            metrics::event(
                identity,
                "removed",
                json!({"api_ms": t.elapsed().as_millis()}),
            );
            tokio::time::sleep(Duration::from_secs(2)).await;
            // Same token, still unexpired: does the server let us back in?
            let retry = connect_viewer(room_name, identity, turn, &token_str).await;
            match retry {
                Ok((r, e)) => {
                    metrics::event(identity, "rejoin_with_old_token", json!({"accepted": true}));
                    room = r;
                    events = e;
                    waiting = Some((t, "first_frame_after_kick_rejoin"));
                }
                Err(e) => {
                    metrics::event(
                        identity,
                        "rejoin_with_old_token",
                        json!({"accepted": false, "error": format!("{e:#}")}),
                    );
                    // A fresh token issued after removal: allowed again.
                    token_str = token(room_name, identity, false, ttl)?;
                    tokio::time::sleep(Duration::from_millis(1100)).await;
                    let fresh = connect_viewer(room_name, identity, turn, &token_str).await;
                    metrics::event(
                        identity,
                        "rejoin_with_new_token",
                        json!({"accepted": fresh.is_ok(), "error": fresh.as_ref().err().map(|e| format!("{e:#}"))}),
                    );
                    if let Ok((r, e)) = fresh {
                        room = r;
                        events = e;
                        waiting = Some((t, "first_frame_after_new_token"));
                    }
                }
            }
        }
        tokio::select! {
            event = events.recv() => {
                let Some(event) = event else {
                    metrics::event(identity, "room_events_closed", json!({}));
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                };
                match event {
                    RoomEvent::TrackSubscribed { track: RemoteTrack::Video(track), publication: p, .. } => {
                        metrics::event(identity, "subscribed", json!({"sid": p.sid().to_string(), "simulcasted": p.simulcasted(), "mime": p.mime_type()}));
                        let mut stream = NativeVideoStream::new(track.rtc_track());
                        let tx = tx.clone();
                        tokio::spawn(async move {
                            while let Some(frame) = stream.next().await {
                                let size = (frame.buffer.width(), frame.buffer.height());
                                if tx.send(Msg::Frame(size, read_barcode(&frame))).is_err() {
                                    break;
                                }
                            }
                            let _ = tx.send(Msg::Ended);
                        });
                        remote = Some(track);
                        publication = Some(p);
                    }
                    RoomEvent::TrackUnsubscribed { .. } => metrics::event(identity, "unsubscribed", json!({})),
                    RoomEvent::Disconnected { reason } => metrics::event(identity, "disconnected", json!({"reason": format!("{reason:?}")})),
                    RoomEvent::Reconnecting => metrics::event(identity, "reconnecting", json!({})),
                    RoomEvent::Reconnected => metrics::event(identity, "reconnected", json!({})),
                    RoomEvent::ConnectionStateChanged(s) => metrics::event(identity, "connection_state", json!({"state": format!("{s:?}")})),
                    RoomEvent::TrackSubscriptionFailed { error, .. } => metrics::event(identity, "subscription_failed", json!({"error": error.to_string()})),
                    RoomEvent::ParticipantPermissionChanged { permission, .. } => metrics::event(identity, "participant_permission_changed", json!({"can_subscribe": permission.map(|p| p.can_subscribe)})),
                    RoomEvent::Moved { room: r } => metrics::event(identity, "moved_event", json!({"room": r.name})),
                    _ => {}
                }
            }
            msg = rx.recv() => match msg {
                Some(Msg::Frame(size, barcode)) => {
                    received += 1;
                    last_frame = Instant::now();
                    window.frame(size, barcode);
                    if let Some((t, label)) = waiting
                        && label.starts_with("first_frame") {
                            metrics::event(identity, label, json!({"ms": t.elapsed().as_millis()}));
                            waiting = None;
                        }
                }
                Some(Msg::Ended) => metrics::event(identity, "stream_ended", json!({})),
                None => {}
            },
            () = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
        if let Some((t, label)) = waiting
            && label.starts_with("frames_stopped")
            && last_frame.elapsed() > Duration::from_millis(500)
        {
            let ms = last_frame.saturating_duration_since(t).as_millis();
            metrics::event(
                identity,
                label,
                json!({"last_frame_after_ms": ms, "frames_after_action": i64::from(last_frame > t)}),
            );
            waiting = None;
        }
        if last_stats.elapsed() >= Duration::from_secs(1) {
            let dt = last_stats.elapsed().as_secs_f64();
            last_stats = Instant::now();
            if let Some(track) = &remote {
                for stat in track.get_stats().await.unwrap_or_default() {
                    if let RtcStats::InboundRtp(inbound) = stat {
                        let i = &inbound.inbound;
                        let now = json!({"bytes": i.bytes_received, "lost": inbound.received.packets_lost, "freezes": i.freeze_count, "nack": i.nack_count, "pli": i.pli_count, "jbd": i.jitter_buffer_delay, "jbe": i.jitter_buffer_emitted_count});
                        let prev = stats_prev.replace(now.clone()).unwrap_or(now.clone());
                        let d = |k: &str| {
                            now[k].as_f64().unwrap_or(0.0) - prev[k].as_f64().unwrap_or(0.0)
                        };
                        window.bytes(d("bytes").max(0.0) as usize);
                        let jb = if d("jbe") > 0.0 {
                            (d("jbd") / d("jbe") * 1000.0).round()
                        } else {
                            0.0
                        };
                        window.extra.insert("rtc".into(), json!({
                            "lost": d("lost"), "freezes_total": i.freeze_count, "nack": d("nack"), "pli": d("pli"),
                            "jitter_buffer_ms": jb, "decoder": i.decoder_implementation, "fps": i.frames_per_second,
                        }));
                    }
                }
            }
            if let Some(p) = &publication {
                window
                    .extra
                    .insert("subscribed".into(), json!(p.is_subscribed()));
            }
            let _ = dt;
        }
        window.tick();
    }
    metrics::event(identity, "summary", json!({"frames": received}));
    room.close().await.ok();
    Ok(())
}

/// Create a room ahead of the participants, optionally with a playout-delay
/// bound (milliseconds) that LiveKit signals to subscribers.
pub async fn create_room(name: &str, playout: Option<(u32, u32)>) -> Result<()> {
    let options = livekit_api::services::room::CreateRoomOptions::default();
    let room = match playout {
        Some((min, max)) => {
            admin()
                .create_room_with_playout_delay(name, options, min, max)
                .await?
        }
        None => admin().create_room(name, options).await?,
    };
    metrics::event(
        "admin",
        "room_created",
        json!({"name": room.name, "playout": playout}),
    );
    Ok(())
}

/// Fail fast with a clear message when no LiveKit server answers.
pub async fn ping() -> Result<()> {
    match admin().list_rooms(vec![]).await {
        Ok(rooms) => {
            metrics::event("admin", "livekit_ok", json!({"rooms": rooms.len()}));
            Ok(())
        }
        Err(e) => bail!("LiveKit at {HTTP} not reachable: {e}"),
    }
}
