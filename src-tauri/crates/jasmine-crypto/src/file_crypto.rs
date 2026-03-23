use crate::{chunk_nonce, decrypt, encrypt, Result, FILE_KEY_LEN, NONCE_PREFIX_LEN};

const FILE_CHUNK_AAD_PREFIX: [u8; 21] = *b"jasmine/file-chunk/v1";

pub fn encrypt_chunk(
    file_key: &[u8; FILE_KEY_LEN],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
    chunk_index: u32,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let nonce = chunk_nonce(nonce_prefix, chunk_index);
    let aad = chunk_aad(chunk_index);
    encrypt(file_key, &nonce, plaintext, &aad)
}

pub fn decrypt_chunk(
    file_key: &[u8; FILE_KEY_LEN],
    nonce_prefix: &[u8; NONCE_PREFIX_LEN],
    chunk_index: u32,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let nonce = chunk_nonce(nonce_prefix, chunk_index);
    let aad = chunk_aad(chunk_index);
    decrypt(file_key, &nonce, ciphertext, &aad)
}

fn chunk_aad(chunk_index: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(FILE_CHUNK_AAD_PREFIX.len() + 4);
    aad.extend_from_slice(&FILE_CHUNK_AAD_PREFIX);
    aad.extend_from_slice(&chunk_index.to_be_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_crypto_encrypt_chunk_decrypt_chunk_roundtrip() {
        let file_key = decode_hex::<FILE_KEY_LEN>(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
        let nonce_prefix = decode_hex::<NONCE_PREFIX_LEN>("a1a2a3a4a5a6a7a8");
        let plaintext: &[u8] = &b"encrypted file chunk payload"[..];

        let ciphertext = encrypt_chunk(&file_key, &nonce_prefix, 7, plaintext)
            .expect("chunk encryption should succeed");
        let decrypted = decrypt_chunk(&file_key, &nonce_prefix, 7, &ciphertext)
            .expect("chunk decryption should succeed");

        assert_ne!(ciphertext, plaintext);
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn file_crypto_chunk_tamper_detected_during_decrypt() {
        let file_key = decode_hex::<FILE_KEY_LEN>(
            "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
        );
        let nonce_prefix = decode_hex::<NONCE_PREFIX_LEN>("b1b2b3b4b5b6b7b8");
        let mut ciphertext = encrypt_chunk(&file_key, &nonce_prefix, 3, &b"tamper me"[..])
            .expect("chunk encryption should succeed");

        ciphertext[0] ^= 0x01;

        let error = decrypt_chunk(&file_key, &nonce_prefix, 3, &ciphertext)
            .expect_err("tampered chunk must fail authentication");

        assert_eq!(error, crate::CryptoError::DecryptFailed);
    }

    #[test]
    fn file_crypto_sequential_chunks_use_unique_nonces_and_decrypt_correctly() {
        let file_key = decode_hex::<FILE_KEY_LEN>(
            "404142434445464748494a4b4c4d4e4f505152535455565758595a5b5c5d5e5f",
        );
        let nonce_prefix = decode_hex::<NONCE_PREFIX_LEN>("c1c2c3c4c5c6c7c8");
        let first_plaintext: &[u8] = &b"chunk zero"[..];
        let second_plaintext: &[u8] = &b"chunk one"[..];

        let first_ciphertext = encrypt_chunk(&file_key, &nonce_prefix, 0, first_plaintext)
            .expect("first chunk encryption should succeed");
        let second_ciphertext = encrypt_chunk(&file_key, &nonce_prefix, 1, second_plaintext)
            .expect("second chunk encryption should succeed");

        assert_ne!(chunk_nonce(&nonce_prefix, 0), chunk_nonce(&nonce_prefix, 1));
        assert_eq!(
            decrypt_chunk(&file_key, &nonce_prefix, 0, &first_ciphertext)
                .expect("first chunk decrypt should succeed"),
            first_plaintext,
        );
        assert_eq!(
            decrypt_chunk(&file_key, &nonce_prefix, 1, &second_ciphertext)
                .expect("second chunk decrypt should succeed"),
            second_plaintext,
        );
    }

    fn decode_hex<const N: usize>(input: &str) -> [u8; N] {
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
