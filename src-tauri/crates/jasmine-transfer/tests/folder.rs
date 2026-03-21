use std::fs;
use std::path::{Path, PathBuf};

use jasmine_core::protocol::{ProtocolMessage, MAX_PROTOCOL_MESSAGE_BYTES};
use jasmine_transfer::{
    generate_folder_manifest, sanitize_manifest_path, FolderManifestError,
    DEFAULT_FOLDER_MANIFEST_MAX_FILES,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn write_file(path: &Path, bytes: &[u8]) -> PathBuf {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create test parent directories");
    }
    fs::write(path, bytes).expect("write test file");
    path.to_path_buf()
}

#[cfg(unix)]
fn symlink_path(target: &Path, link: &Path, _is_dir: bool) {
    let result = std::os::unix::fs::symlink(target, link);
    result.expect("create unix symlink");
}

#[cfg(windows)]
fn symlink_path(target: &Path, link: &Path, is_dir: bool) {
    let result = if is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    result.expect("create windows symlink");
}

#[test]
fn folder_manifest_happy_path_collects_files_hashes_and_total_size() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("project-notes");
    fs::create_dir_all(folder.join("docs/empty")).expect("create nested directories");

    write_file(&folder.join("a.txt"), b"alpha");
    write_file(&folder.join("docs/b.txt"), b"bravo!");

    let manifest = generate_folder_manifest(&folder, DEFAULT_FOLDER_MANIFEST_MAX_FILES)
        .expect("generate folder manifest");

    assert_eq!(manifest.folder_name, "project-notes");
    assert_eq!(manifest.total_size, 11);
    assert_eq!(manifest.files.len(), 2);
    assert_eq!(manifest.files[0].relative_path, "a.txt");
    assert_eq!(manifest.files[0].size, 5);
    assert_eq!(manifest.files[0].sha256, sha256_hex(b"alpha"));
    assert_eq!(manifest.files[1].relative_path, "docs/b.txt");
    assert_eq!(manifest.files[1].size, 6);
    assert_eq!(manifest.files[1].sha256, sha256_hex(b"bravo!"));

    let payload = ProtocolMessage::FolderManifest {
        folder_transfer_id: "00000000-0000-0000-0000-000000000000".to_string(),
        manifest,
        sender_id: "00000000-0000-0000-0000-000000000000".to_string(),
    }
    .to_json()
    .expect("serialize generated manifest");
    assert!(payload.len() <= MAX_PROTOCOL_MESSAGE_BYTES);
}

#[test]
fn folder_manifest_empty_directory_returns_empty_manifest() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("empty-folder");
    fs::create_dir_all(&folder).expect("create empty folder");

    let manifest = generate_folder_manifest(&folder, DEFAULT_FOLDER_MANIFEST_MAX_FILES)
        .expect("generate empty manifest");

    assert_eq!(manifest.folder_name, "empty-folder");
    assert!(manifest.files.is_empty());
    assert_eq!(manifest.total_size, 0);
}

#[test]
fn sanitize_manifest_path_rejects_unsafe_inputs_and_normalizes_windows_separators() {
    let unsafe_paths = [
        "../secret.txt",
        "nested/../../secret.txt",
        "/absolute/path.txt",
        "\\absolute\\path.txt",
        "C:/windows/system32.txt",
        "C:relative-on-drive.txt",
        "safe\0name.txt",
        "safe/\u{001f}control.txt",
        "nested\\..\\secret.txt",
        "nested//hole.txt",
    ];

    for candidate in unsafe_paths {
        let error = sanitize_manifest_path(candidate).expect_err("path should be rejected");
        assert!(matches!(error, FolderManifestError::UnsafePath(_)));
    }

    assert_eq!(
        sanitize_manifest_path("nested\\sub\\file.txt").expect("normalize path"),
        "nested/sub/file.txt"
    );
}

#[test]
fn folder_manifest_skips_symlink_files_and_directories() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("photos");
    let external_dir = temp.path().join("external-dir");
    fs::create_dir_all(&folder).expect("create manifest folder");
    fs::create_dir_all(&external_dir).expect("create external dir");

    write_file(&folder.join("keep.txt"), b"keep");
    let external_file = write_file(&temp.path().join("outside.txt"), b"outside");
    write_file(&external_dir.join("nested.txt"), b"nested");
    symlink_path(&external_file, &folder.join("link-file.txt"), false);
    symlink_path(&external_dir, &folder.join("link-dir"), true);

    let manifest = generate_folder_manifest(&folder, DEFAULT_FOLDER_MANIFEST_MAX_FILES)
        .expect("generate folder manifest");

    assert_eq!(manifest.files.len(), 1);
    assert_eq!(manifest.files[0].relative_path, "keep.txt");
    assert_eq!(manifest.files[0].sha256, sha256_hex(b"keep"));
}

#[test]
fn folder_manifest_rejects_more_than_200_files() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("too-many-files");
    fs::create_dir_all(&folder).expect("create folder");

    for index in 0..=DEFAULT_FOLDER_MANIFEST_MAX_FILES {
        write_file(&folder.join(format!("file-{index:03}.txt")), b"x");
    }

    let error = generate_folder_manifest(&folder, DEFAULT_FOLDER_MANIFEST_MAX_FILES)
        .expect_err("manifest should reject file count overflow");
    assert!(matches!(
        error,
        FolderManifestError::FileLimitExceeded {
            max_files: DEFAULT_FOLDER_MANIFEST_MAX_FILES
        }
    ));
}

#[test]
fn folder_manifest_rejects_manifests_that_exceed_protocol_size_limit() {
    let temp = tempdir().expect("tempdir");
    let folder = temp.path().join("oversized-manifest");
    let deep_dir = folder.join("d".repeat(120));
    fs::create_dir_all(&deep_dir).expect("create deep directory");

    for index in 0..DEFAULT_FOLDER_MANIFEST_MAX_FILES {
        let file_name = format!("{:03}-{}.txt", index, "f".repeat(108));
        write_file(&deep_dir.join(file_name), b"z");
    }

    let error = generate_folder_manifest(&folder, DEFAULT_FOLDER_MANIFEST_MAX_FILES)
        .expect_err("manifest should exceed protocol limit");
    assert!(matches!(error, FolderManifestError::ManifestTooLarge));
}
