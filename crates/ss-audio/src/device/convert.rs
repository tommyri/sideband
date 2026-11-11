//! Sample-format and channel conversion used by the device callback.
//!
//! These functions do not allocate. The rings and the pending frame buffer
//! are created before the stream starts.

use std::sync::atomic::Ordering;

use cpal::{FromSample, Sample};
use rtrb::{Consumer, Producer};

use super::Counters;

/// How one capture callback maps device channels onto graph channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CaptureLayout {
    /// Interleaved channels in the device buffer.
    pub(crate) device: usize,
    /// `1` or `2`.
    pub(crate) graph: usize,
    /// Mono only: the device input to take, or `None` to average them all.
    /// Always below `device`; see [`super::CaptureChannel::resolve`].
    pub(crate) input: Option<usize>,
}

impl CaptureLayout {
    /// A layout that takes the first input for mono.
    #[cfg(test)]
    pub(crate) const fn first(device: usize, graph: usize) -> Self {
        Self {
            device,
            graph,
            input: Some(0),
        }
    }
}

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
    layout: CaptureLayout,
    output: &mut Producer<f32>,
    counters: &Counters,
) where
    T: Sample,
    f32: FromSample<T>,
{
    if layout.device == 0 || layout.graph == 0 {
        return;
    }
    counters.callbacks.fetch_add(1, Ordering::Relaxed);
    let mut converted = [0.0; 2];
    for frame in input.chunks_exact(layout.device) {
        let count = graph_frame(frame, layout, &mut converted);
        // Half a stereo frame would swap left and right from here on.
        if !push_whole(&converted[..count], output) {
            counters.overruns.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
}

/// Push the whole frames of an interleaved graph-format buffer that fit,
/// oldest first. The rest is dropped and counted as one overrun.
#[cfg_attr(
    not(all(target_os = "macos", feature = "macos-voiceio")),
    allow(dead_code, reason = "the VoiceProcessingIO capture callback")
)]
pub(crate) fn push_frames(
    samples: &[f32],
    channels: usize,
    output: &mut Producer<f32>,
    counters: &Counters,
) {
    if channels == 0 {
        return;
    }
    let whole = samples.len() / channels * channels;
    let fit = (output.slots() / channels * channels).min(whole);
    push_whole(&samples[..fit], output);
    if fit < whole {
        counters.overruns.fetch_add(1, Ordering::Relaxed);
    }
}

/// Convert one callback buffer into interleaved `graph_channels` f32 samples.
///
/// A full ring increments the overrun counter once and drops the rest of the buffer.
pub(crate) fn push_capture<T>(
    input: &[T],
    layout: CaptureLayout,
    output: &mut Producer<f32>,
    framer: &mut Framer,
    counters: &Counters,
) where
    T: Sample,
    f32: FromSample<T>,
{
    if layout.device == 0 || layout.graph == 0 {
        return;
    }
    let mut converted = [0.0; 2];
    for frame in input.chunks_exact(layout.device) {
        let count = graph_frame(frame, layout, &mut converted);
        framer.push(&converted[..count], output, counters);
    }
    counters.callbacks.fetch_add(1, Ordering::Relaxed);
}

/// The callback's side of a playback ring. Tests replace it to land producer
/// commits between two reads of one callback.
pub(crate) trait PlaybackRing {
    /// Samples ready to read. Only grows while the callback runs.
    fn slots(&self) -> usize;
    /// The oldest sample, if any.
    fn pop(&mut self) -> Option<f32>;
}

impl PlaybackRing for Consumer<f32> {
    fn slots(&self) -> usize {
        Consumer::slots(self)
    }

    fn pop(&mut self) -> Option<f32> {
        Consumer::pop(self).ok()
    }
}

