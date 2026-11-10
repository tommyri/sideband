//! Sample-format and channel conversion used by the device callback.
//!
//! These functions do not allocate. The rings and the pending frame buffer
//! are created before the stream starts.

use std::sync::atomic::Ordering;

use cpal::{FromSample, Sample};
use rtrb::{Consumer, Producer};

use super::Counters;

/// Write device samples into `pending` and flush whole 10 ms frames to `output`.
pub(crate) struct Framer {
    pending: Vec<f32>,
    filled: usize,
}

impl Framer {
    pub(crate) fn new(frame_samples: usize) -> Self {
        Self {
            pending: vec![0.0; frame_samples],
            filled: 0,
        }
    }

    /// Append interleaved graph-rate samples. Whole frames are pushed to `output`.
    pub(crate) fn push(
        &mut self,
        samples: &[f32],
        output: &mut Producer<f32>,
        counters: &Counters,
    ) {
        for sample in samples {
            self.pending[self.filled] = *sample;
            self.filled += 1;
            if self.filled == self.pending.len() {
                flush_frame(&self.pending, output, counters);
                self.filled = 0;
            }
        }
    }
}

/// Record whether a capture buffer holds anything but digital zero.
///
/// Returns `true` once a sample differs from the format's equilibrium. The
/// callback stops calling this after the first `true`, so a live stream pays
/// for one scan.
pub(crate) fn note_signal<T: Sample>(input: &[T], counters: &Counters) -> bool {
    if input.iter().any(|sample| *sample != T::EQUILIBRIUM) {
        counters.signal.store(true, Ordering::Release);
        true
    } else {
        false
    }
}

/// Convert one callback buffer and push interleaved f32 samples immediately.
///
/// Used when a worker, not the callback, has to resample before framing.
pub(crate) fn push_unframed<T>(
    input: &[T],
    device_channels: usize,
    graph_channels: usize,
    output: &mut Producer<f32>,
    counters: &Counters,
) where
    T: Sample,
    f32: FromSample<T>,
{
    if device_channels == 0 || graph_channels == 0 {
        return;
    }
    counters.callbacks.fetch_add(1, Ordering::Relaxed);
    let mut converted = [0.0; 8];
    let mut index = 0;
    while index + device_channels <= input.len() {
        let count = graph_frame(
            &input[index..index + device_channels],
            graph_channels,
            &mut converted,
        );
        // Half a stereo frame would swap left and right from here on.
        if !push_whole(&converted[..count], output) {
            counters.overruns.fetch_add(1, Ordering::Relaxed);
            return;
        }
        index += device_channels;
    }
}

/// Convert one callback buffer into interleaved `graph_channels` f32 samples.
///
/// A full ring increments the overrun counter once and drops the rest of the buffer.
pub(crate) fn push_capture<T>(
    input: &[T],
    device_channels: usize,
    graph_channels: usize,
    output: &mut Producer<f32>,
    framer: &mut Framer,
    counters: &Counters,
) where
    T: Sample,
    f32: FromSample<T>,
{
    if device_channels == 0 || graph_channels == 0 {
        return;
    }
    let mut converted = [0.0; 8];
    let mut index = 0;
    while index + device_channels <= input.len() {
        let frame = &input[index..index + device_channels];
        let count = graph_frame(frame, graph_channels, &mut converted);
        framer.push(&converted[..count], output, counters);
        index += device_channels;
    }
    counters.callbacks.fetch_add(1, Ordering::Relaxed);
}

/// Fill a device callback from the playback ring. A short ring writes silence
/// for the rest of the callback and increments the underrun counter once.
/// What is left in the ring afterwards is published as `queued`.
pub(crate) fn pull_playback<T>(
    output: &mut [T],
    device_channels: usize,
    graph_channels: usize,
    input: &mut Consumer<f32>,
    counters: &Counters,
) where
    T: Sample + FromSample<f32>,
{
    if device_channels == 0 || graph_channels == 0 {
        return;
    }
    counters.callbacks.fetch_add(1, Ordering::Relaxed);
    let frames = output.len() / device_channels;
    let mut starved = false;
    for frame in 0..frames {
        let mut graph = [0.0; 2];
        if !starved && input.slots() >= graph_channels {
            for sample in graph.iter_mut().take(graph_channels) {
                *sample = input.pop().unwrap_or(0.0);
            }
        } else {
            starved = true;
        }
        write_device_frame(
            &mut output[frame * device_channels..(frame + 1) * device_channels],
            &graph[..graph_channels.min(2)],
        );
    }
    if starved {
        counters.underruns.fetch_add(1, Ordering::Relaxed);
    }
    counters.queued.store(input.slots(), Ordering::Relaxed);
}

fn graph_frame<T>(frame: &[T], graph_channels: usize, out: &mut [f32; 8]) -> usize
where
    T: Sample,
    f32: FromSample<T>,
{
    if graph_channels == 1 {
        let sum: f32 = frame.iter().map(|sample| f32::from_sample(*sample)).sum();
        out[0] = sum / f32::from(u16::try_from(frame.len()).unwrap_or(u16::MAX));
        1
    } else {
        let left = f32::from_sample(frame[0]);
        let right = frame
            .get(1)
            .map_or(left, |sample| f32::from_sample(*sample));
        out[0] = left;
        out[1] = right;
        2
    }
}

