use std::fs;
use std::path::{Path, PathBuf};

use hostname::get;
use jasmine_core::identity::DeviceIdentity;
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
        assert_eq!(
            jasmine_core::identity::load(dir.path()).expect("reload updated identity"),
            updated
        );
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
            assert!(Uuid::parse_str(&recovered.device_id).is_ok());
            assert_eq!(
                jasmine_core::identity::load(dir.path()).expect("reload recovered identity"),
                recovered
            );
        }
    }
}
