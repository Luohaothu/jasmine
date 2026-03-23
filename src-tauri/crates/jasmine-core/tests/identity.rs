use std::fs;
use std::path::{Path, PathBuf};

use hostname::get;
use jasmine_core::identity::DeviceIdentity;
use serde_json::json;
use uuid::Uuid;

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("jasmine-core-identity-tests-{}", Uuid::new_v4()));

        fs::create_dir_all(&path).expect("create temp test directory");

        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn identity_file(&self) -> PathBuf {
        self.path.join("identity.json")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn hostname_string() -> String {
    get()
        .expect("resolve hostname")
        .to_string_lossy()
        .into_owned()
}

mod identity {
    use super::*;

    #[test]
    fn generate_identity_uses_uuid_v4_and_hostname_defaults() {
        let identity = jasmine_core::identity::generate();
        let parsed = Uuid::parse_str(&identity.device_id).expect("parse device id as uuid");

        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(identity.display_name, hostname_string());
        assert_eq!(identity.avatar_path, None);
        assert_eq!(identity.protocol_version, 2);
        assert!(!identity.public_key.is_empty());
        assert!(identity.created_at > 0);
    }

    #[test]
    fn save_and_load_round_trip_persists_identity_json() {
        let dir = TestDir::new();
        let identity = DeviceIdentity {
            device_id: Uuid::new_v4().to_string(),
            display_name: "Desk Jasmine".to_string(),
            avatar_path: Some("/tmp/avatar.png".to_string()),
            created_at: 42,
            public_key: "public-key-v2".to_string(),
            protocol_version: 2,
        };

        jasmine_core::identity::save(dir.path(), &identity).expect("save identity");

        assert!(dir.identity_file().exists());
        assert_eq!(
            jasmine_core::identity::load(dir.path()).expect("load identity"),
            identity
        );
    }

    #[test]
    fn load_creates_and_persists_default_identity_when_file_is_missing() {
        let dir = TestDir::new();

        let identity = jasmine_core::identity::load(dir.path()).expect("load missing identity");

        assert!(dir.identity_file().exists());
        assert_eq!(identity.display_name, hostname_string());
        assert_eq!(identity.protocol_version, 2);
        assert!(!identity.public_key.is_empty());
        assert_eq!(
            jasmine_core::identity::load(dir.path()).expect("reload generated identity"),
            identity
        );
    }

    #[test]
    fn load_keeps_identity_stable_across_multiple_calls() {
        let dir = TestDir::new();

        let first = jasmine_core::identity::load(dir.path()).expect("load first identity");
        let second = jasmine_core::identity::load(dir.path()).expect("load second identity");

        assert_eq!(first, second);
    }

    #[test]
    fn update_name_persists_change_without_replacing_identity() {
        let dir = TestDir::new();
        let original = jasmine_core::identity::load(dir.path()).expect("load original identity");

        let updated = jasmine_core::identity::update_name(dir.path(), "Lan Friend")
            .expect("update identity display name");

        assert_eq!(updated.display_name, "Lan Friend");
        assert_eq!(updated.device_id, original.device_id);
        assert_eq!(updated.avatar_path, original.avatar_path);
        assert_eq!(updated.created_at, original.created_at);
        assert_eq!(updated.public_key, original.public_key);
        assert_eq!(updated.protocol_version, original.protocol_version);
        assert_eq!(
            jasmine_core::identity::load(dir.path()).expect("reload updated identity"),
            updated
        );
    }

    #[test]
    fn update_avatar_persists_change_without_replacing_identity() {
        let dir = TestDir::new();
        let original = jasmine_core::identity::load(dir.path()).expect("load original identity");

        let updated = jasmine_core::identity::update_avatar(dir.path(), "/tmp/avatars/device.png")
            .expect("update avatar path");

        assert_eq!(
            updated.avatar_path.as_deref(),
            Some("/tmp/avatars/device.png")
        );
        assert_eq!(updated.device_id, original.device_id);
        assert_eq!(updated.display_name, original.display_name);
        assert_eq!(updated.created_at, original.created_at);
        assert_eq!(updated.public_key, original.public_key);
        assert_eq!(updated.protocol_version, original.protocol_version);
        assert_eq!(
            jasmine_core::identity::load(dir.path()).expect("reload updated identity"),
            updated
        );
    }

    #[test]
    fn load_with_private_key_creates_key_for_legacy_identity_without_public_key() {
        let dir = TestDir::new();
        let legacy_device_id = Uuid::new_v4().to_string();
        let legacy_identity = json!({
            "device_id": legacy_device_id,
            "display_name": "Legacy Device",
            "avatar_path": "/tmp/legacy-avatar.png",
            "created_at": 11,
        });

        fs::write(
            dir.identity_file(),
            serde_json::to_string(&legacy_identity).expect("serialize legacy identity"),
        )
        .expect("write legacy identity file");

        let (loaded, private_key) = jasmine_core::identity::load_with_private_key(dir.path())
            .expect("load legacy identity");

        assert!(private_key.is_some());
        assert_eq!(loaded.device_id, legacy_device_id);
        assert_eq!(loaded.display_name, "Legacy Device");
        assert_eq!(
            loaded.avatar_path.as_deref(),
            Some("/tmp/legacy-avatar.png")
        );
        assert_eq!(loaded.created_at, 11);
        assert!(!loaded.public_key.is_empty());
        assert_eq!(loaded.protocol_version, 2);

        let persisted: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.identity_file()).expect("read persisted identity"),
        )
        .expect("parse persisted identity");
        assert_eq!(
            persisted["public_key"],
            serde_json::Value::String(loaded.public_key.clone())
        );
        assert_eq!(persisted["protocol_version"], serde_json::json!(2));

        let (reloaded, second_private_key) =
            jasmine_core::identity::load_with_private_key(dir.path())
                .expect("reload upgraded identity");
        assert_eq!(reloaded, loaded);
        assert!(second_private_key.is_none());
    }

    #[test]
    fn load_with_private_key_preserves_public_key_for_legacy_identity() {
        let dir = TestDir::new();
        let legacy_device_id = Uuid::new_v4().to_string();
        let legacy_identity = json!({
            "device_id": legacy_device_id,
            "display_name": "Legacy Key Device",
            "avatar_path": serde_json::Value::Null,
            "created_at": 22,
            "public_key": "legacy-public-key",
            "protocol_version": 1,
        });

        fs::write(
            dir.identity_file(),
            serde_json::to_string_pretty(&legacy_identity).expect("serialize legacy identity"),
        )
        .expect("write legacy identity with public key");

        let (loaded, private_key) = jasmine_core::identity::load_with_private_key(dir.path())
            .expect("load legacy identity");

        assert!(private_key.is_none());
        assert_eq!(loaded.device_id, legacy_device_id);
        assert_eq!(loaded.public_key, "legacy-public-key");
        assert_eq!(loaded.protocol_version, 2);

        let persisted: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.identity_file()).expect("read persisted identity"),
        )
        .expect("parse persisted identity");
        assert_eq!(
            persisted["public_key"],
            serde_json::json!("legacy-public-key")
        );
        assert_eq!(persisted["protocol_version"], serde_json::json!(2));
    }

    mod corrupted {
        use super::*;

        #[test]
        fn load_recovers_from_invalid_json_with_new_identity() {
            let dir = TestDir::new();
            let original =
                jasmine_core::identity::load(dir.path()).expect("create original identity");

            fs::write(dir.identity_file(), "{not-json").expect("write corrupt identity file");

            let recovered = jasmine_core::identity::load(dir.path())
                .expect("recover from corrupt identity file");

            assert_ne!(recovered.device_id, original.device_id);
            assert_eq!(recovered.display_name, hostname_string());
            assert_eq!(recovered.protocol_version, 2);
            assert!(!recovered.public_key.is_empty());
            assert!(Uuid::parse_str(&recovered.device_id).is_ok());
            assert_eq!(
                jasmine_core::identity::load(dir.path()).expect("reload recovered identity"),
                recovered
            );
        }
    }
}
