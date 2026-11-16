//! T-074 measurement roles. Unlike `baseline`, these drive the product code
//! that T-074 added, so the figures are the product's:
//!
//! - publisher: `ss_video::Encoders` (full + low rendition, keyframe and
//!   low on/off handling), `ss_video::rate::RateController`, and
//!   `ss_client_core::media::{RenditionWriter, start_stream, viewer_report}`;
//! - viewer: a per-rendition newest-group demux (as `ss-client-core`'s viewer
//!   hub), `ss_client_core::media::Switcher` and one
//!   `ss_video::decode::VideoDecoder` per rendition, as the desktop viewer.
//!
//! The relay is the real `ss-server` (`baseline-server`).

use std::{
    path::Path,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ss_client_core::media::{
    RenditionWriter, Step, Switcher, VideoTrackSender, start_stream, viewer_report,
};
use ss_net::media::stream::{Frame, GroupReader};
use ss_proto::v1::{self, client_envelope::Msg as Cmd, server_envelope::Msg as Event};
use ss_video::rate::{LinkSample, RATE_TICK, RateController};

use crate::{
    baseline::{Peer, wait_file},
    metrics::{self, Window},
    source::{self, Content, FrameSource},
    stamp,
};

#[derive(Serialize, Deserialize)]
struct TrackInfo {
    track: u32,
    /// Codec name and description per rendition, full first.
    codecs: Vec<(String, Vec<u8>)>,
}

/// Publisher knobs for the scenarios.
pub struct PubOptions {
    pub preset: ss_video::Preset,
    pub seconds: u64,
    /// Refuse the second encoder (fallback test).
    pub single: bool,
    /// Low rendition's own keyframe interval; `None` aligns with the full one.
    pub low_gop: Option<Duration>,
    /// Turn sender-wide rate control off.
    pub fixed_rate: bool,
}

/// Frames -> product encode stage -> product group writer, with the server's
/// keyframe requests, low on/off reports and the rate controller wired in.
#[allow(clippy::too_many_lines)]
pub async fn publish(run_dir: &Path, content: &Content, options: PubOptions) -> Result<()> {
    let preset = options.preset;
    let mut peer = Peer::login(run_dir, "publisher", None).await?;
    peer.join().await?;
    let mut source =
        FrameSource::open(content, preset.width(), preset.height(), preset.fps()).await?;
    let mut config = ss_video::EncoderConfig::new(preset);
    if options.single {
        config.low = ss_video::LowRendition::Off;
    }
    if let Some(gop) = options.low_gop {
        // Zero aligns the low rendition's keyframes with the full one's.
        config.low_gop = (!gop.is_zero()).then_some(gop);
    }
    let mut encoders = ss_video::Encoders::open(&config).await?;
    let renditions = encoders.renditions().to_vec();
    let start = start_stream(&renditions[0].codec, &renditions);
    let v1::ack::Result::TrackId(track) = peer.call(Cmd::StartStream(start)).await? else {
        bail!("expected a track id");
    };
    let info = TrackInfo {
        track,
        codecs: renditions
            .iter()
            .map(|r| (r.codec.name.clone(), r.codec.description.to_vec()))
            .collect(),
    };
    std::fs::write(run_dir.join("t074-track.json"), serde_json::to_vec(&info)?)?;
    let stats = encoders.stats();
    metrics::event(
        "publisher",
        "started",
        json!({"track": track, "encoder": stats.backend, "renditions": renditions.len(),
               "low": format!("{:?}", stats.low), "codecs": info.codecs.iter().map(|c| &c.0).collect::<Vec<_>>()}),
    );
    let mut writer = RenditionWriter::new(VideoTrackSender::new(&peer.conn, track)?);
    let mut rate = RateController::new(preset.bitrate());
    let mut next_rate = Instant::now() + RATE_TICK;
    let mut events = peer.events.take().context("events")?;
    let mut window = Window::new("publisher");
    let mut last_counts = [ss_video::RenditionCounters::default(); 2];
    let mut requests = [0_u64; 2];
    let mut low_wanted = false;
    let deadline = Instant::now() + Duration::from_secs(options.seconds);
    while Instant::now() < deadline {
        let stamped = tokio::select! {
            s = source.next() => s?,
            Some(event) = events.recv() => {
                match event.msg {
                    Some(Event::RequestKeyframe(k)) if k.track_id == track => {
                        let r = u8::try_from(k.rendition.min(1)).unwrap_or(0);
                        requests[usize::from(r)] += 1;
                        encoders.request_keyframe(r);
                    }
                    Some(Event::SubscriberStats(s)) if s.track_id == track => {
                        rate.report(viewer_report(&s.renditions, renditions.len()));
                        let wanted = s.renditions.get(1).is_some_and(|low| low.subscribers > 0);
                        if wanted != low_wanted {
                            metrics::event("publisher", "low_wanted", json!({"wanted": wanted}));
                        }
                        low_wanted = wanted;
                        encoders.set_low_wanted(wanted);
                        if !wanted {
                            writer.finish(1)?;
                        }
                        window.extra.insert("report".into(), json!(s.renditions.iter().map(|r| json!({
                            "i": r.index, "subs": r.subscribers, "congested": r.congested,
                            "resets": r.resets, "backlog_ms": r.max_backlog_ms, "kbps": r.bytes * 8 / 1000,
                        })).collect::<Vec<_>>()));
                    }
                    _ => {}
                }
                continue;
            }
        };
        let now = Instant::now();
        if !options.fixed_rate && now >= next_rate {
            next_rate = now + RATE_TICK;
            let quic = peer.conn.quic().stats();
            let sample = LinkSample {
                written: writer.written(),
                delivered: quic.udp_tx.bytes.saturating_sub(quic.path.lost_bytes),
                rtt: peer.conn.quic().rtt(),
            };
            if let Some(target) = rate.update(now, sample) {
                encoders.set_bitrate(target).await?;
                metrics::event(
                    "publisher",
                    "bitrate",
                    json!({"target": target, "reason": format!("{:?}", rate.reason())}),
                );
            }
        }
        let began = Instant::now();
        let encoded = encoders.encode(stamped.frame).await?;
        let encode_ms = began.elapsed().as_secs_f64() * 1e3;
        let mut full_frames = 0;
        for unit in encoded {
            if unit.rendition == 0 {
                full_frames += 1;
                window.bytes(unit.payload.len());
            }
            writer
                .write(
                    unit.rendition,
                    &Frame {
                        timestamp: unit.timestamp,
                        keyframe: unit.keyframe,
                        payload: unit.payload,
                    },
                )
                .await?;
        }
        if full_frames > 0 {
            window.frame(
                (preset.width(), preset.height()),
                Some((stamped.micros, stamped.seq)),
            );
        }
        window
            .extra
            .insert("encode_ms".into(), json!((encode_ms * 10.0).round() / 10.0));
        if window_due(&window) {
            let stats = encoders.stats();
            let rend = |i: usize| {
                let (now, then) = (stats.renditions[i], last_counts[i]);
                json!({
                    "frames": now.frames - then.frames,
                    "kbps": (now.bytes - then.bytes) * 8 / 1000,
                    "keyframes": now.keyframes - then.keyframes,
                    "key_kbps": (now.keyframe_bytes - then.keyframe_bytes) * 8 / 1000,
                })
            };
            window.extra.insert("full".into(), rend(0));
            window.extra.insert("low".into(), rend(1));
            window
                .extra
                .insert("target_bps".into(), json!(stats.target_bitrate));
            window
                .extra
                .insert("low_state".into(), json!(format!("{:?}", stats.low)));
            window.extra.insert("key_requests".into(), json!(requests));
            window
                .extra
                .insert("rtt_ms".into(), json!(peer.conn.quic().rtt().as_millis()));
            last_counts = stats.renditions;
            let totals = |i: usize| {
                json!({"frames": stats.renditions[i].frames, "bytes": stats.renditions[i].bytes,
                       "keyframes": stats.renditions[i].keyframes, "keyframe_bytes": stats.renditions[i].keyframe_bytes})
            };
            window
                .extra
                .insert("totals".into(), json!([totals(0), totals(1)]));
        }
        window.tick();
    }
    let _ = writer.finish_all();
    let _ = peer.call(Cmd::StopStream(v1::StopStream {})).await;
    Ok(())
}

fn window_due(window: &Window) -> bool {
    window.due()
}

/// Viewer knobs.
pub struct ViewOptions {
    pub seconds: u64,
    pub via: Option<u16>,
    /// Ask for at most 360p, which caps this viewer to the low rendition.
    pub prefer_low: bool,
}

/// Subscribe with renditions, switch with the product switcher, decode each
/// rendition with its own decoder, read barcodes, log every presented frame.
#[allow(clippy::too_many_lines)]
pub async fn view(run_dir: &Path, name: &str, options: ViewOptions) -> Result<()> {
    let started = Instant::now();
    let info: TrackInfo =
        serde_json::from_slice(&wait_file(&run_dir.join("t074-track.json")).await?)?;
    let mut peer = Peer::login(run_dir, name, options.via).await?;
    peer.join().await?;
    peer.call(Cmd::SubscribeTrack(v1::SubscribeTrack {
        track_id: info.track,
        renditions: true,
    }))
    .await?;
    if options.prefer_low {
        peer.call(Cmd::SetPreferredQuality(v1::SetPreferredQuality {
            track_id: info.track,
            max_height: 360,
            max_fps: 0,
        }))
        .await?;
    }
    let mut events = peer.events.take().context("events")?;
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    let mut decoders: Vec<ss_video::decode::VideoDecoder> = info
        .codecs
        .iter()
        .map(|(codec, description)| {
            ss_video::decode::VideoDecoder::open(codec, Bytes::from(description.clone()), false)
        })
        .collect::<Result<_, _>>()?;
    metrics::event(
        name,
        "decoder",
        json!({"backends": decoders.iter().map(|d| d.backend().to_owned()).collect::<Vec<_>>()}),
    );
    // Newest pending group per rendition, as the client hub keeps them.
    let (offer, mut offers) = tokio::sync::mpsc::unbounded_channel::<GroupReader>();
    let quic = peer.conn.quic().clone();
    let role = name.to_owned();
    tokio::spawn(async move {
        let mut newest: [Option<u32>; 2] = [None, None];
        while let Ok(incoming) = quic.accept_uni().await {
            match tokio::time::timeout(Duration::from_secs(1), GroupReader::from_stream(incoming))
                .await
            {
                Ok(Ok(reader)) => {
                    let lane = usize::from(reader.header.rendition());
                    let id = reader.header.group_id;
                    if newest[lane].is_none_or(|old| id.wrapping_sub(old).cast_signed() > 0) {
                        if let Some(old) = newest[lane]
                            && id.wrapping_sub(old) != 1
                        {
                            metrics::event(
                                &role,
                                "group_gap",
                                json!({"rendition": lane, "from": old, "to": id}),
                            );
                        }
                        newest[lane] = Some(id);
                        metrics::event(
                            &role,
                            "group",
                            json!({"rendition": lane, "id": id, "ts": reader.header.media.timestamp}),
                        );
                        if offer.send(reader).is_err() {
                            break;
                        }
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
    let mut switcher = Switcher::default();
    let mut window = Window::new(name);
    let mut shown: Option<u8> = None;
    let mut bytes = [0_u64; 2];
    let mut lost = 0_u64;
    let mut last_request: Option<Instant> = None;
    let deadline = started + Duration::from_secs(options.seconds);
    while Instant::now() < deadline {
        let steps = tokio::select! {
            Some(group) = offers.recv() => { switcher.offer(group); continue; }
            steps = switcher.next() => steps,
            () = tokio::time::sleep(Duration::from_millis(200)) => { window.tick(); continue; }
        };
        for step in steps {
            let Step::Frame {
                rendition,
                frame,
                restart,
            } = step
            else {
                lost += 1;
                if last_request.is_none_or(|t| t.elapsed() > Duration::from_millis(500)) {
                    last_request = Some(Instant::now());
                    let _ = peer
                        .call(Cmd::RequestKeyframe(v1::RequestKeyframe {
                            track_id: info.track,
                            rendition: 0,
                        }))
                        .await;
                }
                continue;
            };
            let index = usize::from(rendition.min(1));
            bytes[index] += u64::try_from(frame.payload.len()).unwrap_or(0);
            window.bytes(frame.payload.len());
            let decoder = &mut decoders[index.min(info.codecs.len() - 1)];
            if restart {
                decoder.reset()?;
            }
            match decoder.decode(&frame.payload, frame.timestamp, frame.keyframe) {
                Ok(pictures) => {
                    for picture in pictures {
                        let barcode = source::read_frame(&picture, stamp::now_micros());
                        let size = picture.size();
                        window.frame((size.width, size.height), barcode);
                        if shown != Some(rendition) {
                            metrics::event(
                                name,
                                "rendition",
                                json!({"rendition": rendition, "width": size.width, "height": size.height}),
                            );
                            shown = Some(rendition);
                        }
                        println!(
                            "{}",
                            json!({"t_ms": stamp::now_micros() / 1000, "role": name, "f": rendition,
                                   "lat": barcode.map(|(m, _)| stamp::now_micros().saturating_sub(m) / 1000)})
                        );
                    }
                }
                Err(e) => {
                    metrics::event(name, "decode_error", json!({"error": e.to_string()}));
                    decoder.recover()?;
                }
            }
        }
        window.extra.insert("rendition".into(), json!(shown));
        window.extra.insert("bytes".into(), json!(bytes));
        window.extra.insert("lost".into(), json!(lost));
        window
            .extra
            .insert("rtt_ms".into(), json!(peer.conn.quic().rtt().as_millis()));
        window.tick();
    }
    Ok(())
}
