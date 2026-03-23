use x25519_dalek::{PublicKey, StaticSecret};

use crate::{
    decrypt, derive_session_keys, encrypt, CryptoError, EncryptedFrame, Result, SessionKeys,
    FILE_KEY_LEN, MSG_KEY_LEN, NONCE_PREFIX_LEN,
};

const SESSION_AAD: &[u8] = b"jasmine/session/v1";
const SESSION_HKDF_SALT: &[u8] = b"jasmine/session/v1/salt";
const FILE_DERIVATION_SALT_PREFIX: &[u8] = b"jasmine/file-transfer/v1";
const MESSAGE_NONCE_PREFIX_LEN: usize = 4;

#[derive(Debug)]
pub struct SessionKeyManager;

pub struct PendingSession {
    our_identity: [u8; 32],
    peer_identity: PublicKey,
    our_ephemeral: StaticSecret,
}

#[derive(Debug)]
pub struct Session {
    session_keys: SessionKeys,
    send_counter: u64,
    receive_counter: u64,
}

impl SessionKeyManager {
    #[must_use]
    pub fn initiate_key_exchange(
        our_identity: &StaticSecret,
        peer_public_key: &PublicKey,
    ) -> (PublicKey, PendingSession) {
        let our_ephemeral = StaticSecret::random_from_rng(rand_core::OsRng);
        let our_ephemeral_public = PublicKey::from(&our_ephemeral);

        let pending = PendingSession {
            our_identity: our_identity.to_bytes(),
            peer_identity: *peer_public_key,
            our_ephemeral,
        };

        (our_ephemeral_public, pending)
    }

    pub fn complete_key_exchange_initiator(
        pending: PendingSession,
        peer_ephemeral_pk: &PublicKey,
    ) -> Result<Session> {
        let our_identity = StaticSecret::from(pending.our_identity);
        let session_keys = derive_session(
            &our_identity,
            &pending.our_ephemeral,
            &pending.peer_identity,
            peer_ephemeral_pk,
        )?;

        Ok(Session::new(session_keys))
    }

    pub fn accept_key_exchange(
        our_identity: &StaticSecret,
        peer_identity_pk: &PublicKey,
        peer_ephemeral_pk: &PublicKey,
    ) -> Result<(PublicKey, Session)> {
        let our_ephemeral = StaticSecret::random_from_rng(rand_core::OsRng);
        let our_ephemeral_public = PublicKey::from(&our_ephemeral);
        let session_keys = derive_session(
            our_identity,
            &our_ephemeral,
            peer_identity_pk,
            peer_ephemeral_pk,
        )?;

        Ok((our_ephemeral_public, Session::new(session_keys)))
    }
}

impl Session {
    fn new(session_keys: SessionKeys) -> Self {
        Self {
            session_keys,
            send_counter: 0,
            receive_counter: 0,
        }
    }

    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<EncryptedFrame> {
        let nonce = self.message_nonce(self.send_counter);
        let ciphertext = encrypt(&self.session_keys.msg_key, &nonce, plaintext, SESSION_AAD)?;

        self.increment_send_counter()?;

        Ok(EncryptedFrame { nonce, ciphertext })
    }

    pub fn decrypt_message(&mut self, frame: &EncryptedFrame) -> Result<Vec<u8>> {
        if frame.nonce[..MESSAGE_NONCE_PREFIX_LEN]
            != self.session_keys.nonce_prefix[..MESSAGE_NONCE_PREFIX_LEN]
        {
            return Err(CryptoError::SessionNoncePrefixMismatch);
        }

        let frame_counter = u64::from_be_bytes(
            frame.nonce[MESSAGE_NONCE_PREFIX_LEN..]
                .try_into()
                .expect("session frame counter must be 8 bytes"),
        );

        if frame_counter != self.receive_counter {
            return Err(CryptoError::SessionReplayDetected {
                expected: self.receive_counter as u32,
                actual: frame_counter as u32,
            });
        }

        let plaintext = decrypt(
            &self.session_keys.msg_key,
            &frame.nonce,
            &frame.ciphertext,
            SESSION_AAD,
        )?;

        self.increment_receive_counter()?;
        Ok(plaintext)
    }

    #[must_use]
    pub fn file_crypto_material(&self) -> ([u8; FILE_KEY_LEN], [u8; NONCE_PREFIX_LEN]) {
        (self.session_keys.file_key, self.session_keys.nonce_prefix)
    }

    #[must_use]
    pub fn derive_file_crypto_material(
        &self,
        file_id: &str,
    ) -> ([u8; FILE_KEY_LEN], [u8; NONCE_PREFIX_LEN]) {
        let mut session_material =
            Vec::with_capacity(MSG_KEY_LEN + FILE_KEY_LEN + NONCE_PREFIX_LEN);
        session_material.extend_from_slice(&self.session_keys.msg_key);
        session_material.extend_from_slice(&self.session_keys.file_key);
        session_material.extend_from_slice(&self.session_keys.nonce_prefix);

        let derived = derive_session_keys(&session_material, &file_derivation_salt(file_id))
            .expect("fixed per-file derivation lengths must remain valid");

        (derived.file_key, derived.nonce_prefix)
    }

