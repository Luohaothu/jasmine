use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use jasmine_core::settings::{AppSettings, SettingsService};

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("jasmine-core-settings-tests-{}", Uuid::new_v4()));

        fs::create_dir_all(&path).expect("create temp test directory");

        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn settings_file(&self) -> PathBuf {
        self.path.join("settings.json")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn default_settings() -> AppSettings {
    AppSettings::default()
}

#[test]
fn settings_load_returns_defaults_when_file_is_missing() {
    let dir = TestDir::new();
    let service = SettingsService::new(dir.path());

    let settings = service.load().expect("load missing settings");

    assert_eq!(settings, default_settings());
    assert!(settings.download_dir.ends_with("Downloads/Jasmine/"));
    assert_eq!(settings.max_concurrent_transfers, 3);
}

#[test]
fn settings_save_then_load_round_trips_json() {
    let dir = TestDir::new();
    let service = SettingsService::new(dir.path());
    let expected = AppSettings {
        download_dir: "/tmp/custom_downloads/".to_string(),
        max_concurrent_transfers: 4,
    };

    service.save(&expected).expect("save settings");

    assert!(dir.settings_file().exists());

    let loaded = service.load().expect("load saved settings");
    assert_eq!(loaded, expected);

    let raw = fs::read_to_string(dir.settings_file()).expect("read settings file");
    assert!(raw.contains("\n  \"download_dir\""));
    assert!(raw.contains("\"max_concurrent_transfers\""));
}

#[test]
fn settings_load_recovers_defaults_from_corrupted_json() {
    let dir = TestDir::new();
    let service = SettingsService::new(dir.path());

    fs::write(dir.settings_file(), "{{\"download_dir\"\"}").expect("write corrupt settings");

    let loaded = service.load().expect("load corrupted settings");

    assert_eq!(loaded, default_settings());
}

#[test]
fn settings_update_clamps_max_concurrent_transfers_to_range() {
    let dir = TestDir::new();
    let service = SettingsService::new(dir.path());

    let updated = service
        .update(|settings| {
            settings.max_concurrent_transfers = 0;
        })
        .expect("update zero max");

    assert_eq!(updated.max_concurrent_transfers, 1);

    let updated = service
        .update(|settings| {
            settings.max_concurrent_transfers = 10;
        })
        .expect("update over max max");

    assert_eq!(updated.max_concurrent_transfers, 5);
}

#[test]
fn settings_load_is_forward_compatible_when_fields_missing() {
    let dir = TestDir::new();
    let service = SettingsService::new(dir.path());

    fs::write(
        dir.settings_file(),
        "{\n  \"download_dir\": \"/tmp/old\"\n}",
    )
    .expect("write legacy settings");

    let loaded = service.load().expect("load legacy settings");

    assert_eq!(loaded.download_dir, "/tmp/old");
    assert_eq!(loaded.max_concurrent_transfers, 3);
}

#[test]
fn settings_load_falls_back_to_default_when_download_dir_is_empty() {
    let dir = TestDir::new();
    let service = SettingsService::new(dir.path());

    fs::write(
        dir.settings_file(),
        "{\n  \"download_dir\": \"\",\n  \"max_concurrent_transfers\": 2\n}",
    )
    .expect("write empty download_dir settings");

    let loaded = service.load().expect("load empty download_dir settings");

    assert_eq!(loaded.download_dir, default_settings().download_dir);
    assert_eq!(loaded.max_concurrent_transfers, 2);
}
