//! One frame source for both backends: the deterministic pattern (CPU I420) or
//! native screen capture (ScreenCaptureKit `CVPixelBuffer`, no copy). Every
//! frame carries the capture-time barcode.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use moq_video::{Frame, I420, Size, Surface};

use crate::{pattern, stamp};

/// What to capture.
#[derive(Clone, Debug)]
pub enum Content {
    /// Screen-like pattern (`pattern`) or the full-motion worst case (`pattern-hard`).
    Pattern { hard: bool },
    /// A display by native id; `None` is the main display.
    Screen(Option<String>),
}

impl std::str::FromStr for Content {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pattern" => Ok(Self::Pattern { hard: false }),
            "pattern-hard" => Ok(Self::Pattern { hard: true }),
            "screen" => Ok(Self::Screen(None)),
            other => match other.strip_prefix("screen:") {
                Some(id) => Ok(Self::Screen(Some(id.to_owned()))),
                None => bail!("content is pattern, pattern-hard, screen or screen:<display id>"),
            },
        }
    }
}

/// A stamped frame plus the microsecond wall time written into it.
pub struct Stamped {
    pub frame: Frame,
    pub micros: u64,
    pub seq: u16,
}

pub struct FrameSource {
    size: Size,
    started: Instant,
    seq: u64,
    pattern: Option<(tokio::time::Interval, bool)>,
    capture: Option<moq_video::capture::Stream>,
}

impl FrameSource {
    pub async fn open(content: &Content, width: u32, height: u32, fps: u32) -> Result<Self> {
        let size = Size::new(width, height);
        match content {
            Content::Pattern { hard } => {
                let mut tick = tokio::time::interval(Duration::from_secs(1) / fps);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                Ok(Self {
                    size,
                    started: Instant::now(),
                    seq: 0,
                    pattern: Some((tick, *hard)),
                    capture: None,
                })
            }
            Content::Screen(id) => {
                // Same capture settings as ss_video::Publisher::open.
                let mut config = moq_video::capture::Config::default();
                config.source = moq_video::capture::Source::Display(id.clone());
                config.width = Some(width);
                config.height = Some(height);
                config.framerate = Some(moq_video::Rate::new(fps, 1)?);
                config.cursor = true;
                let capture = moq_video::capture::open(&config)
                    .await
                    .context("open screen capture")?;
                Ok(Self {
                    size,
                    started: Instant::now(),
                    seq: 0,
                    pattern: None,
                    capture: Some(capture),
                })
            }
        }
    }

    pub async fn next(&mut self) -> Result<Stamped> {
        let n = self.seq;
        self.seq += 1;
        let seq = (n & 0xffff) as u16;
        let (w, h) = (self.size.width as usize, self.size.height as usize);
        if let Some((tick, hard)) = &mut self.pattern {
            tick.tick().await;
            let mut data = vec![0_u8; w * h * 3 / 2];
            pattern::fill(&mut data, w, h, n, *hard);
            let micros = stamp::now_micros();
            stamp::write(&mut data, w, w, micros, seq);
            let timestamp = moq_net::Timestamp::from_micros(
                u64::try_from(self.started.elapsed().as_micros()).unwrap_or(0),
            )?;
            let frame = Frame::new(Surface::I420(I420::new(self.size, data)?), timestamp);
            return Ok(Stamped { frame, micros, seq });
        }
        let capture = self.capture.as_mut().expect("capture or pattern");
        let mut frame = capture.read().await?.context("capture ended")?;
        if frame.size() != self.size {
            frame = frame.resize(self.size, &moq_video::resize::Config::default())?;
        }
        let micros = stamp::now_micros();
        let frame = match frame.surface {
            Surface::PixelBuffer(pixels) => {
                crate::pixels::with_luma(pixels.buffer(), true, |luma, stride, width| {
                    stamp::write(luma, stride, width, micros, seq);
                })
                .context("stamp pixel buffer")?;
                Frame::new(Surface::PixelBuffer(pixels), frame.timestamp)
            }
            other => {
                let i420 = other.into_i420()?;
                let mut data = i420.into_data();
                stamp::write(&mut data, w, w, micros, seq);
                Frame::new(Surface::I420(I420::new(self.size, data)?), frame.timestamp)
            }
        };
        Ok(Stamped { frame, micros, seq })
    }
}

/// Read a barcode from a decoded moq-video frame.
pub fn read_frame(frame: &Frame, now: u64) -> Option<(u64, u16)> {
    match &frame.surface {
        Surface::PixelBuffer(pixels) => {
            crate::pixels::with_luma(pixels.buffer(), false, |luma, stride, width| {
                stamp::read(luma, stride, width, now)
            })
            .flatten()
        }
        Surface::I420(i420) => {
            let w = i420.width() as usize;
            stamp::read(i420.data(), w, w, now)
        }
        #[allow(unreachable_patterns)]
        _ => None,
    }
}
