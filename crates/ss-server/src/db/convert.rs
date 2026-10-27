//! Conversions between SQLite columns and domain values.

use std::str::FromStr;

use ss_core::{Permissions, Timestamp, VoiceProfile};

use super::Error;

pub(crate) fn stamp(millis: i64) -> Timestamp {
    Timestamp::from_millis(millis)
}

pub(crate) fn stamp_opt(millis: Option<i64>) -> Option<Timestamp> {
    millis.map(Timestamp::from_millis)
}

pub(crate) fn permissions_to_i64(permissions: Permissions) -> Result<i64, Error> {
    i64::try_from(permissions.bits()).map_err(|_error| Error::Corrupt("permission bits"))
}

pub(crate) fn permissions_from_i64(raw: i64) -> Result<Permissions, Error> {
    let bits = u64::try_from(raw).map_err(|_error| Error::Corrupt("permission bits"))?;
    Permissions::from_bits(bits).ok_or(Error::Corrupt("permission bits"))
}

pub(crate) fn profile_from_text(text: Option<String>) -> Result<Option<VoiceProfile>, Error> {
    text.map(|value| VoiceProfile::from_str(&value).map_err(Error::from))
        .transpose()
}

pub(crate) fn flag(value: i64) -> Result<bool, Error> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(Error::Corrupt("boolean flag")),
    }
}

pub(crate) fn hash32(bytes: Vec<u8>) -> Result<[u8; 32], Error> {
    bytes
        .try_into()
        .map_err(|_bytes: Vec<u8>| Error::Corrupt("sha256"))
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the query result is finished once its row count is read"
)]
pub(crate) fn changed(result: sqlx::sqlite::SqliteQueryResult) -> Result<(), Error> {
    if result.rows_affected() == 0 {
        Err(Error::NotFound)
    } else {
        Ok(())
    }
}
