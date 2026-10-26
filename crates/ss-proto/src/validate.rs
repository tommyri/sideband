//! Conversions and checks on wire ids and permission bitsets.
//!
//! `ss-core` owns the domain types and known permission bits. These helpers
//! convert their protobuf forms without changing the wire representation.

use bytes::Bytes;
use ss_core::{Id as CoreId, Permissions};

use crate::v1::{Id, Role, SetChannelOverride};

/// Length of a ULID on the wire, in bytes.
pub const ID_LEN: usize = 16;

/// `ADMINISTRATOR`. Bypasses channel overrides.
pub const PERMISSION_ADMINISTRATOR: u64 = Permissions::ADMINISTRATOR.bits();
/// `MANAGE_SERVER`. Server settings, invites, and webhooks.
pub const PERMISSION_MANAGE_SERVER: u64 = Permissions::MANAGE_SERVER.bits();
/// `MANAGE_CHANNELS`. Create, update, delete, and move channels.
pub const PERMISSION_MANAGE_CHANNELS: u64 = Permissions::MANAGE_CHANNELS.bits();
/// `MANAGE_ROLES`. Edit roles below the actor's highest role.
pub const PERMISSION_MANAGE_ROLES: u64 = Permissions::MANAGE_ROLES.bits();
/// `MANAGE_MESSAGES`. Delete others' messages.
pub const PERMISSION_MANAGE_MESSAGES: u64 = Permissions::MANAGE_MESSAGES.bits();
/// `KICK_MEMBERS`.
pub const PERMISSION_KICK_MEMBERS: u64 = Permissions::KICK_MEMBERS.bits();
/// `BAN_MEMBERS`.
pub const PERMISSION_BAN_MEMBERS: u64 = Permissions::BAN_MEMBERS.bits();
/// `MUTE_MEMBERS`. Server mute and unmute.
pub const PERMISSION_MUTE_MEMBERS: u64 = Permissions::MUTE_MEMBERS.bits();
/// `MOVE_MEMBERS`. Move users between voice channels.
pub const PERMISSION_MOVE_MEMBERS: u64 = Permissions::MOVE_MEMBERS.bits();
/// `VIEW_CHANNEL`. See a channel and its history.
pub const PERMISSION_VIEW_CHANNEL: u64 = Permissions::VIEW_CHANNEL.bits();
/// `SEND_MESSAGES`.
pub const PERMISSION_SEND_MESSAGES: u64 = Permissions::SEND_MESSAGES.bits();
/// `ATTACH_FILES`.
pub const PERMISSION_ATTACH_FILES: u64 = Permissions::ATTACH_FILES.bits();
/// `MENTION_EVERYONE`.
pub const PERMISSION_MENTION_EVERYONE: u64 = Permissions::MENTION_EVERYONE.bits();
/// `CONNECT`. Join a voice channel.
pub const PERMISSION_CONNECT: u64 = Permissions::CONNECT.bits();
/// `SPEAK`. Send audio.
pub const PERMISSION_SPEAK: u64 = Permissions::SPEAK.bits();
/// `STREAM`. Share a screen.
pub const PERMISSION_STREAM: u64 = Permissions::STREAM.bits();
/// `CREATE_INVITES`.
pub const PERMISSION_CREATE_INVITES: u64 = Permissions::CREATE_INVITES.bits();
/// `VIEW_AUDIT_LOG`.
pub const PERMISSION_VIEW_AUDIT_LOG: u64 = Permissions::VIEW_AUDIT_LOG.bits();
/// `CHANGE_NICKNAME`.
pub const PERMISSION_CHANGE_NICKNAME: u64 = Permissions::CHANGE_NICKNAME.bits();

/// Every permission bit defined in `docs/plan/03-data-model.md`.
pub const KNOWN_PERMISSION_BITS: u64 = Permissions::all().bits();

/// Default `@everyone` mask from `docs/plan/03-data-model.md`.
pub const EVERYONE_PERMISSIONS: u64 = Permissions::EVERYONE.bits();

/// Default `Admin` mask from `docs/plan/03-data-model.md`.
pub const ADMIN_PERMISSIONS: u64 = Permissions::ADMIN.bits();

/// An id byte string was not a 16-byte ULID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum IdError {
    /// The slice length was not [`ID_LEN`].
    #[error("id must be {ID_LEN} bytes, got {len}")]
    InvalidLength {
        /// Actual length in bytes.
        len: usize,
    },
}

/// A permission bitset used bits this schema does not define.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PermissionError {
    /// `bits` has flags outside [`KNOWN_PERMISSION_BITS`].
    #[error("permission bits {bits:#x} include unknown flags")]
    UnknownBits {
        /// The rejected bitset.
        bits: u64,
    },
}

/// Converts a required wire id to the domain type, rejecting invalid lengths.
pub fn id_from_bytes(id: &[u8]) -> Result<CoreId, IdError> {
    CoreId::try_from(id).map_err(|_| IdError::InvalidLength { len: id.len() })
}

/// Converts a domain id to the 16-byte protobuf representation.
pub fn id_to_bytes(id: CoreId) -> Bytes {
    Bytes::copy_from_slice(&id.as_bytes())
}

/// Converts wire permission bits without truncating unknown flags.
///
/// Use [`Permissions::bits`] for the reverse conversion. This checks the mask;
/// it does not resolve roles or expand `ADMINISTRATOR` to other permissions.
pub const fn permissions_from_bits(bits: u64) -> Result<Permissions, PermissionError> {
    match Permissions::from_bits(bits) {
        Some(permissions) => Ok(permissions),
        None => Err(PermissionError::UnknownBits { bits }),
    }
}

