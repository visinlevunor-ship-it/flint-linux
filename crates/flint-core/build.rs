//! Embeds the distro version (./VERSION) and a build id into flint-core.
//!
//! Build id: $FLINT_BUILD_ID if set (reproducible/release builds), otherwise the
//! short git commit hash, otherwise "nogit".

use std::{env, fs, path::Path, path::PathBuf, process::Command};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest.join("..").join("..");
    let version_file = root.join("VERSION");

    println!("cargo:rerun-if-changed={}", version_file.display());
    println!("cargo:rerun-if-env-changed=FLINT_BUILD_ID");
    for p in [".git/HEAD", ".git/refs"] {
        let path = root.join(p);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }

    let version = fs::read_to_string(&version_file)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", version_file.display()))
        .trim()
        .to_string();
    let valid = !version.is_empty()
        && version.split('.').count() == 2
        && version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !valid {
        panic!("VERSION must look like MAJOR.MINOR (e.g. 0.1), got {version:?}");
    }

    let build = env::var("FLINT_BUILD_ID")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| git_hash(&root).unwrap_or_else(|| "nogit".to_string()));

    println!("cargo:rustc-env=FLINT_VERSION={version}");
    println!("cargo:rustc-env=FLINT_BUILD={build}");
}

fn git_hash(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}
