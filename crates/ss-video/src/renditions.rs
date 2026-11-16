//! The encode stage: one captured frame in, the full rendition and, while a
//! viewer is on it, the low rendition out.
//!
//! The low rendition is 640×360 at 15 fps, about 300 kb/s, scaled from the
//! same captured frame and encoded by a second hardware session. When that
//! session cannot open, the stage publishes the full rendition alone and says
//! so once in the log and in [`EncoderStats::low`].

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use bytes::Bytes;

use crate::{Error, Preset, h264};

/// Low rendition size, frame rate and bitrate.
pub const LOW_WIDTH: u32 = 640;
/// Low rendition height.
pub const LOW_HEIGHT: u32 = 360;
/// Low rendition frames per second.
pub const LOW_FPS: u32 = 15;
/// Low rendition target bitrate.
pub const LOW_BITRATE: u64 = 300_000;
/// Low rendition keyframe interval. With a 2 s GOP aligned to the full
/// rendition, keyframes took 21 % of the low rendition's bitrate on a
/// screen-like pattern and 59 % on a mostly static desktop; with 10 s and a
/// keyframe whenever a viewer switches to it, 7.5 % (T-074 measurements).
pub const LOW_GOP: Duration = Duration::from_secs(10);

/// Whether to offer a second, low rendition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LowRendition {
    /// Open a second hardware session if the machine has one.
    #[default]
    Auto,
    /// Publish one rendition, as if the second session were refused.
    Off,
}

/// Encode-stage settings.
#[derive(Clone, Debug)]
pub struct EncoderConfig {
    /// Full rendition: resolution, frame rate and bitrate ceiling.
    pub preset: Preset,
    /// Require hardware for the full rendition rather than allowing OpenH264.
    pub hardware_only: bool,
    /// Whether to offer the low rendition.
    pub low: LowRendition,
    /// The low rendition's own keyframe interval, [`LOW_GOP`] by default; it
    /// relies on a keyframe whenever a viewer switches to it. `None` cuts it
    /// on the same frames as the full rendition's keyframes (a 2 s GOP).
    pub low_gop: Option<Duration>,
}

impl EncoderConfig {
    /// The full rendition of `preset`, with the low rendition when available.
    #[must_use]
    pub fn new(preset: Preset) -> Self {
        Self {
            preset,
            hardware_only: false,
            low: LowRendition::Auto,
            low_gop: Some(LOW_GOP),
        }
    }
}

/// Codec metadata sent in `StartStream`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecInfo {
    /// WebCodecs-style codec string, such as `avc3.640028`.
    pub name: String,
    /// AVC decoder configuration, when the encoder supplies one.
    pub description: Bytes,
}

/// One rendition as announced with the track.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenditionInfo {
    /// 0 for the full rendition, 1 for the low one.
    pub index: u8,
    /// Encoded width.
    pub width: u32,
    /// Encoded height.
    pub height: u32,
    /// Encoded frame rate.
    pub fps: u32,
    /// Target (ceiling) bitrate.
    pub bitrate: u64,
    /// This rendition's codec string and configuration.
    pub codec: CodecInfo,
}

/// One H.264 Annex-B access unit ready for Sideband transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedFrame {
    /// Rendition it belongs to: 0 full, 1 low.
    pub rendition: u8,
    /// Timestamp on the shared 48 kHz media clock.
    pub timestamp: u32,
    /// True for an IDR carrying in-band parameter sets.
    pub keyframe: bool,
    /// Complete Annex-B access unit.
    pub payload: Bytes,
}

/// State of the low rendition.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LowState {
    /// Turned off by configuration.
    #[default]
    Off,
    /// The second encoder could not open; one rendition is published.
    Unavailable(String),
    /// Open, but no viewer is on it, so no frames are fed to it.
    Idle,
    /// Encoding for at least one viewer.
    Encoding,
}

/// Counters for one rendition since opening.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenditionCounters {
    /// Access units emitted.
    pub frames: u64,
    /// Bytes emitted.
    pub bytes: u64,
    /// Keyframes emitted.
    pub keyframes: u64,
    /// Bytes of those keyframes.
    pub keyframe_bytes: u64,
}

impl RenditionCounters {
    fn add(&mut self, frame: &EncodedFrame) {
        let bytes = u64::try_from(frame.payload.len()).unwrap_or(u64::MAX);
        self.frames += 1;
        self.bytes = self.bytes.saturating_add(bytes);
        if frame.keyframe {
            self.keyframes += 1;
            self.keyframe_bytes = self.keyframe_bytes.saturating_add(bytes);
        }
    }
}

