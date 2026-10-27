//! Device format selection. Pure so the buffer and rate rules can be tested
//! without opening a host.

use cpal::{BufferSize, SampleFormat, SupportedBufferSize, SupportedStreamConfigRange};

use super::GRAPH_RATE;

/// What the rest of the audio graph sends and receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphConfig {
    /// `1` for mono, `2` for stereo.
    pub channels: u16,
}

impl GraphConfig {
    /// Mono 48 kHz frames.
    pub const MONO: Self = Self { channels: 1 };
    /// Stereo 48 kHz frames.
    pub const STEREO: Self = Self { channels: 2 };

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

/// Pick the format closest to 48 kHz and the requested channel count.
///
/// Preference order is channel count, then a rate range that contains 48 kHz,
/// then `f32`. The sample rate itself is 48 kHz when the range allows it, and
/// the nearest endpoint otherwise.
pub(crate) fn choose_format(
    ranges: &[SupportedStreamConfigRange],
    graph_channels: u16,
) -> Option<ChosenFormat> {
    let range = ranges
        .iter()
        .copied()
        .min_by_key(|range| rank(*range, graph_channels))?;
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

fn rank(range: SupportedStreamConfigRange, graph_channels: u16) -> (u8, u32, u8) {
    let channel_rank = u8::from(range.channels() != graph_channels);
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

    use super::{choose_buffer_size, choose_format, frames_for};

    fn range(
        channels: u16,
        min: u32,
        max: u32,
        format: SampleFormat,
        buffer: SupportedBufferSize,
    ) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, min, max, buffer, format)
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
