use aes_gcm::{aead::{Aead, KeyInit}, Aes256Gcm, Key, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hmac::Hmac;
use pbkdf2::pbkdf2;
use rand::RngCore;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

pub const SALT_LEN:    usize = 32;
pub const IV_LEN:      usize = 12;
pub const TAG_LEN:     usize = 16;
pub const PBKDF2_ITER: u32   = 310_000;

pub fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32], &'static str> {
    let mut key = [0u8; 32];
    pbkdf2::<Hmac<Sha256>>(password.as_bytes(), salt, PBKDF2_ITER, &mut key)
        .map_err(|_| "key derivation failed")?;
    Ok(key)
}

pub fn encrypt(plaintext: &str, password: &str) -> Result<String, &'static str> {
    let mut salt = [0u8; SALT_LEN];
    let mut iv   = [0u8; IV_LEN];
    rand::thread_rng().fill_bytes(&mut salt);
    rand::thread_rng().fill_bytes(&mut iv);

    let mut key_bytes = derive_key(password, &salt)?;
    let cipher        = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    key_bytes.zeroize();

    let ct_tag = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_bytes())
        .map_err(|_| "encryption failed")?;

    let ct_len    = ct_tag.len() - TAG_LEN;
    let (ct, tag) = ct_tag.split_at(ct_len);

    let mut out = Vec::with_capacity(SALT_LEN + IV_LEN + TAG_LEN + ct.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&iv);
    out.extend_from_slice(tag);
    out.extend_from_slice(ct);
    Ok(B64.encode(&out))
}

pub fn decrypt(b64: &str, password: &str) -> Result<String, &'static str> {
    let buf = B64.decode(b64.trim()).map_err(|_| "base64 error")?;
    if buf.len() < SALT_LEN + IV_LEN + TAG_LEN {
        return Err("ciphertext too short");
    }
    let salt       = &buf[..SALT_LEN];
    let iv         = &buf[SALT_LEN..SALT_LEN + IV_LEN];
    let tag        = &buf[SALT_LEN + IV_LEN..SALT_LEN + IV_LEN + TAG_LEN];
    let ciphertext = &buf[SALT_LEN + IV_LEN + TAG_LEN..];

    let mut key_bytes = derive_key(password, salt)?;
    let cipher        = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes));
    key_bytes.zeroize();

    let mut ct_tag = Vec::with_capacity(ciphertext.len() + TAG_LEN);
    ct_tag.extend_from_slice(ciphertext);
    ct_tag.extend_from_slice(tag);

    let plain = cipher
        .decrypt(Nonce::from_slice(iv), ct_tag.as_slice())
        .map_err(|_| "decryption failed — wrong password?")?;

    String::from_utf8(plain).map_err(|_| "utf-8 decode failed")
}

pub fn hash_password(pw: &str) -> Result<String, &'static str> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let salt_hex = hex::encode(salt);
    let mut hash = [0u8; 64];
    pbkdf2::<Hmac<Sha256>>(pw.as_bytes(), salt_hex.as_bytes(), PBKDF2_ITER, &mut hash)
        .map_err(|_| "key derivation failed")?;
    let encoded = hex::encode(hash);
    hash.zeroize();
    Ok(format!("{}:{}", salt_hex, encoded))
}

pub fn verify_password(pw: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.splitn(2, ':').collect();
    if parts.len() != 2 { return false; }
    let expected = match hex::decode(parts[1]) { Ok(v) => v, Err(_) => return false };
    // Always compare 64 bytes to prevent a tampered stored hash from reducing
    // the comparison to a trivially brute-forceable length.
    if expected.len() != 64 { return false; }
    let mut actual = [0u8; 64];
    if pbkdf2::<Hmac<Sha256>>(pw.as_bytes(), parts[0].as_bytes(), PBKDF2_ITER, &mut actual).is_err() {
        actual.zeroize();
        return false;
    }
    let result = actual.ct_eq(expected.as_slice()).into();
    actual.zeroize();
    result
}
