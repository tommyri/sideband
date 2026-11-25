//! Signed transcripts for server-scoped Ed25519 profile authentication.

/// TLS exporter label used with an empty context and a 32-byte output.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-Sideband-profile-v1";

/// A source device delegates recovery of this existing local member to its membership authority.
#[must_use]
pub fn recovery_authorization(membership: &[u8], user: &[u8]) -> Vec<u8> {
    [
        b"Sideband/v1/backup-recovery\0".as_slice(),
        membership,
        user,
    ]
    .concat()
}

/// Membership approval for one 32-byte, server-specific device key.
#[must_use]
pub fn device_authorization(device_key: &[u8]) -> Vec<u8> {
    [b"Sideband/v1/authorize-device\0".as_slice(), device_key].concat()
}

/// Proof of device possession bound to this TLS connection and membership.
/// Both public keys are exactly 32 bytes on the wire.
#[must_use]
pub fn login_proof(binding: &[u8; 32], membership_key: &[u8], device_key: &[u8]) -> Vec<u8> {
    [
        b"Sideband/v1/profile-login\0".as_slice(),
        binding,
        membership_key,
        device_key,
    ]
    .concat()
}