    #[must_use]
    pub fn sender_key_distribution_material(&self) -> [u8; MSG_KEY_LEN] {
        self.session_keys.msg_key
    }

    fn message_nonce(&self, counter: u64) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[..MESSAGE_NONCE_PREFIX_LEN]
            .copy_from_slice(&self.session_keys.nonce_prefix[..MESSAGE_NONCE_PREFIX_LEN]);
        nonce[MESSAGE_NONCE_PREFIX_LEN..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }

    fn increment_send_counter(&mut self) -> Result<()> {
        self.send_counter
            .checked_add(1)
            .map(|value| {
                self.send_counter = value;
            })
            .ok_or(CryptoError::SessionCounterOverflow)
    }

    fn increment_receive_counter(&mut self) -> Result<()> {
        self.receive_counter
            .checked_add(1)
            .map(|value| {
                self.receive_counter = value;
            })
            .ok_or(CryptoError::SessionCounterOverflow)
    }
}

fn derive_session(
    our_identity: &StaticSecret,
    our_ephemeral: &StaticSecret,
    peer_identity: &PublicKey,
    peer_ephemeral: &PublicKey,
) -> Result<SessionKeys> {
    let mut first = our_identity.diffie_hellman(peer_ephemeral).to_bytes();
    let mut second = our_ephemeral.diffie_hellman(peer_identity).to_bytes();
    let third = our_ephemeral.diffie_hellman(peer_ephemeral).to_bytes();

    if first > second {
        std::mem::swap(&mut first, &mut second);
    }

    let mut shared_secret = Vec::with_capacity(first.len() + second.len() + third.len());
    shared_secret.extend_from_slice(&first);
    shared_secret.extend_from_slice(&second);
    shared_secret.extend_from_slice(&third);

    derive_session_keys(&shared_secret, SESSION_HKDF_SALT)
}

