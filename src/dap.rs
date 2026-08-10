use serde_json::{json, Value};
use zed_extension_api as zed;

/// Parse and validate a debug task's raw JSON configuration string. Pulled out
/// of `build_dap_binary` so its error paths (malformed JSON, non-object
/// config) are unit-testable without a `zed::Worktree` — a WASM host
/// resource that cannot be constructed outside a real extension host, so it
/// can never appear in a native `#[cfg(test)]` unit test.
fn parse_debug_task_config(raw: &str) -> zed::Result<Value> {
    let config_json: Value = serde_json::from_str(raw).map_err(|e| {
        format!(
            "AL DAP: failed to parse debug task configuration as JSON: {e}. \
             Check the launch.json entry for syntax errors."
        )
    })?;
    if !config_json.is_object() {
        return Err("AL DAP: debug task configuration must be a JSON object".to_string());
    }
    Ok(config_json)
}

/// Build the DAP binary configuration for the AL debug adapter.
///
/// `al_lsp_path` is the resolved binary path from the full resolution chain
/// (user config → in-memory session cache → PATH → on-disk cached download →
/// GitHub release download) performed by the caller.
/// al-lsp's `--dap` mode runs the native DAP server over stdio. Zed
/// communicates with it using the DAP protocol for breakpoints, stepping, etc.
///
/// `user_settings` is the `lsp."al-lsp".settings` object. The native adapter
/// (`--dap`) is the default; `al.useOfficialDap: true` opts into Microsoft's
/// EditorServices.Host proxy (`--dap-legacy`) - see `resolve_dap_backend_flag`.
pub fn build_dap_binary(
    config: zed::DebugTaskDefinition,
    al_lsp_path: String,
    worktree: &zed::Worktree,
    user_settings: Option<&Value>,
) -> zed::Result<zed::DebugAdapterBinary> {
    let config_json = parse_debug_task_config(&config.config)?;
    let request = dap_request_kind(config_json)?;

    let workspace_path = worktree.root_path();

    let backend_flag = crate::settings::resolve_dap_backend_flag(user_settings);
    // `al-lsp --dap`/`--dap-legacy` take no launch-config CLI flags: the
    // native adapter reads its project root from the process `cwd` (set
    // below) and all BC connection/launch settings (`server`, `launchBrowser`,
    // etc.) arrive over the DAP protocol itself, from `request_args` /
    // `config.config` — see `BcDebugConfig::from_dap_args`. `/projectRoot:`
    // is kept only because `cwd` already makes it redundant, not load-bearing;
    // there used to be `/server:`/`/browser:` flags here too, but al-lsp never
    // parsed them (and `"browser"` was never even a real schema key — the real
    // one is `launchBrowser`), so they were pure dead plumbing. Removed.
    let args = vec![
        backend_flag.to_string(),
        format!("/projectRoot:{}", workspace_path),
    ];

    let mut envs = Vec::new();
    let compile_settings = crate::settings::compile_settings_for_child(user_settings);
    if compile_settings
        .as_object()
        .is_some_and(|settings| !settings.is_empty())
    {
        envs.push((
            "AL_DAP_SETTINGS_JSON".to_string(),
            serde_json::to_string(&compile_settings)
                .map_err(|error| format!("AL DAP: cannot serialize compile settings: {error}"))?,
        ));
    }
    if let Some(path) = crate::settings::resolve_dotnet_path(user_settings) {
        envs.push(("AL_DOTNET_PATH".to_string(), path));
    }

    Ok(zed::DebugAdapterBinary {
        command: Some(al_lsp_path),
        arguments: args,
        envs,
        cwd: Some(workspace_path.to_string()),
        connection: None,
        request_args: zed::StartDebuggingRequestArguments {
            configuration: config.config,
            request,
        },
    })
}

