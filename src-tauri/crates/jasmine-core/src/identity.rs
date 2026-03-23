use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use jasmine_crypto::{generate_identity_keypair, public_key_to_base64};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{CoreError, Result};

pub const IDENTITY_FILE_NAME: &str = "identity.json";

pub const CURRENT_PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceIdentity {
    pub device_id: String,
    pub display_name: String,
    pub avatar_path: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub protocol_version: u32,
}

impl DeviceIdentity {
    pub fn generate() -> Self {
        let (identity, _) = Self::generate_with_private_key();
        identity
    }

    pub fn generate_with_private_key() -> (Self, Vec<u8>) {
        let (private_key, public_key) = generate_identity_keypair();

        (
            Self {
                device_id: Uuid::new_v4().to_string(),
                display_name: default_display_name(),
                avatar_path: None,
                created_at: unix_timestamp_now(),
                public_key: public_key_to_base64(&public_key),
                protocol_version: CURRENT_PROTOCOL_VERSION,
            },
            private_key.to_bytes().to_vec(),
        )
    }

    fn needs_upgrade(&self) -> bool {
        self.public_key.is_empty() || self.protocol_version != CURRENT_PROTOCOL_VERSION
    }

    fn validate(&self) -> Result<()> {
        let uuid = Uuid::parse_str(&self.device_id)
            .map_err(|_| CoreError::Validation("device_id must be a valid UUID".to_string()))?;

        if uuid.get_version_num() != 4 {
            return Err(CoreError::Validation(
                "device_id must be a UUIDv4 value".to_string(),
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct IdentityStore {
    identity_path: PathBuf,
}

impl IdentityStore {
    pub fn new(app_data_dir: impl AsRef<Path>) -> Self {
        Self {
            identity_path: app_data_dir.as_ref().join(IDENTITY_FILE_NAME),
        }
    }

    pub fn identity_path(&self) -> &Path {
        &self.identity_path
    }

    pub fn load(&self) -> Result<DeviceIdentity> {
        Ok(self.load_with_private_key()?.0)
    }

    pub fn load_with_private_key(&self) -> Result<(DeviceIdentity, Option<Vec<u8>>)> {
        match fs::read_to_string(&self.identity_path) {
            Ok(contents) => match serde_json::from_str::<DeviceIdentity>(&contents) {
                Ok(mut identity) => self.upgrade_and_persist_identity(&mut identity),
                Err(_) => self.regenerate_and_save(),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.regenerate_and_save()
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, identity: &DeviceIdentity) -> Result<()> {
        identity.validate()?;

        let parent = self.identity_path.parent().ok_or_else(|| {
            CoreError::Persistence("identity path is missing a parent directory".to_string())
        })?;

        fs::create_dir_all(parent)?;
        let contents = serde_json::to_string_pretty(identity)?;
        fs::write(&self.identity_path, contents)?;

        Ok(())
    }

    pub fn update_name(&self, name: impl Into<String>) -> Result<DeviceIdentity> {
        let (mut identity, _private_key) = self.load_with_private_key()?;
        identity.display_name = name.into();
        self.save(&identity)?;

        Ok(identity)
    }

    pub fn update_avatar(&self, avatar_path: impl Into<String>) -> Result<DeviceIdentity> {
        let (mut identity, _private_key) = self.load_with_private_key()?;
        identity.avatar_path = Some(avatar_path.into());
        self.save(&identity)?;

        Ok(identity)
    }

    fn regenerate_and_save(&self) -> Result<(DeviceIdentity, Option<Vec<u8>>)> {
        let (identity, private_key) = DeviceIdentity::generate_with_private_key();
        self.save(&identity)?;

        Ok((identity, Some(private_key)))
    }

    fn upgrade_and_persist_identity(
        &self,
        identity: &mut DeviceIdentity,
    ) -> Result<(DeviceIdentity, Option<Vec<u8>>)> {
        if identity.validate().is_err() {
            return self.regenerate_and_save();
        }

        if !identity.needs_upgrade() {
            return Ok((identity.clone(), None));
        }

        let private_key = if identity.public_key.is_empty() {
            let (private_key, public_key) = generate_identity_keypair();
            identity.public_key = public_key_to_base64(&public_key);
            identity.protocol_version = CURRENT_PROTOCOL_VERSION;
            Some(private_key.to_bytes().to_vec())
        } else {
            identity.protocol_version = CURRENT_PROTOCOL_VERSION;
            None
        };

        self.save(identity)?;
        Ok((identity.clone(), private_key))
    }
}

pub fn generate() -> DeviceIdentity {
    DeviceIdentity::generate()
}

pub fn generate_with_private_key() -> (DeviceIdentity, Vec<u8>) {
    DeviceIdentity::generate_with_private_key()
}

pub fn load(app_data_dir: impl AsRef<Path>) -> Result<DeviceIdentity> {
    IdentityStore::new(app_data_dir).load()
}

pub fn load_with_private_key(
    app_data_dir: impl AsRef<Path>,
) -> Result<(DeviceIdentity, Option<Vec<u8>>)> {
    IdentityStore::new(app_data_dir).load_with_private_key()
}

pub fn save(app_data_dir: impl AsRef<Path>, identity: &DeviceIdentity) -> Result<()> {
    IdentityStore::new(app_data_dir).save(identity)
}

pub fn update_name(
    app_data_dir: impl AsRef<Path>,
    name: impl Into<String>,
) -> Result<DeviceIdentity> {
    IdentityStore::new(app_data_dir).update_name(name)
}

pub fn update_avatar(
    app_data_dir: impl AsRef<Path>,
    avatar_path: impl Into<String>,
) -> Result<DeviceIdentity> {
    IdentityStore::new(app_data_dir).update_avatar(avatar_path)
}

fn default_display_name() -> String {
    match hostname::get() {
        Ok(name) => {
            let display_name = name.to_string_lossy().trim().to_string();
            if display_name.is_empty() {
                "Unknown Device".to_string()
            } else {
                display_name
            }
        }
        Err(_) => "Unknown Device".to_string(),
    }
}

fn unix_timestamp_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
