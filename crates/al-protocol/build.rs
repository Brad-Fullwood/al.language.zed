//! Bake a build identity into every binary that links al-protocol.
//!
//! `al-explorer` and `al-lsp` both link this crate, so both carry the same two
//! constants and a client can tell whether the daemon answering it came from
//! the same build. See `src/identity.rs` for how they are compared.
//!
//! - `AL_DAEMON_VERSION` is the `al-lsp` package version, read from the sibling
//!   manifest rather than `CARGO_PKG_VERSION` so the client and the daemon
//!   agree on which released version the pair belongs to.
//! - `AL_BUILD_HASH` is the git commit plus a dirty marker, empty outside a git
//!   checkout.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=AL_BUILD_HASH");

    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("Cargo must provide CARGO_MANIFEST_DIR"),
    );

    let al_lsp_manifest = manifest_dir
        .parent()
        .map(|crates| crates.join("al-lsp").join("Cargo.toml"));
    let version = al_lsp_manifest
        .as_deref()
        .inspect(|path| println!("cargo:rerun-if-changed={}", path.display()))
        .and_then(package_version)
        .unwrap_or_else(|| {
            std::env::var("CARGO_PKG_VERSION").expect("Cargo must provide CARGO_PKG_VERSION")
        });
    println!("cargo:rustc-env=AL_DAEMON_VERSION={version}");

    let hash = std::env::var("AL_BUILD_HASH")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| git_build_hash(&manifest_dir))
        .unwrap_or_default();
    println!("cargo:rustc-env=AL_BUILD_HASH={hash}");
}

/// The `version` of the `[package]` table in a Cargo manifest.
fn package_version(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some(rest) = line.strip_prefix("version") {
            let rest = rest.trim_start().strip_prefix('=')?.trim();
            let value = rest.trim_matches('"');
            if !value.is_empty() && value != rest {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// `<short commit>` for a clean tree, `<short commit>-dirty` when tracked files
/// have been modified, `None` when this is not a git checkout.
fn git_build_hash(manifest_dir: &Path) -> Option<String> {
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(manifest_dir)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };

    let commit = git(&["rev-parse", "--short=12", "HEAD"]).filter(|sha| !sha.is_empty())?;

    // Rebuild the constant when the checked-out commit changes. A worktree's
    // git directory is not `<repo>/.git`, so ask git where HEAD actually lives.
    if let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
    }

    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|status| !status.is_empty());
    Some(if dirty {
        format!("{commit}-dirty")
    } else {
        commit
    })
}