fn write_device_frame<T>(output: &mut [T], graph: &[f32])
where
    T: Sample + FromSample<f32>,
{
    match (graph.len(), output.len()) {
        (1, _) => {
            let sample = T::from_sample(graph[0]);
            for slot in output {
                *slot = sample;
            }
        }
        (_, 1) => {
            let average = graph.iter().sum::<f32>()
                / f32::from(u16::try_from(graph.len()).unwrap_or(u16::MAX));
            output[0] = T::from_sample(average);
        }
        _ => {
            for (slot, sample) in output.iter_mut().zip(graph.iter().copied()) {
                *slot = T::from_sample(sample);
            }
            if output.len() > graph.len() {
                let last = T::from_sample(*graph.last().unwrap_or(&0.0));
                for slot in &mut output[graph.len()..] {
                    *slot = last;
                }
            }
        }
    }
}

fn flush_frame(frame: &[f32], output: &mut Producer<f32>, counters: &Counters) {
    // A partial frame would shift every later 10 ms frame and, in stereo,
    // swap the channels. Drop the frame whole.
    if !push_whole(frame, output) {
        counters.overruns.fetch_add(1, Ordering::Relaxed);
    }
}

/// Push all of `samples` or none of them. Does not allocate.
pub(crate) fn push_whole(samples: &[f32], output: &mut Producer<f32>) -> bool {
    let Ok(chunk) = output.write_chunk_uninit(samples.len()) else {
        return false;
    };
    chunk.fill_from_iter(samples.iter().copied());
    true
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use rtrb::RingBuffer;

    use super::{Framer, note_signal, pull_playback, push_capture, push_unframed};
    use crate::device::Counters;

    #[test]
    fn stereo_i16_downmix_is_allocation_free() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(4_096);
        let mut framer = Framer::new(4);
        let counters = Counters::default();
        let input = vec![0i16, i16::MAX, 0, i16::MAX];
        let allocations = allocation_counter::measure(|| {
            push_capture(&input, 2, 1, &mut producer, &mut framer, &counters);
            push_capture(&input, 2, 1, &mut producer, &mut framer, &counters);
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(counters.callbacks.load(Ordering::Relaxed), 2);
        let frame: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok()).collect();
        assert_eq!(frame.len(), 4);
        let expected = f32::from(i16::MAX) / f32::from(i16::MIN.unsigned_abs()) / 2.0;
        for sample in frame {
            assert!((sample - expected).abs() < 1e-4, "{sample} vs {expected}");
        }
    }

    #[test]
    fn digital_silence_is_not_a_signal() {
        let counters = Counters::default();
        let zeros = [0.0f32; 480];
        let unsigned_silence = [u16::MAX / 2 + 1; 480];
        let allocations = allocation_counter::measure(|| {
            assert!(!note_signal(&zeros, &counters));
            assert!(!note_signal(&unsigned_silence, &counters));
        });
        assert_eq!(allocations.count_total, 0);
        assert!(!counters.has_signal());
        let mut noise = zeros;
        noise[300] = 1.0e-7;
        assert!(note_signal(&noise, &counters));
        assert!(counters.has_signal());
    }

    #[test]
    fn playback_underrun_writes_silence_and_counts_once() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(8);
        producer.push(0.5).unwrap();
        let counters = Counters::default();
        let mut output = [1.0f32; 4];
        pull_playback(&mut output, 1, 1, &mut consumer, &counters);
        assert_eq!(output[0], 0.5);
        assert_eq!(output[1], 0.0);
        assert_eq!(counters.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(counters.callbacks.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_full_capture_ring_drops_whole_frames_and_keeps_stereo_aligned() {
        // Room for one 2-sample stereo frame and a half: the half must stay empty.
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(3);
        let mut framer = Framer::new(2);
        let counters = Counters::default();
        let input = [0.25f32, -0.25, 0.25, -0.25];
        push_capture(&input, 2, 2, &mut producer, &mut framer, &counters);
        assert_eq!(consumer.slots(), 2);
        assert_eq!(counters.overruns.load(Ordering::Relaxed), 1);
        assert_eq!(consumer.pop().unwrap(), 0.25);
        assert_eq!(consumer.pop().unwrap(), -0.25);

        let (mut producer, mut consumer) = RingBuffer::<f32>::new(3);
        let counters = Counters::default();
        push_unframed(&input, 2, 2, &mut producer, &counters);
        assert_eq!(consumer.slots(), 2);
        assert_eq!(counters.overruns.load(Ordering::Relaxed), 1);
        // The next callback still starts on a left sample.
        let _ = consumer.pop();
        let _ = consumer.pop();
        push_unframed(&[0.5f32, -0.5], 2, 2, &mut producer, &counters);
        assert_eq!(consumer.pop().unwrap(), 0.5);
    }

    #[test]
    fn playback_callback_publishes_what_it_left_in_the_ring() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(16);
        for _ in 0..10 {
            producer.push(0.1).unwrap();
        }
        let counters = Counters::default();
        let mut output = [0.0f32; 6];
        pull_playback(&mut output, 2, 2, &mut consumer, &counters);
        assert_eq!(counters.queued.load(Ordering::Relaxed), 4);
    }
}
