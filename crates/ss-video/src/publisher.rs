//! Capture, scale, and encode one selected source.

use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::{Error, Preset, Source, h264};

/// Publisher settings chosen by the host UI.
#[derive(Clone, Debug)]
pub struct PublisherConfig {
    /// Display or window to capture.
    pub source: Source,
    /// Output resolution, frame rate, and bitrate.
    pub preset: Preset,
    /// Whether the captured picture includes the cursor.
    pub cursor: bool,
    /// Require hardware rather than allowing OpenH264 fallback.
    pub hardware_only: bool,
}

/// Codec metadata sent in `StartStream`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecInfo {
    /// WebCodecs-style codec string, such as `avc3.640028`.
    pub name: String,
    /// AVC decoder configuration, when the encoder supplies one.
    pub description: Bytes,
}

/// One H.264 Annex-B access unit ready for SocialSpeak transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncodedFrame {
    /// Timestamp on the shared 48 kHz media clock.
    pub timestamp: u32,
    /// True for an IDR carrying in-band parameter sets.
    pub keyframe: bool,
    /// Complete Annex-B access unit.
    pub payload: Bytes,
}

/// Rolling encoder diagnostics.
#[derive(Clone, Debug, Default)]
pub struct EncoderStats {
    /// Native backend selected by `moq-video`.
    pub backend: String,
    /// Frames emitted since opening.
    pub frames: u64,
    /// Encoded bytes emitted since opening.
    pub bytes: u64,
    /// Frames per second over the session lifetime.
    pub fps: f64,
    /// Bits per second over the session lifetime.
    pub bitrate: f64,
    /// Most recent encode duration.
    pub encode_time: Duration,
    /// Slowest encode duration seen.
    pub max_encode_time: Duration,
}

/// Pull-driven screen capture and encoder pipeline.
pub struct Publisher {
    capture: moq_video::capture::Stream,
    encoder: moq_video::encode::Sink,
    size: moq_video::Size,
    started: Instant,
    force_keyframe: bool,
    waiting_for_first_keyframe: bool,
    stats: EncoderStats,
}

impl Publisher {
    /// Open capture and the best native H.264 encoder, then probe exact codec metadata.
    pub async fn open(config: PublisherConfig) -> Result<(Self, CodecInfo), Error> {
        let rate = moq_video::Rate::new(config.preset.fps(), 1)?;
        let mut capture_config = moq_video::capture::Config::default();
        capture_config.source = config.source.native();
        capture_config.width = Some(config.preset.width());
        capture_config.height = Some(config.preset.height());
        capture_config.framerate = Some(rate);
        capture_config.cursor = config.cursor;
        let capture = moq_video::capture::open(&capture_config).await?;

        let mut encoder_config =
            moq_video::encode::Config::new(config.preset.width(), config.preset.height(), rate);
        encoder_config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(config.preset.bitrate()));
        encoder_config.kind = if config.hardware_only {
            moq_video::encode::Kind::Hardware
        } else {
            moq_video::encode::Kind::Auto
        };
        let codec = encoder_config.probe().await?;
        let encoder = moq_video::encode::Sink::open(&encoder_config).await?;
        let backend = encoder.name().to_owned();
        let info = CodecInfo {
            name: codec.codec.to_string(),
            description: codec.description.unwrap_or_default(),
        };
        Ok((
            Self {
                capture,
                encoder,
                size: encoder_config.size(),
                started: Instant::now(),
                force_keyframe: false,
                waiting_for_first_keyframe: true,
                stats: EncoderStats {
                    backend,
                    ..EncoderStats::default()
                },
            },
            info,
        ))
    }

    /// Force the next submitted picture to open a new IDR group.
    pub fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Capture and encode the next picture. `None` means no access unit was ready yet.
    pub async fn next(&mut self) -> Result<Option<Vec<EncodedFrame>>, Error> {
        let frame = self.capture.read().await?.ok_or(Error::SourceEnded)?;
        let frame = if frame.size() == self.size {
            frame
        } else {
            frame.resize(self.size, &moq_video::resize::Config::default())?
        };
        if self.force_keyframe {
            self.encoder.cut().await?;
            self.force_keyframe = false;
        }
        let began = Instant::now();
        let encoded = self.encoder.encode(frame).await?;
        let encode_time = began.elapsed();
        if encoded.is_empty() {
            return Ok(None);
        }
        let frames = encoded
            .into_iter()
            .map(|frame| EncodedFrame {
                timestamp: clock_48k(frame.timestamp.as_micros()),
                keyframe: h264::is_idr(&frame.payload),
                payload: frame.payload,
            })
            .collect::<Vec<_>>();
        if self.waiting_for_first_keyframe && !frames.iter().any(|frame| frame.keyframe) {
            return Err(Error::MissingKeyframe);
        }
        self.waiting_for_first_keyframe = false;
        self.record(&frames, encode_time);
        Ok(Some(frames))
    }

    /// Current encoder diagnostics.
    #[must_use]
    pub fn stats(&self) -> EncoderStats {
        self.stats.clone()
    }

    #[allow(clippy::cast_precision_loss)]
    fn record(&mut self, frames: &[EncodedFrame], encode_time: Duration) {
        self.stats.frames = self
            .stats
            .frames
            .saturating_add(u64::try_from(frames.len()).unwrap_or(u64::MAX));
        let bytes = frames.iter().fold(0_u64, |total, frame| {
            total.saturating_add(u64::try_from(frame.payload.len()).unwrap_or(u64::MAX))
        });
        self.stats.bytes = self.stats.bytes.saturating_add(bytes);
        self.stats.encode_time = encode_time;
        self.stats.max_encode_time = self.stats.max_encode_time.max(encode_time);
        let elapsed = self.started.elapsed().as_secs_f64().max(f64::EPSILON);
        self.stats.fps = self.stats.frames as f64 / elapsed;
        self.stats.bitrate = self.stats.bytes as f64 * 8.0 / elapsed;
    }
}

