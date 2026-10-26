//! Time-ordered identifiers. ULIDs are stored as 16 bytes and shown as
//! Crockford base32.

use core::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// A SocialSpeak identifier.
///
/// Text form is 26-character Crockford base32. [`Id::as_bytes`] is the
/// 16-byte big-endian form stored in SQLite. Ordering is ULID byte order,
/// which is also creation order, so message ids can page history.
///
/// Serde uses the Crockford string. With the `sqlx` feature, `Id` implements
/// `sqlx::Type`, `sqlx::Encode`, and `sqlx::Decode` as a 16-byte `BLOB`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[must_use]
pub struct Id(ulid::Ulid);

impl Id {
    /// Generates an id from the current time and a random source.
    ///
    /// Successive calls are not guaranteed to be monotonic. Use a
    /// `ulid::Generator` at the call site when strict ordering matters.
    pub fn generate() -> Self {
        Self(ulid::Ulid::generate())
    }

    /// The all-zero id.
    pub const fn nil() -> Self {
        Self(ulid::Ulid::nil())
    }

    /// Wraps 16 bytes. Every 16-byte sequence is a valid id.
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(ulid::Ulid::from_bytes(bytes))
    }

    /// The 16-byte big-endian form.
    ///
    /// Returns a copy. The id does not store a byte slice that could be borrowed.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0.to_bytes()
    }

    /// Milliseconds since the Unix epoch stored in the high 48 bits.
    #[must_use]
    pub const fn timestamp_ms(self) -> u64 {
        self.0.timestamp_ms()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "Id({self})")
    }
}

impl FromStr for Id {
    type Err = CoreError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match ulid::Ulid::from_str(text) {
            Ok(id) => Ok(Self(id)),
            Err(ulid::DecodeError::InvalidLength) => Err(CoreError::InvalidIdLength),
            Err(ulid::DecodeError::InvalidChar) => Err(CoreError::InvalidIdCharacter),
        }
    }
}

impl From<[u8; 16]> for Id {
    fn from(bytes: [u8; 16]) -> Self {
        Self::from_bytes(bytes)
    }
}

impl From<Id> for [u8; 16] {
    fn from(id: Id) -> Self {
        id.as_bytes()
    }
}

impl From<ulid::Ulid> for Id {
    fn from(id: ulid::Ulid) -> Self {
        Self(id)
    }
}

impl From<Id> for ulid::Ulid {
    fn from(id: Id) -> Self {
        id.0
    }
}

impl TryFrom<&[u8]> for Id {
    type Error = CoreError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let length = bytes.len();
        let array: [u8; 16] = bytes
            .try_into()
            .map_err(|_| CoreError::InvalidIdByteLength { length })?;
        Ok(Self::from_bytes(array))
    }
}

