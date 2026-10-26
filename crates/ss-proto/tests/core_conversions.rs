//! Domain values retain their bytes and bits across protobuf encoding.

use bytes::Bytes;
use proptest::prelude::*;
use prost::Message;
use ss_core::{Id, Permissions};
use ss_proto::{
    v1,
    validate::{
        IdError, PermissionError, id_from_bytes, id_to_bytes, permissions_from_bits, validate_id,
        validate_optional_id, validate_permissions,
    },
};

proptest! {
    #[test]
    fn ids_round_trip_through_raw_bytes_and_protobuf(bytes in any::<[u8; 16]>()) {
        let id = Id::from_bytes(bytes);
        let raw = id_to_bytes(id);
        prop_assert_eq!(raw.as_ref(), bytes.as_slice());
        prop_assert_eq!(id_from_bytes(&raw), Ok(id));
        prop_assert_eq!(validate_id(&raw), Ok(()));

        let wire = v1::Id::from(id);
        prop_assert_eq!(&wire.value, &raw);
        let decoded = v1::Id::decode(wire.encode_to_vec().as_slice()).unwrap();
        prop_assert_eq!(Id::try_from(&decoded), Ok(id));
        prop_assert_eq!(Id::try_from(decoded), Ok(id));
    }

    #[test]
    fn malformed_wire_ids_are_rejected(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        prop_assume!(bytes.len() != 16);
        let error = IdError::InvalidLength { len: bytes.len() };
        prop_assert_eq!(id_from_bytes(&bytes), Err(error));
        prop_assert_eq!(validate_id(&bytes), Err(error));
        let optional = if bytes.is_empty() { Ok(()) } else { Err(error) };
        prop_assert_eq!(validate_optional_id(&bytes), optional);
        let wire = v1::Id { value: Bytes::from(bytes) };
        prop_assert_eq!(Id::try_from(&wire), Err(error));
        prop_assert_eq!(Id::try_from(wire), Err(error));
    }

    #[test]
    fn permissions_round_trip_through_role_scalar(bits in 0_u64..(1 << 19)) {
        let permissions = permissions_from_bits(bits).unwrap();
        prop_assert_eq!(permissions.bits(), bits);
        let role = v1::Role { permissions: permissions.bits(), ..Default::default() };
        let decoded = v1::Role::decode(role.encode_to_vec().as_slice()).unwrap();
        prop_assert_eq!(decoded.permissions, bits);
        prop_assert_eq!(permissions_from_bits(decoded.permissions), Ok(permissions));
        prop_assert_eq!(decoded.validate_permission_bits(), Ok(()));
    }
}

#[test]
fn unknown_bits_are_rejected_even_with_administrator() {
    for bit in 19..64 {
        let bits = (1_u64 << bit) | Permissions::ADMINISTRATOR.bits();
        let error = PermissionError::UnknownBits { bits };
        assert_eq!(permissions_from_bits(bits), Err(error));
        assert_eq!(validate_permissions(bits), Err(error));
        let role = v1::Role {
            permissions: bits,
            ..Default::default()
        };
        assert_eq!(role.validate_permission_bits(), Err(error));
        for (allow, deny) in [(bits, 0), (0, bits)] {
            let overwrite = v1::SetChannelOverride {
                allow,
                deny,
                ..Default::default()
            };
            assert_eq!(overwrite.validate_permission_bits(), Err(error));
        }
    }
}

#[test]
fn permission_conversion_does_not_resolve_roles() {
    for permissions in [
        Permissions::empty(),
        Permissions::ADMIN,
        Permissions::EVERYONE,
        Permissions::all(),
    ] {
        assert_eq!(permissions_from_bits(permissions.bits()), Ok(permissions));
    }
    assert_eq!(permissions_from_bits(1).unwrap().bits(), 1);
}