/// Rolling encoder diagnostics.
#[derive(Clone, Debug, Default)]
pub struct EncoderStats {
    /// Native backend selected by `moq-video` for the full rendition.
    pub backend: String,
    /// Full-rendition frames emitted since opening.
    pub frames: u64,
    /// Full-rendition bytes emitted since opening.
    pub bytes: u64,
    /// Full-rendition frames per second over the session lifetime.
    pub fps: f64,
    /// Full-rendition bits per second over the session lifetime.
    pub bitrate: f64,
    /// Most recent encode duration, both renditions together.
    pub encode_time: Duration,
    /// Slowest encode duration seen.
    pub max_encode_time: Duration,
    /// Full rendition's current target bitrate.
    pub target_bitrate: u64,
    /// Per-rendition counters: full, low.
    pub renditions: [RenditionCounters; 2],
    /// Low rendition state.
    pub low: LowState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Keyframes {
    AlignedWithFull,
    OwnInterval,
}

struct Low {
    encoder: moq_video::encode::Sink,
    size: moq_video::Size,
    /// Fed only while a viewer is on the low rendition.
    wanted: bool,
    cut: bool,
    /// Cut on the full rendition's keyframes rather than on its own interval.
    keyframes: Keyframes,
    next_at: Option<u128>,
    waiting_for_keyframe: bool,
}

/// Full rendition plus the optional low one, fed with captured frames.
pub struct Encoders {
    full: moq_video::encode::Sink,
    size: moq_video::Size,
    low: Option<Low>,
    force_keyframe: bool,
    waiting_for_first_keyframe: bool,
    started: Instant,
    stats: EncoderStats,
    renditions: Vec<RenditionInfo>,
}

impl Encoders {
    /// Open the full encoder, then probe a second hardware session for the
    /// low rendition. Returns the renditions to announce, full first.
    pub async fn open(config: &EncoderConfig) -> Result<Self, Error> {
        let preset = config.preset;
        let rate = moq_video::Rate::new(preset.fps(), 1)?;
        let mut full_config = moq_video::encode::Config::new(preset.width(), preset.height(), rate);
        full_config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(preset.bitrate()));
        full_config.kind = if config.hardware_only {
            moq_video::encode::Kind::Hardware
        } else {
            moq_video::encode::Kind::Auto
        };
        let codec = full_config.probe().await?;
        let full = moq_video::encode::Sink::open(&full_config).await?;
        let mut renditions = vec![RenditionInfo {
            index: 0,
            width: preset.width(),
            height: preset.height(),
            fps: preset.fps(),
            bitrate: preset.bitrate(),
            codec: CodecInfo {
                name: codec.codec.to_string(),
                description: codec.description.unwrap_or_default(),
            },
        }];
        let (low, state) = match config.low {
            LowRendition::Off => (None, LowState::Off),
            LowRendition::Auto => match open_low(config.low_gop).await {
                Ok((low, info)) => {
                    renditions.push(info);
                    (Some(low), LowState::Idle)
                }
                Err(error) => {
                    tracing::warn!(%error, "second video encoder unavailable; publishing one rendition");
                    (None, LowState::Unavailable(error.to_string()))
                }
            },
        };
        Ok(Self {
            stats: EncoderStats {
                backend: full.name().to_owned(),
                target_bitrate: preset.bitrate(),
                low: state,
                ..EncoderStats::default()
            },
            full,
            size: full_config.size(),
            low,
            force_keyframe: false,
            waiting_for_first_keyframe: true,
            started: Instant::now(),
            renditions,
        })
    }

    /// Renditions to announce with the track, full first.
    #[must_use]
    pub fn renditions(&self) -> &[RenditionInfo] {
        &self.renditions
    }

    /// Force a keyframe on one rendition's next frame.
    pub fn request_keyframe(&mut self, rendition: u8) {
        match (rendition, self.low.as_mut()) {
            (0, _) => self.force_keyframe = true,
            (_, Some(low)) => low.cut = true,
            (_, None) => {}
        }
    }

    /// Feed the low rendition only while the server reports a viewer on it.
    /// Resuming starts with a keyframe.
    pub fn set_low_wanted(&mut self, wanted: bool) {
        if let Some(low) = self.low.as_mut() {
            if wanted && !low.wanted {
                low.cut = true;
                low.waiting_for_keyframe = true;
                low.next_at = None;
            }
            low.wanted = wanted;
            self.stats.low = if wanted {
                LowState::Encoding
            } else {
                LowState::Idle
            };
        }
    }

