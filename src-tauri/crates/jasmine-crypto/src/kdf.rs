use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{CryptoError, Result};

pub const MSG_KEY_LEN: usize = 32;
pub const FILE_KEY_LEN: usize = 32;
pub const NONCE_PREFIX_LEN: usize = 8;
const MAX_OKM_LEN: usize = 255 * 32;

const MSG_INFO: &[u8] = b"jasmine/msg/v1";
const FILE_INFO: &[u8] = b"jasmine/file/v1";
const NONCE_INFO: &[u8] = b"jasmine/nonce/v1";

#[derive(Debug, Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct SessionKeys {
    pub msg_key: [u8; MSG_KEY_LEN],
    pub file_key: [u8; FILE_KEY_LEN],
    pub nonce_prefix: [u8; NONCE_PREFIX_LEN],
}

pub fn derive_session_keys(shared_secret: &[u8], salt: &[u8]) -> Result<SessionKeys> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), shared_secret);

    let msg_key = expand_key::<MSG_KEY_LEN>(&hkdf, MSG_INFO)?;
    let file_key = expand_key::<FILE_KEY_LEN>(&hkdf, FILE_INFO)?;
    let nonce_prefix = expand_key::<NONCE_PREFIX_LEN>(&hkdf, NONCE_INFO)?;

    Ok(SessionKeys {
        msg_key,
        file_key,
        nonce_prefix,
    })
}

#[must_use]
pub fn chunk_nonce(nonce_prefix: &[u8; NONCE_PREFIX_LEN], chunk_index: u32) -> [u8; 12] {
    let mut nonce = [0u8; 12];
    nonce[..NONCE_PREFIX_LEN].copy_from_slice(nonce_prefix);
    nonce[NONCE_PREFIX_LEN..].copy_from_slice(&chunk_index.to_be_bytes());
    nonce
}

fn expand_key<const N: usize>(hkdf: &Hkdf<Sha256>, info: &[u8]) -> Result<[u8; N]> {
    if N > MAX_OKM_LEN {
        return Err(CryptoError::InvalidDerivationLength {
            requested: N,
            max: MAX_OKM_LEN,
        });
    }

    let mut output = [0u8; N];
    hkdf.expand(info, &mut output)
        .map_err(|_| CryptoError::InvalidDerivationLength {
            requested: N,
            max: MAX_OKM_LEN,
        })?;

    Ok(output)
}

#[cfg(test)]
mod tests {
    use hkdf::Hkdf;

    use super::*;

    #[test]
    fn kdf_rfc5869_tc1_domain_separated_outputs_match_expected_values() {
        let shared_secret = decode_hex::<22>("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b");
        let salt = decode_hex::<13>("000102030405060708090a0b0c");

        let (prk, hkdf) = Hkdf::<Sha256>::extract(Some(&salt), &shared_secret);
        assert_eq!(
            prk.as_slice(),
            &decode_hex::<32>("077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"),
            "tc1 prk must match RFC 5869",
        );

        let keys = derive_session_keys(&shared_secret, &salt)
            .expect("tc1 domain-separated outputs must be derivable");

        assert_eq!(
            keys.msg_key,
            decode_hex::<MSG_KEY_LEN>(
                "ee10b9954615c43ec3f2e64b501654fbb68169161cbb65efae2851bfb851b48f",
            ),
        );
        assert_eq!(
            keys.file_key,
            decode_hex::<FILE_KEY_LEN>(
                "513c7400794749599653cfa5bb710c5aa9273ad37da03a41c6f74c5c844a50c2",
            ),
        );
        assert_eq!(
            keys.nonce_prefix,
            decode_hex::<NONCE_PREFIX_LEN>("68ff08548103f739")
        );

        assert_eq!(
            keys.msg_key.as_slice(),
            hkdf_expand(&hkdf, b"jasmine/msg/v1", 32).as_slice()
        );
        assert_eq!(
            keys.file_key.as_slice(),
            hkdf_expand(&hkdf, b"jasmine/file/v1", 32).as_slice(),
        );
        assert_eq!(
            keys.nonce_prefix.as_slice(),
            hkdf_expand(&hkdf, b"jasmine/nonce/v1", 8).as_slice(),
        );
    }

    #[test]
    fn kdf_rfc5869_tc2_precomputed_outputs_match_reference_prk() {
        let prk =
            decode_hex::<32>("06a6b88c5853361a06104c9ceb35b45cef760014904671014a193f40c15fc244");
        let hkdf = Hkdf::<Sha256>::from_prk(&prk).expect("RFC 5869 tc2 prk should be valid");

        assert_eq!(
            hkdf_expand(&hkdf, b"jasmine/msg/v1", 32),
            decode_hex::<MSG_KEY_LEN>(
                "3cf0605ed38bd7683269e5ac6f24dc3a057e917a9e719fc051e7fc37a408b52c",
            )
            .to_vec(),
        );
        assert_eq!(
            hkdf_expand(&hkdf, b"jasmine/file/v1", 32),
            decode_hex::<FILE_KEY_LEN>(
                "490a8618f7e3ea842b4108b5faa9e644bbe811889ec03b7ba79fb9b52b0ee855",
            )
            .to_vec(),
        );
        assert_eq!(
            hkdf_expand(&hkdf, b"jasmine/nonce/v1", 8),
            decode_hex::<NONCE_PREFIX_LEN>("22ff63dda8f6272d").to_vec(),
        );
    }

