//! Channel argument parsing. A ULID is used as-is. A decimal integer becomes a
//! stable 16-byte id so two processes that pass `--channel 1` join the same
//! spike channel.

use ss_core::Id;
use std::str::FromStr;

/// The channel text was neither a ULID nor a decimal integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("channel must be a ULID or a decimal integer")]
pub struct ChannelError;

/// Resolve `--channel` to the id the spike server treats as a voice channel.
pub fn channel_from_arg(text: &str) -> Result<Id, ChannelError> {
    if let Ok(id) = Id::from_str(text) {
        return Ok(id);
    }
    let number: u64 = text.parse().map_err(|_| ChannelError)?;
    let mut bytes = [0_u8; 16];
    bytes[8..].copy_from_slice(&number.to_be_bytes());
    Ok(Id::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_are_stable_and_distinct() {
        let one = channel_from_arg("1").unwrap();
        assert_eq!(one, channel_from_arg("1").unwrap());
        assert_ne!(one, channel_from_arg("2").unwrap());
        assert_eq!(&one.as_bytes()[8..], &1_u64.to_be_bytes());
    }

    #[test]
    fn ulids_round_trip() {
        let id = Id::generate();
        assert_eq!(channel_from_arg(&id.to_string()).unwrap(), id);
    }

    #[test]
    fn rejects_blank_text() {
        assert_eq!(channel_from_arg(""), Err(ChannelError));
        assert_eq!(channel_from_arg("channel"), Err(ChannelError));
    }
}