    /// Retune the full rendition's average bitrate. Never forces a keyframe.
    pub async fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
        self.full
            .set_bitrate(moq_net::bandwidth::Rate::from_bps(bitrate))
            .await?;
        self.stats.target_bitrate = bitrate;
        Ok(())
    }

    /// Encode one captured picture into every active rendition.
    pub async fn encode(&mut self, frame: moq_video::Frame) -> Result<Vec<EncodedFrame>, Error> {
        let began = Instant::now();
        let frame = Arc::new(frame);
        let micros = frame.timestamp.as_micros();
        let full_frame = if frame.size() == self.size {
            Arc::clone(&frame)
        } else {
            Arc::new(frame.resize(self.size, &moq_video::resize::Config::default())?)
        };
        if self.force_keyframe {
            self.full.cut().await?;
            self.force_keyframe = false;
        }
        let mut out: Vec<EncodedFrame> = self
            .full
            .encode(full_frame)
            .await?
            .into_iter()
            .map(|unit| encoded(0, &unit))
            .collect();
        if self.waiting_for_first_keyframe && !out.is_empty() {
            if !out.iter().any(|frame| frame.keyframe) {
                return Err(Error::MissingKeyframe);
            }
            self.waiting_for_first_keyframe = false;
        }
        let full_keyframe = out.iter().any(|frame| frame.keyframe);
        if let Some(low) = self.low.as_mut().filter(|low| low.wanted) {
            if full_keyframe && low.keyframes == Keyframes::AlignedWithFull {
                low.cut = true;
            }
            // Decimate by capture time: one low frame per 1/15 s, tolerant of
            // a capture cadence that wanders by up to half a full frame.
            let period = 1_000_000 / u128::from(LOW_FPS);
            if low.next_at.is_none_or(|next| micros >= next) {
                let slack = 500_000 / u128::from(self.renditions[0].fps);
                low.next_at = Some(micros + period - slack.min(period / 2));
                let small = frame.resize(low.size, &moq_video::resize::Config::default())?;
                if low.cut {
                    low.encoder.cut().await?;
                    low.cut = false;
                }
                for unit in low.encoder.encode(small).await? {
                    let frame = encoded(1, &unit);
                    if low.waiting_for_keyframe && !frame.keyframe {
                        continue;
                    }
                    low.waiting_for_keyframe = false;
                    out.push(frame);
                }
            }
        }
        self.record(&out, began.elapsed());
        Ok(out)
    }

    /// Current encoder diagnostics.
    #[must_use]
    pub fn stats(&self) -> EncoderStats {
        self.stats.clone()
    }

    #[allow(clippy::cast_precision_loss)]
    fn record(&mut self, frames: &[EncodedFrame], encode_time: Duration) {
        for frame in frames {
            self.stats.renditions[usize::from(frame.rendition.min(1))].add(frame);
        }
        let full = self.stats.renditions[0];
        self.stats.frames = full.frames;
        self.stats.bytes = full.bytes;
        self.stats.encode_time = encode_time;
        self.stats.max_encode_time = self.stats.max_encode_time.max(encode_time);
        let elapsed = self.started.elapsed().as_secs_f64().max(f64::EPSILON);
        self.stats.fps = full.frames as f64 / elapsed;
        self.stats.bitrate = full.bytes as f64 * 8.0 / elapsed;
    }
}

async fn open_low(gop: Option<Duration>) -> Result<(Low, RenditionInfo), Error> {
    let rate = moq_video::Rate::new(LOW_FPS, 1)?;
    let mut config = moq_video::encode::Config::new(LOW_WIDTH, LOW_HEIGHT, rate);
    config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(LOW_BITRATE));
    // Only a second hardware session: a software one would cost the CPU the
    // full rendition's hardware encoder saves.
    config.kind = moq_video::encode::Kind::Hardware;
    // Aligned: the full rendition's keyframes cut this one, so its own
    // interval only has to be longer than theirs.
    config.gop =
        moq_video::encode::Gop::keyframe_every(gop.unwrap_or(Duration::from_secs(600)), rate);
    let codec = config.probe().await?;
    let encoder = moq_video::encode::Sink::open(&config).await?;
    let info = RenditionInfo {
        index: 1,
        width: LOW_WIDTH,
        height: LOW_HEIGHT,
        fps: LOW_FPS,
        bitrate: LOW_BITRATE,
        codec: CodecInfo {
            name: codec.codec.to_string(),
            description: codec.description.unwrap_or_default(),
        },
    };
    Ok((
        Low {
            encoder,
            size: config.size(),
            wanted: false,
            cut: true,
            keyframes: if gop.is_none() {
                Keyframes::AlignedWithFull
            } else {
                Keyframes::OwnInterval
            },
            next_at: None,
            waiting_for_keyframe: true,
        },
        info,
    ))
}

