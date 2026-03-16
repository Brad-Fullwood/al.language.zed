mod dap;
mod discovery;
mod platform;
mod settings;

use std::collections::HashMap;
use zed_extension_api::{self as zed, settings::LspSettings, Result};
use serde_json::{json, Value};

struct AlExtension;

/// Deep-merge `overrides` into `base`, returning the merged result.
/// - Objects are merged recursively (override keys replace base keys)
/// - All other types: override replaces base entirely
///
/// NOTE: These are separate Cargo packages (WASM vs native) that can't share code
/// without a shared crate, which would add complexity for a 16-line utility.
fn merge_json(base: &Value, overrides: &Value) -> Value {
    match (base, overrides) {
        (Value::Object(base_map), Value::Object(override_map)) => {
            let mut merged = base_map.clone();
            for (key, override_val) in override_map {
                let merged_val = match merged.get(key) {
                    Some(base_val) => merge_json(base_val, override_val),
                    None => override_val.clone(),
                };
                merged.insert(key.clone(), merged_val);
            }
            Value::Object(merged)
        }
        (_, override_val) => override_val.clone(),
    }
}

impl zed::Extension for AlExtension {
    fn new() -> Self {
        Self
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
            .map(|args| args.iter().cloned().collect())
            .unwrap_or_else(|| vec!["--stdio".to_string()]);

        // Explicit binary path from settings takes unconditional priority.
        // This is the recommended configuration path — no auto-download or
        // discovery is attempted if a path is provided.
        let user_configured_path = settings
            .binary
            .as_ref()
            .and_then(|b| b.path.as_ref())
            .map(|p| p.to_string());

        // Determine the AL EditorServices.Host path (passed as first arg to the proxy).
        // Priority: user setting > PATH discovery > "auto" (proxy's own discovery).
        let al_server_path = user_configured_path
            .clone()
            .or_else(|| worktree.which("Microsoft.Dynamics.Nav.EditorServices.Host"))
            .unwrap_or_else(|| "auto".to_string());

        // Find the bundled proxy binary (existence-checked).
        // If the user supplied an explicit binary path we skip proxy discovery entirely
        // and treat the configured path as the proxy itself.
        if let Some(proxy_path) = discovery::find_proxy_path(&env_map) {
            let mut proxy_args = vec![al_server_path];
            proxy_args.extend(user_args);

            return Ok(zed::Command {
                command: proxy_path,
                args: proxy_args,
                env: vec![("AL_LSP_DEBUG".to_string(), "1".to_string())],
            });
        }

        // Proxy binary was not found at the installed extension path.
        // If the user configured an explicit binary path, try to use it directly —
        // it may point to a standalone al-lsp binary.
        if let Some(explicit_path) = user_configured_path {
            return Ok(zed::Command {
                command: explicit_path,
                args: user_args,
                env: vec![("AL_LSP_DEBUG".to_string(), "1".to_string())],
            });
        }

        // Neither bundled proxy nor user-configured path is available.
        let platform = platform::detect_platform(&env_map);
        let expected_path = if let Some(home) = env_map.get("HOME") {
            let base = platform.extensions_base(home);
            format!("{}/al.language.zed/bin/{}/{}", base, platform.bin_dir(), platform.binary_name())
        } else {
            "<HOME not set>".to_string()
        };

        Err(format!(
            "AL Language Server proxy not found.\n\
            \n\
            The proxy binary was not found at the expected extension path:\n\
            {expected_path}\n\
            \n\
            To fix: set the binary path explicitly in Zed settings:\n\
            {{\n\
              \"lsp\": {{\n\
                \"al-language-server\": {{\n\
                  \"binary\": {{\n\
                    \"path\": \"/path/to/al-lsp\"\n\
                  }}\n\
                }}\n\
              }}\n\
            }}\n\
            \n\
            Debug info: shell_env entries={}, HOME={:?}, platform={:?}",
            env_map.len(),
            env_map.get("HOME"),
            platform,
        ))
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
            "alResourceConfigurationSettings": user_config,
            "setActiveWorkspace": true,
            "dependencyParentWorkspacePath": null,
            "expectedProjectReferenceDefinitions": [],
            "activeWorkspaceClosure": {}
        });

        // Allow full initializationOptions override
        if let Some(user_init_opts) = user_init_options {
            init_options = merge_json(&init_options, user_init_opts);
        }

        Ok(Some(init_options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        language_server_id: &zed::LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<serde_json::Value>> {
        let workspace_path = worktree.root_path();

        // User overrides only — proxy provides defaults from package.json
        let mut al_config = json!({
            "workspaceRootPath": workspace_path
        });

        if let Ok(lsp_settings) = LspSettings::for_worktree(language_server_id.as_ref(), worktree) {
            if let Some(user_settings) = &lsp_settings.settings {
                al_config = settings::apply_al_settings_to_config(&al_config, user_settings);
            }
        }

        Ok(Some(json!({ "al": al_config })))
    }

    fn get_dap_binary(
        &mut self,
        _adapter_name: String,
        config: zed::DebugTaskDefinition,
        user_provided_debug_adapter_path: Option<String>,
        worktree: &zed::Worktree,
    ) -> Result<zed::DebugAdapterBinary> {
        dap::get_dap_binary(config, user_provided_debug_adapter_path, worktree)
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        config: serde_json::Value,
    ) -> Result<zed::StartDebuggingRequestArgumentsRequest> {
        dap::dap_request_kind(config)
    }

    fn dap_config_to_scenario(
        &mut self,
        config: zed::DebugConfig,
    ) -> Result<zed::DebugScenario> {
        dap::dap_config_to_scenario(config)
    }
}

zed::register_extension!(AlExtension);
