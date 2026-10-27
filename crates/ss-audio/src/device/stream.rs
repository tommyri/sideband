//! Open `cpal` streams. The callback converts format. A worker resamples
//! only when the device rate is not 48 kHz.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use cpal::{
    FromSample, SampleFormat, SizedSample, StreamInstant,
    traits::{DeviceTrait, StreamTrait},
};
use rtrb::{Consumer, Producer, RingBuffer};

use super::{
    DeviceId, Error, SAMPLES_PER_CHANNEL,
    convert::{Framer, pull_playback, push_capture, push_unframed},
    select::{GraphConfig, choose_format},
};
use crate::resample::{GRAPH_RATE, RateConverter};

/// Underruns, overruns, callback count, and whether resampling is on.
#[derive(Debug, Default)]
pub struct Counters {
    pub(crate) underruns: AtomicU64,
    pub(crate) overruns: AtomicU64,
    pub(crate) callbacks: AtomicU64,
    pub(crate) resampler_active: AtomicBool,
}

impl Counters {
    /// Playback callbacks that ran out of samples, plus host xruns on playback.
    #[must_use]
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Capture callbacks that could not push a full frame, plus host xruns on capture.
    #[must_use]
    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Data callbacks observed so far.
    #[must_use]
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Relaxed)
    }

    /// `true` when a worker is resampling this stream.
    #[must_use]
    pub fn resampler_active(&self) -> bool {
        self.resampler_active.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Default)]
struct Clock {
    valid: AtomicBool,
    endpoint: AtomicU64,
}

impl Clock {
    fn publish(&self, endpoint: StreamInstant) {
        let nanos = u64::try_from(endpoint.as_nanos()).unwrap_or(u64::MAX);
        self.endpoint.store(nanos, Ordering::Release);
        self.valid.store(true, Ordering::Release);
    }

    fn endpoint(&self) -> Option<StreamInstant> {
        if !self.valid.load(Ordering::Acquire) {
            return None;
        }
        Some(StreamInstant::from_nanos(
            self.endpoint.load(Ordering::Acquire),
        ))
    }
}

/// An open microphone. Dropping it stops the callback and the worker.
pub struct CaptureStream {
    stream: Option<cpal::Stream>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    lost: Arc<AtomicBool>,
    clock: Arc<Clock>,
    device_id: DeviceId,
    device_rate: u32,
}

/// An open speaker. The paired producer accepts 48 kHz frames.
pub struct PlaybackStream {
    stream: Option<cpal::Stream>,
    worker: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    counters: Arc<Counters>,
    lost: Arc<AtomicBool>,
    clock: Arc<Clock>,
    device_id: DeviceId,
    device_rate: u32,
}

impl CaptureStream {
    pub(crate) fn open(
        device: &cpal::Device,
        config: GraphConfig,
        device_id: DeviceId,
    ) -> Result<(Self, Consumer<f32>), Error> {
        let chosen = choose_input(device, config)?;
        let direct = chosen.sample_rate == GRAPH_RATE;
        let graph_channels = usize::from(config.channels);
        let device_channels = usize::from(chosen.channels);
        let (graph_tx, graph_rx) = RingBuffer::new(ring_samples(GRAPH_RATE, graph_channels));
        let counters = Arc::new(Counters::default());
        counters.resampler_active.store(!direct, Ordering::Relaxed);
        let lost = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(Clock::default());
        let stop = Arc::new(AtomicBool::new(false));
        let endpoint = Endpoint {
            counters: Arc::clone(&counters),
            lost: Arc::clone(&lost),
            clock: Arc::clone(&clock),
        };
        let (stream, worker) = if direct {
            let stream = build_capture(CaptureBuild {
                device,
                chosen: &chosen,
                device_channels,
                graph_channels,
                whole_frames: true,
                output: graph_tx,
                endpoint: endpoint.clone_arcs(),
            })?;
            (stream, None)
        } else {
            let (device_tx, device_rx) =
                RingBuffer::new(ring_samples(chosen.sample_rate, graph_channels));
            let stream = build_capture(CaptureBuild {
                device,
                chosen: &chosen,
                device_channels,
                graph_channels,
                whole_frames: false,
                output: device_tx,
                endpoint: endpoint.clone_arcs(),
            })?;
            let converter = RateConverter::new(chosen.sample_rate, GRAPH_RATE, graph_channels)
                .map_err(|_| Error::Resampler)?;
            let worker = spawn_resample_worker(
                "ss-audio-capture",
                Arc::clone(&stop),
                device_rx,
                graph_tx,
                converter,
                Arc::clone(&counters),
                true,
            )?;
            (stream, Some(worker))
        };
        stream.play().map_err(Error::Backend)?;
        tracing::info!(
            device = %device_id,
            rate = chosen.sample_rate,
            channels = chosen.channels,
            resample = !direct,
            "opened capture"
        );
        Ok((
            Self {
                stream: Some(stream),
                worker,
                stop,
                counters,
                lost,
                clock,
                device_id,
                device_rate: chosen.sample_rate,
            },
            graph_rx,
        ))
    }

