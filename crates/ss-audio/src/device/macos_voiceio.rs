//! Apple's full-duplex VoiceProcessingIO device backend.
//!
//! A single Audio Unit owns capture and render so its echo canceller sees the
//! exact far-end signal sent to the speakers. The graph-facing rings remain
//! 48 kHz interleaved `f32`, matching the portable cpal backend.
//!
//! Capture uses a raw input proc rather than `coreaudio-rs`'s
//! `set_input_callback`. That wrapper reads the stream format with
//! `AudioUnitGetProperty` and grows its buffer with `Vec::resize` on the
//! real-time thread whenever a slice differs from the size it read at
//! setup. Here the buffer is sized from `kAudioUnitProperty_MaximumFramesPerSlice`
//! before the unit starts, and the proc only renders into it and pushes.

use std::{
    ffi::c_void,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::Duration,
};

use block2::RcBlock;
use coreaudio::audio_unit::{
    AudioUnit, Element, IOType, SampleFormat, Scope, StreamFormat,
    audio_format::LinearPcmFlags,
    render_callback::{Args, data},
};
use objc2_audio_toolbox::{
    AURenderCallbackStruct, AUVoiceIOMutedSpeechActivityEventListener,
    AUVoiceIOSpeechActivityEvent, kAUVoiceIOProperty_BypassVoiceProcessing,
    kAUVoiceIOProperty_MuteOutput, kAUVoiceIOProperty_MutedSpeechActivityEventListener,
    kAUVoiceIOProperty_VoiceProcessingEnableAGC, kAudioOutputUnitProperty_EnableIO,
    kAudioOutputUnitProperty_IsRunning, kAudioOutputUnitProperty_SetInputCallback,
    kAudioUnitProperty_Latency, kAudioUnitProperty_MaximumFramesPerSlice,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use rtrb::{Consumer, Producer, RingBuffer};

use super::{
    Counters, Error, GRAPH_RATE, GraphConfig,
    convert::{self, PlaybackRing},
};

/// Smallest capture buffer, in frames, whatever the unit reports. 4096 is
/// the slice limit Apple documents for I/O units on macOS.
const MIN_CAPTURE_FRAMES: u32 = 4_096;

const NO_SPEECH_EVENT: u8 = 0;
const SPEECH_STARTED: u8 = 1;
const SPEECH_ENDED: u8 = 2;

type SpeechListener = RcBlock<dyn Fn(AUVoiceIOSpeechActivityEvent)>;

/// Initial settings for Apple's microphone processing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VoiceProcessingConfig {
    /// Disable Apple's AEC/NS/AGC processing while retaining the duplex unit.
    /// Control Center Voice Isolation or Wide Spectrum may override this.
    pub bypass: bool,
    /// Enable Apple's automatic gain control on the processed microphone.
    pub automatic_gain_control: bool,
    /// Start with the processed microphone uplink muted.
    pub muted: bool,
}

impl Default for VoiceProcessingConfig {
    fn default() -> Self {
        Self {
            bypass: false,
            automatic_gain_control: true,
            muted: false,
        }
    }
}

/// Speech detected by VoiceProcessingIO while its microphone output is muted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutedSpeechActivity {
    Started,
    Ended,
}

/// One full-duplex VoiceProcessingIO Audio Unit.
///
/// Dropping this value stops and disposes the unit. Its paired capture consumer
/// and playback producer use the same contracts as the portable device rings.
pub struct VoiceProcessingStream {
    // Declared before `capture`: fields drop in order, so the unit is
    // stopped and disposed before the state its input proc uses is freed.
    unit: AudioUnit,
    _capture: CaptureState,
    capture_counters: Arc<Counters>,
    playback_counters: Arc<Counters>,
    speech_activity: Arc<AtomicU8>,
    _speech_listener: Option<SpeechListener>,
}

