//! HTTPS account linking messages exchanged with an authenticated human member.

use crate::Id;
use serde::{Deserialize, Serialize};

/// Server-confirmed membership and current session for the confirmation screen.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReview {
    /// Existing server member being linked.
    pub member_id: Id,
    /// Non-secret session identifier, used to bind both proofs to this login.
    pub session_id: Id,
    /// Server login name.
    pub username: String,
    /// Highest role, including the default member role.
    pub role: String,
    /// Currently linked membership public key, when the member has one.
    pub membership_key: Option<Vec<u8>>,
    /// Whether this account has enrolled the requesting device.
    pub device_active: bool,
    /// Whether the current bearer session belongs to that active device.
    pub device_session: bool,
}

/// Account-link proof from the two keys derived for this server.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkAccount {
    /// Membership authority public key.
    pub membership_key: [u8; 32],
    /// Installation-specific signing public key.
    pub device_key: [u8; 32],
    /// Authorization from the membership key.
    pub membership_signature: Vec<u8>,
    /// Proof of possession of the device key.
    pub device_signature: Vec<u8>,
}

/// Domain-separated approval of one device for one existing user and session.
#[must_use]
pub fn transcript(member: Id, session: Id, membership: &[u8; 32], device: &[u8; 32]) -> Vec<u8> {
    [
        b"Sideband/v1/link-existing-account\0".as_slice(),
        &member.as_bytes(),
        &session.as_bytes(),
        membership,
        device,
    ]
    .concat()
}
