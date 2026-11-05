//! Thread-confined native decoding and wrap-safe video scheduling.
use crate::{Error, Frame};
use bytes::Bytes;
use moq_video::decode::{Config, Decoder, Kind};
use std::time::{Duration, Instant};

/// One decoder owned, used and dropped on the viewer thread.
pub struct VideoDecoder {
    catalog: hang::catalog::VideoConfig,
    config: Config,
    decoder: Decoder,
}
impl VideoDecoder {
    /// Open H.264 with native output (hardware preferred, software fallback).
    pub fn open(name: &str, description: Bytes, software: bool) -> Result<Self, Error> {
        let codec: hang::catalog::VideoCodec = name
            .parse()
            .map_err(|_| moq_video::Error::UnsupportedCodec(name.into()))?;
        let mut catalog = hang::catalog::VideoConfig::new(codec);
        catalog.description = (!description.is_empty()).then_some(description);
        let mut config = Config::new();
        if software {
            config.kind = Kind::Software;
        }
        let decoder = Decoder::new(&catalog, &config)?;
        Ok(Self {
            catalog,
            config,
            decoder,
        })
    }
    /// Selected backend for diagnostics.
    pub fn backend(&self) -> &str {
        self.decoder.name()
    }
    /// Reset reference pictures. The next accepted picture must be a keyframe.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.decoder = Decoder::new(&self.catalog, &self.config)?;
        Ok(())
    }
    /// Recover from a hardware decode failure through the software backend.
    pub fn recover(&mut self) -> Result<(), Error> {
        self.config.kind = Kind::Software;
        self.reset()
    }
    /// Decode a complete access unit; no transport or extra queue is hidden here.
    pub fn decode(
        &mut self,
        bytes: &Bytes,
        timestamp: u32,
        keyframe: bool,
    ) -> Result<Vec<Frame>, Error> {
        let time = moq_net::Timestamp::from_micros(u64::from(timestamp) * 1_000_000 / 48_000)
            .expect("u32 timestamp fits");
        Ok(self.decoder.decode(bytes, time, keyframe)?)
    }
}
/// Signed distance on the wrapping 48 kHz timestamp base (less than half a cycle).
#[must_use]
pub const fn delta(video: u32, audio: u32) -> i32 {
    video.wrapping_sub(audio).cast_signed()
}
/// Whether to present, wait briefly, or discard an obsolete picture.
#[derive(Debug, PartialEq, Eq)]
pub enum Timing {
    Present,
    Wait(Duration),
    Drop,
}
/// Schedule against an actual audio render sample when available. Without audio,
/// preserve sender cadence relative to the first arriving frame, never wall time.
#[derive(Default)]
pub struct Playout {
    anchor: Option<(u32, Instant)>,
}
impl Playout {
    /// Forget the old GOP's timing after reset/reconnect.
    pub fn reset(&mut self) {
        self.anchor = None;
    }
    /// Keep skew within 60 ms; cap stale/no-audio timing rather than queue seconds.
    pub fn timing(&mut self, timestamp: u32, now: Instant, audio: Option<u32>) -> Timing {
        let reference = audio.unwrap_or_else(|| {
            let (stamp, at) = *self.anchor.get_or_insert((timestamp, now));
            stamp.wrapping_add(
                u32::try_from(
                    (now.saturating_duration_since(at).as_micros() * 48 / 1000) % (1_u128 << 32),
                )
                .expect("modulo u32"),
            )
        });
        match delta(timestamp, reference) {
            n if n < -2880 => Timing::Drop,
            n if n > 0 => Timing::Wait(
                Duration::from_micros(u64::try_from(n).unwrap_or(0) * 1000 / 48)
                    .min(Duration::from_millis(60)),
            ),
            _ => Timing::Present,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scheduling_handles_wrap_lateness_audio_and_reset() {
        let now = Instant::now();
        let mut p = Playout::default();
        assert_eq!(delta(5, u32::MAX - 4), 10);
        assert_eq!(p.timing(0, now, None), Timing::Present);
        assert_eq!(
            p.timing(0, now + Duration::from_millis(100), None),
            Timing::Drop
        );
        assert_eq!(
            p.timing(4800, now, Some(0)),
            Timing::Wait(Duration::from_millis(60))
        );
        p.reset();
        assert_eq!(p.timing(99, now, None), Timing::Present);
    }
    #[test]
    fn software_roundtrip_recovers_after_decoder_reset() {
        use moq_video::{Size, Surface, encode};
        let mut cfg = encode::Config::new(64, 64, moq_video::Rate::new(30, 1).unwrap());
        cfg.kind = encode::Kind::Software;
        let mut encoder = encode::Encoder::new(&cfg).unwrap();
        let mut decoder = VideoDecoder::open("avc3.42001e", Bytes::new(), true).unwrap();
        for timestamp in [0, 4800] {
            encoder.cut().unwrap();
            decoder.reset().unwrap();
            assert!(
                decoder
                    .decode(&Bytes::from_static(&[0, 0, 1, 1]), timestamp, false)
                    .unwrap()
                    .is_empty()
            );
            let surface = Surface::rgba(&vec![128; 64 * 64 * 4], Size::new(64, 64)).unwrap();
            let frame = Frame::new(surface, moq_net::Timestamp::ZERO);
            let units = encoder.encode(&frame).unwrap();
            assert!(!units.is_empty());
            let pictures = decoder.decode(&units[0].payload, timestamp, true).unwrap();
            assert!(!pictures.is_empty());
            assert_eq!(pictures[0].size(), Size::new(64, 64));
        }
    }
}
