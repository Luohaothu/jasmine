use std::fs;
use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use keyring::{Entry, Error as KeyringError};
use rand_core::{OsRng, RngCore};
use zeroize::Zeroize;

use crate::keypair::KEY_MATERIAL_LEN;
use crate::{decrypt, encrypt, generate_nonce, CryptoError, Result, StaticSecret};

const KEYRING_SERVICE: &str = "jasmine";
const APP_NAME: &str = "jasmine";
const FALLBACK_FILE_NAME: &str = "keystore.enc";
const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = SALT_LEN + NONCE_LEN;
const ARGON2_OUTPUT_LEN: usize = 32;
const ARGON2_M_COST_KIB: u32 = 64 * 1024;
const ARGON2_T_COST: u32 = 3;
const ARGON2_P_COST: u32 = 1;
const FALLBACK_AAD_PREFIX: &[u8] = b"jasmine/private-key/v1";

pub fn store_private_key(device_id: &str, secret: &StaticSecret) -> Result<()> {
    Keystore::system().store_private_key(device_id, secret)
}

pub fn load_private_key(device_id: &str) -> Result<Option<StaticSecret>> {
    Keystore::system().load_private_key(device_id)
}

pub fn delete_private_key(device_id: &str) -> Result<()> {
    Keystore::system().delete_private_key(device_id)
}

#[derive(Debug, Clone)]
struct Keystore {
    keyring: KeyringBackend,
    fallback: FallbackResolver,
}

impl Keystore {
    fn system() -> Self {
        Self {
            keyring: KeyringBackend::System,
            fallback: FallbackResolver::system(),
        }
    }

    #[cfg(test)]
    fn fallback_only(fallback_root: PathBuf, machine_material: Vec<u8>) -> Self {
        Self {
            keyring: KeyringBackend::Unavailable,
            fallback: FallbackResolver::fixed(fallback_root, machine_material),
        }
    }

    fn store_private_key(&self, device_id: &str, secret: &StaticSecret) -> Result<()> {
        validate_device_id(device_id)?;

        let mut secret_bytes = secret.to_bytes();
        let result = match self.keyring {
            KeyringBackend::System => match keyring_entry(device_id) {
                Ok(entry) => match entry.set_secret(&secret_bytes) {
                    Ok(()) => match self.fallback.delete_private_key(device_id) {
                        Ok(()) | Err(CryptoError::AppDataDirUnavailable) => Ok(()),
                        Err(error) => Err(error),
                    },
                    Err(error) if should_fallback_from_keyring(&error) => {
                        self.fallback.store_private_key(device_id, &secret_bytes)
                    }
                    Err(error) => Err(map_keyring_error(error)),
                },
                Err(error) if should_fallback_from_keyring(&error) => {
                    self.fallback.store_private_key(device_id, &secret_bytes)
                }
                Err(error) => Err(map_keyring_error(error)),
            },
            KeyringBackend::Unavailable => {
                self.fallback.store_private_key(device_id, &secret_bytes)
            }
        };

        secret_bytes.zeroize();
        result
    }

    fn load_private_key(&self, device_id: &str) -> Result<Option<StaticSecret>> {
        validate_device_id(device_id)?;

        match self.keyring {
            KeyringBackend::System => match keyring_entry(device_id) {
                Ok(entry) => match entry.get_secret() {
                    Ok(mut secret_bytes) => {
                        let secret = static_secret_from_slice(&secret_bytes);
                        secret_bytes.zeroize();
                        secret.map(Some)
                    }
                    Err(KeyringError::NoEntry) => self.fallback.load_private_key(device_id),
                    Err(KeyringError::BadEncoding(_)) => Err(CryptoError::SecureStorageCorrupted),
                    Err(error) if should_fallback_from_keyring(&error) => {
                        self.fallback.load_private_key(device_id)
                    }
                    Err(error) => Err(map_keyring_error(error)),
                },
                Err(error) if should_fallback_from_keyring(&error) => {
                    self.fallback.load_private_key(device_id)
                }
                Err(error) => Err(map_keyring_error(error)),
            },
            KeyringBackend::Unavailable => self.fallback.load_private_key(device_id),
        }
    }

