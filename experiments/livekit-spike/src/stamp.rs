//! A capture-time barcode written into the luma plane and read back after decode.
//!
//! 72 blocks in one row at the top-left corner: 48 bits of wall-clock
//! microseconds, a 16-bit frame number and an 8-bit checksum. Blocks are 24
//! luma pixels square at 1920 wide and scale with the frame, so a lower
//! simulcast layer (960 or 640 wide) still reads. Sender and viewers run on one
//! host, so they share `SystemTime`.

use std::time::{SystemTime, UNIX_EPOCH};

const BITS: usize = 72;
const BLOCK_AT_1920: f64 = 24.0;
const MASK48: u64 = (1 << 48) - 1;

/// Wall-clock microseconds since the Unix epoch.
pub fn now_micros() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_micros(),
    )
    .unwrap_or(u64::MAX)
}

fn checksum(micros: u64, seq: u16) -> u8 {
    let mut x = micros ^ (u64::from(seq) << 48) ^ 0xA5;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 29;
    (x & 0xff) as u8
}

fn bits(micros: u64, seq: u16) -> [bool; BITS] {
    let micros = micros & MASK48;
    let sum = checksum(micros, seq);
    let mut out = [false; BITS];
    for (i, bit) in out.iter_mut().enumerate() {
        *bit = match i {
            0..48 => (micros >> i) & 1 == 1,
            48..64 => (seq >> (i - 48)) & 1 == 1,
            _ => (sum >> (i - 64)) & 1 == 1,
        };
    }
    out
}

fn block(width: usize) -> f64 {
    BLOCK_AT_1920 * width as f64 / 1920.0
}

/// Write the barcode into a luma plane (`stride` bytes per row).
pub fn write(luma: &mut [u8], stride: usize, width: usize, micros: u64, seq: u16) {
    let size = block(width);
    let rows = size.ceil() as usize;
    for (i, bit) in bits(micros, seq).iter().enumerate() {
        let x0 = (i as f64 * size) as usize;
        let x1 = (((i + 1) as f64) * size) as usize;
        let value = if *bit { 235 } else { 16 };
        for row in 0..rows {
            let start = row * stride + x0;
            let end = row * stride + x1.min(width);
            if end <= luma.len() {
                luma[start..end].fill(value);
            }
        }
    }
}

/// Read the barcode back. `None` when the checksum fails (corrupt or absent).
pub fn read(luma: &[u8], stride: usize, width: usize, now: u64) -> Option<(u64, u16)> {
    let size = block(width);
    let cy = (size / 2.0) as usize;
    let mut micros = 0_u64;
    let mut seq = 0_u16;
    let mut sum = 0_u8;
    for i in 0..BITS {
        let cx = ((i as f64 + 0.5) * size) as usize;
        let mut total = 0_u32;
        let mut count = 0_u32;
        for dy in cy.saturating_sub(1)..=cy + 1 {
            for dx in cx.saturating_sub(1)..=cx + 1 {
                if let Some(value) = luma.get(dy * stride + dx) {
                    total += u32::from(*value);
                    count += 1;
                }
            }
        }
        if count == 0 {
            return None;
        }
        let bit = total / count > 125;
        if bit {
            match i {
                0..48 => micros |= 1 << i,
                48..64 => seq |= 1 << (i - 48),
                _ => sum |= 1 << (i - 64),
            }
        }
    }
    if checksum(micros, seq) != sum {
        return None;
    }
    // Restore the upper bits from the reader's clock (stamps are seconds old at most).
    let base = now & !MASK48;
    let mut full = base | micros;
    if full > now + (1 << 40) {
        full = full.wrapping_sub(1 << 48);
    }
    Some((full, seq))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_full_and_scaled() {
        for width in [1920_usize, 960, 640] {
            let height = width * 9 / 16;
            let mut luma = vec![128_u8; width * height];
            let now = now_micros();
            write(&mut luma, width, width, now, 4242);
            assert_eq!(read(&luma, width, width, now + 5000), Some((now, 4242)));
        }
    }
}