fn new_voice_processing_unit() -> Result<AudioUnit, Error> {
    for attempt in 0..10 {
        match AudioUnit::new_uninitialized(IOType::VoiceProcessingIO) {
            Ok(unit) => return Ok(unit),
            Err(coreaudio::Error::NoMatchingDefaultAudioUnitFound) if attempt < 9 => {
                // CoreAudio briefly removes the component while a previous
                // VoiceProcessingIO instance tears down during a mode switch.
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(Error::VoiceProcessing(error)),
        }
    }
    unreachable!("the retry loop always returns on its final attempt")
}

impl VoiceProcessingStream {
    pub(crate) fn open(
        capture: GraphConfig,
        playback: GraphConfig,
        config: VoiceProcessingConfig,
    ) -> Result<(Self, Consumer<f32>, Producer<f32>), Error> {
        check_channels(capture)?;
        check_channels(playback)?;

        let capture_channels = usize::from(capture.channels);
        let playback_channels = usize::from(playback.channels);
        let (capture_tx, capture_rx) = RingBuffer::new(ring_samples(capture_channels));
        let (playback_tx, playback_rx) = RingBuffer::new(ring_samples(playback_channels));
        let capture_counters = Arc::new(Counters::default());
        let playback_counters = Arc::new(Counters::default());

        // Created before the unit so an early return disposes the unit first.
        let mut capture_state =
            CaptureState::new(capture_tx, Arc::clone(&capture_counters), capture_channels);
        let mut unit = new_voice_processing_unit()?;
        enable_duplex(&mut unit)?;
        unit.set_stream_format(format(capture.channels), Scope::Output, Element::Input)
            .map_err(Error::VoiceProcessing)?;
        unit.set_stream_format(format(playback.channels), Scope::Input, Element::Output)
            .map_err(Error::VoiceProcessing)?;

        capture_state.reserve(max_frames_per_slice(&unit)?);
        install_capture(&mut unit, &mut capture_state)?;
        install_playback(&mut unit, playback_rx, Arc::clone(&playback_counters))?;
        set_bool(
            &mut unit,
            kAUVoiceIOProperty_BypassVoiceProcessing,
            config.bypass,
        )?;
        set_bool(
            &mut unit,
            kAUVoiceIOProperty_VoiceProcessingEnableAGC,
            config.automatic_gain_control,
        )?;
        set_bool(&mut unit, kAUVoiceIOProperty_MuteOutput, config.muted)?;

        let speech_activity = Arc::new(AtomicU8::new(NO_SPEECH_EVENT));
        let speech_listener = install_speech_listener(&mut unit, Arc::clone(&speech_activity));

        unit.initialize().map_err(Error::VoiceProcessing)?;
        // Initialising may raise the limit; no callback runs before `start`.
        capture_state.reserve(max_frames_per_slice(&unit)?);
        unit.start().map_err(Error::VoiceProcessing)?;
        tracing::info!(
            capture_channels,
            playback_channels,
            bypass = config.bypass,
            agc = config.automatic_gain_control,
            "opened Apple VoiceProcessingIO"
        );
        Ok((
            Self {
                unit,
                _capture: capture_state,
                capture_counters,
                playback_counters,
                speech_activity,
                _speech_listener: speech_listener,
            },
            capture_rx,
            playback_tx,
        ))
    }

    /// Turn all Apple microphone processing off or on.
    pub fn set_bypass(&mut self, bypass: bool) -> Result<(), Error> {
        set_bool(
            &mut self.unit,
            kAUVoiceIOProperty_BypassVoiceProcessing,
            bypass,
        )
    }

    /// Enable or disable Apple's automatic gain control.
    pub fn set_automatic_gain_control(&mut self, enabled: bool) -> Result<(), Error> {
        set_bool(
            &mut self.unit,
            kAUVoiceIOProperty_VoiceProcessingEnableAGC,
            enabled,
        )
    }

    /// Mute or unmute the processed microphone uplink.
    pub fn set_muted(&mut self, muted: bool) -> Result<(), Error> {
        set_bool(&mut self.unit, kAUVoiceIOProperty_MuteOutput, muted)
    }

    /// Whether Apple voice processing is bypassed.
    pub fn bypassed(&self) -> Result<bool, Error> {
        get_bool(&self.unit, kAUVoiceIOProperty_BypassVoiceProcessing)
    }

    /// Whether Apple's automatic gain control is enabled.
    pub fn automatic_gain_control(&self) -> Result<bool, Error> {
        get_bool(&self.unit, kAUVoiceIOProperty_VoiceProcessingEnableAGC)
    }

    /// Whether the processed microphone uplink is muted.
    pub fn muted(&self) -> Result<bool, Error> {
        get_bool(&self.unit, kAUVoiceIOProperty_MuteOutput)
    }

    /// Most recent muted-talker transition, consuming it if present.
    pub fn take_muted_speech_activity(&self) -> Option<MutedSpeechActivity> {
        match self.speech_activity.swap(NO_SPEECH_EVENT, Ordering::AcqRel) {
            SPEECH_STARTED => Some(MutedSpeechActivity::Started),
            SPEECH_ENDED => Some(MutedSpeechActivity::Ended),
            _ => None,
        }
    }

    /// Capture callback counters.
    #[must_use]
    pub fn capture_counters(&self) -> &Counters {
        &self.capture_counters
    }

    /// Playback callback counters.
    #[must_use]
    pub fn playback_counters(&self) -> &Counters {
        &self.playback_counters
    }

    /// Processing latency reported by the Audio Unit.
    pub fn latency(&self) -> Result<Duration, Error> {
        let seconds: f64 = self
            .unit
            .get_property(kAudioUnitProperty_Latency, Scope::Global, Element::Output)
            .map_err(Error::VoiceProcessing)?;
        Ok(Duration::from_secs_f64(seconds.max(0.0)))
    }

    /// Whether CoreAudio reports this unit as running.
    pub fn is_running(&self) -> Result<bool, Error> {
        get_bool(&self.unit, kAudioOutputUnitProperty_IsRunning)
    }

    /// The software T-053 chain must remain bypassed while this backend is in use.
    #[must_use]
    pub const fn requires_software_processing_bypass(&self) -> bool {
        true
    }
}

impl Drop for VoiceProcessingStream {
    fn drop(&mut self) {
        if let Err(error) = self.unit.stop() {
            tracing::debug!(%error, "could not stop VoiceProcessingIO during drop");
        }
    }
}

fn enable_duplex(unit: &mut AudioUnit) -> Result<(), Error> {
    let enabled = 1_u32;
    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Input,
        Element::Input,
        Some(&enabled),
    )
    .map_err(Error::VoiceProcessing)?;
    unit.set_property(
        kAudioOutputUnitProperty_EnableIO,
        Scope::Output,
        Element::Output,
        Some(&enabled),
    )
    .map_err(Error::VoiceProcessing)
}

