use std::time::Duration;

use ss_core::VoiceProfile;

use super::{Error, OpusPacket, channels, frame_samples, native};

/// Ordered-stream decoder with Deep PLC enabled and bandwidth extension off.
/// Codec construction allocates; decoding reuses storage.
pub struct Decoder {
    inner: native::Decoder,
    samples: usize,
    channels: usize,
    pcm: Box<[f32]>,
    #[cfg(feature = "dred")]
    dred: native::Dred,
    #[cfg(feature = "dred")]
    recovered: Box<[f32]>,
}

impl Decoder {
    /// Construct a mono or stereo decoder for 20 ms packets.
    pub fn new(channel_count: u8) -> Result<Self, Error> {
        Self::with_frame_duration(channel_count, Duration::from_millis(20))
    }

    /// Construct a decoder matching a profile, including its initial PLC length.
    pub fn for_profile(profile: VoiceProfile) -> Result<Self, Error> {
        let params = profile.encoder_params();
        Self::with_frame_duration(params.channels, params.frame_size)
    }

    /// Construct for 10 or 20 ms packets. Packet duration stays fixed for this
    /// stream; recreate on a profile change, including changes before first audio.
    pub fn with_frame_duration(channel_count: u8, duration: Duration) -> Result<Self, Error> {
        channels(channel_count)?;
        let samples = frame_samples(duration)?;
        let channels = usize::from(channel_count);
        Ok(Self {
            inner: native::Decoder::new(channel_count)?,
            samples,
            channels,
            pcm: vec![0.0; samples * channels].into_boxed_slice(),
            #[cfg(feature = "dred")]
            dred: native::Dred::new()?,
            #[cfg(feature = "dred")]
            recovered: vec![0.0; 48_000 * channels].into_boxed_slice(),
        })
    }

    fn validate(&self, packet: &[u8]) -> Result<(), Error> {
        if OpusPacket::toc(packet)?.samples_per_channel() != self.samples {
            return Err(Error::Duration);
        }
        Ok(())
    }

    /// Decode one packet or conceal one missing frame (`None`). For FEC, pass
    /// the following packet with `fec = true`, then decode that same packet
    /// again with `fec = false`. If no FEC exists, libopus performs PLC.
    pub fn decode(&mut self, packet: Option<&[u8]>, fec: bool) -> Result<&[f32], Error> {
        if let Some(packet) = packet {
            self.validate(packet)?;
        }
        let samples = self.inner.decode(packet, fec, &mut self.pcm)?;
        Ok(&self.pcm[..samples * self.channels])
    }

    /// Recover consecutive frames immediately preceding `packet`, oldest first.
    /// Decode the packet normally afterwards. Rejects zero, more than one second,
    /// or insufficient redundancy without advancing the audio decoder state;
    /// callers may then use PLC. Output can be mono or interleaved stereo.
    #[cfg(feature = "dred")]
    pub fn decode_dred(&mut self, packet: &[u8], lost_frames: usize) -> Result<&[f32], Error> {
        self.validate(packet)?;
        let total = lost_frames
            .checked_mul(self.samples)
            .filter(|n| (1..=48_000).contains(n))
            .ok_or(Error::DredDuration)?;
        self.dred.parse(packet, total)?;
        let output = &mut self.recovered[..total * self.channels];
        for (index, frame) in output
            .chunks_exact_mut(self.samples * self.channels)
            .enumerate()
        {
            self.inner
                .decode_dred(&self.dred, total - index * self.samples, frame)?;
        }
        Ok(output)
    }
}
