use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;

pub const SETTINGS_FILE_NAME: &str = "settings.json";
const DEFAULT_DOWNLOAD_DIR: &str = "~/Downloads/Jasmine/";
const MIN_MAX_CONCURRENT_TRANSFERS: u8 = 1;
const MAX_MAX_CONCURRENT_TRANSFERS: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub download_dir: String,
    pub max_concurrent_transfers: u8,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            download_dir: DEFAULT_DOWNLOAD_DIR.to_string(),
            max_concurrent_transfers: 3,
        }
    }
}

impl AppSettings {
    fn apply_constraints(&mut self) {
        if self.download_dir.trim().is_empty() {
            self.download_dir = DEFAULT_DOWNLOAD_DIR.to_string();
        }

        self.max_concurrent_transfers = self
            .max_concurrent_transfers
            .clamp(MIN_MAX_CONCURRENT_TRANSFERS, MAX_MAX_CONCURRENT_TRANSFERS);
    }

    pub fn normalize(mut self) -> Self {
        self.apply_constraints();
        self
    }
}

#[derive(Debug, Clone)]
pub struct SettingsService {
    settings_path: PathBuf,
}

impl SettingsService {
    pub fn new(app_data_dir: impl AsRef<Path>) -> Self {
        Self {
            settings_path: app_data_dir.as_ref().join(SETTINGS_FILE_NAME),
        }
    }

    pub fn settings_path(&self) -> &Path {
        &self.settings_path
    }

    pub fn load(&self) -> Result<AppSettings> {
        match fs::read_to_string(&self.settings_path) {
            Ok(contents) => {
                let mut settings = serde_json::from_str::<AppSettings>(&contents)
                    .unwrap_or_else(|_| AppSettings::default());
                settings.apply_constraints();
                Ok(settings)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(AppSettings::default())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, settings: &AppSettings) -> Result<()> {
        let parent = self.settings_path.parent().ok_or_else(|| {
            crate::CoreError::Persistence("settings path is missing a parent directory".to_string())
        })?;

        fs::create_dir_all(parent)?;
        let normalized = settings.clone().normalize();
        let contents = serde_json::to_string_pretty(&normalized)?;
        fs::write(&self.settings_path, contents)?;

        Ok(())
    }

    pub fn update<F>(&self, f: F) -> Result<AppSettings>
    where
        F: FnOnce(&mut AppSettings),
    {
        let mut settings = self.load()?;
        f(&mut settings);
        settings.apply_constraints();
        self.save(&settings)?;

        Ok(settings)
    }
}
