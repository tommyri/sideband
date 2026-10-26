//! Voice encoder profiles from the media datagram section of the protocol.

use core::{str::FromStr, time::Duration};

use crate::error::CoreError;

/// Encoder profile selected by a voice channel's settings.
///
/// Stored as `voice`, `music`, or `low_latency`. [`VoiceProfile::Voice`] is
/// the default. Bitrate, channel count, Opus application, and frame size come
/// from the table in `02-protocol.md` §4. A kilobit in that table is 1000 bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum VoiceProfile {
    /// Speech. 48–96 kb/s, mono, 20 ms frames.
    #[default]
    Voice,
    /// Music. 96–192 kb/s, stereo, 20 ms frames.
    Music,
    /// Low delay. 64 kb/s, mono, 10 ms frames.
    LowLatency,
}

impl VoiceProfile {
    /// The `voice_profile` text stored on a channel.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Music => "music",
            Self::LowLatency => "low_latency",
        }
    }

    /// Encoder settings for this profile.
    #[must_use]
    pub const fn encoder_params(self) -> EncoderParams {
        match self {
            Self::Voice => EncoderParams {
                min_bitrate_bps: 48_000,
                max_bitrate_bps: 96_000,
                default_bitrate_bps: 80_000,
                channels: 1,
                application: OpusApplication::Voip,
                frame_size: Duration::from_millis(20),
                inband_fec: true,
                expected_packet_loss_percent: Some(5),
            },
            Self::Music => EncoderParams {
                min_bitrate_bps: 96_000,
                max_bitrate_bps: 192_000,
                default_bitrate_bps: 128_000,
                channels: 2,
                application: OpusApplication::Audio,
                frame_size: Duration::from_millis(20),
                inband_fec: true,
                expected_packet_loss_percent: None,
            },
            Self::LowLatency => EncoderParams {
                min_bitrate_bps: 64_000,
                max_bitrate_bps: 64_000,
                default_bitrate_bps: 64_000,
                channels: 1,
                application: OpusApplication::RestrictedLowdelay,
                frame_size: Duration::from_millis(10),
                inband_fec: true,
                expected_packet_loss_percent: None,
            },
        }
    }
}

impl core::fmt::Display for VoiceProfile {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for VoiceProfile {
    type Err = CoreError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "voice" => Ok(Self::Voice),
            "music" => Ok(Self::Music),
            "low_latency" => Ok(Self::LowLatency),
            _ => Err(CoreError::UnknownVoiceProfile {
                value: text.to_owned(),
            }),
        }
    }
}

/// Opus coding mode (`OPUS_APPLICATION_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum OpusApplication {
    /// `OPUS_APPLICATION_VOIP`.
    Voip,
    /// `OPUS_APPLICATION_AUDIO`.
    Audio,
    /// `OPUS_APPLICATION_RESTRICTED_LOWDELAY`.
    RestrictedLowdelay,
}

/// Settings an Opus encoder takes from a [`VoiceProfile`].
///
/// Bitrates are bits per second. `min_bitrate_bps` and `max_bitrate_bps` are
/// inclusive. The protocol table gives `low_latency` a single bitrate, so
/// that profile's minimum and maximum are both `64_000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EncoderParams {
    /// Lowest bitrate a channel may configure, in bits per second.
    pub min_bitrate_bps: u32,
    /// Highest bitrate a channel may configure, in bits per second.
    pub max_bitrate_bps: u32,
    /// Bitrate used when the channel does not override it, in bits per second.
    pub default_bitrate_bps: u32,
    /// Opus channel count. `1` is mono, `2` is stereo.
    pub channels: u8,
    /// Opus application mode.
    pub application: OpusApplication,
    /// Opus frame duration.
    pub frame_size: Duration,
    /// In-band forward error correction (`OPUS_SET_INBAND_FEC`).
    pub inband_fec: bool,
    /// Expected packet loss percent (`OPUS_SET_PACKET_LOSS_PERC`).
    ///
    /// `Some` when the protocol table states a percent. `None` when it says
    /// FEC is on without a percent (`music`, `low_latency`).
    pub expected_packet_loss_percent: Option<u8>,
}

impl EncoderParams {
    /// Whether `bitrate_bps` is inside the inclusive profile range.
    #[must_use]
    pub const fn contains_bitrate(self, bitrate_bps: u32) -> bool {
        bitrate_bps >= self.min_bitrate_bps && bitrate_bps <= self.max_bitrate_bps
    }
}

#[cfg(test)]
mod tests {
    use core::{str::FromStr, time::Duration};

    use crate::{
        error::CoreError,
        voice::{EncoderParams, OpusApplication, VoiceProfile},
    };

    struct Expected {
        profile: VoiceProfile,
        text: &'static str,
        min_bitrate_bps: u32,
        default_bitrate_bps: u32,
        max_bitrate_bps: u32,
        channels: u8,
        application: OpusApplication,
        frame: Duration,
        loss: Option<u8>,
    }

    fn table() -> [Expected; 3] {
        [
            Expected {
                profile: VoiceProfile::Voice,
                text: "voice",
                min_bitrate_bps: 48_000,
                default_bitrate_bps: 80_000,
                max_bitrate_bps: 96_000,
                channels: 1,
                application: OpusApplication::Voip,
                frame: Duration::from_millis(20),
                loss: Some(5),
            },
            Expected {
                profile: VoiceProfile::Music,
                text: "music",
                min_bitrate_bps: 96_000,
                default_bitrate_bps: 128_000,
                max_bitrate_bps: 192_000,
                channels: 2,
                application: OpusApplication::Audio,
                frame: Duration::from_millis(20),
                loss: None,
            },
            Expected {
                profile: VoiceProfile::LowLatency,
                text: "low_latency",
                min_bitrate_bps: 64_000,
                default_bitrate_bps: 64_000,
                max_bitrate_bps: 64_000,
                channels: 1,
                application: OpusApplication::RestrictedLowdelay,
                frame: Duration::from_millis(10),
                loss: None,
            },
        ]
    }

    #[test]
    fn encoder_params_match_the_protocol_table() {
        for row in table() {
            let params = row.profile.encoder_params();
            assert_eq!(
                params,
                EncoderParams {
                    min_bitrate_bps: row.min_bitrate_bps,
                    max_bitrate_bps: row.max_bitrate_bps,
                    default_bitrate_bps: row.default_bitrate_bps,
                    channels: row.channels,
                    application: row.application,
                    frame_size: row.frame,
                    inband_fec: true,
                    expected_packet_loss_percent: row.loss,
                }
            );
            assert!(params.contains_bitrate(row.min_bitrate_bps));
            assert!(params.contains_bitrate(row.default_bitrate_bps));
            assert!(params.contains_bitrate(row.max_bitrate_bps));
            assert!(!params.contains_bitrate(row.min_bitrate_bps.saturating_sub(1)));
            if row.max_bitrate_bps < u32::MAX {
                assert!(!params.contains_bitrate(row.max_bitrate_bps + 1));
            }
            assert_eq!(row.profile.as_str(), row.text);
            assert_eq!(row.profile.to_string(), row.text);
            assert_eq!(VoiceProfile::from_str(row.text).unwrap(), row.profile);
        }
        assert_eq!(VoiceProfile::default(), VoiceProfile::Voice);
    }

    #[test]
    fn unknown_profile_text_is_an_error() {
        for text in ["", "Voice", "VOICE", "low-latency", "speech"] {
            assert_eq!(
                VoiceProfile::from_str(text),
                Err(CoreError::UnknownVoiceProfile {
                    value: text.to_owned(),
                })
            );
        }
    }
}
