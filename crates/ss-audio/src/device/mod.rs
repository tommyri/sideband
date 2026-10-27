//! Capture and playback devices.
//!
//! The real-time callback converts sample format and channel count into a
//! ring. Resampling, when the device is not already at 48 kHz, runs on a
//! worker thread. Readers of a capture ring, and writers of a playback ring,
//! see 10 ms frames: 480 samples per channel at [`crate::resample::GRAPH_RATE`].

mod convert;
mod manager;
mod select;
mod stream;
mod watch;

pub use manager::DeviceManager;
pub use select::GraphConfig;
pub use stream::{CaptureStream, Counters, PlaybackStream};
pub use watch::{DeviceEvent, DeviceId, DeviceInfo, Direction};

pub use crate::{mixer::SAMPLES_PER_CHANNEL, resample::GRAPH_RATE};

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
}
