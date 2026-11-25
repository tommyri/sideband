//! Wire format for a short-lived, end-to-end encrypted desktop pairing.
use serde::{Deserialize, Serialize};

/// Maximum size of a base64-encoded delivery.
pub const MAX_DELIVERY: usize = 1024 * 1024;
/// Lifetime measured independently by the relay and both clients.
pub const TTL_SECONDS: u64 = 300;

/// Public half of an invitation, authenticated by the invitation secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    /// Trusted device's ephemeral X25519 public key.
    pub key: [u8; 32],
    /// MAC over the relay identity and public key.
    pub proof: [u8; 32],
}

/// A single requesting device. Immutable once the relay accepts it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    /// Requesting device's ephemeral X25519 public key.
    pub key: [u8; 32],
    /// Human-readable name included in the verified transcript.
    pub name: String,
    /// Hash of the random retrieval credential.
    pub reader: [u8; 32],
    /// Invitation-secret authentication of the entire request.
    pub proof: [u8; 32],
}

/// A device label suitable for displaying in the approval question.
pub fn valid_device_name(name: &str) -> bool {
    !name.is_empty()
        && name == name.trim()
        && name.chars().count() <= 64
        && !name.chars().any(char::is_control)
}

/// All requests identify the invitation by an opaque derived locator.
#[derive(Serialize, Deserialize)]
pub struct Request<T> {
    /// Derived from the secret and relay identity.
    pub slot: [u8; 32],
    /// Role-specific transition.
    pub action: T,
}

/// Requests requiring the originating enrolled device session.
#[derive(Serialize, Deserialize)]
pub enum Owner {
    /// Create a slot, replacing any earlier invitation from the same member.
    Start(Offer),
    /// Look for a requesting device.
    Inspect,
    /// Release ciphertext only to the reviewed claim.
    Approve {
        /// Reviewed claim authenticator.
        proof: [u8; 32],
        /// Encrypted transfer.
        delivery: String,
    },
    /// Invalidate the code and remove pending ciphertext.
    Cancel,
}

/// Requests from a computer that does not yet have a server account.
#[derive(Serialize, Deserialize)]
pub enum Guest {
    /// Reserve this invitation for a specific public key.
    Join(Claim),
    /// Retrieve an approved transfer.
    Poll([u8; 32]),
    /// Abandon the request or acknowledge a successful installation.
    Finish([u8; 32]),
}

/// Non-cacheable response to a state transition.
#[derive(Serialize, Deserialize)]
pub enum Reply {
    /// Waiting for a claim or explicit approval.
    Waiting,
    /// Authenticated public offer for the joining device.
    Offered(Offer),
    /// Immutable claim for the owner's review.
    Claimed(Claim),
    /// Ciphertext for the approved device.
    Delivery(String),
    /// Transition completed.
    Done,
}