fn encoded(rendition: u8, unit: &moq_video::encode::Encoded) -> EncodedFrame {
    EncodedFrame {
        rendition,
        timestamp: clock_48k(unit.timestamp.as_micros()),
        keyframe: h264::is_idr(&unit.payload),
        payload: unit.payload.clone(),
    }
}

/// Position on the wrapping 48 kHz clock shared with voice.
pub(crate) fn clock_48k(micros: u128) -> u32 {
    let ticks = micros.saturating_mul(48) / 1_000;
    let wrapped = ticks % (u128::from(u32::MAX) + 1);
    u32::try_from(wrapped).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_uses_shared_audio_clock_and_wraps() {
        assert_eq!(clock_48k(1_000_000), 48_000);
        assert_eq!(clock_48k(20_000), 960);
        let wrapping = ((u128::from(u32::MAX) + 1) * 1_000).div_ceil(48);
        assert!(clock_48k(wrapping) < 48);
    }

    fn pattern(n: u32, size: moq_video::Size) -> moq_video::Frame {
        let mut pixels = vec![0x80; moq_video::I420::len(size).unwrap()];
        for (i, p) in pixels.iter_mut().enumerate().step_by(7) {
            *p = u8::try_from((u32::try_from(i).unwrap() / 640 + n * 5) % 251).unwrap();
        }
        moq_video::Frame::new(
            moq_video::Surface::I420(moq_video::I420::new(size, pixels).unwrap()),
            moq_net::Timestamp::from_millis(u64::from(n) * 1000 / 30).unwrap(),
        )
    }

    async fn run(encoders: &mut Encoders, frames: std::ops::Range<u32>) -> Vec<EncodedFrame> {
        let size = moq_video::Size::new(1280, 720);
        let mut out = Vec::new();
        for n in frames {
            out.extend(encoders.encode(pattern(n, size)).await.unwrap());
        }
        out
    }

    // The software encoder stands in for the full rendition; the low one is
    // hardware-only, so this machine may or may not offer it.
    #[cfg(feature = "software")]
    #[tokio::test]
    async fn refused_second_encoder_publishes_one_rendition() {
        let mut config = EncoderConfig::new(Preset::Hd30);
        config.low = LowRendition::Off;
        let mut encoders = Encoders::open(&config).await.unwrap();
        assert_eq!(encoders.renditions().len(), 1);
        assert_eq!(encoders.stats().low, LowState::Off);
        encoders.set_low_wanted(true);
        encoders.request_keyframe(1);
        let out = run(&mut encoders, 0..10).await;
        assert!(out.iter().all(|f| f.rendition == 0));
        assert!(out.first().is_some_and(|f| f.keyframe));
        // A bitrate change keeps the GOP: no keyframe beyond the first.
        encoders.set_bitrate(500_000).await.unwrap();
        let after = run(&mut encoders, 10..20).await;
        assert!(after.iter().all(|f| !f.keyframe));
        assert_eq!(encoders.stats().target_bitrate, 500_000);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires two VideoToolbox sessions"]
    async fn low_rendition_runs_only_while_wanted_and_starts_with_a_keyframe() {
        let mut config = EncoderConfig::new(Preset::Hd30);
        config.low_gop = None;
        let mut encoders = Encoders::open(&config).await.unwrap();
        assert_eq!(encoders.renditions().len(), 2, "{:?}", encoders.stats().low);
        let idle = run(&mut encoders, 0..10).await;
        assert!(idle.iter().all(|f| f.rendition == 0));
        encoders.set_low_wanted(true);
        let low: Vec<_> = run(&mut encoders, 10..70)
            .await
            .into_iter()
            .filter(|f| f.rendition == 1)
            .collect();
        assert!(low[0].keyframe);
        // 15 of 30 frames per second, with the full rendition's keyframe at
        // frame 60 cutting the low one on the same capture time.
        assert!((28..=32).contains(&low.len()), "{}", low.len());
        let full_key = clock_48k(60 * 1_000_000 / 30);
        assert!(low.iter().any(|f| f.keyframe && f.timestamp == full_key));
        encoders.set_low_wanted(false);
        let off = run(&mut encoders, 70..80).await;
        assert!(off.iter().all(|f| f.rendition == 0));
    }
}