fn max_frames_per_slice(unit: &AudioUnit) -> Result<u32, Error> {
    unit.get_property(
        kAudioUnitProperty_MaximumFramesPerSlice,
        Scope::Global,
        Element::Output,
    )
    .map_err(Error::VoiceProcessing)
}

fn install_capture(unit: &mut AudioUnit, state: &mut CaptureState) -> Result<(), Error> {
    let refcon = state.attach(*unit.as_ref());
    let callback = AURenderCallbackStruct {
        inputProc: Some(raw::input_proc),
        inputProcRefCon: refcon.as_ptr(),
    };
    unit.set_property(
        kAudioOutputUnitProperty_SetInputCallback,
        Scope::Global,
        Element::Output,
        Some(&callback),
    )
    .map_err(Error::VoiceProcessing)
}

fn install_playback(
    unit: &mut AudioUnit,
    mut input: Consumer<f32>,
    counters: Arc<Counters>,
) -> Result<(), Error> {
    type CallbackArgs = Args<data::Interleaved<f32>>;
    // `set_render_callback` hands over the unit's own buffer: no property
    // reads and no allocation in the callback.
    unit.set_render_callback(move |args: CallbackArgs| {
        let channels = args.data.channels;
        pull_playback(args.data.buffer, channels, &mut input, &counters);
        Ok(())
    })
    .map_err(Error::VoiceProcessing)
}

fn install_speech_listener(
    unit: &mut AudioUnit,
    activity: Arc<AtomicU8>,
) -> Option<SpeechListener> {
    let listener: SpeechListener = RcBlock::new(move |event: AUVoiceIOSpeechActivityEvent| {
        let value = if event == AUVoiceIOSpeechActivityEvent::HasStarted {
            SPEECH_STARTED
        } else if event == AUVoiceIOSpeechActivityEvent::HasEnded {
            SPEECH_ENDED
        } else {
            NO_SPEECH_EVENT
        };
        if value != NO_SPEECH_EVENT {
            activity.store(value, Ordering::Release);
        }
    });
    let pointer: AUVoiceIOMutedSpeechActivityEventListener = RcBlock::as_ptr(&listener);
    match unit.set_property(
        kAUVoiceIOProperty_MutedSpeechActivityEventListener,
        Scope::Global,
        Element::Output,
        Some(&pointer),
    ) {
        Ok(()) => Some(listener),
        Err(error) => {
            tracing::debug!(%error, "muted-talker detection is unavailable");
            None
        }
    }
}

fn set_bool(unit: &mut AudioUnit, property: u32, value: bool) -> Result<(), Error> {
    let value = u32::from(value);
    unit.set_property(property, Scope::Global, Element::Output, Some(&value))
        .map_err(Error::VoiceProcessing)
}

