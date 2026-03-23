use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Key, Nonce,
};
use rand_core::{OsRng, RngCore};

use crate::{CryptoError, Result};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedFrame {
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

impl EncryptedFrame {
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(NONCE_LEN + self.ciphertext.len());
        output.extend_from_slice(&self.nonce);
        output.extend_from_slice(&self.ciphertext);
        output
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < NONCE_LEN {
            return Err(CryptoError::InvalidEncryptedFrame {
                actual: bytes.len(),
            });
        }

        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&bytes[..NONCE_LEN]);

        Ok(Self {
            nonce,
            ciphertext: bytes[NONCE_LEN..].to_vec(),
        })
    }
}

#[must_use]
pub fn generate_nonce() -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

pub fn encrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    cipher(key)
        .encrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::EncryptionFailed)
}

pub fn decrypt(
    key: &[u8; KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    cipher(key)
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| CryptoError::DecryptFailed)
}

fn cipher(key: &[u8; KEY_LEN]) -> Aes256Gcm {
    let key = Key::<Aes256Gcm>::from_slice(key);
    Aes256Gcm::new(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_encrypt_decrypt_roundtrip_with_known_plaintext() {
        let key = decode_hex_array::<KEY_LEN>(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
        let nonce = decode_hex_array::<NONCE_LEN>("00112233445566778899aabb");
        let plaintext = b"jasmine encrypted payload";
        let aad = b"chat-message:v1";

        let ciphertext =
            encrypt(&key, &nonce, plaintext, aad).expect("roundtrip encryption should succeed");
        let decrypted =
            decrypt(&key, &nonce, &ciphertext, aad).expect("roundtrip decryption should succeed");

        assert_ne!(ciphertext, plaintext);
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn symmetric_tampered_ciphertext_returns_error() {
        let key = decode_hex_array::<KEY_LEN>(
            "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
        );
        let nonce = decode_hex_array::<NONCE_LEN>("c0c1c2c3c4c5c6c7c8c9cacb");
        let aad = b"transfer-offer";
        let mut ciphertext = encrypt(&key, &nonce, b"tamper me", aad)
            .expect("encryption should succeed before tamper");

        ciphertext[0] ^= 0x01;

        let error = decrypt(&key, &nonce, &ciphertext, aad)
            .expect_err("tampered ciphertext must fail authentication");

        assert_eq!(error, CryptoError::DecryptFailed);
    }

    #[test]
    fn symmetric_tampered_aad_returns_error() {
        let key = decode_hex_array::<KEY_LEN>(
            "404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f",
        );
        let nonce = decode_hex_array::<NONCE_LEN>("0f1e2d3c4b5a69788796a5b4");
        let plaintext = b"authenticated metadata";
        let aad = b"original-aad";
        let ciphertext =
            encrypt(&key, &nonce, plaintext, aad).expect("encryption should succeed with aad");

        let error = decrypt(&key, &nonce, &ciphertext, b"modified-aad")
            .expect_err("changed aad must fail authentication");

        assert_eq!(error, CryptoError::DecryptFailed);
    }

    #[test]
    fn symmetric_empty_plaintext_roundtrip_works() {
        let key = decode_hex_array::<KEY_LEN>(
            "606162636465666768696a6b6c6d6e6f707172737475767778797a7b7c7d7e7f",
        );
        let nonce = decode_hex_array::<NONCE_LEN>("a1a2a3a4a5a6a7a8a9aaabac");
        let aad = b"empty-body";

        let ciphertext =
            encrypt(&key, &nonce, b"", aad).expect("empty plaintext encryption should succeed");
        let decrypted = decrypt(&key, &nonce, &ciphertext, aad)
            .expect("empty plaintext decryption should succeed");

        assert_eq!(decrypted, Vec::<u8>::new());
        assert_eq!(ciphertext.len(), 16);
    }

    #[test]
    fn symmetric_large_plaintext_roundtrip_works() {
        let key = decode_hex_array::<KEY_LEN>(
            "808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f",
        );
        let nonce = decode_hex_array::<NONCE_LEN>("0102030405060708090a0b0c");
        let plaintext: Vec<u8> = (0u8..=255).cycle().take(64 * 1024).collect();
        let aad = b"64kb-payload";

        let ciphertext = encrypt(&key, &nonce, &plaintext, aad)
            .expect("large plaintext encryption should succeed");
        let decrypted = decrypt(&key, &nonce, &ciphertext, aad)
            .expect("large plaintext decryption should succeed");

        assert_eq!(decrypted, plaintext);
        assert_eq!(ciphertext.len(), 64 * 1024 + 16);
    }

    #[test]
    fn symmetric_nist_encrypt_vector_matches_expected_output() {
        let key = decode_hex_array::<KEY_LEN>(
            "31bdadd96698c204aa9ce1448ea94ae1fb4a9a0b3c9d773b51bb1822666b8f22",
        );
        let nonce = decode_hex_array::<NONCE_LEN>("0d18e06c7c725ac9e362e1ce");
        let plaintext = decode_hex_vec("2db5168e932556f8089a0622981d017d");
        let expected =
            decode_hex_vec("fa4362189661d163fcd6a56d8bf0405ad636ac1bbedd5cc3ee727dc2ab4a9489");

        let ciphertext =
            encrypt(&key, &nonce, &plaintext, b"").expect("nist encrypt vector should succeed");
        let decrypted =
            decrypt(&key, &nonce, &ciphertext, b"").expect("nist decrypt vector should succeed");

        assert_eq!(ciphertext, expected);
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn symmetric_nist_failure_vector_rejects_invalid_tag() {
        let key = decode_hex_array::<KEY_LEN>(
            "c997768e2d14e3d38259667a6649079de77beb4543589771e5068e6cd7cd0b14",
        );
        let nonce = decode_hex_array::<NONCE_LEN>("835090aed9552dbdd45277e2");
        let ciphertext =
            decode_hex_vec("9f6607d68e22ccf21928db0986be126ef32617f67c574fd9f44ef76ff880ab9f");

        let error = decrypt(&key, &nonce, &ciphertext, b"")
            .expect_err("nist failure vector must fail authentication");

        assert_eq!(error, CryptoError::DecryptFailed);
    }

    #[test]
    fn symmetric_nonce_generation_produces_12_bytes_and_different_outputs() {
        let first = generate_nonce();
        let second = generate_nonce();

        assert_eq!(first.len(), NONCE_LEN);
        assert_eq!(second.len(), NONCE_LEN);
        assert_ne!(first, second);
    }

    #[test]
    fn encrypted_frame_roundtrip_preserves_nonce_and_ciphertext() {
        let nonce = decode_hex_array::<NONCE_LEN>("112233445566778899aabbcc");
        let ciphertext = decode_hex_vec("deadbeef00112233445566778899");
        let frame = EncryptedFrame {
            nonce,
            ciphertext: ciphertext.clone(),
        };

        let bytes = frame.to_bytes();
        let decoded = EncryptedFrame::from_bytes(&bytes)
            .expect("serialized frame should decode successfully");

        assert_eq!(&bytes[..NONCE_LEN], nonce.as_slice());
        assert_eq!(&bytes[NONCE_LEN..], ciphertext.as_slice());
        assert_eq!(decoded, frame);
    }

    #[test]
    fn encrypted_frame_from_bytes_rejects_too_short_input() {
        let bytes = [0u8; NONCE_LEN - 1];

        let error =
            EncryptedFrame::from_bytes(&bytes).expect_err("short frame input must be rejected");

        assert_eq!(
            error,
            CryptoError::InvalidEncryptedFrame {
                actual: NONCE_LEN - 1,
            }
        );
    }

    fn decode_hex_array<const N: usize>(input: &str) -> [u8; N] {
        let bytes = decode_hex_vec(input);
        bytes
            .try_into()
            .unwrap_or_else(|value: Vec<u8>| panic!("expected {N} bytes, got {}", value.len()))
    }

    fn decode_hex_vec(input: &str) -> Vec<u8> {
        let cleaned: String = input.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert_eq!(cleaned.len() % 2, 0, "hex input must have even length");

        cleaned
            .as_bytes()
            .chunks_exact(2)
            .map(|chunk| {
                let hex = std::str::from_utf8(chunk).expect("hex data must be utf8");
                u8::from_str_radix(hex, 16).expect("hex input must be valid")
            })
            .collect()
    }
}