    /// Stream clock, the same base as [`Self::last_capture`].
    #[must_use]
    pub fn now(&self) -> StreamInstant {
        self.stream
            .as_ref()
            .map_or(StreamInstant::ZERO, StreamTrait::now)
    }

    /// Capture time of the most recent callback, if one has run.
    #[must_use]
    pub fn last_capture(&self) -> Option<StreamInstant> {
        self.clock.endpoint()
    }

    /// `now` minus the last capture timestamp.
    #[must_use]
    pub fn capture_delay(&self) -> Option<Duration> {
        Some(self.now().saturating_duration_since(self.last_capture()?))
    }

    /// Shared counters. Loading them does not allocate.
    #[must_use]
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// The host reported that this device is gone or the stream must be rebuilt.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Device this stream was opened on.
    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Rate the device was opened at, before resampling.
    #[must_use]
    pub const fn device_rate(&self) -> u32 {
        self.device_rate
    }

    pub(crate) fn lost_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.lost)
    }
}

impl PlaybackStream {
    pub(crate) fn open(
        device: &cpal::Device,
        config: GraphConfig,
        device_id: DeviceId,
    ) -> Result<(Self, Producer<f32>), Error> {
        let chosen = choose_output(device, config)?;
        let direct = chosen.sample_rate == GRAPH_RATE;
        let graph_channels = usize::from(config.channels);
        let device_channels = usize::from(chosen.channels);
        let (graph_tx, graph_rx) = RingBuffer::new(ring_samples(GRAPH_RATE, graph_channels));
        let counters = Arc::new(Counters::default());
        counters.resampler_active.store(!direct, Ordering::Relaxed);
        let lost = Arc::new(AtomicBool::new(false));
        let clock = Arc::new(Clock::default());
        let stop = Arc::new(AtomicBool::new(false));
        let endpoint = Endpoint {
            counters: Arc::clone(&counters),
            lost: Arc::clone(&lost),
            clock: Arc::clone(&clock),
        };
        let (stream, worker) = if direct {
            let stream = build_playback(PlaybackBuild {
                device,
                chosen: &chosen,
                device_channels,
                graph_channels,
                input: graph_rx,
                endpoint: endpoint.clone_arcs(),
            })?;
            (stream, None)
        } else {
            let (device_tx, device_rx) =
                RingBuffer::new(ring_samples(chosen.sample_rate, graph_channels));
            let stream = build_playback(PlaybackBuild {
                device,
                chosen: &chosen,
                device_channels,
                graph_channels,
                input: device_rx,
                endpoint: endpoint.clone_arcs(),
            })?;
            let converter = RateConverter::new(GRAPH_RATE, chosen.sample_rate, graph_channels)
                .map_err(|_| Error::Resampler)?;
            let worker = spawn_resample_worker(
                "ss-audio-playback",
                Arc::clone(&stop),
                graph_rx,
                device_tx,
                converter,
                Arc::clone(&counters),
                false,
            )?;
            (stream, Some(worker))
        };
        stream.play().map_err(Error::Backend)?;
        tracing::info!(
            device = %device_id,
            rate = chosen.sample_rate,
            channels = chosen.channels,
            resample = !direct,
            "opened playback"
        );
        Ok((
            Self {
                stream: Some(stream),
                worker,
                stop,
                counters,
                lost,
                clock,
                device_id,
                device_rate: chosen.sample_rate,
            },
            graph_tx,
        ))
    }

    /// Stream clock, the same base as [`Self::last_playback`].
    #[must_use]
    pub fn now(&self) -> StreamInstant {
        self.stream
            .as_ref()
            .map_or(StreamInstant::ZERO, StreamTrait::now)
    }

    /// Predicted playback time of the most recent callback, if one has run.
    #[must_use]
    pub fn last_playback(&self) -> Option<StreamInstant> {
        self.clock.endpoint()
    }

    /// Last playback timestamp minus `now`. This is the output buffer delay.
    #[must_use]
    pub fn playback_delay(&self) -> Option<Duration> {
        Some(self.last_playback()?.saturating_duration_since(self.now()))
    }

    /// Shared counters. Loading them does not allocate.
    #[must_use]
    pub fn counters(&self) -> &Counters {
        &self.counters
    }

