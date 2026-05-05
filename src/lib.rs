mod dap;
mod discovery;
mod platform;
mod settings;

#[cfg(test)]
mod merge_json_test;
#[cfg(test)]
mod settings_test;

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use zed_extension_api::{self as zed, settings::LspSettings, Result};

const GITHUB_REPO: &str = "Brad-Fullwood/zed-al";

struct AlExtension {
    /// Path to a previously downloaded al-lsp binary in the extension work dir.
    /// Set after a successful GitHub release download; re-checked on each call.
    cached_binary_path: Option<String>,
}

/// Deep-merge `overrides` into `base`, returning the merged result.
/// - Objects are merged recursively (override keys replace base keys)
/// - All other types: override replaces base entirely
///
/// NOTE: These are separate Cargo packages (WASM vs native) that can't share code
/// without a shared crate, which would add complexity for a small utility.
fn merge_json(base: &Value, overrides: &Value) -> Value {
    match (base, overrides) {
        (Value::Object(base_obj), Value::Object(override_obj)) => {
            let mut merged = base_obj.clone();
            for (key, override_value) in override_obj {
                let value = merged
                    .get(key)
                    .map(|base_value| merge_json(base_value, override_value))
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

        let release = zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;

        let (os, arch) = zed::current_platform();
        let asset_name = format!(
            "al-{os}-{arch}.tar.gz",
            os = match os {
                zed::Os::Mac => "macos",
                zed::Os::Linux => "linux",
                zed::Os::Windows => "windows",
            },
            arch = match arch {
                zed::Architecture::Aarch64 => "aarch64",
                zed::Architecture::X86 => "x86",
                zed::Architecture::X8664 => "x86_64",
            },
        );

        let asset = release
            .assets
            .iter()
            .find(|a| a.name == asset_name)
            .ok_or_else(|| {
                format!(
                    "No al-lsp release asset found for this platform ({asset_name}). \
                     Set the binary path manually in Zed settings: \
                     {{\"lsp\": {{\"al-lsp\": {{\"binary\": {{\"path\": \"/path/to/al-lsp\"}}}}}}}}"
                )
            })?;

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

            zed::download_file(
                &asset.download_url,
                &version_dir,
                zed::DownloadedFileType::GzipTar,
            )
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
        let env_vec = worktree.shell_env();
        let env_map: HashMap<String, String> = env_vec.into_iter().collect();

        let settings = LspSettings::for_worktree(language_server_id.as_ref(), worktree)?;

        // User-configured binary arguments (e.g. ["--stdio"]).
        let user_args: Vec<String> = settings
            .binary
            .as_ref()
            .and_then(|b| b.arguments.as_ref())
            .map(|args| args.to_vec())
            .unwrap_or_else(|| vec!["--stdio".to_string()]);

        // Explicit binary path from settings takes unconditional priority.
        // This is the recommended configuration path — no auto-download or
        // discovery is attempted if a path is provided. Must be checked before
        // proxy discovery so users can override a stale legacy proxy install.
        let user_configured_path = settings
            .binary
            .as_ref()
            .and_then(|b| b.path.as_ref())
            .map(|p| p.to_string());

        if let Some(path) = user_configured_path.clone() {
            return Ok(zed::Command {
                command: path,
                args: user_args,
                env: vec![],
            });
        }

        // Check for bundled proxy binary at the installed extension path.
        // If found, use it (proxy discovers EditorServices.Host itself).
        if let Some(proxy_path) = discovery::find_proxy_path(&env_map) {
            // EditorServices.Host path for the proxy: PATH discovery > "auto".
            // NOTE: user_configured_path is for al-lsp, not EditorServices.Host.
            let al_server_path = worktree
                .which("Microsoft.Dynamics.Nav.EditorServices.Host")
                .unwrap_or_else(|| "auto".to_string());

            let mut proxy_args = vec![al_server_path];
            proxy_args.extend(user_args);

            return Ok(zed::Command {
                command: proxy_path,
                args: proxy_args,
                env: vec![],
            });
        }

        // No bundled proxy — resolve al-lsp via cached path, PATH, or GitHub download.
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
