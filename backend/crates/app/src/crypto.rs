use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// AES-256-GCM, output = nonce(12) || ciphertext.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("{e}"))?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|e| anyhow!("encrypt: {e}"))?;
    let mut out = nonce.to_vec();
    out.extend(ct);
    Ok(out)
}

pub fn decrypt(key: &[u8; 32], data: &[u8]) -> Result<Vec<u8>> {
    if data.len() < 13 {
        return Err(anyhow!("ciphertext too short"));
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("{e}"))?;
    cipher
        .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
        .map_err(|_| anyhow!("decrypt failed (wrong MASTER_KEY?)"))
}

pub fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

pub fn sha256_hex(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex_bytes(b: &[u8]) -> String {
    Sha256::digest(b)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub fn sha256_b64url(s: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_roundtrip_with_random_nonce() {
        let k = [1u8; 32];
        let a = encrypt(&k, b"secret").unwrap();
        let b = encrypt(&k, b"secret").unwrap();
        assert_ne!(a, b, "nonce must differ per encryption");
        assert_eq!(decrypt(&k, &a).unwrap(), b"secret");
    }

    #[test]
    fn wrong_key_or_tampering_fails() {
        let a = encrypt(&[1u8; 32], b"secret").unwrap();
        assert!(decrypt(&[2u8; 32], &a).is_err());
        let mut t = a.clone();
        *t.last_mut().unwrap() ^= 1;
        assert!(decrypt(&[1u8; 32], &t).is_err());
        assert!(decrypt(&[1u8; 32], &[0u8; 5]).is_err());
    }

    #[test]
    fn hashes_are_stable() {
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // RFC 7636 appendix B
        assert_eq!(
            sha256_b64url("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(random_token().len(), 43);
    }
}
