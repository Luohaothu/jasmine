pub mod file_crypto;
pub mod kdf;
pub mod keypair;
pub mod keystore;
pub mod sender_keys;
pub mod session;
pub mod symmetric;

use thiserror::Error;

pub use file_crypto::{decrypt_chunk, encrypt_chunk};
pub use kdf::{
    chunk_nonce, derive_session_keys, SessionKeys, FILE_KEY_LEN, MSG_KEY_LEN, NONCE_PREFIX_LEN,
};
pub use keypair::{
    fingerprint, generate_identity_keypair, public_key_from_base64, public_key_to_base64,
    PublicKeyBytes, SecretKeyBytes,
};
pub use keystore::{delete_private_key, load_private_key, store_private_key, Keystore};
pub use sender_keys::{
    create_distribution_message, generate_sender_key, process_distribution_message, SenderKey,
    SenderKeyDistribution, SENDER_KEY_MATERIAL_LEN,
};
pub use session::{PendingSession, Session, SessionKeyManager};
pub use symmetric::{decrypt, encrypt, generate_nonce, EncryptedFrame};
pub use x25519_dalek::{PublicKey, StaticSecret};

pub type Result<T> = std::result::Result<T, CryptoError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CryptoError {
    #[error("invalid base64 public key: {0}")]
    InvalidBase64PublicKey(String),
    #[error("invalid public key length: expected 32 bytes, got {actual}")]
    InvalidPublicKeyLength { actual: usize },
    #[error("encrypted frame must be at least 12 bytes, got {actual}")]
    InvalidEncryptedFrame { actual: usize },
    #[error("invalid private key length: expected 32 bytes, got {actual}")]
    InvalidPrivateKeyLength { actual: usize },
    #[error("invalid keystore file: expected at least 44 bytes, got {actual}")]
    InvalidKeystoreFile { actual: usize },
    #[error("application data directory unavailable")]
    AppDataDirUnavailable,
    #[error("machine-specific key material unavailable")]
    MachineKeyMaterialUnavailable,
    #[error("key derivation failed")]
    KeyDerivationFailed,
    #[error("secure storage returned corrupt data")]
    SecureStorageCorrupted,
    #[error("secure storage error: {0}")]
    SecureStorage(String),
    #[error("persistence error: {0}")]
    Persistence(String),
    #[error("invalid keystore identifier: {0}")]
    InvalidKeystoreIdentifier(String),
    #[error("encryption failed")]
    EncryptionFailed,
    #[error("decryption failed")]
    DecryptFailed,
    #[error("session message nonce prefix does not match session state")]
    SessionNoncePrefixMismatch,
    #[error("session replay detected: expected frame {expected}, got {actual}")]
    SessionReplayDetected { expected: u32, actual: u32 },
    #[error("session counter overflow while advancing message index")]
    SessionCounterOverflow,
    #[error("hkdf expand requested {requested} bytes, max allowed is {max}")]
    InvalidDerivationLength { requested: usize, max: usize },
    #[error("invalid sender key payload: expected 40 bytes, got {actual}")]
    InvalidSenderKeyPayload { actual: usize },
}
