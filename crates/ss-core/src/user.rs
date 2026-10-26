//! User kinds stored in `users.kind`.

use crate::error::CoreError;

/// Kind of a user row.
///
/// Discriminants are the `users.kind` integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[repr(i64)]
pub enum UserKind {
    /// `0`. A human account.
    User = 0,
    /// `1`. A bot account.
    Bot = 1,
}

impl UserKind {
    /// `users.kind` as stored in SQLite.
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        self as i64
    }
}

impl TryFrom<i64> for UserKind {
    type Error = CoreError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::User),
            1 => Ok(Self::Bot),
            _ => Err(CoreError::UnknownUserKind(value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{error::CoreError, user::UserKind};

    #[test]
    fn discriminants_roundtrip_and_unknown_values_fail() {
        assert_eq!(UserKind::User.as_i64(), 0);
        assert_eq!(UserKind::Bot.as_i64(), 1);
        assert_eq!(UserKind::try_from(0).unwrap(), UserKind::User);
        assert_eq!(UserKind::try_from(1).unwrap(), UserKind::Bot);
        assert_eq!(UserKind::try_from(2), Err(CoreError::UnknownUserKind(2)));
        assert_eq!(UserKind::try_from(-1), Err(CoreError::UnknownUserKind(-1)));
    }
}