pub fn dap_request_kind(config: Value) -> zed::Result<zed::StartDebuggingRequestArgumentsRequest> {
    let request = config
        .get("request")
        .and_then(|r| r.as_str())
        .unwrap_or("launch");

    match request {
        "attach" => Ok(zed::StartDebuggingRequestArgumentsRequest::Attach),
        "launch" => Ok(zed::StartDebuggingRequestArgumentsRequest::Launch),
        other => Err(format!("AL DAP: unsupported request type `{other}`")),
    }
}

pub fn dap_config_to_scenario(config: zed::DebugConfig) -> zed::Result<zed::DebugScenario> {
    let mut al_config = serde_json::Map::new();

    // `adapter` and `label` are the schema's *required* properties
    // (debug_adapter_schemas/al.json) — `type`/`name`/`projectDir` are not
    // declared there at all (and `projectDir` is never parsed by
    // `BcDebugConfig::from_dap_args`), so a scenario built from those instead
    // failed schema validation on the "new debug session" flow.
    al_config.insert("adapter".to_string(), json!("al"));
    al_config.insert("label".to_string(), json!(config.label));

    match &config.request {
        zed::DebugRequest::Launch(launch) => {
            al_config.insert("request".to_string(), json!("launch"));
            if !launch.program.is_empty() {
                al_config.insert("server".to_string(), json!(launch.program));
            }
            // Launch-only defaults — not applicable when attaching to a running session.
            al_config.insert("breakOnError".to_string(), json!(true));
            al_config.insert("launchBrowser".to_string(), json!(true));
            al_config.insert("authentication".to_string(), json!("MicrosoftEntraID"));
            al_config.insert("environmentType".to_string(), json!("OnPrem"));
        }
        zed::DebugRequest::Attach(_) => {
            al_config.insert("request".to_string(), json!("attach"));
        }
    }

    // The native launch request already performs the verified build and atomic
    // artifact selection before publishing. Adding a shell build task here
    // duplicated that work and, on fresh gallery installs, referenced a bare
    // `al-explorer` that was not on the user's PATH. Attach never builds
    // either, so both request kinds intentionally leave `build` empty.
    let build = None;

    Ok(zed::DebugScenario {
        label: config.label,
        adapter: "al".to_string(),
        build,
        config: Value::Object(al_config).to_string(),
        tcp_connection: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── dap_request_kind ────────────────────────────────────────────────

    #[test]
    fn dap_request_kind_defaults_to_launch_when_absent() {
        assert_eq!(
            dap_request_kind(json!({})).unwrap(),
            zed::StartDebuggingRequestArgumentsRequest::Launch
        );
    }

    #[test]
    fn dap_request_kind_maps_launch_and_attach() {
        assert_eq!(
            dap_request_kind(json!({ "request": "launch" })).unwrap(),
            zed::StartDebuggingRequestArgumentsRequest::Launch
        );
        assert_eq!(
            dap_request_kind(json!({ "request": "attach" })).unwrap(),
            zed::StartDebuggingRequestArgumentsRequest::Attach
        );
    }

    #[test]
    fn dap_request_kind_rejects_unknown_request_type() {
        let error = dap_request_kind(json!({ "request": "snapshot" })).unwrap_err();
        assert!(
            error.contains("unsupported request type") && error.contains("snapshot"),
            "unexpected error: {error}"
        );
    }

    // ── parse_debug_task_config / build_dap_binary error paths ─────────
    //
    // build_dap_binary itself needs a `zed::Worktree`, a WASM host resource
    // that cannot be constructed in a native unit test, so its error paths
    // are exercised through the pure helper it delegates to first.

    #[test]
    fn parse_debug_task_config_rejects_malformed_json() {
        let error = parse_debug_task_config("{not json").unwrap_err();
        assert!(
            error.contains("failed to parse debug task configuration as JSON"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_debug_task_config_rejects_non_object_json() {
        let error = parse_debug_task_config("[1, 2, 3]").unwrap_err();
        assert_eq!(
            error,
            "AL DAP: debug task configuration must be a JSON object"
        );
    }

    #[test]
    fn parse_debug_task_config_accepts_a_well_formed_object() {
        let config = parse_debug_task_config(r#"{"request": "launch"}"#).unwrap();
        assert!(config.is_object());
        assert_eq!(
            dap_request_kind(config).unwrap(),
            zed::StartDebuggingRequestArgumentsRequest::Launch
        );
    }

    // ── dap_config_to_scenario ───────────────────────────────────────────

    fn launch_config(label: &str, program: &str) -> zed::DebugConfig {
        zed::DebugConfig {
            label: label.to_string(),
            adapter: "al".to_string(),
            request: zed::DebugRequest::Launch(zed::LaunchRequest {
                program: program.to_string(),
                cwd: None,
                args: Vec::new(),
                envs: Vec::new(),
            }),
            stop_on_entry: None,
        }
    }

    fn attach_config(label: &str) -> zed::DebugConfig {
        zed::DebugConfig {
            label: label.to_string(),
            adapter: "al".to_string(),
            request: zed::DebugRequest::Attach(zed::AttachRequest { process_id: None }),
            stop_on_entry: None,
        }
    }

    /// Every key the scenario emits must be one the shipped debug schema
    /// actually declares — this is what regressed when the scenario emitted
    /// `type`/`name`/`projectDir` instead of the schema's required
    /// `adapter`/`label`.
    fn assert_scenario_validates_against_schema(scenario: &zed::DebugScenario) {
        let schema: Value =
            serde_json::from_str(include_str!("../debug_adapter_schemas/al.json")).unwrap();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let known_keys: std::collections::BTreeSet<&str> = schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();

        let config: Value = serde_json::from_str(&scenario.config).unwrap();
        let config_obj = config.as_object().unwrap();

        for key in &required {
            assert!(
                config_obj.contains_key(*key),
                "scenario config is missing schema-required key {key:?}: {}",
                scenario.config
            );
        }
        for key in config_obj.keys() {
            assert!(
                known_keys.contains(key.as_str()),
                "scenario config emits {key:?}, which debug_adapter_schemas/al.json does not declare: {}",
                scenario.config
            );
        }
    }

    #[test]
    fn dap_config_to_scenario_launch_validates_against_schema() {
        let scenario =
            dap_config_to_scenario(launch_config("My Session", "http://bcserver")).unwrap();
        assert_eq!(scenario.label, "My Session");
        assert_eq!(scenario.adapter, "al");
        assert!(scenario.build.is_none());
        assert_scenario_validates_against_schema(&scenario);

        let config: Value = serde_json::from_str(&scenario.config).unwrap();
        assert_eq!(config["adapter"], json!("al"));
        assert_eq!(config["label"], json!("My Session"));
        assert_eq!(config["request"], json!("launch"));
        assert_eq!(config["server"], json!("http://bcserver"));
        // Dropped dead keys: neither is declared by the schema.
        assert!(config.get("type").is_none());
        assert!(config.get("name").is_none());
        assert!(config.get("projectDir").is_none());
    }

    #[test]
    fn dap_config_to_scenario_launch_omits_server_when_program_is_empty() {
        let scenario = dap_config_to_scenario(launch_config("Empty Program", "")).unwrap();
        let config: Value = serde_json::from_str(&scenario.config).unwrap();
        assert!(config.get("server").is_none());
    }

    #[test]
    fn dap_config_to_scenario_attach_validates_against_schema() {
        let scenario = dap_config_to_scenario(attach_config("Attach Session")).unwrap();
        assert_eq!(scenario.label, "Attach Session");
        assert!(scenario.build.is_none());
        assert_scenario_validates_against_schema(&scenario);

        let config: Value = serde_json::from_str(&scenario.config).unwrap();
        assert_eq!(config["adapter"], json!("al"));
        assert_eq!(config["request"], json!("attach"));
        // Launch-only defaults must not leak into an attach scenario.
        assert!(config.get("launchBrowser").is_none());
        assert!(config.get("breakOnError").is_none());
    }
}
