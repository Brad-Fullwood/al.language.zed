use serde_json::{json, Value};
use zed_extension_api as zed;

/// Build the DAP binary configuration for the AL debug adapter.
///
/// `al_lsp_path` is the resolved binary path from the full 4-step resolution
/// chain (user config → cache → PATH → download) performed by the caller.
/// al-lsp's `--dap` mode runs the DAP server over stdio. Zed communicates
/// with it using the DAP protocol for breakpoints, stepping, etc.
pub fn build_dap_binary(
    config: zed::DebugTaskDefinition,
    al_lsp_path: String,
    worktree: &zed::Worktree,
) -> zed::Result<zed::DebugAdapterBinary> {
    let workspace_path = worktree.root_path();

    // Silently defaulting to {} on a parse failure was masking malformed
    // launch.json from the user (T073). The WASM extension can't `tracing::`
    // — it has no host I/O — but it CAN return a structured error so Zed
    // surfaces a real diagnostic in the debugger panel instead of starting
    // up against an empty config and producing confusing downstream errors.
    let config_json: Value = match serde_json::from_str(&config.config) {
        Ok(v) => v,
        Err(e) => {
            return Err(format!(
                "AL DAP: failed to parse debug task configuration as JSON: {e}. \
                 Check the launch.json entry for syntax errors."
            ));
        }
    };
    // If the parsed JSON isn't an object we still want to keep going (an
    // empty Object is a valid no-customisation case), but flatten any
    // unexpected shape to {} rather than letting `.get(...)` accesses
    // misbehave on an array/string root.
    let config_json = if config_json.is_object() {
        config_json
    } else {
        json!({})
    };

    let request_type = config_json
        .get("request")
        .and_then(|r| r.as_str())
        .unwrap_or("launch");

    let request = if request_type == "attach" {
        zed::StartDebuggingRequestArgumentsRequest::Attach
    } else {
        zed::StartDebuggingRequestArgumentsRequest::Launch
    };

    let mut args = vec![
        "--dap".to_string(),
        format!("/projectRoot:{}", workspace_path),
    ];

    if let Some(server) = config_json.get("server").and_then(|s| s.as_str()) {
        args.push(format!("/server:{}", server));
    }
    if let Some(browser) = config_json.get("browser").and_then(|b| b.as_str()) {
        args.push(format!("/browser:{}", browser));
    }

    Ok(zed::DebugAdapterBinary {
        command: Some(al_lsp_path),
        arguments: args,
        envs: vec![],
        cwd: Some(workspace_path.to_string()),
        connection: None,
        request_args: zed::StartDebuggingRequestArguments {
            configuration: config.config,
            request,
        },
    })
}

/// Determine the DAP request kind (launch or attach) from config.
pub fn dap_request_kind(config: Value) -> zed::Result<zed::StartDebuggingRequestArgumentsRequest> {
    let request = config
        .get("request")
        .and_then(|r| r.as_str())
        .unwrap_or("launch");

    match request {
        "attach" => Ok(zed::StartDebuggingRequestArgumentsRequest::Attach),
        _ => Ok(zed::StartDebuggingRequestArgumentsRequest::Launch),
    }
}

/// Convert a DAP config to a debug scenario.
pub fn dap_config_to_scenario(config: zed::DebugConfig) -> zed::Result<zed::DebugScenario> {
    let mut al_config = serde_json::Map::new();

    al_config.insert("type".to_string(), json!("al"));
    al_config.insert("name".to_string(), json!(config.label));

    match &config.request {
        zed::DebugRequest::Launch(launch) => {
            al_config.insert("request".to_string(), json!("launch"));
            if !launch.program.is_empty() {
                al_config.insert("server".to_string(), json!(launch.program));
            }
            if let Some(cwd) = &launch.cwd {
                al_config.insert("projectDir".to_string(), json!(cwd));
            }
            // Launch-only defaults — not applicable when attaching to a running session.
            al_config.insert("breakOnError".to_string(), json!(true));
            al_config.insert("launchBrowser".to_string(), json!(true));
            al_config.insert("authentication".to_string(), json!("UserPassword"));
            al_config.insert("environmentType".to_string(), json!("OnPrem"));
        }
        zed::DebugRequest::Attach(_) => {
            al_config.insert("request".to_string(), json!("attach"));
        }
    }

    Ok(zed::DebugScenario {
        label: config.label,
        adapter: "al".to_string(),
        build: Some(zed::BuildTaskDefinition::Template(
            zed::BuildTaskDefinitionTemplatePayload {
                locator_name: None,
                template: zed::BuildTaskTemplate {
                    label: "AL: Compile".to_string(),
                    command: "al".to_string(),
                    args: vec!["compile".to_string()],
                    env: Default::default(),
                    cwd: None,
                },
            },
        )),
        config: Value::Object(al_config).to_string(),
        tcp_connection: None,
    })
}
