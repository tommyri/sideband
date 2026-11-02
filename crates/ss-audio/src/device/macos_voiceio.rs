//! Apple's full-duplex VoiceProcessingIO device backend.
//!
//! A single Audio Unit owns capture and render so its echo canceller sees the
//! exact far-end signal sent to the speakers. The graph-facing rings remain
//! 48 kHz interleaved `f32`, matching the portable cpal backend.

use std::{
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
    AUVoiceIOMutedSpeechActivityEventListener, AUVoiceIOSpeechActivityEvent,
    kAUVoiceIOProperty_BypassVoiceProcessing, kAUVoiceIOProperty_MuteOutput,
    kAUVoiceIOProperty_MutedSpeechActivityEventListener,
    kAUVoiceIOProperty_VoiceProcessingEnableAGC, kAudioOutputUnitProperty_EnableIO,
    kAudioOutputUnitProperty_IsRunning, kAudioUnitProperty_Latency,
};
use rtrb::{Consumer, Producer, RingBuffer};

use super::{Counters, Error, GRAPH_RATE, GraphConfig};

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
    unit: AudioUnit,
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

        let mut unit = new_voice_processing_unit()?;
        enable_duplex(&mut unit)?;
        unit.set_stream_format(format(capture.channels), Scope::Output, Element::Input)
            .map_err(Error::VoiceProcessing)?;
        unit.set_stream_format(format(playback.channels), Scope::Input, Element::Output)
            .map_err(Error::VoiceProcessing)?;

        install_capture(&mut unit, capture_tx, Arc::clone(&capture_counters))?;
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

fn install_capture(
    unit: &mut AudioUnit,
    mut output: Producer<f32>,
    counters: Arc<Counters>,
) -> Result<(), Error> {
    type CallbackArgs = Args<data::Interleaved<f32>>;
    let mut has_signal = false;
    unit.set_input_callback(move |args: CallbackArgs| {
        counters.callbacks.fetch_add(1, Ordering::Relaxed);
        if !has_signal && args.data.buffer.iter().any(|sample| *sample != 0.0) {
            counters.signal.store(true, Ordering::Release);
            has_signal = true;
        }
        if push_capture(args.data.buffer, &mut output) {
            counters.overruns.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    })
    .map_err(Error::VoiceProcessing)
}

fn install_playback(
    unit: &mut AudioUnit,
    mut input: Consumer<f32>,
    counters: Arc<Counters>,
) -> Result<(), Error> {
    type CallbackArgs = Args<data::Interleaved<f32>>;
    unit.set_render_callback(move |args: CallbackArgs| {
        counters.callbacks.fetch_add(1, Ordering::Relaxed);
        if pull_playback(args.data.buffer, &mut input) {
            counters.underruns.fetch_add(1, Ordering::Relaxed);
        }
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

/// Returns true when samples were dropped.
fn push_capture(samples: &[f32], output: &mut Producer<f32>) -> bool {
    for sample in samples {
        if output.push(*sample).is_err() {
            return true;
        }
    }
    false
}

/// Returns true when silence had to fill an underrun.
fn pull_playback(samples: &mut [f32], input: &mut Consumer<f32>) -> bool {
    let mut underrun = false;
    for sample in samples {
        *sample = input.pop().unwrap_or_else(|_| {
            underrun = true;
            0.0
        });
    }
    underrun
}

#[cfg(test)]
mod tests {
    use super::{MutedSpeechActivity, VoiceProcessingConfig, pull_playback, push_capture};
    use crate::device::{DeviceManager, GraphConfig};
    use rtrb::RingBuffer;

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
        assert!(!push_capture(&[0.25, -0.5], &mut producer));
        let mut output = [1.0; 3];
        assert!(pull_playback(&mut output, &mut consumer));
        assert_eq!(output, [0.25, -0.5, 0.0]);

        assert!(!push_capture(&[1.0, 2.0, 3.0, 4.0], &mut producer));
        assert!(push_capture(&[5.0], &mut producer));
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
