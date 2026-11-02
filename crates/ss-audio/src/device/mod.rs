//! Capture and playback devices.
//!
//! The real-time callback converts sample format and channel count into a
//! ring. Resampling, when the device is not already at 48 kHz, runs on a
//! worker thread. Readers of a capture ring, and writers of a playback ring,
//! see 10 ms frames: 480 samples per channel at [`crate::resample::GRAPH_RATE`].
//!
//! On macOS a denied microphone does not fail in CoreAudio; the stream runs
//! and delivers zeros. [`DeviceManager::open_capture`] reads the stored
//! permission first and returns [`Error::PermissionDenied`]. A capture that
//! still delivers only zeros is reported as [`DeviceEvent::SilentInput`].

mod convert;
#[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
mod macos_voiceio;
mod manager;
mod select;
mod stream;
mod watch;

#[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
pub use macos_voiceio::{MutedSpeechActivity, VoiceProcessingConfig, VoiceProcessingStream};
pub use manager::DeviceManager;
pub use select::GraphConfig;
pub use stream::{CaptureStream, Counters, PlaybackStream};
pub use watch::{DeviceEvent, DeviceId, DeviceInfo, Direction, MicrophoneAccess};

pub use crate::{mixer::SAMPLES_PER_CHANNEL, resample::GRAPH_RATE};

/// A system microphone-processing mode selected by the user in Control Center.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MicrophoneMode {
    /// Standard voice DSP.
    Standard,
    /// Minimal filtering while retaining echo cancellation.
    WideSpectrum,
    /// Speech enhancement with background-noise attenuation.
    VoiceIsolation,
    /// The OS does not expose a known mode on this machine.
    Unknown,
}

/// The user's preferred microphone mode and the mode active on this route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MicrophoneModeStatus {
    pub preferred: MicrophoneMode,
    pub active: MicrophoneMode,
}

/// Read the Control Center microphone-mode state without changing it.
#[must_use]
pub fn microphone_mode_status() -> MicrophoneModeStatus {
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::microphone_mode_status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        MicrophoneModeStatus {
            preferred: MicrophoneMode::Unknown,
            active: MicrophoneMode::Unknown,
        }
    }
}

/// Failures while enumerating or opening a device.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The graph asked for a channel count other than mono or stereo.
    #[error("channel count must be 1 or 2")]
    Channels,
    /// The host has no device in the requested direction.
    #[error("no audio device is available")]
    NoDevice,
    /// The device cannot open an input or output stream.
    #[error("the device does not support this stream")]
    Unsupported,
    /// The device's sample format has no callback conversion.
    #[error("unsupported sample format")]
    SampleFormat,
    /// `cpal` rejected the host, the device, or the stream.
    #[error("audio device error")]
    Backend(#[source] cpal::Error),
    /// The sinc resampler could not be constructed for this device rate.
    #[error("could not build the resampler")]
    Resampler,
    /// The operating system refused to create the resampling thread.
    #[error("could not start the audio worker")]
    Worker,
    /// The user or a device policy denied microphone access to this app.
    #[error("microphone access is denied for this app; allow it in the system privacy settings")]
    PermissionDenied,
    /// Apple's VoiceProcessingIO unit could not be configured or started.
    #[cfg(all(target_os = "macos", feature = "macos-voiceio"))]
    #[error("Apple voice processing audio error")]
    VoiceProcessing(#[source] coreaudio::Error),
}

impl Error {
    /// Keep `cpal`'s permission failures distinct from other host errors.
    pub(crate) fn backend(error: cpal::Error) -> Self {
        if matches!(error.kind(), cpal::ErrorKind::PermissionDenied) {
            Self::PermissionDenied
        } else {
            Self::Backend(error)
        }
    }
}

/// The microphone decision the operating system has stored for this process.
///
/// Reading it never shows a prompt. On macOS 14 and newer it comes from
/// `AVAudioApplication`. Other platforms, and macOS 13, report
/// [`MicrophoneAccess::Unknown`].
#[must_use]
pub fn microphone_access() -> MicrophoneAccess {
    #[cfg(target_os = "macos")]
    {
        crate::platform::macos::microphone_access()
    }
    #[cfg(not(target_os = "macos"))]
    {
        MicrophoneAccess::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn host_permission_failures_become_permission_denied() {
        let denied = cpal::Error::new(cpal::ErrorKind::PermissionDenied);
        assert!(matches!(Error::backend(denied), Error::PermissionDenied));
        let busy = cpal::Error::new(cpal::ErrorKind::DeviceBusy);
        assert!(matches!(Error::backend(busy), Error::Backend(_)));
    }
}
