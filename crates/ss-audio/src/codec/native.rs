//! Missing opusic-c APIs only. Native allocations are owned with matching C
//! destructors. No pointer escapes, no shared decoder access, no runtime alloc.

use std::ptr::{self, NonNull};

use opusic_c::sys;

use super::{Error, MAX_PACKET_BYTES};

fn check(code: i32) -> Result<usize, Error> {
    usize::try_from(code).map_err(|_| Error::Opus(code.into()))
}

fn int(value: usize) -> Result<i32, Error> {
    i32::try_from(value).map_err(|_| Error::Configuration)
}

fn packet_len(packet: &[u8]) -> Result<i32, Error> {
    if packet.is_empty() || packet.len() > MAX_PACKET_BYTES {
        return Err(Error::Packet);
    }
    int(packet.len())
}

pub(super) fn packet_info(packet: &[u8]) -> Result<(usize, usize), Error> {
    let length = packet_len(packet)?;
    let mut sizes = [0i16; 48];
    // SAFETY: packet is nonempty and length-bounded. Sizes has all 48 entries
    // required by libopus; other outputs are documented optional/null pointers.
    let frames = unsafe {
        sys::opus_packet_parse(
            packet.as_ptr(),
            length,
            ptr::null_mut(),
            ptr::null_mut(),
            sizes.as_mut_ptr(),
            ptr::null_mut(),
        )
    };
    let frames = check(frames).map_err(|_| Error::Packet)?;
    // SAFETY: packet is nonempty and its complete framing was validated above.
    let samples = unsafe { sys::opus_packet_get_nb_samples(packet.as_ptr(), length, 48_000) };
    Ok((check(samples).map_err(|_| Error::Packet)?, frames))
}

pub(super) struct Decoder {
    state: NonNull<sys::OpusDecoder>,
    channels: usize,
}

// SAFETY: the state is exclusively owned; libopus permits moving decoder state
// between threads. All operations require &mut self. No pointers escape.
unsafe impl Send for Decoder {}

impl Decoder {
    pub(super) fn new(channels: u8) -> Result<Self, Error> {
        super::channels(channels)?;
        let mut error = 0;
        // SAFETY: valid rate/channel count and writable error pointer. The
        // returned state is owned and freed exactly once by Drop below.
        let state =
            unsafe { sys::opus_decoder_create(48_000, i32::from(channels), &raw mut error) };
        let mut decoder = Self {
            state: NonNull::new(state).ok_or_else(|| Error::Opus(error.into()))?,
            channels: usize::from(channels),
        };
        check(error)?;
        // SAFETY: live uniquely owned state. Both CTLs accept a promoted C int;
        // complexity=10 enables Deep PLC, and BWE=0 disables bandwidth extension.
        unsafe {
            check(sys::opus_decoder_ctl(
                decoder.state.as_mut(),
                sys::OPUS_SET_COMPLEXITY_REQUEST,
                10i32,
            ))?;
            check(sys::opus_decoder_ctl(
                decoder.state.as_mut(),
                sys::OPUS_SET_OSCE_BWE_REQUEST,
                0i32,
            ))?;
        }
        Ok(decoder)
    }

    pub(super) fn decode(
        &mut self,
        packet: Option<&[u8]>,
        fec: bool,
        pcm: &mut [f32],
    ) -> Result<usize, Error> {
        let (data, length) = match packet {
            Some(packet) => (packet.as_ptr(), packet_len(packet)?),
            None => (ptr::null(), 0),
        };
        let samples = int(pcm.len() / self.channels)?;
        // SAFETY: state is live and unique, input remains live through the call
        // (null/zero means PLC), output holds samples * channels writable floats.
        let count = unsafe {
            sys::opus_decode_float(
                self.state.as_mut(),
                data,
                length,
                pcm.as_mut_ptr(),
                samples,
                i32::from(fec),
            )
        };
        check(count)
    }