fn get_bool(unit: &AudioUnit, property: u32) -> Result<bool, Error> {
    let value: u32 = unit
        .get_property(property, Scope::Global, Element::Output)
        .map_err(Error::VoiceProcessing)?;
    Ok(value != 0)
}

fn format(channels: u16) -> StreamFormat {
    StreamFormat {
        sample_rate: f64::from(GRAPH_RATE),
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
        channels: u32::from(channels),
    }
}

fn check_channels(config: GraphConfig) -> Result<(), Error> {
    if matches!(config.channels, 1 | 2) {
        Ok(())
    } else {
        Err(Error::Channels)
    }
}

fn ring_samples(channels: usize) -> usize {
    usize::try_from(u64::from(GRAPH_RATE) / 4 * channels as u64).unwrap_or(usize::MAX)
}

/// Push one rendered capture slice, whole frames only.
fn push_capture(samples: &[f32], channels: usize, output: &mut Producer<f32>, counters: &Counters) {
    convert::push_frames(samples, channels, output, counters);
}

/// Fill one render slice from the playback ring, whole frames only.
fn pull_playback<R: PlaybackRing>(
    samples: &mut [f32],
    channels: usize,
    input: &mut R,
    counters: &Counters,
) {
    convert::pull_playback(samples, channels, channels, input, counters);
}

/// What the capture input proc owns: a slice buffer sized before the unit
/// starts, the ring it fills, and its counters.
struct Capture {
    unit: objc2_audio_toolbox::AudioUnit,
    buffer: Box<[f32]>,
    channels: usize,
    output: Producer<f32>,
    counters: Arc<Counters>,
    has_signal: bool,
}

impl Capture {
    /// One input proc call: `render` fills the buffer list with `frames`
    /// frames, then whole frames go to the ring. Does not allocate.
    fn deliver(
        &mut self,
        frames: u32,
        render: impl FnOnce(NonNull<AudioBufferList>) -> i32,
    ) -> i32 {
        self.counters.callbacks.fetch_add(1, Ordering::Relaxed);
        let samples = frames as usize * self.channels;
        let Some(buffer) = self.buffer.get_mut(..samples) else {
            // The unit broke its own slice limit; drop the slice.
            self.counters.overruns.fetch_add(1, Ordering::Relaxed);
            return 0;
        };
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: u32::try_from(self.channels).unwrap_or(u32::MAX),
                mDataByteSize: u32::try_from(size_of_val(buffer)).unwrap_or(u32::MAX),
                mData: buffer.as_mut_ptr().cast(),
            }],
        };
        let status = render(NonNull::from(&mut list));
        if status != 0 {
            self.counters.overruns.fetch_add(1, Ordering::Relaxed);
            return status;
        }
        let rendered = &self.buffer[..samples];
        if !self.has_signal && rendered.iter().any(|sample| *sample != 0.0) {
            self.counters.signal.store(true, Ordering::Release);
            self.has_signal = true;
        }
        push_capture(rendered, self.channels, &mut self.output, &self.counters);
        0
    }
}

/// Owns the heap [`Capture`] the input proc reaches through its refcon.
///
/// A raw pointer, not a `Box` field: the input proc mutates it from the
/// audio thread while the stream moves, which a `Box` would not allow.
struct CaptureState(NonNull<Capture>);

impl CaptureState {
    fn new(output: Producer<f32>, counters: Arc<Counters>, channels: usize) -> Self {
        let capture = Box::new(Capture {
            unit: ptr::null_mut(),
            buffer: Box::default(),
            channels,
            output,
            counters,
            has_signal: false,
        });
        Self(NonNull::from(Box::leak(capture)))
    }

    /// Size the slice buffer for `max_frames` (at least
    /// [`MIN_CAPTURE_FRAMES`]). Setup only: the unit must not be running.
    fn reserve(&mut self, max_frames: u32) {
        let capture = self.capture_mut();
        let samples = max_frames.max(MIN_CAPTURE_FRAMES) as usize * capture.channels;
        if capture.buffer.len() < samples {
            capture.buffer = vec![0.0; samples].into_boxed_slice();
        }
    }

    /// Record the unit to render from and return the input proc's refcon.
    fn attach(&mut self, unit: objc2_audio_toolbox::AudioUnit) -> NonNull<c_void> {
        self.capture_mut().unit = unit;
        self.0.cast()
    }

    fn capture_mut(&mut self) -> &mut Capture {
        raw::state(self.0)
    }
}

