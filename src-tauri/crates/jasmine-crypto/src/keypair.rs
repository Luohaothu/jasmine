use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{CryptoError, Result};

pub const KEY_MATERIAL_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PublicKeyBytes([u8; KEY_MATERIAL_LEN]);

impl PublicKeyBytes {
    #[must_use]
    pub fn from_public_key(public_key: &PublicKey) -> Self {
        Self(public_key.to_bytes())
    }

    #[must_use]
    pub fn to_public_key(&self) -> PublicKey {
        PublicKey::from(self.0)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_MATERIAL_LEN] {
        &self.0
    }
}

impl From<&PublicKey> for PublicKeyBytes {
    fn from(value: &PublicKey) -> Self {
        Self::from_public_key(value)
    }
}

impl From<PublicKeyBytes> for PublicKey {
    fn from(value: PublicKeyBytes) -> Self {
        value.to_public_key()
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretKeyBytes([u8; KEY_MATERIAL_LEN]);

impl SecretKeyBytes {
    #[must_use]
    pub fn from_secret(secret: &StaticSecret) -> Self {
        Self(secret.to_bytes())
    }

    #[must_use]
    pub fn to_static_secret(&self) -> StaticSecret {
        StaticSecret::from(self.0)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; KEY_MATERIAL_LEN] {
        &self.0
    }
}

impl From<&StaticSecret> for SecretKeyBytes {
    fn from(value: &StaticSecret) -> Self {
        Self::from_secret(value)
    }
}

#[must_use]
pub fn generate_identity_keypair() -> (StaticSecret, PublicKey) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    (secret, public)
}

#[must_use]
pub fn public_key_to_base64(public_key: &PublicKey) -> String {
    STANDARD.encode(public_key.to_bytes())
}

pub fn public_key_from_base64(encoded: &str) -> Result<PublicKey> {
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| CryptoError::InvalidBase64PublicKey(error.to_string()))?;

    let actual = bytes.len();
    if actual != KEY_MATERIAL_LEN {
        return Err(CryptoError::InvalidPublicKeyLength { actual });
    }

    let bytes: [u8; KEY_MATERIAL_LEN] =
        bytes
            .try_into()
            .map_err(|value: Vec<u8>| CryptoError::InvalidPublicKeyLength {
                actual: value.len(),
            })?;

    Ok(PublicKey::from(bytes))
}

#[must_use]
pub fn fingerprint(public_key: &PublicKey) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    let digest = Sha256::digest(public_key.to_bytes());
    let mut output = String::with_capacity(digest.len() * 2 + (digest.len() / 2) - 1);

    for (index, chunk) in digest.chunks_exact(2).enumerate() {
        if index > 0 {
            output.push(' ');
        }

        for &byte in chunk {
            output.push(HEX[(byte >> 4) as usize] as char);
            output.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use x25519_dalek::x25519;

    use super::*;

    #[test]
    fn generate_identity_keypair_produces_32_byte_material() {
        let (secret, public) = generate_identity_keypair();

        assert_eq!(secret.to_bytes().len(), KEY_MATERIAL_LEN);
        assert_eq!(public.to_bytes().len(), KEY_MATERIAL_LEN);
        assert_eq!(PublicKey::from(&secret), public);
    }

    #[test]
    fn public_key_base64_roundtrip_restores_original_key() {
        let (_, public) = generate_identity_keypair();

        let encoded = public_key_to_base64(&public);
        let decoded = public_key_from_base64(&encoded).expect("valid base64 key should decode");

        assert_eq!(decoded, public);
    }

    #[test]
    fn public_key_base64_rejects_wrong_length() {
        let error = public_key_from_base64("AQ==").expect_err("short key must be rejected");

        assert_eq!(error, CryptoError::InvalidPublicKeyLength { actual: 1 });
    }

    #[test]
    fn fingerprint_is_deterministic_for_same_public_key() {
        let public = PublicKey::from(decode_hex::<KEY_MATERIAL_LEN>(
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
        ));

        assert_eq!(
            fingerprint(&public),
            "300C 9C96 03B9 2A4B 39ED 3958 BF92 4011 4804 DB4F D373 012C 0CA4 7432 D634 25AE"
        );
        assert_eq!(fingerprint(&public), fingerprint(&public));
    }

    #[test]
    fn fingerprint_differs_for_different_public_keys() {
        let alice = PublicKey::from(decode_hex::<KEY_MATERIAL_LEN>(
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
        ));
        let bob = PublicKey::from(decode_hex::<KEY_MATERIAL_LEN>(
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f",
        ));

        assert_ne!(fingerprint(&alice), fingerprint(&bob));
    }

    #[test]
    fn secret_key_bytes_roundtrip_preserves_secret_material() {
        let (secret, _) = generate_identity_keypair();
        let wrapped = SecretKeyBytes::from_secret(&secret);

        assert_eq!(wrapped.as_bytes(), &secret.to_bytes());
        assert_eq!(wrapped.to_static_secret().to_bytes(), secret.to_bytes());
    }

    #[test]
    fn rfc7748_section_5_2_vector_one_matches() {
        let scalar = decode_hex::<KEY_MATERIAL_LEN>(
            "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
        );
        let input = decode_hex::<KEY_MATERIAL_LEN>(
            "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
        );
        let expected = decode_hex::<KEY_MATERIAL_LEN>(
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552",
        );

        assert_eq!(x25519(scalar, input), expected);
    }

    #[test]
    fn rfc7748_section_5_2_vector_two_matches() {
        let scalar = decode_hex::<KEY_MATERIAL_LEN>(
            "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
        );
        let input = decode_hex::<KEY_MATERIAL_LEN>(
            "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
        );
        let expected = decode_hex::<KEY_MATERIAL_LEN>(
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957",
        );

        assert_eq!(x25519(scalar, input), expected);
    }

    #[test]
    fn rfc7748_section_6_1_diffie_hellman_matches_for_both_sides() {
        let alice_secret = StaticSecret::from(decode_hex::<KEY_MATERIAL_LEN>(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        ));
        let bob_secret = StaticSecret::from(decode_hex::<KEY_MATERIAL_LEN>(
            "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
        ));
        let expected_alice_public = decode_hex::<KEY_MATERIAL_LEN>(
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a",
        );
        let expected_bob_public = decode_hex::<KEY_MATERIAL_LEN>(
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f",
        );
        let expected_shared = decode_hex::<KEY_MATERIAL_LEN>(
            "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742",
        );

        let alice_public = PublicKey::from(&alice_secret);
        let bob_public = PublicKey::from(&bob_secret);
        let alice_shared = alice_secret.diffie_hellman(&bob_public);
        let bob_shared = bob_secret.diffie_hellman(&alice_public);

        assert_eq!(alice_public.to_bytes(), expected_alice_public);
        assert_eq!(bob_public.to_bytes(), expected_bob_public);
        assert_eq!(alice_shared.to_bytes(), expected_shared);
        assert_eq!(bob_shared.to_bytes(), expected_shared);
    }

    fn decode_hex<const N: usize>(input: &str) -> [u8; N] {
        let cleaned: String = input.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert_eq!(cleaned.len(), N * 2, "hex input should be {N} bytes long");

        let mut output = [0u8; N];
        for (index, chunk) in cleaned.as_bytes().chunks_exact(2).enumerate() {
            let hex = std::str::from_utf8(chunk).expect("hex data must be utf8");
            output[index] = u8::from_str_radix(hex, 16).expect("hex input must be valid");
        }

        output
    }
}