impl TryFrom<Vec<u8>> for Id {
    type Error = CoreError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        let length = bytes.len();
        let array: [u8; 16] = bytes
            .try_into()
            .map_err(|_original: Vec<u8>| CoreError::InvalidIdByteLength { length })?;
        Ok(Self::from_bytes(array))
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Sqlite> for Id {
    fn type_info() -> sqlx::sqlite::SqliteTypeInfo {
        <Vec<u8> as sqlx::Type<sqlx::Sqlite>>::type_info()
    }

    fn compatible(ty: &sqlx::sqlite::SqliteTypeInfo) -> bool {
        <Vec<u8> as sqlx::Type<sqlx::Sqlite>>::compatible(ty)
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Encode<'_, sqlx::Sqlite> for Id {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::sqlite::SqliteArgumentsBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <Vec<u8> as sqlx::Encode<'_, sqlx::Sqlite>>::encode(self.as_bytes().to_vec(), buf)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::Decode<'r, sqlx::Sqlite> for Id {
    fn decode(value: sqlx::sqlite::SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let bytes = <Vec<u8> as sqlx::Decode<'r, sqlx::Sqlite>>::decode(value)?;
        let length = bytes.len();
        let array = <[u8; 16]>::try_from(bytes)
            .map_err(|_original: Vec<u8>| CoreError::InvalidIdByteLength { length })?;
        Ok(Self::from_bytes(array))
    }
}

#[cfg(test)]
mod tests {
    use core::str::FromStr;

    use crate::{Id, error::CoreError};

    const ALL_FF: [u8; 16] = [0xFF; 16];
    const ALL_FF_TEXT: &str = "7ZZZZZZZZZZZZZZZZZZZZZZZZZ";

    #[test]
    fn string_and_bytes_roundtrip() {
        let id = Id::from_bytes(ALL_FF);
        assert_eq!(id.as_bytes(), ALL_FF);
        assert_eq!(id.to_string(), ALL_FF_TEXT);
        assert_eq!(Id::from_str(ALL_FF_TEXT).unwrap(), id);
        assert_eq!(ALL_FF_TEXT.to_ascii_lowercase().parse::<Id>().unwrap(), id);
        assert_eq!(Id::try_from(id.as_bytes().as_slice()).unwrap(), id);
        assert_eq!(Id::try_from(id.as_bytes().to_vec()).unwrap(), id);
        assert_eq!(<[u8; 16]>::from(id), ALL_FF);

        let patterned =
            Id::from_bytes(0x4141_4141_4141_4141_4141_4141_4141_4141_u128.to_be_bytes());
        assert_eq!(patterned.to_string(), "21850M2GA1850M2GA1850M2GA1");
        assert_eq!(
            "21850M2GA1850M2GA1850M2GA1".parse::<Id>().unwrap(),
            patterned
        );
    }

    #[test]
    fn invalid_text_and_bytes_are_errors() {
        let samples = [
            "",
            "7ZZZZZZZZZZZZZZZZZZZZZZZZ",
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZZ",
        ];
        for sample in samples {
            assert_eq!(sample.parse::<Id>(), Err(CoreError::InvalidIdLength));
        }
        let invalid_char = "U0000000000000000000000000";
        assert_eq!(invalid_char.len(), 26);
        assert_eq!(
            invalid_char.parse::<Id>(),
            Err(CoreError::InvalidIdCharacter)
        );

        for bytes in [&[][..], &[0_u8; 15][..], &[0_u8; 17][..], &[0_u8; 32][..]] {
            let error = Id::try_from(bytes).unwrap_err();
            assert_eq!(
                error,
                CoreError::InvalidIdByteLength {
                    length: bytes.len(),
                }
            );
            assert_ne!(error.to_string(), "");
        }
    }

    #[test]
    fn serde_is_the_crockford_string() {
        let id = Id::from_bytes(ALL_FF);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{ALL_FF_TEXT}\""));
        assert_eq!(serde_json::from_str::<Id>(&json).unwrap(), id);

        assert!(serde_json::from_str::<Id>("\"\"").is_err());
        assert!(serde_json::from_str::<Id>("\"not-an-id\"").is_err());
        assert!(serde_json::from_str::<Id>("123").is_err());
        assert!(serde_json::from_str::<Id>("null").is_err());
    }

    #[test]
    fn nil_sorts_before_a_later_id() {
        let nil = Id::nil();
        assert_eq!(nil.as_bytes(), [0_u8; 16]);
        assert_eq!(nil.timestamp_ms(), 0);
        assert!(nil < Id::from_bytes(ALL_FF));
        let generated = Id::generate();
        assert_eq!(Id::from_str(&generated.to_string()).unwrap(), generated);
        assert_eq!(Id::from_bytes(generated.as_bytes()), generated);
    }
}

#[cfg(all(test, feature = "sqlx"))]
mod sqlx_tests {
    use sqlx::{Connection, TypeInfo};

    use crate::Id;

    #[test]
    fn sql_type_is_blob() {
        let info = <Id as sqlx::Type<sqlx::Sqlite>>::type_info();
        assert_eq!(info.name(), "BLOB");
    }

    #[test]
    fn blob_roundtrip_and_rejected_length() {
        sqlx::__rt::test_block_on(async {
            let mut conn = sqlx::SqliteConnection::connect("sqlite::memory:")
                .await
                .unwrap();
            sqlx::query("CREATE TABLE ids (id BLOB NOT NULL)")
                .execute(&mut conn)
                .await
                .unwrap();

            let id = Id::from_bytes([0xAB; 16]);
            sqlx::query("INSERT INTO ids (id) VALUES (?)")
                .bind(id)
                .execute(&mut conn)
                .await
                .unwrap();

            let raw: Vec<u8> = sqlx::query_scalar("SELECT id FROM ids")
                .fetch_one(&mut conn)
                .await
                .unwrap();
            assert_eq!(raw.as_slice(), id.as_bytes().as_slice());

            let loaded: Id = sqlx::query_scalar("SELECT id FROM ids")
                .fetch_one(&mut conn)
                .await
                .unwrap();
            assert_eq!(loaded, id);

            sqlx::query("DELETE FROM ids")
                .execute(&mut conn)
                .await
                .unwrap();
            let short = vec![1_u8, 2, 3];
            sqlx::query("INSERT INTO ids (id) VALUES (?)")
                .bind(short)
                .execute(&mut conn)
                .await
                .unwrap();
            let error = sqlx::query_scalar::<_, Id>("SELECT id FROM ids")
                .fetch_one(&mut conn)
                .await
                .unwrap_err();
            let message = error.to_string();
            assert!(
                message.contains("16"),
                "decode error should name the expected width: {message}"
            );
        });
    }
}
