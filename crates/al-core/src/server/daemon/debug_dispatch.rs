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
/// F-016: resolve `(objectType, objectId)` from the workspace file_index for
/// a given file path, so daemon breakpoints land at the correct BC object
/// instead of `(0, 0)`. Returns `None` when the file isn't indexed yet — the
/// caller must then either supply the metadata explicitly or surface an error.
fn resolve_object_metadata(workspace: &Workspace, file: &str) -> Option<(i32, i32)> {
    use crate::dap::native_dap::kind_to_object_type;
    let path = std::path::PathBuf::from(file);
    let entry = workspace.file_index.object_info.get(&path)?;
    let info = entry.value();
    let id = info.id?;
    Some((kind_to_object_type(&info.kind), id as i32))
}

/// F-015: when a config name is supplied, it MUST match exactly. Falling
/// back to the first config silently masks typos (and could route to the
/// wrong BC environment). Only fall back to the first config when no name
/// was supplied. Extracted for unit-testability — the surrounding
/// `resolve_debug_config` adds project + file IO that is hard to mock.
fn pick_named_config<'a>(
    configs: &'a [crate::launch::BcServerConfig],
    requested_name: Option<&str>,
) -> Result<&'a crate::launch::BcServerConfig, String> {
    match requested_name {
        Some(name) => configs.iter().find(|c| c.name == name).ok_or_else(|| {
            let known: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
            format!("Debug config {name:?} not found. Known configs: {known:?}")
        }),
        None => configs
            .first()
            .ok_or_else(|| "Project debug configuration file has no configs".to_string()),
    }
}