    fn delete_private_key(&self, device_id: &str) -> Result<()> {
        validate_device_id(device_id)?;

        match self.keyring {
            KeyringBackend::System => match keyring_entry(device_id) {
                Ok(entry) => match entry.delete_credential() {
                    Ok(()) | Err(KeyringError::NoEntry) => {
                        self.fallback.delete_private_key(device_id)
                    }
                    Err(error) if should_fallback_from_keyring(&error) => {
                        self.fallback.delete_private_key(device_id)
                    }
                    Err(error) => Err(map_keyring_error(error)),
                },
                Err(error) if should_fallback_from_keyring(&error) => {
                    self.fallback.delete_private_key(device_id)
                }
                Err(error) => Err(map_keyring_error(error)),
            },
            KeyringBackend::Unavailable => self.fallback.delete_private_key(device_id),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy)]
enum KeyringBackend {
    System,
    Unavailable,
}

#[derive(Debug, Clone)]
struct FallbackResolver {
    root: FallbackRoot,
    machine_material: MachineMaterial,
}

impl FallbackResolver {
    fn system() -> Self {
        Self {
            root: FallbackRoot::System,
            machine_material: MachineMaterial::System,
        }
    }

    #[cfg(test)]
    fn fixed(root: PathBuf, machine_material: Vec<u8>) -> Self {
        Self {
            root: FallbackRoot::Fixed(root),
            machine_material: MachineMaterial::Fixed(machine_material),
        }
    }

    fn store_private_key(
        &self,
        device_id: &str,
        secret_bytes: &[u8; KEY_MATERIAL_LEN],
    ) -> Result<()> {
        let path = self.path()?;
        let parent = path.parent().ok_or_else(|| {
            CryptoError::Persistence("private key path is missing a parent directory".to_string())
        })?;

        fs::create_dir_all(parent).map_err(io_persistence_error)?;

        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let nonce = generate_nonce();
        let encoded = self.encrypt_private_key_file(device_id, secret_bytes, &salt, &nonce)?;

        fs::write(path, encoded).map_err(io_persistence_error)
    }

    fn load_private_key(&self, device_id: &str) -> Result<Option<StaticSecret>> {
        let path = self.path()?;
        let encoded = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_persistence_error(error)),
        };

