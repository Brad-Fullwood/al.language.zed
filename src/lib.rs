mod dap;
mod settings;

#[cfg(test)]
mod merge_json_test;
#[cfg(test)]
mod repo_consistency_test;
#[cfg(test)]
mod settings_test;

use serde_json::{json, Value};
use std::fs;
use zed_extension_api::{self as zed, settings::LspSettings, Result};

/// GitHub repository that publishes the `al-lsp` release assets the extension
/// downloads in step 4 of `find_or_download_binary`. This MUST match the actual
/// repository (the `origin` git remote / `extension.toml` `repository` field),
/// otherwise `latest_github_release` fails with "repository not found" and a
/// fresh user's language server never spawns. A CI consistency check
/// (`scripts/check-repo-consistency.sh`) and the `github_repo_matches_extension_toml`
/// unit test guard against this drifting.
const GITHUB_REPO: &str = "Brad-Fullwood/al.language.zed";

struct AlExtension {
    /// Path to a previously downloaded al-lsp binary in the extension work dir.
    /// Set after a successful GitHub release download; re-checked on each call.
    cached_binary_path: Option<String>,
}

/// Maximum nesting depth for `merge_json`. Above this, the override
/// value is used as-is. Defends against stack-overflow / DoS from
/// pathologically nested user settings.
const MERGE_JSON_MAX_DEPTH: u32 = 64;

