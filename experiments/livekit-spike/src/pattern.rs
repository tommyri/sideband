//! Deterministic I420 test patterns. Frame `n` is always the same picture, so
//! both backends encode identical content.
//!
//! - `fill(.., hard = false)`: screen-like. A static gradient, a page of
//!   text-like glyphs where one new glyph is "typed" per frame and the page
//!   scrolls by one line every two seconds, and a 240 px box moving across it.
//! - `fill(.., hard = true)`: worst case. The gradient moves 4 px per frame and
//!   the text band scrolls 2 px per frame, so every macroblock changes.

/// Fill tightly packed I420 planes for frame `n`.
pub fn fill(data: &mut [u8], width: usize, height: usize, n: u64, hard: bool) {
    let (luma, chroma) = data.split_at_mut(width * height);
    let (u, v) = chroma.split_at_mut(width * height / 4);
    let shift = if hard { (n * 4) as usize } else { 0 };
    let scroll_x = if hard { (n * 2) as usize } else { 0 };
    // Soft: one line (14 px) of scroll every 60 frames; glyphs typed so far.
    let line_scroll = if hard { 0 } else { (n / 60) as usize };
    let typed = if hard {
        usize::MAX
    } else {
        (n % 60) as usize * 4 + 1200
    };
    let band = height / 3..height * 2 / 3;
    let bx = bounce(n * 9, width - 240);
    let by = bounce(n * 5, height - 240);
    for y in 0..height {
        let row = &mut luma[y * width..(y + 1) * width];
        if band.contains(&y) {
            let line = (y - band.start) / 14 + line_scroll;
            let cy = (y - band.start) % 14;
            for (x, px) in row.iter_mut().enumerate() {
                let sx = x + scroll_x;
                let col = sx / 8;
                let glyph = ((y - band.start) / 14) * (width / 8) + col;
                let cell = col ^ (line * 31);
                let cx = sx % 8;
                let on = glyph < typed
                    && cy < 11
                    && (cell.wrapping_mul(2_654_435_761) >> (cx + cy * 3) as u32) & 1 == 1;
                *px = if on { 30 } else { 225 };
            }
        } else {
            for (x, px) in row.iter_mut().enumerate() {
                *px = 16 + (((x + y + shift) % 440) / 2) as u8;
            }
        }
        if (by..by + 240).contains(&y) {
            row[bx..bx + 240].fill(((n * 3) % 200 + 20) as u8);
        }
    }
    let cw = width / 2;
    for y in 0..height / 2 {
        for x in 0..cw {
            u[y * cw + x] = (64 + (x + shift / 2) % 128) as u8;
            v[y * cw + x] = (64 + (y + shift / 3) % 128) as u8;
        }
    }
}

fn bounce(t: u64, range: usize) -> usize {
    let period = (range * 2) as u64;
    let p = (t % period) as usize;
    if p < range { p } else { 2 * range - p }
}
