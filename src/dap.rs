use serde_json::{json, Value};
use zed_extension_api as zed;

/// Build the DAP binary configuration for the AL debug adapter.
///
/// `al_lsp_path` is the resolved binary path from the full 4-step resolution
/// chain (user config → cache → PATH → download) performed by the caller.
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
    let workspace_path = worktree.root_path();

    let config_json: Value = serde_json::from_str(&config.config).map_err(|e| {
        format!(
            "AL DAP: failed to parse debug task configuration as JSON: {e}. \
             Check the launch.json entry for syntax errors."
        )
    })?;
    if !config_json.is_object() {
        return Err("AL DAP: debug task configuration must be a JSON object".to_string());
    }

    let request_type = config_json
        .get("request")
        .and_then(|r| r.as_str())
        .unwrap_or("launch");

    let request = match request_type {
        "attach" => zed::StartDebuggingRequestArgumentsRequest::Attach,
        "launch" => zed::StartDebuggingRequestArgumentsRequest::Launch,
        other => return Err(format!("AL DAP: unsupported request type `{other}`")),
    };

    let backend_flag = crate::settings::resolve_dap_backend_flag(user_settings);
    let mut args = vec![
        backend_flag.to_string(),
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

    al_config.insert("type".to_string(), json!("al"));
    al_config.insert("name".to_string(), json!(config.label));

    let is_attach = matches!(config.request, zed::DebugRequest::Attach(_));

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

    // Attach configurations connect to an existing BC session and must not
    // compile or publish the project first.
    let build = if is_attach {
        None
    } else {
        Some(zed::BuildTaskDefinition::Template(
            zed::BuildTaskDefinitionTemplatePayload {
                locator_name: None,
                template: zed::BuildTaskTemplate {
                    label: "AL: Compile".to_string(),
                    command: "al-explorer".to_string(),
                    args: vec!["compile".to_string()],
                    env: Default::default(),
                    cwd: None,
                },
            },
        ))
    };

    Ok(zed::DebugScenario {
        label: config.label,
        adapter: "al".to_string(),
        build,
        config: Value::Object(al_config).to_string(),
        tcp_connection: None,
    })
}
