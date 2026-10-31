//! Far-end reference for the echo canceller.
//!
//! The playback worker pushes each finished mixer frame before it writes the
//! playback ring. The capture worker drains whatever arrived before it
//! processes a capture frame. Neither side allocates, locks, or blocks.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::mixer::StereoFrame;

/// Most far-end frames handed to one [`super::Chain::process`] call. Older
/// frames beyond this are discarded as stale and counted.
pub const MAX_RENDER_BATCH: usize = 8;

/// Create a tap that queues up to `frames` far-end frames (at least
/// [`MAX_RENDER_BATCH`]). Allocates; call it during session setup.
#[must_use]
pub fn render_tap(frames: usize) -> (RenderSender, RenderReceiver) {
    let (producer, consumer) = RingBuffer::new(frames.max(MAX_RENDER_BATCH));
    let dropped = Arc::new(AtomicU64::new(0));
    (
        RenderSender {
            producer,
            dropped: Arc::clone(&dropped),
        },
        RenderReceiver {
            consumer,
            batch: vec![[0.0; crate::mixer::SAMPLES_PER_CHANNEL * 2]; MAX_RENDER_BATCH]
                .into_boxed_slice(),
            dropped,
        },
    )
}

/// Playback-worker end of the tap.
pub struct RenderSender {
    producer: Producer<StereoFrame>,
    dropped: Arc<AtomicU64>,
}

impl RenderSender {
    /// Queue a copy of the mixer output. Returns false, and counts a drop,
    /// when the capture worker has fallen behind and the queue is full.
    pub fn push(&mut self, frame: &StereoFrame) -> bool {
        if self.producer.push(*frame).is_ok() {
            true
        } else {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            false
        }
    }

    /// Frames lost to a full queue or discarded as stale.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// Capture-worker end of the tap.
pub struct RenderReceiver {
    consumer: Consumer<StereoFrame>,
    batch: Box<[StereoFrame]>,
    dropped: Arc<AtomicU64>,
}

impl RenderReceiver {
    /// Frames waiting. Each is 10 ms of the [`super::EchoPath`] playback queue.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.consumer.slots()
    }

    /// Take every waiting frame, oldest first. When more than
    /// [`MAX_RENDER_BATCH`] are waiting, the oldest excess is discarded.
    pub fn drain(&mut self) -> &[StereoFrame] {
        let waiting = self.consumer.slots();
        let stale = waiting.saturating_sub(MAX_RENDER_BATCH);
        for _ in 0..stale {
            let _ = self.consumer.pop();
        }
        if stale > 0 {
            self.dropped.fetch_add(stale as u64, Ordering::Relaxed);
        }
        let mut count = 0;
        while count < self.batch.len() {
            match self.consumer.pop() {
                Ok(frame) => {
                    self.batch[count] = frame;
                    count += 1;
                }
                Err(_) => break,
            }
        }
        &self.batch[..count]
    }

    /// Frames lost to a full queue or discarded as stale.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}
