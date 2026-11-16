//! Capture, scale, and encode one selected source.

use crate::{
    Error, Source,
    renditions::{CodecInfo, EncodedFrame, EncoderConfig, EncoderStats, Encoders, LowRendition},
};

/// Publisher settings chosen by the host UI.
#[derive(Clone, Debug)]
pub struct PublisherConfig {
    /// Display or window to capture.
    pub source: Source,
    /// Output resolution, frame rate, and bitrate.
    pub preset: crate::Preset,
    /// Whether the captured picture includes the cursor.
    pub cursor: bool,
    /// Require hardware rather than allowing OpenH264 fallback.
    pub hardware_only: bool,
    /// Whether to offer a second, low rendition.
    pub low: LowRendition,
}

/// Pull-driven screen capture feeding the [`Encoders`] stage.
pub struct Publisher {
    capture: moq_video::capture::Stream,
    encoders: Encoders,
}

impl Publisher {
    /// Open capture and the native encoders, then probe exact codec metadata.
    pub async fn open(config: PublisherConfig) -> Result<(Self, CodecInfo), Error> {
        let rate = moq_video::Rate::new(config.preset.fps(), 1)?;
        let mut capture_config = moq_video::capture::Config::default();
        capture_config.source = config.source.native();
        capture_config.width = Some(config.preset.width());
        capture_config.height = Some(config.preset.height());
        capture_config.framerate = Some(rate);
        capture_config.cursor = config.cursor;
        let capture = moq_video::capture::open(&capture_config).await?;
        let mut encoder = EncoderConfig::new(config.preset);
        encoder.hardware_only = config.hardware_only;
        encoder.low = config.low;
        let encoders = Encoders::open(&encoder).await?;
        let info = encoders.renditions()[0].codec.clone();
        Ok((Self { capture, encoders }, info))
    }

    /// The encode stage: renditions, keyframes, bitrate, low on/off.
    pub fn encoders(&mut self) -> &mut Encoders {
        &mut self.encoders
    }

    /// Force the next picture of `rendition` to open a new IDR group.
    pub fn request_keyframe(&mut self) {
        self.encoders.request_keyframe(0);
    }

    /// Capture and encode the next picture. `None` means no access unit was ready yet.
    pub async fn next(&mut self) -> Result<Option<Vec<EncodedFrame>>, Error> {
        let frame = self.capture.read().await?.ok_or(Error::SourceEnded)?;
        let frames = self.encoders.encode(frame).await?;
        Ok((!frames.is_empty()).then_some(frames))
    }

    /// Current encoder diagnostics.
    #[must_use]
    pub fn stats(&self) -> EncoderStats {
        self.encoders.stats()
    }
}

#[cfg(test)]
mod tests {
    use crate::h264;
    use std::time::{Duration, Instant};

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
    #[ignore = "hardware performance acceptance test; run with --release"]
    async fn videotoolbox_two_sessions_hold_full_motion_1080p30() {
        let rate = moq_video::Rate::new(30, 1).unwrap();
        let mut config = moq_video::encode::Config::new(1920, 1080, rate);
        config.kind = moq_video::encode::Kind::Hardware;
        config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(4_000_000));
        let mut full = moq_video::encode::Sink::open(&config).await.unwrap();
        let low_rate = moq_video::Rate::new(15, 1).unwrap();
        let mut low_config = moq_video::encode::Config::new(640, 360, low_rate);
        low_config.kind = moq_video::encode::Kind::Hardware;
        low_config.bitrate = Some(moq_net::bandwidth::Rate::from_bps(300_000));
        let mut low = moq_video::encode::Sink::open(&low_config).await.unwrap();
        eprintln!("backends: {} + {}", full.name(), low.name());
        let size = config.size();
        // Full-frame motion: every macroblock changes every frame.
        let frames: Vec<_> = (0..30_u32)
            .map(|n| {
                let mut pixels = vec![0x80; moq_video::I420::len(size).unwrap()];
                for (i, p) in pixels.iter_mut().enumerate() {
                    let i = u32::try_from(i).unwrap();
                    *p = u8::try_from(i.wrapping_mul(2_654_435_761).wrapping_add(n * 97) >> 24)
                        .unwrap();
                }
                moq_video::I420::new(size, pixels).unwrap()
            })
            .collect();
        let began = Instant::now();
        let (mut full_bytes, mut low_bytes) = (0, 0);
        for n in 0..300_usize {
            let frame = std::sync::Arc::new(moq_video::Frame::new(
                moq_video::Surface::I420(frames[n % frames.len()].clone()),
                moq_net::Timestamp::from_millis(u64::try_from(n).unwrap() * 33).unwrap(),
            ));
            for unit in full.encode(frame.clone()).await.unwrap() {
                full_bytes += unit.payload.len();
            }
            if n % 2 == 0 {
                let small = frame
                    .resize(low_config.size(), &moq_video::resize::Config::default())
                    .unwrap();
                for unit in low.encode(small).await.unwrap() {
                    low_bytes += unit.payload.len();
                }
            }
        }
        let elapsed = began.elapsed();
        let fps = 300.0 / elapsed.as_secs_f64();
        eprintln!(
            "two sessions: {fps:.1} fps unpaced, full {} kB, low {} kB",
            full_bytes / 1000,
            low_bytes / 1000
        );
        assert!(fps >= 30.0);
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