fn resolve_debug_config(
    workspace: &Workspace,
    params: &serde_json::Value,
) -> Result<crate::dap::bc_debug::BcDebugConfig, String> {
    use crate::dap::bc_debug::BcDebugConfig;
    use crate::launch::find_launch_config;

    let project_root = workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .ok_or_else(|| "No active project".to_string())?;

    let debug_file = find_launch_config(&project_root).ok_or_else(|| {
        "No debug configuration found in project (.zed/debug.json or .vscode/launch.json)"
            .to_string()
    })?;

    let config_name = params.get("config").and_then(|v| v.as_str());
    let bc_cfg = pick_named_config(&debug_file.configs, config_name)?;

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

    Ok(BcDebugConfig {
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
                    Ok(c) => c,
                    Err(msg) => {
                        return Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INVALID_PARAMS,
                                message: msg,
                            }),
                            ..Default::default()
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
                        ..Default::default()
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("Debug start failed: {e}"),
                    }),
                    ..Default::default()
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
                        ..Default::default()
                    };
                }
            };
            // Reject missing/overflowing `line` rather than silently defaulting
            // to line 0 — a client bug that omits the field would otherwise
            // create a phantom breakpoint at the top of the file.
            let Some(line) = params
                .get("line")
                .and_then(|v| v.as_u64())
                .and_then(|n| u32::try_from(n).ok())
            else {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: "Missing or out-of-range 'line' parameter (must be 0..=u32::MAX)"
                            .to_string(),
                    }),
                    ..Default::default()
                };
            };
            let condition = params
                .get("condition")
                .and_then(|v| v.as_str())
                .map(String::from);

            // F-016: prefer caller-supplied objectType/objectId; otherwise
            // resolve from the workspace file_index. Defaulting to (0, 0)
            // routes the breakpoint at the wrong object — BC accepts the
            // request but never hits the line.
            //
            // `as i32` would silently wrap an out-of-range value; `try_from`
            // rejects it so an upstream caller bug surfaces instead of
            // silently routing at the wrong object.
            let resolved = resolve_object_metadata(workspace, &file);
            let obj_type = params
                .get("objectType")
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok())
                .or(resolved.map(|(t, _)| t));
            let obj_id = params
                .get("objectId")
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok())
                .or(resolved.map(|(_, i)| i));

            let (obj_type, obj_id) = match (obj_type, obj_id) {
                (Some(t), Some(i)) => (t, i),
                _ => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: format!(
                                "Cannot resolve object metadata for breakpoint in {file:?} — \
                                 file is not indexed and caller did not supply \
                                 objectType/objectId"
                            ),
                        }),
                        ..Default::default()
                    };
                }
            };

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => {
                    let bps: Vec<(u32, Option<&str>)> = vec![(line, condition.as_deref())];
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
                                ..Default::default()
                            }
                        }
                        Err(e) => Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INTERNAL_ERROR,
                                message: format!("set_breakpoints failed: {e}"),
                            }),
                            ..Default::default()
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
                        ..Default::default()
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("state() failed: {e}"),
                        }),
                        ..Default::default()
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
                        ..Default::default()
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
                        ..Default::default()
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("eval() failed: {e}"),
                        }),
                        ..Default::default()
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
                        ..Default::default()
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("continue() failed: {e}"),
                        }),
                        ..Default::default()
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
                        ..Default::default()
                    },
                    Err(e) => Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("step() failed: {e}"),
                        }),
                        ..Default::default()
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
                        ..Default::default()
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
                    ..Default::default()
                },
                Some(session) => {
                    let stop_result = session.stop().await;
                    *guard = None;
                    match stop_result {
                        Ok(()) => Response {
                            id,
                            result: Some(serde_json::json!({"cmd": "stop", "status": "stopped"})),
                            error: None,
                            ..Default::default()
                        },
                        Err(e) => Response {
                            id,
                            result: None,
                            error: Some(RpcError {
                                code: error_codes::INTERNAL_ERROR,
                                message: format!("stop() failed: {e}"),
                            }),
                            ..Default::default()
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
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod pick_named_config_tests {
    use super::pick_named_config;
    use crate::launch::{AuthMethod, BcServerConfig, EnvironmentType};

    fn cfg(name: &str) -> BcServerConfig {
        BcServerConfig {
            name: name.to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: None,
            server_instance: None,
            port: None,
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::UserPassword,
            accept_invalid_certs: false,
        }
    }

    #[test]
    fn no_name_returns_first_config() {
        // Positive: backward-compatible default behaviour when caller does
        // not specify a config name.
        let configs = vec![cfg("alpha"), cfg("beta")];
        let picked = pick_named_config(&configs, None).expect("first should win");
        assert_eq!(picked.name, "alpha");
    }

    #[test]
    fn matching_name_returns_that_config() {
        // Positive: exact-match path.
        let configs = vec![cfg("alpha"), cfg("beta")];
        let picked = pick_named_config(&configs, Some("beta")).expect("beta should match");
        assert_eq!(picked.name, "beta");
    }

    #[test]
    fn unknown_name_is_not_found_no_fallback() {
        // Negative (the F-015 invariant): a typo in the config name MUST
        // surface as an error, NOT silently route to the first config.
        let configs = vec![cfg("alpha"), cfg("beta")];
        let err = pick_named_config(&configs, Some("alfa")).expect_err("typo must error");
        assert!(err.contains("\"alfa\""), "{err}");
        assert!(err.contains("alpha") && err.contains("beta"), "{err}");
    }

    #[test]
    fn empty_config_list_errors_when_no_name_given() {
        // Negative: missing-config-list path is its own clear error.
        let err = pick_named_config(&[], None).expect_err("empty list must error");
        assert!(err.contains("no configs"));
    }
}

#[cfg(test)]
mod resolve_object_metadata_tests {
    use super::resolve_object_metadata;
    use crate::workspace::Workspace;

    #[test]
    fn resolves_indexed_codeunit_to_object_type_and_id() {
        // Positive (F-016 invariant): when the file is indexed, the helper
        // must return the BC object type code and id, NOT (0, 0).
        let ws = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/SomeCodeunit.al");
        let src = "codeunit 50100 \"Some Codeunit\"\n{\n}\n".to_string();
        ws.file_index.add_file(path.clone(), src);

        let (obj_type, obj_id) = resolve_object_metadata(&ws, "/tmp/SomeCodeunit.al")
            .expect("indexed file should resolve");
        // codeunit kind → bc_object_type::CODEUNIT (don't pin the exact int —
        // assert it's non-zero, which is the F-016 invariant).
        assert!(
            obj_type > 0,
            "object_type should be non-zero, got {obj_type}"
        );
        assert_eq!(obj_id, 50100);
    }

    #[test]
    fn returns_none_for_unindexed_file() {
        // Negative: when the file isn't in the index, return None so the
        // caller can either accept caller-supplied metadata or error out
        // cleanly instead of silently using (0, 0).
        let ws = Workspace::new();
        assert!(resolve_object_metadata(&ws, "/nonexistent/Foo.al").is_none());
    }
}
