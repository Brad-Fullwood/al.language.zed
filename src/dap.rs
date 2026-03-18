use serde_json::{json, Value};
use zed_extension_api as zed;

/// Build the DAP binary configuration for the AL debug adapter.
///
/// al-lsp's `--dap` mode runs the DAP server over stdio. Zed communicates
/// with it using the DAP protocol for breakpoints, stepping, etc.
pub fn get_dap_binary(
    config: zed::DebugTaskDefinition,
    user_provided_debug_adapter_path: Option<String>,
    worktree: &zed::Worktree,
) -> zed::Result<zed::DebugAdapterBinary> {
    let workspace_path = worktree.root_path();

    let config_json: Value =
        serde_json::from_str(&config.config).unwrap_or_else(|_| json!({}));

    let request_type = config_json
        .get("request")
        .and_then(|r| r.as_str())
        .unwrap_or("launch");

    let request = if request_type == "attach" {
        zed::StartDebuggingRequestArgumentsRequest::Attach
    } else {
        zed::StartDebuggingRequestArgumentsRequest::Launch
    };

    // Find al-lsp binary: user-provided path > PATH lookup
    let al_lsp_path = if let Some(path) = user_provided_debug_adapter_path {
        path
    } else if let Some(path) = worktree.which("al-lsp") {
        path
    } else {
        return Err(
            "al-lsp not found. Install al-lsp or set the debug adapter path in Zed settings."
                .to_string(),
        );
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
        envs: vec![("AL_DAP_CAPTURE".to_string(), "/tmp/dap-capture.log".to_string())],
        cwd: Some(workspace_path.to_string()),
        connection: None,
        request_args: zed::StartDebuggingRequestArguments {
            configuration: config.config,
            request,
        },
    })
}

/// Determine the DAP request kind (launch or attach) from config.
pub fn dap_request_kind(
    config: Value,
) -> zed::Result<zed::StartDebuggingRequestArgumentsRequest> {
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
        }
        zed::DebugRequest::Attach(_) => {
            al_config.insert("request".to_string(), json!("attach"));
        }
    }

    al_config.insert("breakOnError".to_string(), json!(true));
    al_config.insert("launchBrowser".to_string(), json!(true));
    al_config.insert("authentication".to_string(), json!("UserPassword"));
    al_config.insert("environmentType".to_string(), json!("OnPrem"));

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