fn file_derivation_salt(file_id: &str) -> Vec<u8> {
    let file_id_bytes = file_id.as_bytes();
    let mut salt = Vec::with_capacity(
        FILE_DERIVATION_SALT_PREFIX.len() + std::mem::size_of::<u64>() + file_id_bytes.len(),
    );
    salt.extend_from_slice(FILE_DERIVATION_SALT_PREFIX);
    salt.extend_from_slice(&(file_id_bytes.len() as u64).to_be_bytes());
    salt.extend_from_slice(file_id_bytes);
    salt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_identity_keypair;

    fn expected_message_nonce(nonce_prefix: &[u8; NONCE_PREFIX_LEN], counter: u64) -> [u8; 12] {
        let mut nonce = [0u8; 12];
        nonce[..MESSAGE_NONCE_PREFIX_LEN]
            .copy_from_slice(&nonce_prefix[..MESSAGE_NONCE_PREFIX_LEN]);
        nonce[MESSAGE_NONCE_PREFIX_LEN..].copy_from_slice(&counter.to_be_bytes());
        nonce
    }

    #[test]
    fn session_full_handshake_yields_matching_session_keys() {
        let (alice_identity, alice_identity_pk) = generate_identity_keypair();
        let (bob_identity, bob_identity_pk) = generate_identity_keypair();

        let (alice_ephemeral_pk, pending_alice) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk, bob_session) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk,
        )
        .expect("bob should build session");
        let alice_session =
            SessionKeyManager::complete_key_exchange_initiator(pending_alice, &bob_ephemeral_pk)
                .expect("alice should build session");

        assert_eq!(alice_session.session_keys, bob_session.session_keys);
        assert_eq!(alice_session.send_counter, 0);
        assert_eq!(alice_session.receive_counter, 0);
    }

    #[test]
    fn session_encrypt_message_interoperates_with_peer_decrypt() {
        let (alice_identity, alice_identity_pk) = generate_identity_keypair();
        let (bob_identity, bob_identity_pk) = generate_identity_keypair();

        let (alice_ephemeral_pk, pending_alice) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk, mut bob_session) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk,
        )
        .expect("bob should build session");
        let mut alice_session =
            SessionKeyManager::complete_key_exchange_initiator(pending_alice, &bob_ephemeral_pk)
                .expect("alice should build session");

        let payload = b"secret payload";
        let frame = alice_session
            .encrypt_message(payload)
            .expect("alice encrypt should succeed");
        let plaintext = bob_session
            .decrypt_message(&frame)
            .expect("bob decrypt should succeed");

        assert_eq!(plaintext, payload);
    }

    #[test]
    fn session_counter_increments_prevent_nonce_reuse() {
        let (alice_identity, alice_identity_pk) = generate_identity_keypair();
        let (bob_identity, bob_identity_pk) = generate_identity_keypair();

        let (alice_ephemeral_pk, pending_alice) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk, _) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk,
        )
        .expect("bob should build session");
        let mut alice_session =
            SessionKeyManager::complete_key_exchange_initiator(pending_alice, &bob_ephemeral_pk)
                .expect("alice should build session");

        let first = alice_session
            .encrypt_message(b"first")
            .expect("first encrypt should succeed");
        let second = alice_session
            .encrypt_message(b"second")
            .expect("second encrypt should succeed");

        assert_eq!(
            first.nonce,
            expected_message_nonce(&alice_session.session_keys.nonce_prefix, 0)
        );
        assert_eq!(
            second.nonce,
            expected_message_nonce(&alice_session.session_keys.nonce_prefix, 1)
        );
        assert_ne!(first.nonce, second.nonce);
        assert_eq!(alice_session.send_counter, 2);
    }

    #[test]
    fn session_message_nonce_uses_four_byte_prefix_and_u64_counter() {
        let (alice_identity, alice_identity_pk) = generate_identity_keypair();
        let (bob_identity, bob_identity_pk) = generate_identity_keypair();

        let (alice_ephemeral_pk, pending_alice) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk, mut bob_session) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk,
        )
        .expect("bob should build session");
        let mut alice_session =
            SessionKeyManager::complete_key_exchange_initiator(pending_alice, &bob_ephemeral_pk)
                .expect("alice should build session");

        let counter = 0x0102_0304_0506_0708_u64;
        alice_session.send_counter = counter;
        bob_session.receive_counter = counter;

        let frame = alice_session
            .encrypt_message(b"large counter payload")
            .expect("alice encrypt should succeed");
        let plaintext = bob_session
            .decrypt_message(&frame)
            .expect("bob decrypt should succeed");

        assert_eq!(plaintext, b"large counter payload");
        assert_eq!(
            frame.nonce,
            expected_message_nonce(&alice_session.session_keys.nonce_prefix, counter)
        );
        assert_eq!(alice_session.send_counter, counter + 1);
        assert_eq!(bob_session.receive_counter, counter + 1);
    }

    #[test]
    fn session_duplicate_frame_decrypt_rejected_as_replay() {
        let (alice_identity, alice_identity_pk) = generate_identity_keypair();
        let (bob_identity, bob_identity_pk) = generate_identity_keypair();

        let (alice_ephemeral_pk, pending_alice) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk, mut bob_session) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk,
        )
        .expect("bob should build session");
        let mut alice_session =
            SessionKeyManager::complete_key_exchange_initiator(pending_alice, &bob_ephemeral_pk)
                .expect("alice should build session");

        let frame = alice_session
            .encrypt_message(b"replay check")
            .expect("alice encrypt should succeed");

        let first = bob_session
            .decrypt_message(&frame)
            .expect("first decrypt should succeed");

        let replay = bob_session.decrypt_message(&frame);

        assert_eq!(first, b"replay check");
        assert!(matches!(
            replay,
            Err(CryptoError::SessionReplayDetected {
                expected: 1,
                actual: 0
            })
        ));
    }

    #[test]
    fn session_ephemeral_randomness_creates_distinct_session_keys() {
        let (alice_identity, alice_identity_pk) = generate_identity_keypair();
        let (bob_identity, bob_identity_pk) = generate_identity_keypair();

        let (alice_ephemeral_pk_one, pending_one) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk_one, bob_session_one) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk_one,
        )
        .expect("first bob session should build");
        let alice_session_one =
            SessionKeyManager::complete_key_exchange_initiator(pending_one, &bob_ephemeral_pk_one)
                .expect("first alice session should build");

        let (alice_ephemeral_pk_two, pending_two) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk_two, bob_session_two) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk_two,
        )
        .expect("second bob session should build");
        let alice_session_two =
            SessionKeyManager::complete_key_exchange_initiator(pending_two, &bob_ephemeral_pk_two)
                .expect("second alice session should build");

        assert_ne!(
            alice_session_one.session_keys,
            alice_session_two.session_keys
        );
        assert_ne!(bob_session_one.session_keys, bob_session_two.session_keys);
    }

    #[test]
    fn session_file_crypto_derivation_is_stable_per_file_and_shared_between_peers() {
        let (alice_identity, alice_identity_pk) = generate_identity_keypair();
        let (bob_identity, bob_identity_pk) = generate_identity_keypair();

        let (alice_ephemeral_pk, pending_alice) =
            SessionKeyManager::initiate_key_exchange(&alice_identity, &bob_identity_pk);
        let (bob_ephemeral_pk, bob_session) = SessionKeyManager::accept_key_exchange(
            &bob_identity,
            &alice_identity_pk,
            &alice_ephemeral_pk,
        )
        .expect("bob should build session");
        let alice_session =
            SessionKeyManager::complete_key_exchange_initiator(pending_alice, &bob_ephemeral_pk)
                .expect("alice should build session");

        let first_file = "offer-alpha";
        let second_file = "offer-beta";

        let alice_first = alice_session.derive_file_crypto_material(first_file);
        let alice_first_again = alice_session.derive_file_crypto_material(first_file);
        let bob_first = bob_session.derive_file_crypto_material(first_file);
        let alice_second = alice_session.derive_file_crypto_material(second_file);

        assert_eq!(alice_first, alice_first_again);
        assert_eq!(alice_first, bob_first);
        assert_ne!(alice_first, alice_second);
    }
}