    #[test]
    fn kdf_derive_session_keys_is_deterministic_for_same_inputs() {
        let shared_secret =
            decode_hex::<32>("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let salt = decode_hex::<13>("000102030405060708090a0b0c");

        let first =
            derive_session_keys(&shared_secret, &salt).expect("first derivation should succeed");
        let second =
            derive_session_keys(&shared_secret, &salt).expect("second derivation should succeed");

        assert_eq!(first, second);
    }

    #[test]
    fn kdf_derive_session_keys_changes_with_salt_variation() {
        let shared_secret =
            decode_hex::<32>("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let first_salt = decode_hex::<13>("000102030405060708090a0b0c");
        let second_salt = decode_hex::<13>("0b0a090807060504030201000f");

        let first = derive_session_keys(&shared_secret, &first_salt)
            .expect("first derivation should succeed");
        let second = derive_session_keys(&shared_secret, &second_salt)
            .expect("second derivation should succeed");

        assert_ne!(first, second);
    }

    #[test]
    fn kdf_derive_session_keys_changes_with_shared_secret_variation() {
        let first_secret =
            decode_hex::<32>("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let second_secret =
            decode_hex::<32>("0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20");
        let salt = decode_hex::<13>("000102030405060708090a0b0c");

        let first =
            derive_session_keys(&first_secret, &salt).expect("first derivation should succeed");
        let second =
            derive_session_keys(&second_secret, &salt).expect("second derivation should succeed");

        assert_ne!(first, second);
    }

    #[test]
    fn kdf_derive_session_keys_generates_distinct_msg_and_file_keys() {
        let shared_secret =
            decode_hex::<32>("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let salt = decode_hex::<13>("0f0e0d0c0b0a09080706050403");

        let keys = derive_session_keys(&shared_secret, &salt).expect("keys should be derivable");

        assert_ne!(keys.msg_key, keys.file_key);
    }

    #[test]
    fn kdf_chunk_nonce_deterministic_for_same_prefix_and_index() {
        let nonce_prefix = decode_hex::<NONCE_PREFIX_LEN>("0011223344556677");

        let first = chunk_nonce(&nonce_prefix, 5);
        let second = chunk_nonce(&nonce_prefix, 5);

        assert_eq!(first, second);
    }

    #[test]
    fn kdf_chunk_nonce_sequential_values_are_unique() {
        let nonce_prefix = decode_hex::<NONCE_PREFIX_LEN>("aabbccddeeff0011");

        let first = chunk_nonce(&nonce_prefix, 1);
        let second = chunk_nonce(&nonce_prefix, 2);
        let third = chunk_nonce(&nonce_prefix, 3);

        assert_ne!(first, second);
        assert_ne!(second, third);
        assert_ne!(first, third);
        assert_eq!(&first[..NONCE_PREFIX_LEN], &nonce_prefix);
        assert_eq!(&second[..NONCE_PREFIX_LEN], &nonce_prefix);
        assert_eq!(&third[..NONCE_PREFIX_LEN], &nonce_prefix);
    }

    #[test]
    fn kdf_expand_key_rejects_overflow_requests() {
        let hkdf = Hkdf::<Sha256>::new(None, &[0u8; 32]);
        let mut overflow = vec![0u8; MAX_OKM_LEN + 1];

        assert!(hkdf.expand(b"jasmine/msg/v1", &mut overflow).is_err());
    }

    fn hkdf_expand(hkdf: &Hkdf<Sha256>, info: &[u8], len: usize) -> Vec<u8> {
        let mut output = vec![0u8; len];
        hkdf.expand(info, &mut output)
            .expect("hkdf expand should produce requested bytes for valid length");
        output
    }

    fn decode_hex<const N: usize>(input: &str) -> [u8; N] {
        let bytes = decode_hex_vec(input);
        bytes
            .try_into()
            .unwrap_or_else(|value: Vec<u8>| panic!("expected {N} bytes, got {}", value.len()))
    }

    fn decode_hex_vec(input: &str) -> Vec<u8> {
        let cleaned: String = input.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert_eq!(cleaned.len() % 2, 0, "hex input length must be even");

        cleaned
            .as_bytes()
            .chunks_exact(2)
            .map(|chunk| {
                let byte = std::str::from_utf8(chunk).expect("hex data must be utf-8");
                u8::from_str_radix(byte, 16).expect("hex input must be valid")
            })
            .collect()
    }
}