        self.decrypt_private_key_file(device_id, &encoded).map(Some)
    }

    fn delete_private_key(&self, _device_id: &str) -> Result<()> {
        let path = self.path()?;
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_persistence_error(error)),
        }
    }

    fn encrypt_private_key_file(
        &self,
        device_id: &str,
        secret_bytes: &[u8; KEY_MATERIAL_LEN],
        salt: &[u8; SALT_LEN],
        nonce: &[u8; NONCE_LEN],
    ) -> Result<Vec<u8>> {
        let mut derived_key = self.derive_file_key(device_id, salt)?;
        let aad = fallback_aad(device_id);
        let ciphertext = encrypt(&derived_key, nonce, secret_bytes, &aad);
        derived_key.zeroize();

        let ciphertext = ciphertext?;
        let mut output = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        output.extend_from_slice(salt);
        output.extend_from_slice(nonce);
        output.extend_from_slice(&ciphertext);

        Ok(output)
    }

    fn decrypt_private_key_file(&self, device_id: &str, encoded: &[u8]) -> Result<StaticSecret> {
        if encoded.len() < HEADER_LEN {
            return Err(CryptoError::InvalidKeystoreFile {
                actual: encoded.len(),
            });
        }

        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&encoded[..SALT_LEN]);

        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&encoded[SALT_LEN..HEADER_LEN]);

        let mut derived_key = self.derive_file_key(device_id, &salt)?;
        let aad = fallback_aad(device_id);
        let plaintext = decrypt(&derived_key, &nonce, &encoded[HEADER_LEN..], &aad);
        derived_key.zeroize();

        let mut plaintext = plaintext?;
        let secret = static_secret_from_slice(&plaintext);
        plaintext.zeroize();
        secret
    }

    fn derive_file_key(
        &self,
        device_id: &str,
        salt: &[u8; SALT_LEN],
    ) -> Result<[u8; ARGON2_OUTPUT_LEN]> {
        let params = Params::new(
            ARGON2_M_COST_KIB,
            ARGON2_T_COST,
            ARGON2_P_COST,
            Some(ARGON2_OUTPUT_LEN),
        )
        .map_err(|_| CryptoError::KeyDerivationFailed)?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);

        let mut machine_material = self.machine_material.resolve()?;
        let mut password = Vec::with_capacity(
            FALLBACK_AAD_PREFIX.len() + 1 + device_id.len() + 1 + machine_material.len(),
        );
        password.extend_from_slice(FALLBACK_AAD_PREFIX);
        password.push(0);
        password.extend_from_slice(device_id.as_bytes());
        password.push(0);
        password.extend_from_slice(&machine_material);

        let mut output = [0u8; ARGON2_OUTPUT_LEN];
        let derive_result = argon2.hash_password_into(&password, salt, &mut output);
        password.zeroize();
        machine_material.zeroize();

        match derive_result {
            Ok(()) => Ok(output),
            Err(_) => {
                output.zeroize();
                Err(CryptoError::KeyDerivationFailed)
            }
        }
    }

    fn path(&self) -> Result<PathBuf> {
        let root = self.root.resolve()?;
        Ok(root.join(FALLBACK_FILE_NAME))
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone)]
enum FallbackRoot {
    System,
    Fixed(PathBuf),
}

impl FallbackRoot {
    fn resolve(&self) -> Result<PathBuf> {
        match self {
            Self::System => system_app_data_dir(),
            Self::Fixed(path) => Ok(path.clone()),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone)]
enum MachineMaterial {
    System,
    Fixed(Vec<u8>),
}

impl MachineMaterial {
    fn resolve(&self) -> Result<Vec<u8>> {
        match self {
            Self::System => system_machine_material(),
            Self::Fixed(bytes) => Ok(bytes.clone()),
        }
    }
}

fn keyring_entry(device_id: &str) -> std::result::Result<Entry, KeyringError> {
    Entry::new(KEYRING_SERVICE, device_id)
}

fn should_fallback_from_keyring(error: &KeyringError) -> bool {
    matches!(
        error,
        KeyringError::NoStorageAccess(_)
            | KeyringError::PlatformFailure(_)
            | KeyringError::Ambiguous(_)
    )
}

fn map_keyring_error(error: KeyringError) -> CryptoError {
    match error {
        KeyringError::BadEncoding(_) => CryptoError::SecureStorageCorrupted,
        other => CryptoError::SecureStorage(other.to_string()),
    }
}

fn static_secret_from_slice(bytes: &[u8]) -> Result<StaticSecret> {
    let actual = bytes.len();
    let bytes: [u8; KEY_MATERIAL_LEN] = bytes
        .try_into()
        .map_err(|_| CryptoError::InvalidPrivateKeyLength { actual })?;

    Ok(StaticSecret::from(bytes))
}

fn validate_device_id(device_id: &str) -> Result<()> {
    if device_id.trim().is_empty()
        || device_id == "."
        || device_id == ".."
        || device_id.contains('/')
        || device_id.contains('\\')
    {
        return Err(CryptoError::InvalidKeystoreIdentifier(
            device_id.to_string(),
        ));
    }

    Ok(())
}

fn system_app_data_dir() -> Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            return Ok(PathBuf::from(app_data).join(APP_NAME));
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return Ok(PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join(APP_NAME));
        }
    }

    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(data_home).join(APP_NAME));
    }

    if let Some(home) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join(APP_NAME));
    }

    Err(CryptoError::AppDataDirUnavailable)
}

