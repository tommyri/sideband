//! Authenticated, client-encrypted profile copies on a chosen community server.
use crate::{Client, Error, http::read_bounded, profile::scoped_key};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Method, StatusCode, header};
use ring::{aead, hmac, signature::KeyPair};
use ss_net::pins::ServerId;
use std::time::Duration;

const MAX_COPY: usize = 512 * 1024;
const FORMAT: u8 = 1;
const PURPOSE: &[u8] = b"Sideband/v1/encrypted-profile-copy\0";

/// Key material scoped to this community. The recoverable root is not retained.
pub struct RelayKey {
    server: ServerId,
    membership: String,
    key: aead::LessSafeKey,
}

impl RelayKey {
    /// Derive a separate encryption key and identify the expected local membership.
    pub fn new(root: &[u8; 32], server: ServerId) -> Result<Self, Error> {
        let membership = scoped_key(root, b"Sideband/v1/membership\0", &server)?;
        let mut mac = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, root));
        mac.update(PURPOSE);
        mac.update(server.as_str().as_bytes());
        let key = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, mac.sign().as_ref())
            .map_err(|_| Error::Protocol("could not derive the profile encryption key"))?;
        Ok(Self {
            server,
            membership: URL_SAFE_NO_PAD.encode(membership.public_key().as_ref()),
            key: aead::LessSafeKey::new(key),
        })
    }

    /// Encrypt one portable profile snapshot with a fresh random 96-bit nonce.
    pub fn seal(&self, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
        if plaintext.len() > MAX_COPY - 29 {
            return Err(Error::Protocol("profile copy is too large"));
        }
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce)
            .map_err(|_| Error::Protocol("could not generate encryption nonce"))?;
        let mut bytes = Vec::with_capacity(plaintext.len() + 29);
        bytes.push(FORMAT);
        bytes.extend_from_slice(&nonce);
        bytes.extend_from_slice(plaintext);
        let tag = self
            .key
            .seal_in_place_separate_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(PURPOSE),
                &mut bytes[13..],
            )
            .map_err(|_| Error::Protocol("could not encrypt profile"))?;
        bytes.extend_from_slice(tag.as_ref());
        Ok(bytes)
    }

    /// Authenticate and open a downloaded copy. Reject corrupt or foreign ciphertext.
    pub fn open<'a>(&self, bytes: &'a mut [u8]) -> Result<&'a [u8], Error> {
        let malformed = || Error::Protocol("the encrypted profile copy could not be verified");
        if !(29..=MAX_COPY).contains(&bytes.len()) || bytes[0] != FORMAT {
            return Err(malformed());
        }
        let nonce =
            aead::Nonce::try_assume_unique_for_key(&bytes[1..13]).map_err(|_| malformed())?;
        self.key
            .open_in_place(nonce, aead::Aad::from(PURPOSE), &mut bytes[13..])
            .map(|plain| &*plain)
            .map_err(|_| malformed())
    }
}

/// Exact version returned by the relay and its encrypted bytes.
pub struct RelayCopy {
    /// Opaque quoted HTTP ETag for conditional writes.
    pub etag: String,
    /// Includes format byte, nonce, ciphertext and authentication tag.
    pub ciphertext: Vec<u8>,
}

impl Client {
    fn relay_request(
        &self,
        key: &RelayKey,
        method: Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, Error> {
        if self.address().id()? != key.server {
            return Err(Error::Protocol("profile key belongs to a different relay"));
        }
        let session = self.http_session()?;
        Ok(session
            .http
            .request(method, session.base.join(path).expect("static relay URL"))
            .bearer_auth(&session.token)
            .header("x-sideband-membership", &key.membership)
            .timeout(Duration::from_secs(12)))
    }

    /// Whether this authenticated, linked device is currently eligible for the relay.
    pub async fn relay_eligible(&self, key: &RelayKey) -> Result<bool, Error> {
        let response = self
            .relay_request(key, Method::GET, "api/v1/profile-sync/status")?
            .send()
            .await?;
        match response.status() {
            StatusCode::NO_CONTENT => Ok(true),
            StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => Ok(false),
            _ => Err(Error::Protocol("could not check profile sync availability")),
        }
    }

    /// Retrieve the member's encrypted copy over the authenticated TLS session.
    pub async fn relay_copy(&self, key: &RelayKey) -> Result<Option<RelayCopy>, Error> {
        let response = self
            .relay_request(key, Method::GET, "api/v1/profile-sync/copy")?
            .send()
            .await?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if response.status() != StatusCode::OK {
            return Err(relay_error(response.status()));
        }
        let etag = response
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .filter(|v| v.len() == 45 && v.starts_with('"') && v.ends_with('"'))
            .ok_or(Error::Protocol("invalid relay copy revision"))?
            .to_owned();
        let ciphertext = read_bounded(response, MAX_COPY).await?;
        Ok(Some(RelayCopy { etag, ciphertext }))
    }

    /// Write only if the previous revision still matches. Replaying a completed write is safe.
    pub async fn write_relay_copy(
        &self,
        key: &RelayKey,
        previous: Option<&str>,
        ciphertext: Vec<u8>,
    ) -> Result<(), Error> {
        if !(29..=MAX_COPY).contains(&ciphertext.len()) {
            return Err(Error::Protocol("invalid encrypted profile size"));
        }
        let request = self.relay_request(key, Method::PUT, "api/v1/profile-sync/copy")?;
        let request = match previous {
            Some(tag) => request.header(header::IF_MATCH, tag),
            None => request.header(header::IF_NONE_MATCH, "*"),
        };
        let response = request
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(ciphertext)
            .send()
            .await?;
        if response.status() == StatusCode::NO_CONTENT {
            Ok(())
        } else {
            Err(relay_error(response.status()))
        }
    }
}

fn relay_error(status: StatusCode) -> Error {
    Error::Protocol(match status {
        StatusCode::NOT_FOUND => "This server is no longer offering profile sync.",
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
            "Sign in with this profile on the server, then try again."
        }
        StatusCode::PRECONDITION_FAILED => "The encrypted copy changed. Check the server again.",
        _ => "The sync server could not complete the transfer. Try again.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn copies_are_randomized_authenticated_and_specific_to_profile_and_server() {
        let server = ServerId::new("one.example", 443).unwrap();
        let key = RelayKey::new(&[1; 32], server.clone()).unwrap();
        let data = b"private memberships";
        let sealed = key.seal(data).unwrap();
        assert_ne!(sealed, key.seal(data).unwrap());
        assert!(!sealed.windows(data.len()).any(|w| w == data));
        assert_eq!(key.open(&mut sealed.clone()).unwrap(), data);
        for wrong in [
            RelayKey::new(&[2; 32], server).unwrap(),
            RelayKey::new(&[1; 32], ServerId::new("two.example", 443).unwrap()).unwrap(),
        ] {
            assert!(wrong.open(&mut sealed.clone()).is_err());
        }
        for i in [0, 1, 13, sealed.len() - 1] {
            let mut corrupt = sealed.clone();
            corrupt[i] ^= 1;
            assert!(key.open(&mut corrupt).is_err());
        }
        assert!(key.open(&mut sealed[..20].to_vec()).is_err());
    }
}