/// Fill a device callback from the playback ring. A short ring writes silence
/// for the rest of the callback and increments the underrun counter once.
/// What is left in the ring afterwards is published as `queued`.
///
/// The callback reads the ring's fill once and takes only whole frames of
/// it. Reading sample by sample until the ring is empty would let a commit
/// that lands mid-callback start in the right channel and swap left and
/// right for good.
pub(crate) fn pull_playback<T, R>(
    output: &mut [T],
    device_channels: usize,
    graph_channels: usize,
    input: &mut R,
    counters: &Counters,
) where
    T: Sample + FromSample<f32>,
    R: PlaybackRing,
{
    if device_channels == 0 || graph_channels == 0 {
        return;
    }
    counters.callbacks.fetch_add(1, Ordering::Relaxed);
    let frames = output.len() / device_channels;
    // The producer only adds to the ring, so these pops cannot fail.
    let ready = (input.slots() / graph_channels).min(frames);
    for (index, device_frame) in output.chunks_exact_mut(device_channels).enumerate() {
        let mut graph = [0.0; 2];
        if index < ready {
            for sample in graph.iter_mut().take(graph_channels) {
                *sample = input.pop().unwrap_or(0.0);
            }
        }
        write_device_frame(device_frame, &graph[..graph_channels.min(2)]);
    }
    if ready < frames {
        counters.underruns.fetch_add(1, Ordering::Relaxed);
    }
    counters.queued.store(input.slots(), Ordering::Relaxed);
}

