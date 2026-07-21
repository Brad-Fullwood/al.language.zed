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

/// Repository that publishes the extension's `al-lsp` release assets.
const GITHUB_REPO: &str = "Brad-Fullwood/al.language.zed";

struct AlExtension {
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

fn manual_install_hint(os: zed::Os) -> String {
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

fn spawn_failure_message(os: zed::Os, asset_name: &str) -> String {
    format!(
        "Could not find an al-lsp release asset named '{asset_name}' for this platform. {}",
        manual_install_hint(os)
    )
}

fn release_lookup_failure_message(os: zed::Os, underlying: &str) -> String {
    format!(
        "Could not fetch an al-lsp release from https://github.com/{GITHUB_REPO}/releases \
         ({underlying}). If no release has been published yet, or your network blocks \
         GitHub, the language server cannot be downloaded automatically. {}",
        manual_install_hint(os)
    )
}

#[cfg_attr(not(zed_api_0_8), allow(dead_code))]
fn al_settings_schema() -> Option<Value> {
    Some(
        serde_json::from_str(include_str!("../schemas/settings.json"))
            .expect("bundled AL settings schema must be valid JSON"),
    )
}

/// Deep-merge `overrides` into `base`, returning the merged result.
fn merge_json(base: &Value, overrides: &Value) -> Value {
    merge_json_inner(base, overrides, 0)
}

fn merge_json_inner(base: &Value, overrides: &Value, depth: u32) -> Value {
    if depth >= MERGE_JSON_MAX_DEPTH {
        return overrides.clone();
    }
    match (base, overrides) {
        (Value::Object(base_obj), Value::Object(override_obj)) => {
            let mut merged = base_obj.clone();
            for (key, override_value) in override_obj {
                if let Some(slot) = merged.get_mut(key) {
                    *slot = merge_json_inner(slot, override_value, depth + 1);
                } else {
                    merged.insert(key.clone(), override_value.clone());
                }
            }
            Value::Object(merged)
        }
        (_, override_value) => override_value.clone(),
    }
}

impl AlExtension {
    /// Resolves an explicit, cached, installed, or downloadable `al-lsp` binary.
    fn find_or_download_binary(
        &mut self,
        status_id: Option<&zed::LanguageServerId>,
        worktree: &zed::Worktree,
        user_configured_path: Option<&str>,
    ) -> Result<String> {
        if let Some(path) = user_configured_path {
            return Ok(path.to_string());
        }

        if let Some(path) = &self.cached_binary_path {
            if fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
            self.cached_binary_path = None;
        }

        if let Some(path) = worktree.which("al-lsp") {
            return Ok(path);
        }

        if let Some(id) = status_id {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::CheckingForUpdate,
            );
        }

        let (os, arch) = zed::current_platform();

        let release = zed::latest_github_release(
            GITHUB_REPO,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )
        .map_err(|e| release_lookup_failure_message(os, &e))?;

        let arch_name = match arch {
            zed::Architecture::Aarch64 => "aarch64",
            #[cfg(not(zed_api_0_8))]
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
            if let Some(id) = status_id {
                zed::set_language_server_installation_status(
                    id,
                    &zed::LanguageServerInstallationStatus::Downloading,
                );
            }

            zed::download_file(&asset.download_url, &version_dir, archive_type)
                .map_err(|e| format!("Failed to download al-lsp: {e}"))?;

            zed::make_file_executable(&binary_path)
                .map_err(|e| format!("Failed to make al-lsp executable: {e}"))?;

            // Cleanup failure only leaves an obsolete cached release.
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

        // Explicit binary arguments take precedence over the backend setting.
        let user_args = settings::resolve_server_args(
            settings
                .binary
                .as_ref()
                .and_then(|b| b.arguments.as_ref())
                .map(|args| args.to_vec()),
            settings.settings.as_ref(),
        );

        let user_configured_path = settings
            .binary
            .as_ref()
            .and_then(|b| b.path.as_ref())
            .map(|p| p.to_string());

        let binary_path = self.find_or_download_binary(
            Some(language_server_id),
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

        if let Some(user_init_opts) = user_init_options {
            init_options = merge_json(&init_options, user_init_opts);
        }

        Ok(Some(init_options))
    }

    // `language_server_workspace_configuration_schema` and
    // `language_server_initialization_options_schema` drive settings.json
    // autocomplete + validation for the AL settings block. They exist only on
    // the 0.8.x+ extension API, so they are gated behind the `zed_api_0_8` cfg
    // that `build.rs` sets when Cargo.lock resolves zed_extension_api to 0.8.0+
    // (via `scripts/use-api.sh dev`). On the committed/published 0.7.0 build
    // (loads on Stable Zed, accepted by the registry) the cfg is off
    // and these compile out — same artifact as before. Schema content lives in
    // schemas/settings.json; see `al_settings_schema`.
    #[cfg(zed_api_0_8)]
    fn language_server_workspace_configuration_schema(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Option<serde_json::Value> {
        al_settings_schema()
    }

    #[cfg(zed_api_0_8)]
    fn language_server_initialization_options_schema(
        &mut self,
        _language_server_id: &zed::LanguageServerId,
        _worktree: &zed::Worktree,
    ) -> Option<serde_json::Value> {
        // initialization_options is `{ "workspacePath": …, "al": <settings> }`
        // (see language_server_initialization_options), so the AL settings
        // schema is nested under the "al" property.
        al_settings_schema().map(|schema| {
            json!({
                "type": "object",
                "properties": { "al": schema }
            })
        })
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

    fn context_server_command(
        &mut self,
        _context_server_id: &zed::ContextServerId,
        _project: &zed::Project,
    ) -> Result<zed::Command> {
        // The MCP server reuses the al-lsp binary (`al-lsp mcp`). The
        // context-server API hands us a Project (not a Worktree), so the
        // 4-step download chain isn't available here — PATH is the contract
        // (set up by `make install`), with an actionable error otherwise.
        Ok(zed::Command {
            command: "al-lsp".to_string(),
            args: vec!["mcp".to_string()],
            env: vec![],
        })
    }

    fn get_dap_binary(
        &mut self,
        _adapter_name: String,
        config: zed::DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<zed::DebugAdapterBinary> {
        // Resolve al-lsp via the same 4-step chain used for LSP:
        // user config → cached download → PATH → GitHub release download.
        // No LanguageServerId exists on the DAP path (and the released API has
        // no way to construct one), so download progress is not surfaced in
        // the status UI — see find_or_download_binary's status_id doc.
        let al_lsp_path = self.find_or_download_binary(
            None,
            worktree,
            user_provided_debug_adapter_path.as_deref(),
        )?;

        // Read the same lsp."al-lsp".settings block the LSP uses so the
        // al.useOfficialDap toggle lives alongside al.useOfficialLsp. The DAP
        // path has no LanguageServerId, so key the lookup on the server id.
        let lsp_settings = LspSettings::for_worktree("al-lsp", worktree).ok();
        let user_settings = lsp_settings.as_ref().and_then(|s| s.settings.as_ref());
        dap::build_dap_binary(config, al_lsp_path, worktree, user_settings)
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
