//! Minimal Annex-B inspection used to place SocialSpeak group boundaries.

/// Whether an Annex-B access unit contains an H.264 IDR slice.
pub(crate) fn is_idr(bytes: &[u8]) -> bool {
    nal_units(bytes).any(|nal| nal.first().is_some_and(|header| header & 0x1f == 5))
}

fn nal_units(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    StartCodes { bytes, offset: 0 }.filter_map(|start| {
        let payload = start + start_code_len(&bytes[start..]);
        let end = find_start(bytes, payload).unwrap_or(bytes.len());
        (payload < end).then_some(&bytes[payload..end])
    })
}

struct StartCodes<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Iterator for StartCodes<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let start = find_start(self.bytes, self.offset)?;
        self.offset = start + start_code_len(&self.bytes[start..]);
        Some(start)
    }
}

fn find_start(bytes: &[u8], from: usize) -> Option<usize> {
    (from..bytes.len().saturating_sub(2)).find(|&index| {
        bytes[index] == 0
            && bytes[index + 1] == 0
            && (bytes[index + 2] == 1
                || (bytes.get(index + 2) == Some(&0) && bytes.get(index + 3) == Some(&1)))
    })
}

fn start_code_len(bytes: &[u8]) -> usize {
    if bytes.get(2) == Some(&1) { 3 } else { 4 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_idr_after_parameter_sets() {
        let access_unit = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5,
        ];
        assert!(is_idr(&access_unit));
        assert!(!is_idr(&[0, 0, 1, 0x41, 1, 2]));
        assert!(!is_idr(&[]));
    }
}
