//! Device format selection. Pure so the buffer and rate rules can be tested
//! without opening a host.

use cpal::{BufferSize, SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

use super::GRAPH_RATE;

/// Which device inputs make up mono capture.
///
/// A microphone on input 1 of a multi-input interface keeps its level only
/// when that one input is taken: averaging two inputs costs 6 dB, eight
/// inputs 18 dB, plus the noise of the unused ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureChannel {
    /// One input, counted from zero. An index the device does not have
    /// falls back to the first input.
    Index(u16),
    /// The mean of all device inputs.
    Average,
}

impl Default for CaptureChannel {
    /// The first input.
    fn default() -> Self {
        Self::FIRST
    }
}

impl CaptureChannel {
    /// The first device input, the default.
    pub const FIRST: Self = Self::Index(0);

    /// The zero-based input to take from a device with `device_channels`
    /// inputs, or `None` to average them.
    #[must_use]
    pub fn resolve(self, device_channels: usize) -> Option<usize> {
        match self {
            Self::Average if device_channels > 1 => None,
            Self::Index(index) if usize::from(index) < device_channels => Some(usize::from(index)),
            Self::Average | Self::Index(_) => Some(0),
        }
    }
}

/// What the rest of the audio graph sends and receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphConfig {
    /// `1` for mono, `2` for stereo.
    pub channels: u16,
    /// Mono capture only: the device inputs to take. Stereo capture takes
    /// inputs 1 and 2; playback and Apple's VoiceProcessingIO ignore it.
    pub input: CaptureChannel,
}

impl GraphConfig {
    /// Mono 48 kHz frames from the first device input.
    pub const MONO: Self = Self {
        channels: 1,
        input: CaptureChannel::FIRST,
    };
    /// Stereo 48 kHz frames.
    pub const STEREO: Self = Self {
        channels: 2,
        input: CaptureChannel::FIRST,
    };

    /// The same graph, capturing from `input`.
    #[must_use]
    pub const fn with_input(self, input: CaptureChannel) -> Self {
        Self { input, ..self }
    }

    /// Interleaved samples in one 10 ms frame.
    #[must_use]
    pub const fn samples_per_frame(self) -> usize {
        self.channels as usize * super::SAMPLES_PER_CHANNEL
    }
}

/// The stream configuration chosen for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChosenFormat {
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    pub(crate) sample_format: SampleFormat,
    pub(crate) buffer_size: BufferSize,
}

/// A list offering every channel count from 1 to at least this many comes
/// from a plug-in that converts any layout, not from the hardware: ALSA's
/// `plug` (cpal caps its `u32::MAX` at 64), the `default`, `pulse` and
/// `pipewire` ALSA plug-ins (32 or 64), and cpal's PulseAudio host (31).
const CONVERTING_CHANNELS: u16 = 31;

/// The inputs a capture device has, from the configs its host lists.
///
/// The count is the most channels any config offers: CoreAudio lists one
/// config with the device's total, WASAPI shared mode the mix format, and
/// PipeWire the node's channels. A converting plug-in (see
/// [`CONVERTING_CHANNELS`]) offers every count, so its layout is the stereo
/// one cpal picks as its default config, or the most it offers below two.
/// `None` when the host lists no input config.
pub(crate) fn input_count(ranges: &[SupportedStreamConfigRange]) -> Option<u16> {
    let most = ranges
        .iter()
        .map(SupportedStreamConfigRange::channels)
        .max()?;
    let converting = most >= CONVERTING_CHANNELS
        && (1..=most).all(|count| ranges.iter().any(|range| range.channels() == count));
    Some(if converting { 2 } else { most })
}