fn graph_frame<T>(frame: &[T], layout: CaptureLayout, out: &mut [f32; 2]) -> usize
where
    T: Sample,
    f32: FromSample<T>,
{
    if layout.graph == 1 {
        out[0] = if let Some(sample) = layout.input.and_then(|input| frame.get(input)) {
            f32::from_sample(*sample)
        } else {
            let sum: f32 = frame.iter().map(|sample| f32::from_sample(*sample)).sum();
            sum / f32::from(u16::try_from(frame.len()).unwrap_or(u16::MAX))
        };
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

/// A deterministic model of the threads on either side of a ring.
#[cfg(test)]
pub(crate) mod model {
    use std::collections::VecDeque;

    use proptest::prelude::*;
    use rtrb::{Consumer, Producer, RingBuffer};

    use super::{PlaybackRing, push_whole};

    /// Stereo frame `n` is `(n, -n)`, starting at 1, so silence (`0, 0`) and
    /// a swap (`-n, n`) are both visible.
    pub(crate) fn frame(n: u32) -> [f32; 2] {
        let value = f32::from(u16::try_from(n).unwrap_or(u16::MAX));
        [value, -value]
    }

    /// A playback ring whose producer commits whole frames at scripted
    /// points: before the `at`-th access the callback makes to the ring.
    pub(crate) struct Scripted {
        producer: Producer<f32>,
        consumer: Consumer<f32>,
        accesses: usize,
        commits: VecDeque<(usize, usize)>,
        next: u32,
    }

    impl Scripted {
        pub(crate) fn new(capacity: usize) -> Self {
            let (producer, consumer) = RingBuffer::new(capacity);
            Self {
                producer,
                consumer,
                accesses: 0,
                commits: VecDeque::new(),
                next: 1,
            }
        }

        /// The worker's push: all of `frames` stereo frames, or none.
        pub(crate) fn commit(&mut self, frames: usize) {
            if self.producer.slots() < frames * 2 {
                return;
            }
            let mut samples = Vec::with_capacity(frames * 2);
            for _ in 0..frames {
                samples.extend(frame(self.next));
                self.next += 1;
            }
            assert!(push_whole(&samples, &mut self.producer));
        }

        /// Commits to land while the next callback runs, counted from its
        /// first access to the ring.
        pub(crate) fn during_next_callback(&mut self, commits: &[(usize, usize)]) {
            self.accesses = 0;
            let mut commits = commits.to_vec();
            commits.sort_unstable();
            self.commits = commits.into();
        }

        fn access(&mut self) {
            self.accesses += 1;
            while let Some(&(at, frames)) = self.commits.front() {
                if at > self.accesses {
                    break;
                }
                self.commits.pop_front();
                self.commit(frames);
            }
        }

        /// Land what the callback did not reach.
        pub(crate) fn finish_callback(&mut self) {
            while let Some((_, frames)) = self.commits.pop_front() {
                self.commit(frames);
            }
        }
    }

    impl PlaybackRing for Scripted {
        fn slots(&self) -> usize {
            // `slots` takes `&self`; the commit lands on the following pop.
            self.consumer.slots()
        }

        fn pop(&mut self) -> Option<f32> {
            self.access();
            self.consumer.pop().ok()
        }
    }

    /// Checks what one callback wrote: every device frame is silence or the
    /// next stereo frame in order, left first.
    pub(crate) struct Listener {
        next: u32,
    }

    impl Listener {
        pub(crate) const fn new() -> Self {
            Self { next: 1 }
        }

        pub(crate) fn hear(&mut self, output: &[f32], device_channels: usize) {
            for device_frame in output.chunks_exact(device_channels) {
                let heard = [device_frame[0], device_frame[1]];
                if heard == [0.0, 0.0] {
                    continue;
                }
                assert_eq!(heard, frame(self.next), "playback frame out of place");
                self.next += 1;
            }
        }
    }

    /// Reads capture like the worker: whole frames once enough are queued.
    /// Frames may be missing (dropped on overrun) but never split or swapped.
    pub(crate) struct Reader {
        last: f32,
    }

    impl Reader {
        pub(crate) const fn new() -> Self {
            Self { last: 0.0 }
        }

        pub(crate) fn read(&mut self, consumer: &mut Consumer<f32>, frames: usize) {
            if consumer.slots() < frames * 2 {
                return;
            }
            for _ in 0..frames {
                let left = consumer.pop().unwrap();
                let right = consumer.pop().unwrap();
                assert!(
                    left > self.last,
                    "capture frame {left}, {right} after {}",
                    self.last
                );
                assert_eq!(right, -left, "capture frame split or swapped");
                self.last = left;
            }
        }
    }

    /// One step of a capture or playback schedule.
    #[derive(Clone, Debug)]
    pub(crate) enum Step {
        /// The producer side moves this many stereo frames.
        Produce(usize),
        /// The consumer side asks for this many stereo frames. On playback,
        /// `(access, frames)` commits land while it runs.
        Consume(usize, Vec<(usize, usize)>),
    }

    pub(crate) fn schedule() -> impl Strategy<Value = Vec<Step>> {
        let step = prop_oneof![
            (1..=6usize).prop_map(Step::Produce),
            (
                1..=12usize,
                prop::collection::vec((0..30usize, 1..=3usize), 0..4)
            )
                .prop_map(|(frames, commits)| Step::Consume(frames, commits)),
        ];
        prop::collection::vec(step, 1..80)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use proptest::prelude::*;
    use rtrb::RingBuffer;

    use super::{
        CaptureLayout, Framer,
        model::{Listener, Reader, Scripted, Step, frame, schedule},
        note_signal, pull_playback, push_capture, push_frames, push_unframed,
    };
    use crate::device::Counters;

    #[test]
    fn a_microphone_on_the_first_of_two_inputs_keeps_its_level() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(64);
        let mut framer = Framer::new(4);
        let counters = Counters::default();
        // Input 1 carries the microphone at -6 dBFS; input 2 is unused.
        let input = [0.5f32, 0.0, -0.5, 0.0, 0.5, 0.0, -0.5, 0.0];
        let first = CaptureLayout::first(2, 1);
        push_capture(&input, first, &mut producer, &mut framer, &counters);
        let mono: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok()).collect();
        assert_eq!(mono, [0.5, -0.5, 0.5, -0.5]);

        // The same through the resampling path, and from the second input.
        let second = CaptureLayout {
            input: Some(1),
            ..first
        };
        let swapped = [0.0f32, 0.5, 0.0, -0.5];
        push_unframed(&swapped, second, &mut producer, &counters);
        let mono: Vec<f32> = std::iter::from_fn(|| consumer.pop().ok()).collect();
        assert_eq!(mono, [0.5, -0.5]);
    }

    #[test]
    fn averaging_is_still_available_on_request() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(64);
        let counters = Counters::default();
        let average = CaptureLayout {
            input: None,
            ..CaptureLayout::first(4, 1)
        };
        push_unframed(&[0.8f32, 0.0, 0.0, 0.0], average, &mut producer, &counters);
        assert_eq!(consumer.pop().unwrap(), 0.2);
    }

    #[test]
    fn a_partial_fit_pushes_only_whole_frames() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(5);
        let counters = Counters::default();
        let allocations = allocation_counter::measure(|| {
            push_frames(
                &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0],
                2,
                &mut producer,
                &counters,
            );
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(consumer.slots(), 4);
        assert_eq!(counters.overruns.load(Ordering::Relaxed), 1);
        assert_eq!(consumer.pop().unwrap(), 1.0);
    }

    proptest! {
        #[test]
        fn playback_keeps_stereo_frames_whole_under_any_interleaving(
            steps in schedule(),
            capacity in 2..48usize,
            device_channels in 2..=4usize,
        ) {
            let mut ring = Scripted::new(capacity);
            let mut listener = Listener::new();
            let counters = Counters::default();
            for step in steps {
                match step {
                    Step::Produce(frames) => ring.commit(frames),
                    Step::Consume(frames, commits) => {
                        ring.during_next_callback(&commits);
                        let mut output = vec![9.0f32; frames * device_channels];
                        pull_playback(&mut output, device_channels, 2, &mut ring, &counters);
                        ring.finish_callback();
                        listener.hear(&output, device_channels);
                    }
                }
            }
        }

        #[test]
        fn capture_keeps_stereo_frames_whole_under_any_interleaving(
            steps in schedule(),
            capacity in 1..48usize,
            framed in any::<bool>(),
        ) {
            let (mut producer, mut consumer) = RingBuffer::<f32>::new(capacity);
            let mut framer = Framer::new(4);
            let mut reader = Reader::new();
            let counters = Counters::default();
            let mut next = 1;
            for step in steps {
                match step {
                    Step::Produce(frames) => {
                        let input: Vec<f32> = (0..frames)
                            .flat_map(|offset| frame(next + u32::try_from(offset).unwrap()))
                            .collect();
                        next += u32::try_from(frames).unwrap();
                        let stereo = CaptureLayout::first(2, 2);
                        if framed {
                            push_capture(&input, stereo, &mut producer, &mut framer, &counters);
                        } else {
                            push_unframed(&input, stereo, &mut producer, &counters);
                        }
                    }
                    Step::Consume(frames, _) => reader.read(&mut consumer, frames),
                }
            }
        }
    }

    #[test]
    fn stereo_i16_downmix_is_allocation_free() {
        let (mut producer, mut consumer) = RingBuffer::<f32>::new(4_096);
        let mut framer = Framer::new(4);
        let counters = Counters::default();
        let input = vec![0i16, i16::MAX, 0, i16::MAX];
        let allocations = allocation_counter::measure(|| {
            let average = CaptureLayout {
                input: None,
                ..CaptureLayout::first(2, 1)
            };
            push_capture(&input, average, &mut producer, &mut framer, &counters);
            push_capture(&input, average, &mut producer, &mut framer, &counters);
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
        let stereo = CaptureLayout::first(2, 2);
        push_capture(&input, stereo, &mut producer, &mut framer, &counters);
        assert_eq!(consumer.slots(), 2);
        assert_eq!(counters.overruns.load(Ordering::Relaxed), 1);
        assert_eq!(consumer.pop().unwrap(), 0.25);
        assert_eq!(consumer.pop().unwrap(), -0.25);

        let (mut producer, mut consumer) = RingBuffer::<f32>::new(3);
        let counters = Counters::default();
        push_unframed(&input, stereo, &mut producer, &counters);
        assert_eq!(consumer.slots(), 2);
        assert_eq!(counters.overruns.load(Ordering::Relaxed), 1);
        // The next callback still starts on a left sample.
        let _ = consumer.pop();
        let _ = consumer.pop();
        push_unframed(&[0.5f32, -0.5], stereo, &mut producer, &counters);
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