/// Returns `Ok` when `id` is exactly [`ID_LEN`] bytes.
///
/// # Errors
///
/// Returns [`IdError::InvalidLength`] for any other length, including empty.
pub fn validate_id(id: &[u8]) -> Result<(), IdError> {
    id_from_bytes(id).map(|_| ())
}

/// Returns `Ok` when `id` is empty or exactly [`ID_LEN`] bytes.
///
/// Use this for plain `bytes` fields where an empty value means "absent".
/// `optional bytes` fields should use [`validate_id`] on the `Some` side only.
///
/// # Errors
///
/// Returns [`IdError::InvalidLength`] when the length is neither 0 nor 16.
pub fn validate_optional_id(id: &[u8]) -> Result<(), IdError> {
    if id.is_empty() {
        Ok(())
    } else {
        validate_id(id)
    }
}

/// Validates every id in `ids`.
///
/// # Errors
///
/// Returns the first [`IdError`].
pub fn validate_ids<'a, I>(ids: I) -> Result<(), IdError>
where
    I: IntoIterator<Item = &'a [u8]>,
{
    ids.into_iter().try_for_each(validate_id)
}

/// Returns `Ok` when every set bit in `bits` is a known permission.
///
/// # Errors
///
/// Returns [`PermissionError::UnknownBits`] when any bit above bit 18 is set.
pub const fn validate_permissions(bits: u64) -> Result<(), PermissionError> {
    match permissions_from_bits(bits) {
        Ok(_) => Ok(()),
        Err(error) => Err(error),
    }
}

impl Id {
    /// Returns whether `value` is a 16-byte ULID.
    ///
    /// # Errors
    ///
    /// Returns [`IdError::InvalidLength`] when `value` is not 16 bytes.
    pub fn validate(&self) -> Result<(), IdError> {
        validate_id(&self.value)
    }
}

impl From<CoreId> for Id {
    fn from(id: CoreId) -> Self {
        Self {
            value: id_to_bytes(id),
        }
    }
}

impl TryFrom<&Id> for CoreId {
    type Error = IdError;

    fn try_from(id: &Id) -> Result<Self, Self::Error> {
        id_from_bytes(&id.value)
    }
}

impl TryFrom<Id> for CoreId {
    type Error = IdError;

    fn try_from(id: Id) -> Result<Self, Self::Error> {
        Self::try_from(&id)
    }
}

impl Role {
    /// Returns whether `permissions` uses only known bits.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionError::UnknownBits`] for bits this schema does not define.
    pub const fn validate_permission_bits(&self) -> Result<(), PermissionError> {
        validate_permissions(self.permissions)
    }
}

impl SetChannelOverride {
    /// Returns whether `allow` and `deny` use only known bits.
    ///
    /// # Errors
    ///
    /// Returns [`PermissionError::UnknownBits`] for the first mask that fails.
    pub const fn validate_permission_bits(&self) -> Result<(), PermissionError> {
        if let Err(err) = validate_permissions(self.allow) {
            return Err(err);
        }
        validate_permissions(self.deny)
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::{
        ADMIN_PERMISSIONS, EVERYONE_PERMISSIONS, ID_LEN, KNOWN_PERMISSION_BITS, PermissionError,
        validate_id, validate_ids, validate_optional_id, validate_permissions,
    };
    use crate::v1::{Id, Role, SetChannelOverride};

    #[test]
    fn ids_must_be_sixteen_bytes() {
        assert!(validate_id(&[0; ID_LEN]).is_ok());
        assert_eq!(
            validate_id(&[0; 15]),
            Err(super::IdError::InvalidLength { len: 15 })
        );
        assert_eq!(
            validate_id(&[0; 17]),
            Err(super::IdError::InvalidLength { len: 17 })
        );
        assert!(validate_optional_id(&[]).is_ok());
        assert!(validate_optional_id(&[0; ID_LEN]).is_ok());
        assert!(validate_optional_id(&[1, 2, 3]).is_err());
        assert!(validate_ids([&[0; ID_LEN][..], &[1; ID_LEN][..]]).is_ok());
        assert!(validate_ids([&[0; 4][..]]).is_err());
    }

    #[test]
    fn id_message_uses_the_same_check() {
        let ok = Id {
            value: Bytes::from(vec![0; ID_LEN]),
        };
        assert!(ok.validate().is_ok());
        let bad = Id {
            value: Bytes::new(),
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn permission_bits_match_the_data_model() {
        assert_eq!(ADMIN_PERMISSIONS, 1);
        assert_eq!(KNOWN_PERMISSION_BITS, (1_u64 << 19) - 1);
        assert!(validate_permissions(0).is_ok());
        assert!(validate_permissions(EVERYONE_PERMISSIONS).is_ok());
        assert!(validate_permissions(KNOWN_PERMISSION_BITS).is_ok());
        assert_eq!(
            validate_permissions(1 << 19),
            Err(PermissionError::UnknownBits { bits: 1 << 19 })
        );
        assert!(validate_permissions(EVERYONE_PERMISSIONS | (1 << 30)).is_err());
    }

    #[test]
    fn role_and_override_helpers_reject_unknown_bits() {
        let role = Role {
            id: Bytes::new(),
            name: String::new(),
            color: None,
            permissions: EVERYONE_PERMISSIONS,
            position: 0,
            is_default: true,
        };
        assert!(role.validate_permission_bits().is_ok());

        let override_msg = SetChannelOverride {
            channel_id: Bytes::new(),
            role_id: Bytes::new(),
            allow: ADMIN_PERMISSIONS,
            deny: 1 << 19,
        };
        assert!(override_msg.validate_permission_bits().is_err());
    }
}
