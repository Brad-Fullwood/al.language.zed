//! Debug session dispatcher.

use al_core::workspace::Workspace;
use al_core::jsonrpc::{Response, RpcError};

fn no_session(id: u64) -> Response {
    Response::error(id, al_core::jsonrpc::error_codes::INTERNAL_ERROR, "No active debug session")
}

fn missing_cmd(id: u64, msg: &str) -> Response {
    Response::error(id, al_core::jsonrpc::error_codes::INVALID_PARAMS, msg)
}

pub(super) async fn dispatch_debug(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_core::native_debug::NativeDebugSession;
    use al_core::jsonrpc::error_codes;

    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return missing_cmd(id, "Missing 'cmd' in debug params"),
    };

    match cmd {
        "start" => {
            let config_name = params.get("config").and_then(|v| v.as_str());

            // Resolve launch config from project
            let project_root = match workspace.project.read().await.as_ref().map(|p| p.root.clone()) {
                Some(root) => root,
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: "No project loaded".to_string(),
                        }),
                    };
                }
            };

            let launch_file = al_core::launch::find_launch_config(&project_root);
            let bc_config = match resolve_debug_config(launch_file.as_ref(), config_name) {
                Ok(cfg) => cfg,
                Err(e) => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("Config error: {e}"),
                        }),
                    };
                }
            };

            // Acquire OAuth token
            let tenant = bc_config.tenant.clone();
            let http = reqwest::Client::new();
            let token = match al_core::symbols::oauth::acquire_token(&http, &tenant, |msg| {
                tracing::info!("{msg}");
            }).await {
                Ok(t) => t,
                Err(e) => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("Token acquisition failed: {e}"),
                        }),
                    };
                }
            };

            match NativeDebugSession::start(bc_config, &token).await {
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
            let condition = params.get("condition").and_then(|v| v.as_str()).map(String::from);

            // Resolve AL object type + ID from workspace file index
            let file_path = std::path::PathBuf::from(&file);
            let (object_type, object_id) = match workspace.file_index.object_info.get(&file_path) {
                Some(info) => (kind_to_object_type(&info.kind), info.id.unwrap_or(-1) as i32),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("Cannot resolve object type for file: {file}"),
                        }),
                    };
                }
            };

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => {
                    let bps: Vec<(u32, Option<&str>)> = vec![(line, condition.as_deref())];
                    match session.set_breakpoints(&file, &bps, object_type, object_id).await {
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
            let var_filter = params
                .get("var")
                .and_then(|v| v.as_str())
                .map(String::from);

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

/// Convert an AL object kind string to BC's ObjectTypeWrapper enum value.
pub(super) fn kind_to_object_type(kind: &str) -> i32 {
    match kind.to_lowercase().as_str() {
        "table" => 1,
        "report" => 3,
        "codeunit" => 5,
        "xmlport" => 6,
        "page" => 8,
        "query" => 9,
        "pageextension" => 14,
        "tableextension" => 15,
        "enum" => 16,
        "enumextension" => 17,
        "reportextension" => 22,
        _ => -1,
    }
}

/// Resolve the BcDebugConfig from launch config files.
pub(super) fn resolve_debug_config(
    launch_file: Option<&al_core::launch::DebugConfigFile>,
    config_name: Option<&str>,
) -> std::result::Result<al_dap_client::bc_debug::BcDebugConfig, String> {
    let bc_server = match launch_file {
        Some(df) => {
            if let Some(name) = config_name {
                df.configs.iter().find(|c| c.name == name)
                    .ok_or_else(|| format!("Config '{}' not found in {}", name, df.path.display()))?
                    .clone()
            } else {
                df.configs.first()
                    .ok_or_else(|| format!("No AL configs in {}", df.path.display()))?
                    .clone()
            }
        }
        None => {
            return Err("No debug configuration found (.zed/debug.json or .vscode/launch.json)".to_string());
        }
    };

    // Convert BcServerConfig → BcDebugConfig
    let mut cfg = al_dap_client::bc_debug::BcDebugConfig::default();
    cfg.server = bc_server.server;
    cfg.server_instance = bc_server.server_instance;
    if let Some(port) = bc_server.port {
        cfg.port = port;
    }
    cfg.tenant = bc_server.tenant.unwrap_or_else(|| "default".to_string());
    cfg.environment_name = bc_server.environment_name;
    cfg.accept_invalid_certs = bc_server.accept_invalid_certs;
    cfg.environment_type = match bc_server.environment_type {
        al_core::launch::EnvironmentType::OnPrem => "OnPrem".to_string(),
        al_core::launch::EnvironmentType::Sandbox => "Sandbox".to_string(),
        al_core::launch::EnvironmentType::Production => "Production".to_string(),
    };
    cfg.authentication = match bc_server.authentication {
        al_core::launch::AuthMethod::Windows => "Windows".to_string(),
        al_core::launch::AuthMethod::UserPassword => "UserPassword".to_string(),
        al_core::launch::AuthMethod::AAD => "AAD".to_string(),
    };
    Ok(cfg)
}
