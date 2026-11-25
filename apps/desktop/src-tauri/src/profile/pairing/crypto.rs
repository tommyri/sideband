//! Neither the invitation secret nor either X25519 private key goes to the relay.
use super::{ApiError, invalid};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use data_encoding::BASE32_NOPAD;
use ring::{aead, agreement, digest, hkdf, hmac, rand::SystemRandom};
use ss_core::pairing::{Claim, MAX_DELIVERY, Offer};
use zeroize::Zeroizing;

pub(super) struct Invitation {
    secret: Zeroizing<[u8; 16]>,
    pub(super) pinned: bool,
}
pub(super) fn random<const N: usize>() -> Result<[u8; N], ApiError> {
    let mut out = [0; N];
    getrandom::fill(&mut out).map_err(|_| invalid())?;
    Ok(out)
}
fn transcript(label: &[u8], fields: &[&[u8]]) -> Vec<u8> {
    let mut msg = b"Sideband/v1/device-pairing\0".to_vec();
    msg.extend_from_slice(label);
    msg.push(0);
    for field in fields {
        msg.extend_from_slice(&(field.len() as u64).to_be_bytes());
        msg.extend_from_slice(field);
    }
    msg
}
impl Invitation {
    pub(super) fn new(pinned: bool) -> Result<Self, ApiError> {
        Ok(Self {
            secret: Zeroizing::new(random()?),
            pinned,
        })
    }
    pub(super) fn code(&self) -> String {
        let mut bytes = Zeroizing::new([0u8; 17]);
        bytes[0] = u8::from(self.pinned);
        bytes[1..].copy_from_slice(self.secret.as_ref());
        let code = BASE32_NOPAD.encode(bytes.as_ref());
        code.as_bytes()
            .chunks(4)
            .map(|part| std::str::from_utf8(part).expect("base32"))
            .collect::<Vec<_>>()
            .join("-")
    }
    pub(super) fn parse(code: &str) -> Result<Self, ApiError> {
        let compact: String = code
            .chars()
            .filter(|c| !c.is_ascii_whitespace() && *c != '-')
            .map(|c| c.to_ascii_uppercase())
            .collect();
        let err = || {
            ApiError::new(
                "pair_code",
                "Check the one-time code on the trusted computer and enter it again.",
            )
        };
        let bytes = Zeroizing::new(BASE32_NOPAD.decode(compact.as_bytes()).map_err(|_| err())?);
        let (mode, secret) = bytes.split_first().ok_or_else(err)?;
        if *mode > 1 {
            return Err(err());
        }
        Ok(Self {
            secret: Zeroizing::new(secret.try_into().map_err(|_| err())?),
            pinned: *mode == 1,
        })
    }
    fn mac(&self, msg: &[u8]) -> [u8; 32] {
        hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, self.secret.as_ref()),
            msg,
        )
        .as_ref()
        .try_into()
        .expect("SHA-256")
    }
    fn check(&self, msg: &[u8], tag: &[u8]) -> Result<(), ApiError> {
        hmac::verify(
            &hmac::Key::new(hmac::HMAC_SHA256, self.secret.as_ref()),
            msg,
            tag,
        )
        .map_err(|_| invalid())
    }
    pub(super) fn slot(&self, origin: &str) -> [u8; 32] {
        self.mac(&transcript(b"slot", &[origin.as_bytes()]))
    }
    pub(super) fn offer(&self, slot: &[u8; 32], key: [u8; 32]) -> Offer {
        Offer {
            key,
            proof: self.mac(&transcript(b"offer", &[slot, &key])),
        }
    }
    pub(super) fn claim(
        &self,
        slot: &[u8; 32],
        key: [u8; 32],
        token: &[u8; 32],
        name: String,
    ) -> Claim {
        let reader = digest::digest(&digest::SHA256, token)
            .as_ref()
            .try_into()
            .expect("SHA-256");
        let proof = self.mac(&claim_message(slot, &key, &reader, &name));
        Claim {
            key,
            name,
            reader,
            proof,
        }
    }
    pub(super) fn verify(
        &self,
        slot: &[u8; 32],
        offer: &Offer,
        claim: &Claim,
    ) -> Result<(), ApiError> {
        if !ss_core::pairing::valid_device_name(&claim.name) {
            return Err(invalid());
        }
        self.check(&transcript(b"offer", &[slot, &offer.key]), &offer.proof)?;
        self.check(
            &claim_message(slot, &claim.key, &claim.reader, &claim.name),
            &claim.proof,
        )
    }
}
fn claim_message(slot: &[u8; 32], key: &[u8; 32], reader: &[u8; 32], name: &str) -> Vec<u8> {
    transcript(b"claim", &[slot, key, reader, name.as_bytes()])
}
pub(super) struct Exchange {
    private: agreement::EphemeralPrivateKey,
    pub(super) public: [u8; 32],
}
impl Exchange {
    pub(super) fn new() -> Result<Self, ApiError> {
        let private =
            agreement::EphemeralPrivateKey::generate(&agreement::X25519, &SystemRandom::new())
                .map_err(|_| invalid())?;
        let public = private
            .compute_public_key()
            .map_err(|_| invalid())?
            .as_ref()
            .try_into()
            .map_err(|_| invalid())?;
        Ok(Self { private, public })
    }
    pub(super) fn finish(
        self,
        invitation: &Invitation,
        slot: &[u8; 32],
        offer: &Offer,
        claim: &Claim,
        peer: [u8; 32],
    ) -> Result<Channel, ApiError> {
        invitation.verify(slot, offer, claim)?;
        let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, peer);
        let info = transcript(b"channel", &[slot, &offer.proof, &claim.proof]);
        agreement::agree_ephemeral(self.private, &peer, |shared| {
            let prk =
                hkdf::Salt::new(hkdf::HKDF_SHA256, invitation.secret.as_ref()).extract(shared);
            let mut key = Zeroizing::new([0; 32]);
            prk.expand(&[&info, b"encryption"], hkdf::HKDF_SHA256)
                .and_then(|v| v.fill(key.as_mut()))
                .map_err(|_| invalid())?;
            let mut sas = [0; 32];
            prk.expand(&[&info, b"phrase"], hkdf::HKDF_SHA256)
                .and_then(|v| v.fill(&mut sas))
                .map_err(|_| invalid())?;
            let bits = u64::from_be_bytes(sas[..8].try_into().expect("u64"));
            let words = bip39::Language::English.word_list();
            let phrase = [0, 11, 22]
                .map(|shift| words[((bits >> shift) & 2047) as usize])
                .join(" · ");
            let key = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key.as_ref())
                .map_err(|_| invalid())?;
            Ok(Channel {
                key: aead::LessSafeKey::new(key),
                info,
                phrase,
            })
        })
        .map_err(|_| invalid())?
    }
}
pub(super) struct Channel {
    key: aead::LessSafeKey,
    info: Vec<u8>,
    pub(super) phrase: String,
}
impl Channel {
    pub(super) fn seal(&self, mut bytes: Zeroizing<Vec<u8>>) -> Result<String, ApiError> {
        if base64::encoded_len(bytes.len() + 28, false).is_none_or(|len| len > MAX_DELIVERY) {
            return Err(ApiError::new(
                "pair_size",
                "This profile is too large to send through the pairing relay.",
            ));
        }
        let nonce = random()?;
        self.key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(&self.info),
                &mut *bytes,
            )
            .map_err(|_| invalid())?;
        Ok(URL_SAFE_NO_PAD.encode([nonce.as_slice(), &bytes].concat()))
    }
    pub(super) fn open(&self, encoded: &str) -> Result<Zeroizing<Vec<u8>>, ApiError> {
        if encoded.len() > MAX_DELIVERY {
            return Err(invalid());
        }
        let mut bytes = Zeroizing::new(URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?);
        let (nonce, body) = bytes.split_at_mut_checked(12).ok_or_else(invalid)?;
        let nonce = aead::Nonce::try_assume_unique_for_key(nonce).map_err(|_| invalid())?;
        let clear = self
            .key
            .open_in_place(nonce, aead::Aad::from(&self.info), body)
            .map_err(|_| invalid())?;
        Ok(Zeroizing::new(clear.to_vec()))
    }
}
