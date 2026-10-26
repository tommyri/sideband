//! Unix timestamps stored as integer milliseconds.

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix time in milliseconds, matching the `INTEGER` timestamp columns.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[must_use]
pub struct Timestamp(i64);

impl Timestamp {
    /// A timestamp from a millisecond offset. Negative values are before the epoch.
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// Milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// The current clock, in Unix milliseconds.
    ///
    /// A clock set before the epoch yields `0`. A value that does not fit in
    /// `i64` yields [`i64::MAX`].
    pub fn now() -> Self {
        let millis = match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
            Err(_before_epoch) => 0,
        };
        Self(millis)
    }
}

impl core::fmt::Debug for Timestamp {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "Timestamp({} ms)", self.0)
    }
}

impl From<i64> for Timestamp {
    fn from(millis: i64) -> Self {
        Self::from_millis(millis)
    }
}

impl From<Timestamp> for i64 {
    fn from(timestamp: Timestamp) -> Self {
        timestamp.as_millis()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::Timestamp;

    #[test]
    fn millis_roundtrip() {
        for millis in [0_i64, 1, -1, i64::MAX, i64::MIN] {
            assert_eq!(Timestamp::from_millis(millis).as_millis(), millis);
            assert_eq!(i64::from(Timestamp::from(millis)), millis);
        }
    }

    #[test]
    fn now_matches_the_system_clock() {
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let stamp = Timestamp::now().as_millis();
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let stamp = u128::try_from(stamp).unwrap();
        assert!(stamp >= before);
        assert!(stamp <= after);
    }
}
