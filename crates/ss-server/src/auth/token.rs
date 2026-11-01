//! Session tokens and the one-time owner invite code.
//!
//! The plaintext is base64url without padding. Only the SHA-256 hex digest is
//! stored. Thirty-two bytes is the session token; the owner code is shorter so
//! a person can type it.

use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use super::Error;

const SESSION_BYTES: usize = 32;
const OWNER_CODE_BYTES: usize = 16;

/// Plaintext bearer token and the digest stored in `sessions.token_hash`.
/// The plaintext is wiped when this is dropped.
pub(crate) struct Issued {
    pub token: Zeroizing<String>,
    pub hash: String,
}

/// A new session token.
pub(crate) fn issue() -> Result<Issued, Error> {
    let mut bytes = random_bytes::<SESSION_BYTES>()?;
    let token = Zeroizing::new(encode(&bytes));
    bytes.zeroize();
    let hash = hash(&token);
    Ok(Issued { token, hash })
}

/// A new owner invite code.
pub(crate) fn owner_code() -> Result<String, Error> {
    let mut bytes = random_bytes::<OWNER_CODE_BYTES>()?;
    let code = encode(&bytes);
    bytes.zeroize();
    Ok(code)
}

/// Hex SHA-256 of a bearer token.
pub(crate) fn hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex_encode(digest.as_ref())
}

fn encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn random_bytes<const N: usize>() -> Result<[u8; N], Error> {
    let mut bytes = [0_u8; N];
    getrandom::getrandom(&mut bytes).map_err(|_| Error::Random)?;
    Ok(bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{hash, issue, owner_code};

    #[test]
    fn session_tokens_are_32_bytes_of_base64url_stored_as_sha256() {
        let issued = issue().unwrap();
        assert_eq!(issued.token.len(), 43);
        assert_eq!(issued.hash.len(), 64);
        assert_eq!(issued.hash, hash(&issued.token));
        assert_ne!(issued.hash, *issued.token);
        assert!(!issued.token.contains('='));
    }

    #[test]
    fn owner_codes_are_shorter_and_unique() {
        let first = owner_code().unwrap();
        let second = owner_code().unwrap();
        assert_eq!(first.len(), 22);
        assert_ne!(first, second);
    }
}
