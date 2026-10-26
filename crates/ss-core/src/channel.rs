//! Channel kinds stored in `channels.kind`.

use crate::error::CoreError;

/// Kind of a channel row.
///
/// Discriminants are the `channels.kind` integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i64)]
pub enum ChannelKind {
    /// `0`. Groups channels. Not a place messages are sent.
    Category = 0,
    /// `1`. A text channel.
    Text = 1,
    /// `2`. A voice channel.
    Voice = 2,
    /// `3`. A 1:1 direct message.
    Dm = 3,
    /// `4`. A group direct message.
    GroupDm = 4,
}

impl ChannelKind {
    /// `channels.kind` as stored in SQLite.
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        self as i64
    }

    /// Direct and group DM channels authorize by membership, not by roles.
    #[must_use]
    pub const fn is_direct(self) -> bool {
        matches!(self, Self::Dm | Self::GroupDm)
    }
}

impl TryFrom<i64> for ChannelKind {
    type Error = CoreError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Category),
            1 => Ok(Self::Text),
            2 => Ok(Self::Voice),
            3 => Ok(Self::Dm),
            4 => Ok(Self::GroupDm),
            _ => Err(CoreError::UnknownChannelKind(value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{channel::ChannelKind, error::CoreError};

    #[test]
    fn discriminants_roundtrip_and_unknown_values_fail() {
        let kinds = [
            (ChannelKind::Category, 0, false),
            (ChannelKind::Text, 1, false),
            (ChannelKind::Voice, 2, false),
            (ChannelKind::Dm, 3, true),
            (ChannelKind::GroupDm, 4, true),
        ];
        for (kind, discriminant, direct) in kinds {
            assert_eq!(kind.as_i64(), discriminant);
            assert_eq!(ChannelKind::try_from(discriminant).unwrap(), kind);
            assert_eq!(kind.is_direct(), direct);
        }
        assert_eq!(
            ChannelKind::try_from(-1),
            Err(CoreError::UnknownChannelKind(-1))
        );
        assert_eq!(
            ChannelKind::try_from(5),
            Err(CoreError::UnknownChannelKind(5))
        );
    }
}
