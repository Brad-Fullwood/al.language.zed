//! Build script: auto-detect which `zed_extension_api` line the build resolves
//! to, and enable the `zed_api_0_8` cfg when it is 0.8.0 or newer.
//!
//! The settings-schema `Extension` methods
//! (`language_server_workspace_configuration_schema` /
//! `language_server_initialization_options_schema`) — which drive
//! settings.json autocomplete + validation for the AL settings block — exist
//! only on API 0.8 and newer. The resolved dependency version controls whether
//! those methods are compiled.

use std::path::Path;

fn main() {
    // Declare the cfg so `#[cfg(zed_api_0_8)]` never trips the unexpected-cfg
    // lint (Rust 1.80+), whether or not we end up setting it.
    println!("cargo:rustc-check-cfg=cfg(zed_api_0_8)");

    // The root package is the workspace root, so Cargo.lock sits next to this.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("Cargo must provide CARGO_MANIFEST_DIR to build scripts");
    let lock_path = Path::new(&manifest_dir).join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());

    if api_supports_schema(&lock_path) {
        println!("cargo:rustc-cfg=zed_api_0_8");
    }
}

/// Return true if the `zed_extension_api` version resolved in `Cargo.lock` is
/// 0.8.0 or newer (the API line that exposes the settings-schema methods).
fn api_supports_schema(lock_path: &Path) -> bool {
    let Ok(lock) = std::fs::read_to_string(lock_path) else {
        println!(
            "cargo:warning=unable to read {}; settings schema API detection is disabled",
            lock_path.display()
        );
        return false;
    };

    // Walk `[[package]]` blocks; inside the zed_extension_api block, read its
    // `version`. Dependency-list entries are bare strings (`"zed_extension_api",`)
    // and never match the `name = "…"` form, so they don't trip the scan.
    let mut in_target = false;
    for line in lock.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            in_target = false;
        } else if let Some(name) = parse_toml_str(line, "name") {
            in_target = name == "zed_extension_api";
        } else if in_target {
            if let Some(version) = parse_toml_str(line, "version") {
                return version_ge_0_8(&version);
            }
        }
    }
    false
}

fn parse_toml_str(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let rest = rest.strip_prefix('=')?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

fn version_ge_0_8(version: &str) -> bool {
    let mut parts = version.split('.');
    let major: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u64 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= (0, 8)
}
