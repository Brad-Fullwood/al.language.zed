//! Debug session dispatcher.

use crate::workspace::Workspace;
use al_protocol::jsonrpc::{Response, RpcError};

fn no_session(id: u64) -> Response {
    Response::error(
        id,
        al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
        "No active debug session",
    )
}

fn missing_cmd(id: u64, msg: &str) -> Response {
    Response::error(id, al_protocol::jsonrpc::error_codes::INVALID_PARAMS, msg)
}

/// Build a `BcDebugConfig` from a named (or default) configuration in the project's
/// debug config file (`.zed/debug.json` or `.vscode/launch.json`).
///
/// Fix #4: `al debug start` sends `{"cmd":"start","config":name}` which does not
/// include full DAP launch args. This function looks up the named config file entry
/// and constructs the `BcDebugConfig` from it instead of from `params` directly.
fn resolve_debug_config(
    workspace: &Workspace,
    params: &serde_json::Value,
) -> Option<crate::dap::bc_debug::BcDebugConfig> {
    use crate::dap::bc_debug::BcDebugConfig;
    use crate::launch::find_launch_config;

    let project_root = workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))?;

    let debug_file = find_launch_config(&project_root)?;

    let config_name = params.get("config").and_then(|v| v.as_str());

    // Select the requested config by name, or fall back to the first one.
    let bc_cfg = match config_name {
        Some(name) => debug_file
            .configs
            .iter()
            .find(|c| c.name == name)
            .or_else(|| debug_file.configs.first()),
        None => debug_file.configs.first(),
    }?;

    use crate::launch::{AuthMethod, EnvironmentType};

    let environment_type = match bc_cfg.environment_type {
        EnvironmentType::OnPrem => "OnPrem".to_string(),
        EnvironmentType::Sandbox => "Sandbox".to_string(),
        EnvironmentType::Production => "Production".to_string(),
    };
    let authentication = match bc_cfg.authentication {
        AuthMethod::Windows => "Windows".to_string(),
        AuthMethod::UserPassword => "UserPassword".to_string(),
        AuthMethod::AAD => "AAD".to_string(),
    };

    Some(BcDebugConfig {
        server: bc_cfg.server.clone(),
        server_instance: bc_cfg.server_instance.clone(),
        port: bc_cfg.port.unwrap_or(7049),
        tenant: bc_cfg
            .tenant
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        environment_type,
        environment_name: bc_cfg.environment_name.clone(),
        authentication,
        accept_invalid_certs: bc_cfg.accept_invalid_certs,
        ..BcDebugConfig::default()
    })
}

pub(super) async fn dispatch_debug(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use crate::dap::bc_debug::BcDebugConfig;
    use crate::native_debug::NativeDebugSession;
    use al_protocol::jsonrpc::error_codes;

    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return missing_cmd(id, "Missing 'cmd' in debug params"),
    };

    match cmd {
        "start" => {
            let access_token = params
                .get("accessToken")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            // Fix #4: look up config from project debug file if a config name is given,
            // or fall back to parsing full DAP args from params for backward compat.
            let config = if params.get("config").is_some() || params.get("server").is_none() {
                // Either a named config reference or no server specified — look up from file
                match resolve_debug_config(workspace, params) {
                    Some(c) => c,
                    None => {
                        return Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INVALID_PARAMS,
                                message: "No debug configuration found in project (.zed/debug.json or .vscode/launch.json)".to_string(),
                            }),
                        };
                    }
                }
            } else {
                // Full DAP args provided — parse directly (legacy / direct invocation)
                BcDebugConfig::from_dap_args(params)
            };

            match NativeDebugSession::start(config, access_token).await {
                Ok(session) => {
                    let session_id = session.session_id().to_string();
                    *workspace.debug_session.lock().await = Some(session);
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "start",
                            "status": "running",
                            "session": session_id,
                        })),
                        error: None,
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("Debug start failed: {e}"),
                    }),
                },
            }
        }

        "breakpoint" => {
            let file = match params.get("file").and_then(|v| v.as_str()) {
                Some(f) => f.to_string(),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'file' parameter".to_string(),
                        }),
                    };
                }
            };
            let line = params.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let condition = params
                .get("condition")
                .and_then(|v| v.as_str())
                .map(String::from);

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => {
                    let bps: Vec<(u32, Option<&str>)> = vec![(line, condition.as_deref())];
                    // object_type/object_id: use defaults (0) when not provided by caller
                    let obj_type = params
                        .get("objectType")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0) as i32;
                    let obj_id =
                        params.get("objectId").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                    match session.set_breakpoints(&file, &bps, obj_type, obj_id).await {
                        Ok(verified) => {
                            let bp_json: Vec<serde_json::Value> = verified
                                .iter()
                                .map(|bp| serde_json::to_value(bp).unwrap_or_default())
                                .collect();
                            Response {
                                id,
                                result: Some(serde_json::json!({
                                    "cmd": "breakpoint",
                                    "breakpoints": bp_json,
                                })),
                                error: None,
                            }
                        }
                        Err(e) => Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INTERNAL_ERROR,
                                message: format!("set_breakpoints failed: {e}"),
                            }),
                        },
                    }
                }
            }
        }

        "state" => {
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.state().await {
                    Ok(state) => Response {
                        id,
                        result: serde_json::to_value(state).ok(),
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("state() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "eval" => {
            let expr = match params.get("expr").and_then(|v| v.as_str()) {
                Some(e) => e.to_string(),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'expr' parameter".to_string(),
                        }),
                    };
                }
            };

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.eval(&expr).await {
                    Ok(eval_result) => Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "eval",
                            "result": eval_result.result,
                            "typeName": eval_result.type_name,
                        })),
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("eval() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "continue" => {
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.continue_exec().await {
                    Ok(state) => Response {
                        id,
                        result: serde_json::to_value(state).ok(),
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("continue() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "step" => {
            let step_type = params
                .get("stepType")
                .and_then(|v| v.as_str())
                .unwrap_or("over")
                .to_string();

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.step(&step_type).await {
                    Ok(state) => Response {
                        id,
                        result: serde_json::to_value(state).ok(),
                        error: None,
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("step() failed: {e}"),
                        }),
                    },
                },
            }
        }

        "history" => {
            let var_filter = params.get("var").and_then(|v| v.as_str()).map(String::from);

            let guard = workspace.debug_session.lock().await;
            match guard.as_ref() {
                None => no_session(id),
                Some(session) => {
                    let hits: Vec<serde_json::Value> = session
                        .history(var_filter.as_deref())
                        .iter()
                        .filter_map(|h| serde_json::to_value(h).ok())
                        .collect();
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "history",
                            "hits": hits,
                        })),
                        error: None,
                    }
                }
            }
        }

        "stop" => {
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => Response {
                    id,
                    result: Some(serde_json::json!({"cmd": "stop", "status": "stopped"})),
                    error: None,
                },
                Some(session) => {
                    let stop_result = session.stop().await;
                    *guard = None;
                    match stop_result {
                        Ok(()) => Response {
                            id,
                            result: Some(serde_json::json!({"cmd": "stop", "status": "stopped"})),
                            error: None,
                        },
                        Err(e) => Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INTERNAL_ERROR,
                                message: format!("stop() failed: {e}"),
                            }),
                        },
                    }
                }
            }
        }

        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown debug command: {other}"),
            }),
        },
    }
}