    /// The host reported that this device is gone or the stream must be rebuilt.
    #[must_use]
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Device this stream was opened on.
    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Rate the device was opened at, after resampling.
    #[must_use]
    pub const fn device_rate(&self) -> u32 {
        self.device_rate
    }

    pub(crate) fn lost_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.lost)
    }
}

impl Drop for CaptureStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Drop for PlaybackStream {
    fn drop(&mut self) {
        self.shutdown();
    }
}

trait Shutdown {
    fn stop(&self) -> &AtomicBool;
    fn stream(&mut self) -> &mut Option<cpal::Stream>;
    fn worker(&mut self) -> &mut Option<JoinHandle<()>>;

    fn shutdown(&mut self) {
        self.stop().store(true, Ordering::Release);
        self.stream().take();
        if let Some(worker) = self.worker().take() {
            let _ = worker.join();
        }
    }
}

impl Shutdown for CaptureStream {
    fn stop(&self) -> &AtomicBool {
        &self.stop
    }
    fn stream(&mut self) -> &mut Option<cpal::Stream> {
        &mut self.stream
    }
    fn worker(&mut self) -> &mut Option<JoinHandle<()>> {
        &mut self.worker
    }
}

impl Shutdown for PlaybackStream {
    fn stop(&self) -> &AtomicBool {
        &self.stop
    }
    fn stream(&mut self) -> &mut Option<cpal::Stream> {
        &mut self.stream
    }
    fn worker(&mut self) -> &mut Option<JoinHandle<()>> {
        &mut self.worker
    }
}

struct Chosen {
    sample_rate: u32,
    channels: u16,
    format: SampleFormat,
    config: cpal::StreamConfig,
}

fn choose_input(device: &cpal::Device, config: GraphConfig) -> Result<Chosen, Error> {
    let ranges = device
        .supported_input_configs()
        .map_err(Error::Backend)?
        .collect::<Vec<_>>();
    finish_choice(&ranges, config)
}

fn choose_output(device: &cpal::Device, config: GraphConfig) -> Result<Chosen, Error> {
    let ranges = device
        .supported_output_configs()
        .map_err(Error::Backend)?
        .collect::<Vec<_>>();
    finish_choice(&ranges, config)
}

fn finish_choice(
    ranges: &[cpal::SupportedStreamConfigRange],
    config: GraphConfig,
) -> Result<Chosen, Error> {
    let chosen = choose_format(ranges, config.channels).ok_or(Error::Unsupported)?;
    Ok(Chosen {
        sample_rate: chosen.sample_rate,
        channels: chosen.channels,
        format: chosen.sample_format,
        config: cpal::StreamConfig {
            channels: chosen.channels,
            sample_rate: chosen.sample_rate,
            buffer_size: chosen.buffer_size,
        },
    })
}

struct Endpoint {
    counters: Arc<Counters>,
    lost: Arc<AtomicBool>,
    clock: Arc<Clock>,
}

impl Endpoint {
    fn clone_arcs(&self) -> Self {
        Self {
            counters: Arc::clone(&self.counters),
            lost: Arc::clone(&self.lost),
            clock: Arc::clone(&self.clock),
        }
    }
}

struct CaptureBuild<'a> {
    device: &'a cpal::Device,
    chosen: &'a Chosen,
    device_channels: usize,
    graph_channels: usize,
    whole_frames: bool,
    output: Producer<f32>,
    endpoint: Endpoint,
}

struct PlaybackBuild<'a> {
    device: &'a cpal::Device,
    chosen: &'a Chosen,
    device_channels: usize,
    graph_channels: usize,
    input: Consumer<f32>,
    endpoint: Endpoint,
}

fn build_capture(args: CaptureBuild<'_>) -> Result<cpal::Stream, Error> {
    match args.chosen.format {
        SampleFormat::F32 => build_capture_typed::<f32>(args),
        SampleFormat::I16 => build_capture_typed::<i16>(args),
        SampleFormat::U16 => build_capture_typed::<u16>(args),
        SampleFormat::I32 => build_capture_typed::<i32>(args),
        SampleFormat::U32 => build_capture_typed::<u32>(args),
        SampleFormat::F64 => build_capture_typed::<f64>(args),
        SampleFormat::I8 => build_capture_typed::<i8>(args),
        SampleFormat::U8 => build_capture_typed::<u8>(args),
        _ => Err(Error::SampleFormat),
    }
}