impl Drop for CaptureState {
    fn drop(&mut self) {
        raw::free(self.0);
    }
}

#[allow(unsafe_code)]
mod raw {
    //! The only `unsafe` in this backend: the C input proc and the heap
    //! state it reaches through its refcon.

    use std::{ffi::c_void, ptr::NonNull};

    use objc2_audio_toolbox::{AudioUnitRender, AudioUnitRenderActionFlags};
    use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};

    use super::Capture;

    /// `AURenderCallback` for `kAudioOutputUnitProperty_SetInputCallback`.
    pub(super) unsafe extern "C-unwind" fn input_proc(
        refcon: NonNull<c_void>,
        flags: NonNull<AudioUnitRenderActionFlags>,
        time: NonNull<AudioTimeStamp>,
        bus: u32,
        frames: u32,
        _data: *mut AudioBufferList,
    ) -> i32 {
        // SAFETY: the refcon is the `Capture` that `CaptureState` leaked for
        // this unit. `CaptureState` outlives the unit (it is declared after
        // it in `VoiceProcessingStream` and before it in `open`), and the
        // unit calls this proc on one thread at a time, so this is the only
        // live reference while it runs.
        let capture = unsafe { refcon.cast::<Capture>().as_mut() };
        let unit = capture.unit;
        capture.deliver(frames, |list| {
            // SAFETY: `unit` is the running unit that called this proc, the
            // flags and timestamp are the ones it passed in, and `list`
            // describes a buffer of exactly `frames` frames that stays
            // borrowed for this call.
            unsafe { AudioUnitRender(unit, flags.as_ptr(), time, bus, frames, list) }
        })
    }

    /// Borrow the state from setup code while the unit is not running.
    pub(super) fn state<'a>(capture: NonNull<Capture>) -> &'a mut Capture {
        // SAFETY: called from `CaptureState`'s `&mut self` methods, which run
        // before `start` (or with no unit at all), so the input proc holds no
        // reference. The pointer came from `Box::leak` and is not yet freed.
        unsafe { &mut *capture.as_ptr() }
    }

    /// Free the state. Only from `CaptureState::drop`, after the unit is gone.
    pub(super) fn free(capture: NonNull<Capture>) {
        // SAFETY: the pointer came from `Box::leak` in `CaptureState::new`,
        // is freed once, and the unit that could call the input proc has been
        // disposed (see the field order in `VoiceProcessingStream`).
        drop(unsafe { Box::from_raw(capture.as_ptr()) });
    }

    #[cfg(test)]
    mod tests {
        use std::sync::Arc;

        use rtrb::RingBuffer;

        use super::super::{CaptureState, MIN_CAPTURE_FRAMES};
        use crate::device::Counters;

        #[test]
        fn varying_slices_render_and_push_without_allocating() {
            let (producer, mut consumer) = RingBuffer::<f32>::new(8_192);
            let counters = Arc::new(Counters::default());
            let mut state = CaptureState::new(producer, Arc::clone(&counters), 2);
            state.reserve(1_156);
            let capture = state.capture_mut();
            let mut next = 1.0f32;
            let allocations = allocation_counter::measure(|| {
                // coreaudio-rs re-read the format and resized on each change.
                for frames in [512u32, 471, 470, 1_024, 1] {
                    let status = capture.deliver(frames, |list| {
                        // SAFETY: `deliver` passes a list with one buffer of
                        // `frames` stereo frames that it owns for this call.
                        let buffer = unsafe { list.as_ref().mBuffers[0] };
                        assert_eq!(buffer.mDataByteSize as usize, frames as usize * 2 * 4);
                        // SAFETY: as above; `mData` points at that many f32s.
                        let samples = unsafe {
                            std::slice::from_raw_parts_mut(
                                buffer.mData.cast::<f32>(),
                                frames as usize * 2,
                            )
                        };
                        for frame in samples.as_chunks_mut::<2>().0 {
                            *frame = [next, -next];
                            next += 1.0;
                        }
                        0
                    });
                    assert_eq!(status, 0);
                }
            });
            assert_eq!(allocations.count_total, 0);
            assert_eq!(counters.overruns(), 0);
            assert_eq!(counters.callbacks(), 5);
            assert!(counters.has_signal());
            let mut expected = 1.0f32;
            while let (Ok(left), Ok(right)) = (consumer.pop(), consumer.pop()) {
                assert_eq!((left, right), (expected, -expected));
                expected += 1.0;
            }
            assert_eq!(expected, 512.0 + 471.0 + 470.0 + 1_024.0 + 2.0);

            // A slice past the reserved limit, or a failed render, is
            // counted and dropped; nothing reaches the ring.
            let too_long = MIN_CAPTURE_FRAMES + 1;
            assert_eq!(capture.deliver(too_long, |_| unreachable!()), 0);
            assert_eq!(capture.deliver(16, |_| -1), -1);
            assert_eq!(counters.overruns(), 2);
            assert_eq!(consumer.slots(), 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use rtrb::RingBuffer;

    use super::{MutedSpeechActivity, VoiceProcessingConfig, pull_playback, push_capture};
    use crate::device::{
        Counters, DeviceManager, GraphConfig,
        convert::model::{Listener, Reader, Scripted, Step, frame, schedule},
    };

    #[test]
    fn defaults_enable_processing_and_agc() {
        let config = VoiceProcessingConfig::default();
        assert!(!config.bypass);
        assert!(config.automatic_gain_control);
        assert!(!config.muted);
    }

    #[test]
    fn callbacks_move_samples_without_allocating_or_blocking() {
        let (mut producer, mut consumer) = RingBuffer::new(4);
        let counters = Counters::default();
        let mut output = [1.0; 3];
        let allocations = allocation_counter::measure(|| {
            push_capture(&[0.25, -0.5], 1, &mut producer, &counters);
            pull_playback(&mut output, 1, &mut consumer, &counters);
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(output, [0.25, -0.5, 0.0]);
        assert_eq!(counters.underruns(), 1);

        push_capture(&[1.0, 2.0, 3.0, 4.0], 1, &mut producer, &counters);
        assert_eq!(counters.overruns(), 0);
        push_capture(&[5.0], 1, &mut producer, &counters);
        assert_eq!(counters.overruns(), 1);
    }

    proptest! {
        #[test]
        fn playback_keeps_stereo_frames_whole_under_any_interleaving(
            steps in schedule(),
            capacity in 2..48usize,
        ) {
            let mut ring = Scripted::new(capacity);
            let mut listener = Listener::new();
            let counters = Counters::default();
            for step in steps {
                match step {
                    Step::Produce(frames) => ring.commit(frames),
                    Step::Consume(frames, commits) => {
                        ring.during_next_callback(&commits);
                        let mut output = vec![9.0f32; frames * 2];
                        pull_playback(&mut output, 2, &mut ring, &counters);
                        ring.finish_callback();
                        listener.hear(&output, 2);
                    }
                }
            }
        }

        #[test]
        fn capture_keeps_stereo_frames_whole_under_any_interleaving(
            steps in schedule(),
            capacity in 1..48usize,
        ) {
            let (mut producer, mut consumer) = RingBuffer::<f32>::new(capacity);
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
                        push_capture(&input, 2, &mut producer, &counters);
                    }
                    Step::Consume(frames, _) => reader.read(&mut consumer, frames),
                }
            }
        }
    }

    #[test]
    fn speech_activity_is_an_edge_event() {
        assert_ne!(MutedSpeechActivity::Started, MutedSpeechActivity::Ended);
    }

    #[test]
    #[ignore = "requires real macOS input and output devices"]
    fn opens_toggles_and_stops_a_real_voice_processing_unit() {
        let manager = DeviceManager::new();
        for cycle in 0..3 {
            let (mut stream, _capture, _playback) = manager
                .open_voice_processing(
                    GraphConfig::MONO,
                    GraphConfig::STEREO,
                    VoiceProcessingConfig::default(),
                )
                .expect("open VoiceProcessingIO");
            assert!(stream.is_running().expect("read running state"));
            assert!(!stream.bypassed().expect("read bypass"));
            assert!(
                stream
                    .automatic_gain_control()
                    .expect("read automatic gain control")
            );
            stream.set_bypass(true).expect("set bypass");
            stream
                .set_automatic_gain_control(false)
                .expect("disable automatic gain control");
            stream.set_muted(true).expect("mute processed uplink");
            assert!(stream.bypassed().expect("read changed bypass"));
            assert!(
                !stream
                    .automatic_gain_control()
                    .expect("read changed automatic gain control")
            );
            assert!(stream.muted().expect("read mute"));
            let latency = stream.latency().expect("read latency");
            eprintln!("cycle {cycle}: VoiceProcessingIO reports {latency:?} processing latency");
            assert!(latency.as_secs_f32() < 1.0);
        }
    }
}
