use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const LINUX_RUNTIME_PACKAGES: &[&str] = &["webkit2gtk-4.1", "javascriptcoregtk-4.1", "libsoup-3.0"];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=HOMEBREW_PREFIX");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_LIBDIR");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        emit_linux_runtime_rpaths();
    }

    tauri_build::build()
}

fn emit_linux_runtime_rpaths() {
    let runtime_rpaths = linux_runtime_rpaths();
    if runtime_rpaths.is_empty() {
        return;
    }

    println!("cargo:rustc-link-arg-bin=jasmine=-Wl,--disable-new-dtags");

    for rpath in runtime_rpaths {
        println!(
            "cargo:rustc-link-arg-bin=jasmine=-Wl,-rpath,{}",
            rpath.display()
        );
    }
}

fn linux_runtime_rpaths() -> BTreeSet<PathBuf> {
    let mut rpaths = BTreeSet::new();

    if let Some(prefix) = env::var_os("HOMEBREW_PREFIX") {
        insert_existing_path(&mut rpaths, Path::new(&prefix).join("lib"));
    }

    for package in LINUX_RUNTIME_PACKAGES {
        if let Some(libdir) = pkg_config_libdir(package) {
            insert_existing_path(&mut rpaths, libdir.clone());

            if let Some(prefix_lib) = homebrew_prefix_lib(&libdir) {
                insert_existing_path(&mut rpaths, prefix_lib);
            }
        }
    }

    rpaths
}

fn pkg_config_libdir(package: &str) -> Option<PathBuf> {
    let output = Command::new("pkg-config")
        .args(["--variable=libdir", package])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let libdir = String::from_utf8(output.stdout).ok()?;
    let trimmed = libdir.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn homebrew_prefix_lib(path: &Path) -> Option<PathBuf> {
    let mut prefix = PathBuf::new();

    for component in path.components() {
        if component.as_os_str() == "Cellar" {
            return Some(prefix.join("lib"));
        }
        prefix.push(component.as_os_str());
    }

    None
}

fn insert_existing_path(paths: &mut BTreeSet<PathBuf>, path: PathBuf) {
    if path.is_dir() {
        paths.insert(path);
    }
}