    #[cfg(feature = "dred")]
    pub(super) fn decode_dred(
        &mut self,
        dred: &Dred,
        offset: usize,
        pcm: &mut [f32],
    ) -> Result<(), Error> {
        let samples = int(pcm.len() / self.channels)?;
        let offset = int(offset)?;
        // SAFETY: both states are live, parsed DRED is immutable, decoder is
        // exclusive, and output holds samples * channels writable floats.
        let count = unsafe {
            sys::opus_decoder_dred_decode_float(
                self.state.as_mut(),
                dred.packet.as_ptr(),
                offset,
                pcm.as_mut_ptr(),
                samples,
            )
        };
        if check(count)? != usize::try_from(samples).map_err(|_| Error::Configuration)? {
            return Err(Error::Duration);
        }
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: this is the sole owner of a state returned by decoder_create.
        unsafe {
            sys::opus_decoder_destroy(self.state.as_ptr());
        }
    }
}

#[cfg(feature = "dred")]
pub(super) struct Dred {
    decoder: NonNull<sys::OpusDREDDecoder>,
    packet: NonNull<sys::OpusDRED>,
}

#[cfg(feature = "dred")]
// SAFETY: exclusively owned native states with no thread affinity or escaping
// pointers. Mutation requires &mut self; moving cannot introduce shared access.
unsafe impl Send for Dred {}

#[cfg(feature = "dred")]
impl Dred {
    pub(super) fn new() -> Result<Self, Error> {
        let mut error = 0;
        // SAFETY: writable error pointer; successful state becomes exclusively
        // owned below, with the matching destructor on every exit path.
        let decoder = unsafe { sys::opus_dred_decoder_create(&raw mut error) };
        let decoder = NonNull::new(decoder).ok_or_else(|| Error::Opus(error.into()))?;
        // SAFETY: writable error pointer. Use native allocation to respect the
        // actual library's DRED size/alignment instead of a hardcoded size.
        let packet = unsafe { sys::opus_dred_alloc(&raw mut error) };
        let Some(packet) = NonNull::new(packet) else {
            // SAFETY: allocation failed before ownership transfer; free decoder.
            unsafe {
                sys::opus_dred_decoder_destroy(decoder.as_ptr());
            }
            return Err(Error::Opus(error.into()));
        };
        Ok(Self { decoder, packet })
    }

    pub(super) fn parse(&mut self, packet: &[u8], samples: usize) -> Result<(), Error> {
        let length = packet_len(packet)?;
        if !(1..=48_000).contains(&samples) {
            return Err(Error::DredDuration);
        }
        let samples = int(samples)?;
        // libopus 1.6.1 rounds its parse budget to latent blocks without adding
        // the packet's positive timestamp offset (up to 40 ms). Request one
        // extra block, then check actual coverage against the requested loss.
        // Otherwise a 60 ms loss can report only 55 ms despite ample history.
        let parse_samples = samples + 1920;
        let mut end = 0;
        // SAFETY: states are live and exclusively borrowed; packet length is
        // checked and end is writable. defer_processing=0 fully decodes DRED.
        let available = unsafe {
            sys::opus_dred_parse(
                self.decoder.as_mut(),
                self.packet.as_mut(),
                packet.as_ptr(),
                length,
                parse_samples,
                48_000,
                &raw mut end,
                0,
            )
        };
        check(available)?;
        if available < samples || available == 0 {
            return Err(Error::DredUnavailable);
        }
        Ok(())
    }
}

#[cfg(feature = "dred")]
impl Drop for Dred {
    fn drop(&mut self) {
        // SAFETY: sole owner; each pointer came from the matching allocator.
        unsafe {
            sys::opus_dred_free(self.packet.as_ptr());
            sys::opus_dred_decoder_destroy(self.decoder.as_ptr());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_enables_deep_plc_and_disables_bwe() {
        let mut decoder = Decoder::new(1).unwrap();
        let mut complexity = 0i32;
        let mut bwe = -1i32;
        // SAFETY: live state and correct writable C-int outputs for these CTLs.
        unsafe {
            assert_eq!(
                sys::opus_decoder_ctl(
                    decoder.state.as_mut(),
                    sys::OPUS_GET_COMPLEXITY_REQUEST,
                    &raw mut complexity
                ),
                0
            );
            assert_eq!(
                sys::opus_decoder_ctl(
                    decoder.state.as_mut(),
                    sys::OPUS_GET_OSCE_BWE_REQUEST,
                    &raw mut bwe
                ),
                0
            );
        }
        assert_eq!(complexity, 10);
        assert_eq!(bwe, 0);
    }
}