/// The device channel count a capture stream for `config` should open.
///
/// Mono from input 1 needs one channel, so a mono config is still the first
/// choice. Another input, or the average, needs the device's full count
/// ([`input_count`]): a host that also offers mono (some ALSA `plug`
/// devices) would otherwise open one channel and silently send input 1.
/// Stereo needs two.
pub(crate) fn capture_channels(ranges: &[SupportedStreamConfigRange], config: GraphConfig) -> u16 {
    match input_count(ranges) {
        Some(inputs) if config.channels == 1 && config.input.resolve(inputs.into()) != Some(0) => {
            inputs
        }
        _ => config.channels,
    }
}

/// Pick the format closest to 48 kHz and `channels`.
///
/// Preference order is channel count, then a rate range that contains 48 kHz,
/// then `f32`. The sample rate itself is 48 kHz when the range allows it, and
/// the nearest endpoint otherwise.
pub(crate) fn choose_format(
    ranges: &[SupportedStreamConfigRange],
    channels: u16,
) -> Option<ChosenFormat> {
    let range = ranges
        .iter()
        .copied()
        .min_by_key(|range| rank(*range, channels))?;
    let sample_rate = closest_rate(range.min_sample_rate(), range.max_sample_rate());
    let supported = range.try_with_sample_rate(sample_rate)?;
    Some(ChosenFormat {
        channels: supported.channels(),
        sample_rate: supported.sample_rate(),
        sample_format: supported.sample_format(),
        buffer_size: choose_buffer_size(*supported.buffer_size(), sample_rate),
    })
}

/// Smallest fixed buffer of at least 5 ms, or the host default when the
/// device cannot express that size.
pub(crate) fn choose_buffer_size(supported: SupportedBufferSize, sample_rate: u32) -> BufferSize {
    let min_frames = frames_for(sample_rate, 5);
    match supported {
        SupportedBufferSize::Range { min, max } if min <= max && max >= min_frames => {
            BufferSize::Fixed(min.max(min_frames))
        }
        SupportedBufferSize::Range { .. } | SupportedBufferSize::Unknown => BufferSize::Default,
    }
}

fn rank(range: SupportedStreamConfigRange, channels: u16) -> (u8, u32, u8) {
    let channel_rank = u8::from(range.channels() != channels);
    let rate_rank =
        if range.min_sample_rate() <= GRAPH_RATE && GRAPH_RATE <= range.max_sample_rate() {
            0
        } else {
            let mid = range.min_sample_rate() / 2 + range.max_sample_rate() / 2;
            mid.abs_diff(GRAPH_RATE)
        };
    let format_rank = match range.sample_format() {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I16 => 2,
        SampleFormat::U16 => 3,
        SampleFormat::F64 => 4,
        _ => 5,
    };
    (channel_rank, rate_rank, format_rank)
}

fn closest_rate(min: u32, max: u32) -> u32 {
    GRAPH_RATE.clamp(min, max)
}

