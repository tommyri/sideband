//! Portable file format v1: header, random 96-bit nonce, AES-128-GCM
//! ciphertext + tag, and a SHA-256 checksum for diagnosing damaged copies.
//! The AEAD authenticates the header and contents. The checksum is only a
//! transport integrity check. The 12 English BIP39 words encode a fresh
//! uniform 128-bit AES key plus a transcription checksum.
use super::{ApiError, Body, damaged, wrong_key};
use bip39::{Language, Mnemonic};
use ring::{aead, digest};
use zeroize::Zeroizing;

const HEADER: &[u8] = b"Sideband backup\0\x01";
const TAG: usize = 16;
const CHECKSUM: usize = 32;
pub(super) const MAX_FILE: usize = 8 * 1024 * 1024;

pub(super) fn inspect(file: &[u8]) -> Result<(), ApiError> {
    if file.len() < HEADER.len() + aead::NONCE_LEN + TAG + 32 + CHECKSUM || file.len() > MAX_FILE {
        return Err(damaged());
    }
    if !file.starts_with(b"Sideband backup\0") {
        return Err(damaged());
    }
    if !file.starts_with(HEADER) {
        return Err(ApiError::new(
            "backup_version",
            "This file uses a different backup format. Open it in a compatible version of Sideband.",
        ));
    }
    let (data, checksum) = file.split_at(file.len() - CHECKSUM);
    if digest::digest(&digest::SHA256, data).as_ref() != checksum {
        return Err(damaged());
    }
    Ok(())
}

fn cipher(key: &[u8]) -> Result<aead::LessSafeKey, ApiError> {
    aead::UnboundKey::new(&aead::AES_128_GCM, key)
        .map(aead::LessSafeKey::new)
        .map_err(|_| wrong_key())
}

pub(super) fn seal(root: &[u8], body: &Body) -> Result<(Vec<u8>, String), ApiError> {
    let mut key = Zeroizing::new([0; 16]);
    getrandom::fill(key.as_mut()).map_err(super::storage_error)?;
    let mut nonce = [0; aead::NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(super::storage_error)?;
    seal_with(root, body, key.as_ref(), nonce)
}

pub(super) fn seal_with(
    root: &[u8],
    body: &Body,
    key: &[u8],
    nonce: [u8; aead::NONCE_LEN],
) -> Result<(Vec<u8>, String), ApiError> {
    let words = Mnemonic::from_entropy(key).map_err(super::storage_error)?;
    let mut payload = Zeroizing::new(root.to_vec());
    serde_json::to_writer(&mut *payload, body).map_err(super::storage_error)?;
    if payload.len() + HEADER.len() + aead::NONCE_LEN + TAG + CHECKSUM > MAX_FILE {
        return Err(ApiError::new(
            "backup_size",
            "This profile is too large for this backup format.",
        ));
    }
    cipher(key)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(HEADER),
            &mut *payload,
        )
        .map_err(super::storage_error)?;
    let mut file = [HEADER, &nonce, &payload[..]].concat();
    file.extend_from_slice(digest::digest(&digest::SHA256, &file).as_ref());
    Ok((file, words.to_string()))
}

pub(super) fn open(file: &[u8], words: &str) -> Result<(Zeroizing<[u8; 32]>, Body), ApiError> {
    inspect(file)?;
    if words.len() > 512 {
        return Err(wrong_key());
    }
    let normalized = Zeroizing::new(words.trim().to_ascii_lowercase());
    let phrase =
        Mnemonic::parse_in_normalized(Language::English, &normalized).map_err(|_| wrong_key())?;
    let (entropy, length) = phrase.to_entropy_array();
    let key = Zeroizing::new(entropy);
    if length != 16 {
        return Err(wrong_key());
    }
    let payload = &file[HEADER.len()..file.len() - CHECKSUM];
    let (nonce, encrypted) = payload.split_at(aead::NONCE_LEN);
    let nonce = aead::Nonce::try_assume_unique_for_key(nonce).map_err(|_| damaged())?;
    let mut bytes = Zeroizing::new(encrypted.to_vec());
    let clear = cipher(&key[..16])?
        .open_in_place(nonce, aead::Aad::from(HEADER), &mut bytes)
        .map_err(|_| wrong_key())?;
    let (root, json) = clear.split_first_chunk::<32>().ok_or_else(damaged)?;
    let body: Body = serde_json::from_slice(json).map_err(|_| damaged())?;
    body.validate()?;
    Ok((Zeroizing::new(*root), body))
}
