//! Profile authentication scoped to one server's canonical QUIC address.

use crate::Error;
use ring::{
    hmac,
    signature::{Ed25519KeyPair, KeyPair},
};
use ss_net::pins::ServerId;
use ss_proto::{
    profile::{device_authorization, login_proof},
    v1,
};

/// Only the derived credentials for the selected server survive construction.
/// The profile authority and device root stay in the host's secure storage.
pub struct ProfileCredentials {
    server: ServerId,
    membership: Ed25519KeyPair,
    device: Ed25519KeyPair,
    username: String,
    invite_code: String,
}

impl ProfileCredentials {
    /// Derive a membership from the recoverable profile root and a device
    /// signing key from this installation's independent device root.
    pub fn new(
        membership_root: &[u8; 32],
        device_root: &[u8; 32],
        server: &ServerId,
        username: String,
        invite_code: String,
    ) -> Result<Self, Error> {
        Ok(Self {
            server: server.clone(),
            membership: scoped_key(membership_root, b"Sideband/v1/membership\0", server)?,
            device: scoped_key(device_root, b"Sideband/v1/device\0", server)?,
            username,
            invite_code,
        })
    }

    pub(crate) fn sign(
        self,
        server: &ServerId,
        binding: &[u8; 32],
    ) -> Result<v1::ProfileAuth, Error> {
        if server != &self.server {
            return Err(Error::Protocol(
                "profile credentials belong to another server",
            ));
        }
        let membership = self.membership.public_key().as_ref();
        let device = self.device.public_key().as_ref();
        let signature = self.device.sign(&login_proof(binding, membership, device));
        let authorization = self.membership.sign(&device_authorization(device));
        Ok(v1::ProfileAuth {
            membership_key: membership.to_vec().into(),
            device_key: device.to_vec().into(),
            device_signature: signature.as_ref().to_vec().into(),
            register: Some(v1::ProfileRegister {
                username: self.username,
                invite_code: self.invite_code,
                membership_signature: authorization.as_ref().to_vec().into(),
            }),
        })
    }
}

pub(crate) fn scoped_key(
    root: &[u8; 32],
    purpose: &[u8],
    server: &ServerId,
) -> Result<Ed25519KeyPair, Error> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, root);
    let mut context = hmac::Context::with_key(&key);
    context.update(purpose);
    context.update(server.as_str().as_bytes());
    Ed25519KeyPair::from_seed_unchecked(context.sign().as_ref())
        .map_err(|_| Error::Protocol("invalid profile key"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ED25519, UnparsedPublicKey};

    fn proof(host: &str, port: u16, device_root: u8) -> v1::ProfileAuth {
        let server = ServerId::new(host, port).unwrap();
        ProfileCredentials::new(
            &[1; 32],
            &[device_root; 32],
            &server,
            "Sam".into(),
            String::new(),
        )
        .unwrap()
        .sign(&server, &[3; 32])
        .unwrap()
    }

    #[test]
    fn membership_is_stable_and_scoped_and_devices_are_independent() {
        let a = proof("room.example", 443, 2);
        assert_eq!(a, proof("ROOM.example.", 443, 2));
        for other in [
            proof("other.example", 443, 2),
            proof("room.example", 8443, 2),
        ] {
            assert_ne!(a.membership_key, other.membership_key);
            assert_ne!(a.device_key, other.device_key);
        }
        let other_device = proof("room.example", 443, 4);
        assert_eq!(a.membership_key, other_device.membership_key);
        assert_ne!(a.device_key, other_device.device_key);
        assert_ne!(a.membership_key, proof("room.example", 443, 1).device_key);
    }

    #[test]
    fn refuses_to_send_an_identity_to_a_different_community() {
        let a = ServerId::new("first.example", 443).unwrap();
        let b = ServerId::new("second.example", 443).unwrap();
        let keys =
            ProfileCredentials::new(&[1; 32], &[2; 32], &a, "Sam".into(), String::new()).unwrap();
        assert!(keys.sign(&b, &[3; 32]).is_err());
    }

    #[test]
    fn device_and_membership_sign_the_protocol_transcripts() {
        let auth = proof("room.example", 443, 2);
        let register = auth.register.unwrap();
        UnparsedPublicKey::new(&ED25519, &auth.membership_key)
            .verify(
                &device_authorization(&auth.device_key),
                &register.membership_signature,
            )
            .unwrap();
        let key = UnparsedPublicKey::new(&ED25519, &auth.device_key);
        key.verify(
            &login_proof(&[3; 32], &auth.membership_key, &auth.device_key),
            &auth.device_signature,
        )
        .unwrap();
        assert!(
            key.verify(
                &login_proof(&[4; 32], &auth.membership_key, &auth.device_key),
                &auth.device_signature
            )
            .is_err()
        );
    }
}
