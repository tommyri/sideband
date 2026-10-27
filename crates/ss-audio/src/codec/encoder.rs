use opusic_c::{Application, Bitrate, InbandFec, SampleRate};
use ss_core::{EncoderParams, OpusApplication, VoiceProfile};

use super::{Error, MAX_PACKET_BYTES, channels, frame_samples};

/// A profile-configured Opus encoder with reusable packet storage.
pub struct Encoder {
    inner: opusic_c::Encoder,
    params: EncoderParams,
    samples: usize,
    packet: [u8; MAX_PACKET_BYTES],
}

impl Encoder {
    /// Allocate and configure an encoder. Call off the audio thread.
    pub fn new(profile: VoiceProfile) -> Result<Self, Error> {
        let params = profile.encoder_params();
        let application = match params.application {
            OpusApplication::Voip => Application::Voip,
            OpusApplication::Audio => Application::Audio,
            OpusApplication::RestrictedLowdelay => Application::LowDelay,
            _ => return Err(Error::Configuration),
        };
        let mut inner =
            opusic_c::Encoder::new(channels(params.channels)?, SampleRate::Hz48000, application)?;
        inner.set_bitrate(Bitrate::Value(params.default_bitrate_bps))?;
        inner.set_inband_fec(if params.inband_fec {
            InbandFec::Mode1
        } else {
            InbandFec::Off
        })?;
        if let Some(loss) = params.expected_packet_loss_percent {
            inner.set_packet_loss(loss)?;
        }
        inner.set_complexity(10)?;
        inner.set_vbr(true)?;
        inner.set_dtx(false)?;
        Ok(Self {
            inner,
            params,
            samples: frame_samples(params.frame_size)? * usize::from(params.channels),
            packet: [0; MAX_PACKET_BYTES],
        })
    }

    /// Number of interleaved PCM samples required by each call to `encode`.
    pub fn input_samples(&self) -> usize {
        self.samples
    }

    /// Algorithmic encoder delay in samples per channel at 48 kHz.
    pub fn lookahead_samples(&mut self) -> Result<u32, Error> {
        Ok(self.inner.get_look_ahead()?)
    }

    /// Adapt bitrate within the current profile's inclusive range.
    pub fn set_bitrate(&mut self, bits_per_second: u32) -> Result<(), Error> {
        if !self.params.contains_bitrate(bits_per_second) {
            return Err(Error::Bitrate);
        }
        Ok(self.inner.set_bitrate(Bitrate::Value(bits_per_second))?)
    }

    /// Update the expected packet loss percentage (0 through 100).
    pub fn set_packet_loss(&mut self, percent: u8) -> Result<(), Error> {
        if percent > 100 {
            return Err(Error::PacketLoss);
        }
        Ok(self.inner.set_packet_loss(percent)?)
    }

    /// Encode exactly one frame, without allocating. Rejects NaN and infinity.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8], Error> {
        if pcm.len() != self.samples || !pcm.iter().all(|x| x.is_finite()) {
            return Err(Error::Pcm);
        }
        let count = self.inner.encode_float_to_slice(pcm, &mut self.packet)?;
        Ok(&self.packet[..count])
    }

    /// Configure 0–1000 ms of redundancy in 10 ms increments; zero disables it.
    /// FEC stays enabled. The packet remains bounded by `MAX_PACKET_BYTES`.
    /// This is a maximum history, not a guarantee: libopus budgets redundancy
    /// using bitrate and `set_packet_loss`, and may omit it at low loss.
    #[cfg(feature = "dred")]
    pub fn set_dred_duration_ms(&mut self, milliseconds: u16) -> Result<(), Error> {
        if milliseconds > 1000 || !milliseconds.is_multiple_of(10) {
            return Err(Error::DredDuration);
        }
        // Validated above: at most 100 ten-millisecond units.
        let units = u8::try_from(milliseconds / 10).map_err(|_| Error::DredDuration)?;
        Ok(self.inner.set_dred_duration(units)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_profile_controls_and_adaptation_limits() {
        for profile in [
            VoiceProfile::Voice,
            VoiceProfile::Music,
            VoiceProfile::LowLatency,
        ] {
            let mut encoder = Encoder::new(profile).unwrap();
            let p = profile.encoder_params();
            #[cfg(feature = "dred")]
            assert_eq!(encoder.inner.get_dred_duration().unwrap(), 0);
            assert_eq!(
                encoder.inner.get_bitrate().unwrap(),
                Bitrate::Value(p.default_bitrate_bps)
            );
            assert_eq!(encoder.inner.get_complexity().unwrap(), 10);
            assert_eq!(encoder.inner.get_inband_fec().unwrap(), InbandFec::Mode1);
            assert_eq!(
                encoder.inner.get_packet_loss().unwrap(),
                p.expected_packet_loss_percent.unwrap_or(0)
            );
            assert!(encoder.inner.get_vbr().unwrap());
            assert!(!encoder.inner.get_dtx().unwrap());
            let expected = match profile {
                VoiceProfile::Voice => Application::Voip,
                VoiceProfile::Music => Application::Audio,
                _ => Application::LowDelay,
            };
            assert_eq!(encoder.inner.get_application().unwrap(), expected);
            encoder.set_bitrate(p.min_bitrate_bps).unwrap();
            encoder.set_bitrate(p.max_bitrate_bps).unwrap();
            assert_eq!(
                encoder.set_bitrate(p.min_bitrate_bps - 1),
                Err(Error::Bitrate)
            );
            assert_eq!(
                encoder.set_bitrate(p.max_bitrate_bps + 1),
                Err(Error::Bitrate)
            );
            encoder.set_packet_loss(100).unwrap();
            assert_eq!(encoder.set_packet_loss(101), Err(Error::PacketLoss));
            assert_eq!(encoder.inner.get_packet_loss().unwrap(), 100);
        }
    }
}