fn clock_48k(micros: u128) -> u32 {
    let ticks = micros.saturating_mul(48) / 1_000;
    let wrapped = ticks % (u128::from(u32::MAX) + 1);
    u32::try_from(wrapped).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn forced_keyframe(kind: moq_video::encode::Kind) -> Duration {
        let rate = moq_video::Rate::new(30, 1).unwrap();
        let mut config = moq_video::encode::Config::new(320, 180, rate);
        config.kind = kind;
        let mut encoder = moq_video::encode::Sink::open(&config).await.unwrap();
        let size = config.size();
        let pixels = vec![0x80; moq_video::I420::len(size).unwrap()];
        let first = moq_video::Frame::new(
            moq_video::Surface::I420(moq_video::I420::new(size, pixels.clone()).unwrap()),
            moq_net::Timestamp::from_millis(0).unwrap(),
        );
        let initial = encoder.encode(first).await.unwrap();
        assert!(initial.iter().any(|frame| h264::is_idr(&frame.payload)));

        encoder.cut().await.unwrap();
        let next = moq_video::Frame::new(
            moq_video::Surface::I420(moq_video::I420::new(size, pixels).unwrap()),
            moq_net::Timestamp::from_millis(33).unwrap(),
        );
        let began = Instant::now();
        let output = encoder.encode(next).await.unwrap();
        let elapsed = began.elapsed();
        assert!(output.iter().any(|frame| h264::is_idr(&frame.payload)));
        elapsed
    }

    #[test]
    fn timestamp_uses_shared_audio_clock_and_wraps() {
        assert_eq!(clock_48k(1_000_000), 48_000);
        assert_eq!(clock_48k(20_000), 960);
        let wrapping = ((u128::from(u32::MAX) + 1) * 1_000).div_ceil(48);
        assert!(clock_48k(wrapping) < 48);
    }

    #[tokio::test]
    async fn software_encoder_forces_an_inline_idr() {
        let elapsed = forced_keyframe(moq_video::encode::Kind::Software).await;
        assert!(elapsed < Duration::from_millis(100));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "requires a VideoToolbox hardware encoder"]
    async fn videotoolbox_forces_idr_within_acceptance_budget() {
        let elapsed = forced_keyframe(moq_video::encode::Kind::Hardware).await;
        assert!(elapsed < Duration::from_millis(100));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[ignore = "hardware performance acceptance test"]
    async fn videotoolbox_sustains_1080p30_under_encode_budget() {
        let rate = moq_video::Rate::new(30, 1).unwrap();
        let mut config = moq_video::encode::Config::new(1920, 1080, rate);
        config.kind = moq_video::encode::Kind::Hardware;
        config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(4_000_000));
        let mut encoder = moq_video::encode::Sink::open(&config)
            .await
            .expect("VideoToolbox must sustain the 1080p30 acceptance preset");
        assert_eq!(encoder.name(), "videotoolbox");
        let size = config.size();
        let pixels = vec![0x80; moq_video::I420::len(size).unwrap()];
        let frame = std::sync::Arc::new(moq_video::Frame::new(
            moq_video::Surface::I420(moq_video::I420::new(size, pixels).unwrap()),
            moq_net::Timestamp::from_millis(0).unwrap(),
        ));
        let began = Instant::now();
        for _ in 0..90 {
            let output = encoder.encode(frame.clone()).await.unwrap();
            assert!(!output.is_empty());
        }
        let average = began.elapsed() / 90;
        eprintln!("VideoToolbox 1080p30 average encode: {average:?}");
        assert!(average < Duration::from_millis(15));
    }
}