fn frames_for(sample_rate: u32, milliseconds: u32) -> u32 {
    let frames = u64::from(sample_rate) * u64::from(milliseconds) / 1_000;
    u32::try_from(frames).unwrap_or(u32::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use cpal::{BufferSize, SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

    use super::{
        CaptureChannel, GraphConfig, capture_channels, choose_buffer_size, choose_format,
        frames_for, input_count,
    };

    #[test]
    fn a_missing_input_falls_back_to_the_first() {
        assert_eq!(CaptureChannel::default().resolve(8), Some(0));
        assert_eq!(CaptureChannel::Index(3).resolve(8), Some(3));
        assert_eq!(CaptureChannel::Index(3).resolve(2), Some(0));
        assert_eq!(CaptureChannel::Average.resolve(8), None);
        // Nothing to average on a mono device.
        assert_eq!(CaptureChannel::Average.resolve(1), Some(0));
    }

    fn range(
        channels: u16,
        min: u32,
        max: u32,
        format: SampleFormat,
        buffer: SupportedBufferSize,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, min, max, buffer, format)
    }

    fn channels(counts: impl IntoIterator<Item = u16>) -> Vec<SupportedStreamConfigRange> {
        counts
            .into_iter()
            .map(|count| {
                range(
                    count,
                    48_000,
                    48_000,
                    SampleFormat::F32,
                    SupportedBufferSize::Unknown,
                )
            })
            .collect()
    }

    #[test]
    fn a_device_has_as_many_inputs_as_its_widest_config() {
        // CoreAudio: one config with the device's total.
        assert_eq!(input_count(&channels([8])), Some(8));
        // ALSA `hw` with mono beside the full count.
        assert_eq!(input_count(&channels([1, 4])), Some(4));
        assert_eq!(input_count(&channels([1, 2, 3, 4, 5, 6, 7, 8])), Some(8));
        assert_eq!(input_count(&[]), None);
    }

    #[test]
    fn a_converting_plug_in_counts_as_stereo() {
        // ALSA `plug` and `default`: every count up to 64.
        assert_eq!(input_count(&channels(1..=64)), Some(2));
        // cpal's PulseAudio host: 1 to 31.
        assert_eq!(input_count(&channels(1..=31)), Some(2));
        // Gaps mean the hardware's own list.
        assert_eq!(input_count(&channels([1, 2, 32])), Some(32));
    }

    #[test]
    fn only_a_selection_past_input_one_opens_the_full_count() {
        let ranges = channels([1, 4]);
        let mono = GraphConfig::MONO;
        assert_eq!(capture_channels(&ranges, mono), 1);
        assert_eq!(
            capture_channels(&ranges, mono.with_input(CaptureChannel::Index(1))),
            4
        );
        assert_eq!(
            capture_channels(&ranges, mono.with_input(CaptureChannel::Average)),
            4
        );
        // Input 9 on a four-input device is input 1.
        assert_eq!(
            capture_channels(&ranges, mono.with_input(CaptureChannel::Index(8))),
            1
        );
        let stereo = GraphConfig::STEREO.with_input(CaptureChannel::Average);
        assert_eq!(capture_channels(&ranges, stereo), 2);
        assert_eq!(capture_channels(&[], mono), 1);
    }

    #[test]
    fn prefers_48k_and_the_requested_channel_count() {
        let ranges = [
            range(
                1,
                8_000,
                16_000,
                SampleFormat::F32,
                SupportedBufferSize::Unknown,
            ),
            range(
                2,
                44_100,
                96_000,
                SampleFormat::I16,
                SupportedBufferSize::Unknown,
            ),
            range(
                2,
                48_000,
                48_000,
                SampleFormat::F32,
                SupportedBufferSize::Unknown,
            ),
        ];
        let chosen = choose_format(&ranges, 2).unwrap();
        assert_eq!(chosen.channels, 2);
        assert_eq!(chosen.sample_rate, 48_000);
        assert_eq!(chosen.sample_format, SampleFormat::F32);
    }

    #[test]
    fn falls_back_to_the_nearest_rate() {
        let ranges = [range(
            1,
            8_000,
            16_000,
            SampleFormat::I16,
            SupportedBufferSize::Unknown,
        )];
        let chosen = choose_format(&ranges, 1).unwrap();
        assert_eq!(chosen.sample_rate, 16_000);
    }

    #[test]
    fn buffer_request_is_at_least_five_milliseconds() {
        assert_eq!(frames_for(48_000, 5), 240);
        assert_eq!(
            choose_buffer_size(
                SupportedBufferSize::Range {
                    min: 64,
                    max: 1_024
                },
                48_000
            ),
            BufferSize::Fixed(240)
        );
        assert_eq!(
            choose_buffer_size(
                SupportedBufferSize::Range {
                    min: 512,
                    max: 4_096
                },
                48_000
            ),
            BufferSize::Fixed(512)
        );
        assert_eq!(
            choose_buffer_size(SupportedBufferSize::Range { min: 32, max: 128 }, 48_000),
            BufferSize::Default
        );
        assert_eq!(
            choose_buffer_size(SupportedBufferSize::Unknown, 48_000),
            BufferSize::Default
        );
    }
}