fn build_capture_typed<T>(args: CaptureBuild<'_>) -> Result<cpal::Stream, Error>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let CaptureBuild {
        device,
        chosen,
        device_channels,
        graph_channels,
        whole_frames,
        mut output,
        endpoint,
    } = args;
    let lost = Arc::clone(&endpoint.lost);
    let counters_err = Arc::clone(&endpoint.counters);
    let counters = endpoint.counters;
    let clock = endpoint.clock;
    let mut pending = Framer::new(graph_channels * SAMPLES_PER_CHANNEL);
    device
        .build_input_stream::<T, _, _>(
            chosen.config,
            move |data, info| {
                let timestamp = info.timestamp();
                clock.publish(timestamp.capture);
                if whole_frames {
                    push_capture(
                        data,
                        device_channels,
                        graph_channels,
                        &mut output,
                        &mut pending,
                        &counters,
                    );
                } else {
                    push_unframed(
                        data,
                        device_channels,
                        graph_channels,
                        &mut output,
                        &counters,
                    );
                }
            },
            move |error| on_stream_error(&error, &lost, &counters_err, true),
            None,
        )
        .map_err(Error::Backend)
}

fn build_playback(args: PlaybackBuild<'_>) -> Result<cpal::Stream, Error> {
    match args.chosen.format {
        SampleFormat::F32 => build_playback_typed::<f32>(args),
        SampleFormat::I16 => build_playback_typed::<i16>(args),
        SampleFormat::U16 => build_playback_typed::<u16>(args),
        SampleFormat::I32 => build_playback_typed::<i32>(args),
        SampleFormat::U32 => build_playback_typed::<u32>(args),
        SampleFormat::F64 => build_playback_typed::<f64>(args),
        SampleFormat::I8 => build_playback_typed::<i8>(args),
        SampleFormat::U8 => build_playback_typed::<u8>(args),
        _ => Err(Error::SampleFormat),
    }
}

fn build_playback_typed<T>(args: PlaybackBuild<'_>) -> Result<cpal::Stream, Error>
where
    T: SizedSample + FromSample<f32>,
{
    let PlaybackBuild {
        device,
        chosen,
        device_channels,
        graph_channels,
        mut input,
        endpoint,
    } = args;
    let lost = Arc::clone(&endpoint.lost);
    let counters_err = Arc::clone(&endpoint.counters);
    let counters = endpoint.counters;
    let clock = endpoint.clock;
    device
        .build_output_stream::<T, _, _>(
            chosen.config,
            move |data, info| {
                let timestamp = info.timestamp();
                clock.publish(timestamp.playback);
                pull_playback(data, device_channels, graph_channels, &mut input, &counters);
            },
            move |error| on_stream_error(&error, &lost, &counters_err, false),
            None,
        )
        .map_err(Error::Backend)
}

fn on_stream_error(error: &cpal::Error, lost: &AtomicBool, counters: &Counters, capture: bool) {
    match error.kind() {
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
            lost.store(true, Ordering::Release);
        }
        cpal::ErrorKind::Xrun => {
            if capture {
                counters.overruns.fetch_add(1, Ordering::Relaxed);
            } else {
                counters.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        _ => {}
    }
}

fn spawn_resample_worker(
    name: &str,
    stop: Arc<AtomicBool>,
    mut input: Consumer<f32>,
    mut output: Producer<f32>,
    mut converter: RateConverter,
    counters: Arc<Counters>,
    frame_output: bool,
) -> Result<JoinHandle<()>, Error> {
    let frame_samples = converter.frame_samples();
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let mut batch = Vec::with_capacity(2_048);
            let mut frame = vec![0.0; frame_samples];
            let mut pulled = vec![0.0; 4_096];
            while !stop.load(Ordering::Acquire) {
                batch.clear();
                while batch.len() < batch.capacity() {
                    match input.pop() {
                        Ok(sample) => batch.push(sample),
                        Err(_) => break,
                    }
                }
                if batch.is_empty() {
                    if input.is_abandoned() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
                if converter.push(&batch).is_err() {
                    tracing::debug!("resampler rejected a capture block");
                    break;
                }
                if frame_output {
                    while converter.pull_frame(&mut frame) {
                        push_all(&frame, &mut output, &counters);
                    }
                } else {
                    let count = converter.pull(&mut pulled);
                    push_all(&pulled[..count], &mut output, &counters);
                }
                if output.is_abandoned() {
                    break;
                }
            }
        })
        .map_err(|_| Error::Worker)
}

fn push_all(samples: &[f32], output: &mut Producer<f32>, counters: &Counters) {
    for sample in samples {
        if output.push(*sample).is_err() {
            counters.overruns.fetch_add(1, Ordering::Relaxed);
            return;
        }
    }
}

fn ring_samples(rate: u32, channels: usize) -> usize {
    let quarter_second = u64::from(rate) / 4 * channels as u64;
    usize::try_from(quarter_second.max(1)).unwrap_or(usize::MAX)
}
