use std::time::{SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore};
use uuid::Uuid;

use crate::{decrypt, encrypt, generate_nonce, CryptoError, EncryptedFrame, Result, MSG_KEY_LEN};

pub const SENDER_KEY_MATERIAL_LEN: usize = 32;
const DISTRIBUTION_AAD_PREFIX: &[u8] = b"jasmine/sender-key-distribution/v1";
const SENDER_KEY_PAYLOAD_LEN: usize = 8 + SENDER_KEY_MATERIAL_LEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderKey {
    pub key_id: Uuid,
    pub epoch: u32,
    pub key_material: [u8; SENDER_KEY_MATERIAL_LEN],
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderKeyDistribution {
    pub group_id: String,
    pub sender_id: String,
    pub key_id: Uuid,
    pub epoch: u32,
    pub encrypted_key_material: Vec<u8>,
}

#[must_use]
pub fn generate_sender_key() -> SenderKey {
    let mut key_material = [0u8; SENDER_KEY_MATERIAL_LEN];
    OsRng.fill_bytes(&mut key_material);

    SenderKey {
        key_id: Uuid::new_v4(),
        epoch: 0,
        key_material,
        created_at: now_ms(),
    }
}

pub fn create_distribution_message(
    group_id: impl Into<String>,
    sender_id: impl Into<String>,
    sender_key: &SenderKey,
    recipient_session_key: &[u8; MSG_KEY_LEN],
) -> Result<SenderKeyDistribution> {
    let group_id = group_id.into();
    let sender_id = sender_id.into();
    let aad = distribution_aad(&group_id, &sender_id, sender_key.key_id, sender_key.epoch);
    let nonce = generate_nonce();
    let payload = serialize_sender_key_payload(sender_key);
    let ciphertext = encrypt(recipient_session_key, &nonce, &payload, &aad)?;

    Ok(SenderKeyDistribution {
        group_id,
        sender_id,
        key_id: sender_key.key_id,
        epoch: sender_key.epoch,
        encrypted_key_material: EncryptedFrame { nonce, ciphertext }.to_bytes(),
    })
}

pub fn process_distribution_message(
    distribution: &SenderKeyDistribution,
    session_key: &[u8; MSG_KEY_LEN],
) -> Result<SenderKey> {
    let aad = distribution_aad(
        &distribution.group_id,
        &distribution.sender_id,
        distribution.key_id,
        distribution.epoch,
    );
    let frame = EncryptedFrame::from_bytes(&distribution.encrypted_key_material)?;
    let plaintext = decrypt(session_key, &frame.nonce, &frame.ciphertext, &aad)?;
    deserialize_sender_key_payload(distribution, &plaintext)
}

fn distribution_aad(group_id: &str, sender_id: &str, key_id: Uuid, epoch: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(
        DISTRIBUTION_AAD_PREFIX.len() + group_id.len() + sender_id.len() + 16 + 4 + 2,
    );
    aad.extend_from_slice(DISTRIBUTION_AAD_PREFIX);
    aad.push(0);
    aad.extend_from_slice(group_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(sender_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(key_id.as_bytes());
    aad.extend_from_slice(&epoch.to_be_bytes());
    aad
}

fn serialize_sender_key_payload(sender_key: &SenderKey) -> [u8; SENDER_KEY_PAYLOAD_LEN] {
    let mut payload = [0u8; SENDER_KEY_PAYLOAD_LEN];
    payload[..8].copy_from_slice(&sender_key.created_at.to_be_bytes());
    payload[8..].copy_from_slice(&sender_key.key_material);
    payload
}

fn deserialize_sender_key_payload(
    distribution: &SenderKeyDistribution,
    plaintext: &[u8],
) -> Result<SenderKey> {
    if plaintext.len() != SENDER_KEY_PAYLOAD_LEN {
        return Err(CryptoError::InvalidSenderKeyPayload {
            actual: plaintext.len(),
        });
    }

    let mut key_material = [0u8; SENDER_KEY_MATERIAL_LEN];
    key_material.copy_from_slice(&plaintext[8..]);

    Ok(SenderKey {
        key_id: distribution.key_id,
        epoch: distribution.epoch,
        key_material,
        created_at: i64::from_be_bytes(plaintext[..8].try_into().expect("sender key timestamp")),
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive_session_keys;

    fn derive_test_session_key(seed: &[u8], salt: &[u8]) -> [u8; MSG_KEY_LEN] {
        derive_session_keys(seed, salt)
            .expect("derive session keys")
            .msg_key
    }

    #[test]
    fn sender_key_generate_sender_key_produces_random_material() {
        let first = generate_sender_key();
        let second = generate_sender_key();

        assert_ne!(first.key_id, second.key_id);
        assert_eq!(first.epoch, 0);
        assert_eq!(second.epoch, 0);
        assert_eq!(first.key_material.len(), SENDER_KEY_MATERIAL_LEN);
        assert_eq!(second.key_material.len(), SENDER_KEY_MATERIAL_LEN);
        assert_ne!(first.key_material, second.key_material);
        assert!(first.created_at > 0);
    }

    #[test]
    fn sender_key_distribution_roundtrip_restores_original_sender_key() {
        let mut sender_key = generate_sender_key();
        sender_key.epoch = 3;
        let session_key = derive_test_session_key(b"sender-key-seed", b"sender-key-salt");

        let distribution =
            create_distribution_message("group-123", "device-abc", &sender_key, &session_key)
                .expect("create sender key distribution");
        let restored = process_distribution_message(&distribution, &session_key)
            .expect("process sender key distribution");

        assert_eq!(distribution.group_id, "group-123");
        assert_eq!(distribution.sender_id, "device-abc");
        assert_eq!(distribution.key_id, sender_key.key_id);
        assert_eq!(distribution.epoch, sender_key.epoch);
        assert_eq!(restored, sender_key);
    }

    #[test]
    fn sender_key_wrong_session_rejects_distribution_decrypt() {
        let sender_key = generate_sender_key();
        let distribution_session_key =
            derive_test_session_key(b"sender-key-seed", b"sender-key-salt");
        let wrong_session_key = derive_test_session_key(b"wrong-seed", b"wrong-salt");
        let distribution = create_distribution_message(
            "group-xyz",
            "device-def",
            &sender_key,
            &distribution_session_key,
        )
        .expect("create sender key distribution");

        let error = process_distribution_message(&distribution, &wrong_session_key)
            .expect_err("wrong session key must fail sender key decrypt");

        assert_eq!(error, CryptoError::DecryptFailed);
    }
}