fn system_machine_material() -> Result<Vec<u8>> {
    for path in [
        Path::new("/etc/machine-id"),
        Path::new("/var/lib/dbus/machine-id"),
        Path::new("/etc/hostname"),
        Path::new("/proc/sys/kernel/hostname"),
    ] {
        if let Some(bytes) = read_trimmed_bytes(path) {
            return Ok(bytes);
        }
    }

    for env_var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Some(value) = std::env::var_os(env_var) {
            let trimmed = value.to_string_lossy().trim().as_bytes().to_vec();
            if !trimmed.is_empty() {
                return Ok(trimmed);
            }
        }
    }

    Err(CryptoError::MachineKeyMaterialUnavailable)
}

fn read_trimmed_bytes(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    let trimmed = String::from_utf8_lossy(&bytes).trim().as_bytes().to_vec();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn fallback_aad(device_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(FALLBACK_AAD_PREFIX.len() + 1 + device_id.len());
    aad.extend_from_slice(FALLBACK_AAD_PREFIX);
    aad.push(0);
    aad.extend_from_slice(device_id.as_bytes());
    aad
}

fn io_persistence_error(error: std::io::Error) -> CryptoError {
    CryptoError::Persistence(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn keystore_store_load_roundtrip_uses_encrypted_fallback_when_keyring_unavailable() {
        let temp_dir = TestDir::new();
        let keystore = Keystore::fallback_only(
            temp_dir.path().to_path_buf(),
            b"machine-material-roundtrip".to_vec(),
        );
        let device_id = "device-roundtrip";
        let secret = StaticSecret::from(decode_hex::<KEY_MATERIAL_LEN>(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        ));

        keystore
            .store_private_key(device_id, &secret)
            .expect("fallback store should succeed");

        let loaded = keystore
            .load_private_key(device_id)
            .expect("fallback load should succeed")
            .expect("stored secret should exist");
        let path = keystore
            .fallback
            .path()
            .expect("fallback path should resolve");

        assert_eq!(loaded.to_bytes(), secret.to_bytes());
        assert!(path.exists(), "fallback file should exist after store");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(FALLBACK_FILE_NAME)
        );
    }

    #[test]
    fn keystore_delete_removes_key() {
        let temp_dir = TestDir::new();
        let keystore = Keystore::fallback_only(
            temp_dir.path().to_path_buf(),
            b"machine-material-delete".to_vec(),
        );
        let device_id = "device-delete";
        let secret = StaticSecret::from(decode_hex::<KEY_MATERIAL_LEN>(
            "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
        ));

        keystore
            .store_private_key(device_id, &secret)
            .expect("fallback store should succeed before delete");
        keystore
            .delete_private_key(device_id)
            .expect("delete should remove fallback secret");

        let path = keystore
            .fallback
            .path()
            .expect("fallback path should resolve");

        assert!(!path.exists(), "delete should remove fallback file");
        assert!(
            keystore
                .load_private_key(device_id)
                .expect("load after delete should succeed")
                .is_none(),
            "deleted key should not load"
        );
    }

    #[test]
    fn keystore_load_nonexistent_returns_none() {
        let temp_dir = TestDir::new();
        let keystore = Keystore::fallback_only(
            temp_dir.path().to_path_buf(),
            b"machine-material-missing".to_vec(),
        );

        assert!(
            keystore
                .load_private_key("device-missing")
                .expect("missing key lookup should succeed")
                .is_none(),
            "missing key lookup should return None"
        );
    }

    #[test]
    fn keystore_fallback_known_key_is_deterministic_with_fixed_salt_and_nonce() {
        let temp_dir = TestDir::new();
        let keystore = Keystore::fallback_only(
            temp_dir.path().to_path_buf(),
            b"machine-material-deterministic".to_vec(),
        );
        let device_id = "device-deterministic";
        let secret_bytes = decode_hex::<KEY_MATERIAL_LEN>(
            "a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4",
        );
        let salt = decode_hex::<SALT_LEN>(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
        let nonce = decode_hex::<NONCE_LEN>("202122232425262728292a2b");

        let first = keystore
            .fallback
            .encrypt_private_key_file(device_id, &secret_bytes, &salt, &nonce)
            .expect("deterministic fallback encode should succeed");
        let second = keystore
            .fallback
            .encrypt_private_key_file(device_id, &secret_bytes, &salt, &nonce)
            .expect("second deterministic fallback encode should succeed");

        assert_eq!(first, second);
        assert_eq!(&first[..SALT_LEN], salt.as_slice());
        assert_eq!(&first[SALT_LEN..HEADER_LEN], nonce.as_slice());
        assert_eq!(
            keystore
                .fallback
                .decrypt_private_key_file(device_id, &first)
                .expect("deterministic fallback bytes should decrypt")
                .to_bytes(),
            secret_bytes
        );
    }

    #[test]
    fn keystore_fallback_ciphertext_changes_with_device_id() {
        let temp_dir = TestDir::new();
        let keystore = Keystore::fallback_only(
            temp_dir.path().to_path_buf(),
            b"machine-material-device-id".to_vec(),
        );
        let secret_bytes = decode_hex::<KEY_MATERIAL_LEN>(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        );
        let salt = decode_hex::<SALT_LEN>(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
        let nonce = decode_hex::<NONCE_LEN>("202122232425262728292a2b");

        let alpha = keystore
            .fallback
            .encrypt_private_key_file("device-alpha", &secret_bytes, &salt, &nonce)
            .expect("alpha encryption should succeed");
        let beta = keystore
            .fallback
            .encrypt_private_key_file("device-beta", &secret_bytes, &salt, &nonce)
            .expect("beta encryption should succeed");

        assert_ne!(
            alpha, beta,
            "device_id must change fallback encryption output"
        );
    }

    #[test]
    fn keystore_fallback_file_does_not_contain_plaintext_private_key() {
        let temp_dir = TestDir::new();
        let keystore = Keystore::fallback_only(
            temp_dir.path().to_path_buf(),
            b"machine-material-no-plaintext".to_vec(),
        );
        let device_id = "device-no-plaintext";
        let secret = StaticSecret::from(decode_hex::<KEY_MATERIAL_LEN>(
            "4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d",
        ));

        keystore
            .store_private_key(device_id, &secret)
            .expect("fallback store should succeed");

        let encoded = fs::read(
            keystore
                .fallback
                .path()
                .expect("fallback path should resolve"),
        )
        .expect("fallback file should be readable");
        let secret_bytes = secret.to_bytes();

        assert!(
            !encoded
                .windows(KEY_MATERIAL_LEN)
                .any(|window| window == secret_bytes.as_slice()),
            "fallback file must not contain plaintext key bytes"
        );
    }

    #[test]
    fn keystore_tamper_encrypted_file_returns_error() {
        let temp_dir = TestDir::new();
        let keystore = Keystore::fallback_only(
            temp_dir.path().to_path_buf(),
            b"machine-material-tamper".to_vec(),
        );
        let device_id = "device-tamper";
        let secret = StaticSecret::from(decode_hex::<KEY_MATERIAL_LEN>(
            "e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493",
        ));

        keystore
            .store_private_key(device_id, &secret)
            .expect("fallback store should succeed before tamper");

        let path = keystore
            .fallback
            .path()
            .expect("fallback path should resolve");
        let mut encoded = fs::read(&path).expect("fallback file should exist before tamper");
        let last_index = encoded.len() - 1;
        encoded[last_index] ^= 0x01;
        fs::write(&path, encoded).expect("tampered fallback file should be writable");

        let error = match keystore.load_private_key(device_id) {
            Ok(Some(_)) => panic!("tampered fallback file must not decrypt"),
            Ok(None) => panic!("tampered fallback file must not disappear silently"),
            Err(error) => error,
        };

        assert_eq!(error, CryptoError::DecryptFailed);
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let unique = format!(
                "jasmine-crypto-keystore-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);

            fs::create_dir_all(&path).expect("create temp keystore directory");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
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