/// Validate that a GitHub release version is safe to interpolate into a
/// filesystem path. Allows the chars a normal semver tag uses
/// (`0-9`, `a-z`, `A-Z`, `.`, `-`, `_`, `+`) and nothing else — in particular,
/// no `/` or `..` segments. An empty string is rejected.
fn is_safe_version(v: &str) -> bool {
    !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// Shared, per-OS "how to recover" guidance appended to every spawn-failure
/// message: where to download a build manually, a copy-paste Zed settings
/// snippet pointing `binary.path` at it, and the PATH fallback. Centralised so
/// that *every* failure mode on the download path (no release yet, GitHub API /
/// network failure, no matching asset) tells a new user exactly what to do.
fn manual_install_hint(os: zed::Os) -> String {
    // Per-OS path separator hint for the manual `binary.path` value, so the
    // snippet is correct to paste on the user's actual platform.
    let example_path = match os {
        zed::Os::Windows => r"C:\\path\\to\\al-lsp.exe",
        _ => "/path/to/al-lsp",
    };
    format!(
        "Download a build manually from https://github.com/{GITHUB_REPO}/releases and \
         point Zed at it via settings: \
         {{\"lsp\": {{\"al-lsp\": {{\"binary\": {{\"path\": \"{example_path}\"}}}}}}}}. \
         Alternatively install al-lsp onto your PATH (e.g. `cargo install`)."
    )
}

/// Build an actionable error for when no matching release asset can be found
/// for the current platform. Includes the expected asset name, the releases
/// page to download from manually, and a copy-paste Zed settings snippet so a
/// new user whose language server fails to spawn knows exactly what to do
/// (addresses the #1 new-user failure mode — see ecosystem-roadmap.md QW3).
fn spawn_failure_message(os: zed::Os, asset_name: &str) -> String {
    format!(
        "Could not find an al-lsp release asset named '{asset_name}' for this platform. {}",
        manual_install_hint(os)
    )
}

/// Build an actionable error for when the GitHub release lookup itself fails —
/// most commonly because **no release has been published yet** (the reported
/// new-user blocker), but also network failures, rate limits, or a renamed
/// repository. Without this, `latest_github_release(...)?` surfaces a raw,
/// opaque error (e.g. "no releases found") with no path forward. This is the
/// single most likely first-run failure, so it must be just as actionable as
/// the asset-not-found case.
fn release_lookup_failure_message(os: zed::Os, underlying: &str) -> String {
    format!(
        "Could not fetch an al-lsp release from https://github.com/{GITHUB_REPO}/releases \
         ({underlying}). If no release has been published yet, or your network blocks \
         GitHub, the language server cannot be downloaded automatically. {}",
        manual_install_hint(os)
    )
}

/// Deep-merge `overrides` into `base`, returning the merged result.
/// - Objects are merged recursively (override keys replace base keys)
/// - All other types: override replaces base entirely
/// - Recursion is capped at `MERGE_JSON_MAX_DEPTH`
///
/// NOTE: These are separate Cargo packages (WASM vs native) that can't share code
/// without a shared crate, which would add complexity for a small utility.
fn merge_json(base: &Value, overrides: &Value) -> Value {
    merge_json_inner(base, overrides, 0)
}

fn merge_json_inner(base: &Value, overrides: &Value, depth: u32) -> Value {
    if depth >= MERGE_JSON_MAX_DEPTH {
        // Stop recursing — user wins by default at deep nesting.
        return overrides.clone();
    }
    match (base, overrides) {
        (Value::Object(base_obj), Value::Object(override_obj)) => {
            let mut merged = base_obj.clone();
            for (key, override_value) in override_obj {
                let value = merged
                    .get(key)
                    .map(|base_value| merge_json_inner(base_value, override_value, depth + 1))
                    .unwrap_or_else(|| override_value.clone());
                merged.insert(key.clone(), value);
            }
            Value::Object(merged)
        }
        (_, override_value) => override_value.clone(),
    }
}

impl AlExtension {
    /// Resolve the al-lsp binary path using the priority chain from ISSUE-069:
    /// 1. User-configured explicit path (settings override)
    /// 2. Locally installed binary (extension work dir, previously downloaded)
    /// 3. PATH lookup (dev builds, `cargo install`, system installs)
    /// 4. GitHub release download → cache in work dir
    fn find_or_download_binary(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
        user_configured_path: Option<&str>,
    ) -> Result<String> {
        // 1. User-configured explicit path.
        if let Some(path) = user_configured_path {
            return Ok(path.to_string());
        }

        // 2. Previously downloaded binary still on disk.
        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
            self.cached_binary_path = None;
        }

        // 3. PATH lookup — works for dev builds and `cargo install`.
        if let Some(path) = worktree.which("al-lsp") {
            return Ok(path);
        }

        // 4. Download from GitHub releases.
        zed::set_language_server_installation_status(
            language_server_id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );

        let (os, arch) = zed::current_platform();

        let release = zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )
        .map_err(|e| release_lookup_failure_message(os, &e))?;

        // Release asset naming. Unix targets ship a gzip tarball
        // (`al-<os>-<arch>.tar.gz`); Windows ships a zip
        // (`al-windows-<arch>.zip`) because the Windows release packages only
        // the portable `al-lsp` binary (the Unix-only `al-explorer` daemon
        // client is not built for Windows — see the daemon module's
        // `#[cfg(not(unix))]` stub and ecosystem-roadmap.md item 8). These
        // strings MUST stay in lockstep with the asset names produced by
        // `.github/workflows/release.yml`; the `release_asset_names_match`
        // repo-consistency test guards against drift.
        let arch_name = match arch {
            zed::Architecture::Aarch64 => "aarch64",
            zed::Architecture::X86 => "x86",
            zed::Architecture::X8664 => "x86_64",
        };
        let (asset_name, archive_type) = match os {
            zed::Os::Mac => (
                format!("al-macos-{arch_name}.tar.gz"),
                zed::DownloadedFileType::GzipTar,
            ),
            zed::Os::Linux => (
                format!("al-linux-{arch_name}.tar.gz"),
                zed::DownloadedFileType::GzipTar,
            ),
            zed::Os::Windows => (
                format!("al-windows-{arch_name}.zip"),
                zed::DownloadedFileType::Zip,
            ),
        };

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| spawn_failure_message(os, &asset_name))?;

        // Defense-in-depth: validate the version string is plain alphanumeric/
        // dot/hyphen so a malicious or compromised release tag can't produce a
        // path that escapes the extension's work directory.
        if !is_safe_version(&release.version) {
            return Err(format!(
                "Rejected release version '{}': contains path-unsafe characters",
                release.version
            ));
        }
        let version_dir = format!("al-lsp-{}", release.version);
        let binary_name = match os {
            zed::Os::Windows => "al-lsp.exe",
            _ => "al-lsp",
        };
        let binary_path = format!("{version_dir}/{binary_name}");

        if !fs::metadata(&binary_path).is_ok_and(|m| m.is_file()) {
            zed::set_language_server_installation_status(
                language_server_id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );

            // TLS / integrity: zed::download_file uses Zed's host HTTP
            // client, which forces rustls-with-platform-verifier (validates
            // against the OS root-CA store). For an even stronger guarantee
            // we could SHA-verify against `asset.digest` returned by the
            // GitHub API, but the WASM extension has no easy hashing
            // primitive and trusting platform TLS is already strong. See
            // F-OPEN-008.
            zed::download_file(&asset.download_url, &version_dir, archive_type)
                .map_err(|e| format!("Failed to download al-lsp: {e}"))?;

            zed::make_file_executable(&binary_path)
                .map_err(|e| format!("Failed to make al-lsp executable: {e}"))?;

            // Remove old version directories, guarded to only clean up al-lsp-* dirs.
            // T055: `fs::remove_dir_all` errors are intentionally swallowed — the
            // Zed WASM extension sandbox has no usable logging path, and stale
            // version dirs are best-effort cleanup (a leftover dir is at worst
            // wasted disk space, never a correctness issue).
            if fs::metadata(&version_dir).is_ok_and(|m| m.is_dir()) {
                if let Ok(entries) = fs::read_dir(".") {
                    for entry in entries.flatten() {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        if name.starts_with("al-lsp-") && name != version_dir {
                            let _ = fs::remove_dir_all(entry.path());
                        }
                    }
                }
            }
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

impl zed::Extension for AlExtension {
    fn new() -> Self {
        Self {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;

        // User-configured binary arguments (e.g. ["--stdio"]).
        let user_args: Vec<String> = settings
            .binary
            .as_ref()
            .and_then(|b| b.arguments.as_ref())
            .map(|args| args.to_vec())
            .unwrap_or_else(|| vec!["--stdio".to_string()]);

        // Resolution chain (4 steps): user-configured path → cached download
        // → PATH lookup → GitHub release download. Step 1 is checked inside
        // find_or_download_binary; user_configured_path takes unconditional
        // priority over auto-discovery and downloads.
        let user_configured_path = settings
            .binary
            .as_ref()
            .and_then(|b| b.path.as_ref())
            .map(|p| p.to_string());

        let binary_path = self.find_or_download_binary(
            language_server_id,
            worktree,
            user_configured_path.as_deref(),
        )?;

        Ok(zed::Command {
            command: binary_path,
            args: user_args,
            env: vec![],
        })
    }

    fn language_server_initialization_options(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let workspace_path = worktree.root_path();

        // User overrides only — proxy provides defaults from package.json
        let mut user_config = json!({});
        let mut user_init_options: Option<&Value> = None;

        let lsp_settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree).ok();
        if let Some(ref s) = lsp_settings {
            if let Some(user_settings) = &s.settings {
                user_config = settings::apply_al_settings_to_config(&user_config, user_settings);
            }
            user_init_options = s.initialization_options.as_ref();
        }

        let mut init_options = json!({
            "workspacePath": workspace_path,
            "al": user_config
        });

        // Allow full initializationOptions override
        if let Some(user_init_opts) = user_init_options {
            init_options = merge_json(&init_options, user_init_opts);
        }

        Ok(Some(init_options))
    }

    fn language_server_workspace_configuration_schema(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Option<serde_json::Value> {
        let schema = include_str!("../schemas/settings.json");
        serde_json::from_str(schema).ok()
    }

    fn language_server_initialization_options_schema(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "workspacePath": {
                    "type": "string",
                    "description": "Path to the AL project root (auto-detected from workspace)"
                },
                "al": {
                    "type": "object",
                    "description": "AL language server settings (see workspace configuration schema for details)"
                }
            }
        }))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        // User overrides only — workspace root is already provided via rootUri in initialize
        let mut al_config = json!({});

        if let Ok(lsp_settings) = LspSettings::for_worktree(language_server_id.as_ref(), worktree) {
            if let Some(user_settings) = &lsp_settings.settings {
                al_config = settings::apply_al_settings_to_config(&al_config, user_settings);
            }
        }

        Ok(Some(json!({ "al": al_config })))
    }

    fn get_dap_binary(
        &mut self,
        language_server_id: String,
        config: zed::DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<zed::DebugAdapterBinary> {
        // Resolve al-lsp via the same 4-step chain used for LSP:
        // user config → cached download → PATH → GitHub release download.
        let lsp_id = zed::LanguageServerId::new(language_server_id);
        let al_lsp_path = self.find_or_download_binary(
            &lsp_id,
            worktree,
            user_provided_debug_adapter_path.as_deref(),
        )?;
        dap::build_dap_binary(config, al_lsp_path, worktree)
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: serde_json::Value,
    ) -> Result<zed::StartDebuggingRequestArgumentsRequest> {
        dap::dap_request_kind(config)
    }

    fn dap_config_to_scenario(&mut self, config: zed::DebugConfig) -> Result<zed::DebugScenario> {
        dap::dap_config_to_scenario(config)
    }

    fn dap_locator_create_scenario(
        &mut self,
        _locator_name: String,
        _build_task: zed::TaskTemplate,
        _resolved_label: String,
        _debug_adapter_name: String,
    ) -> Option<zed::DebugScenario> {
        // Do not auto-generate debug scenarios — server/instance values are project-specific
        // and cannot be guessed. Users should configure debug scenarios manually via
        // `.zed/debug.json` with their BC server address and instance name.
        None
    }
}

zed::register_extension!(AlExtension);
