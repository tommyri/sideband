//! 16-bit stereo WAV of the mixed playback quantum. The header is rewritten
//! when the writer is dropped.

use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};

use ss_audio::mixer::StereoFrame;

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

/// A growing PCM file. Drop finishes the header.
pub struct WavWriter {
    file: File,
    frames: u32,
}

impl WavWriter {
    /// Create or replace `path` and write a placeholder header.
    pub fn create(path: &Path) -> io::Result<Self> {
        let mut file = File::create(path)?;
        file.write_all(&header(0))?;
        Ok(Self { file, frames: 0 })
    }

    /// Append one 10 ms stereo quantum. Further frames are ignored past 2^31 bytes.
    pub fn write(&mut self, frame: &StereoFrame) -> io::Result<()> {
        let samples = u32::try_from(frame.len()).unwrap_or(u32::MAX);
        let Some(frames) = self.frames.checked_add(samples) else {
            return Ok(());
        };
        if frames > u32::MAX / 2 {
            return Ok(());
        }
        let mut bytes = [0_u8; 4];
        for sample in frame {
            bytes[..2].copy_from_slice(&pcm16(*sample).to_le_bytes());
            self.file.write_all(&bytes[..2])?;
        }
        self.frames = frames;
        Ok(())
    }
}

impl Drop for WavWriter {
    fn drop(&mut self) {
        let data_bytes = self.frames.saturating_mul(2);
        if self.file.seek(SeekFrom::Start(0)).is_ok() {
            let _ = self.file.write_all(&header(data_bytes));
        }
    }
}

fn header(data_bytes: u32) -> [u8; 44] {
    let mut bytes = [0_u8; 44];
    bytes[0..4].copy_from_slice(b"RIFF");
    bytes[4..8].copy_from_slice(&(36_u32.saturating_add(data_bytes)).to_le_bytes());
    bytes[8..12].copy_from_slice(b"WAVE");
    bytes[12..16].copy_from_slice(b"fmt ");
    bytes[16..20].copy_from_slice(&16_u32.to_le_bytes());
    bytes[20..22].copy_from_slice(&1_u16.to_le_bytes());
    bytes[22..24].copy_from_slice(&CHANNELS.to_le_bytes());
    bytes[24..28].copy_from_slice(&RATE.to_le_bytes());
    bytes[28..32].copy_from_slice(&(RATE * u32::from(CHANNELS) * 2).to_le_bytes());
    bytes[32..34].copy_from_slice(&(CHANNELS * 2).to_le_bytes());
    bytes[34..36].copy_from_slice(&16_u16.to_le_bytes());
    bytes[36..40].copy_from_slice(b"data");
    bytes[40..44].copy_from_slice(&data_bytes.to_le_bytes());
    bytes
}

fn pcm16(sample: f32) -> i16 {
    let scaled = sample.clamp(-1.0, 1.0) * 32_767.0;
    #[allow(clippy::cast_possible_truncation)]
    {
        scaled as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_records_the_pcm_size() {
        let bytes = header(8);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
    }
}
