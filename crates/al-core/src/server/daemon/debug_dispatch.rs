//! Debug session dispatcher.

use crate::workspace::Workspace;
use al_protocol::jsonrpc::{Response, RpcError};
use serde::Serialize;

/// Serialize `state` into a `Response`'s `result`. On serialization failure
/// return an `INTERNAL_ERROR` rather than producing a JSON-RPC response with
/// both `result` and `error` absent (which violates JSON-RPC 2.0 §5.1, since
/// both fields are `skip_serializing_if = "Option::is_none"`). Mirrors the
/// `ok_response` helper in `lsp_dispatch.rs`.
fn state_response<T: Serialize>(id: u64, state: &T, cmd: &str) -> Response {
    match serde_json::to_value(state) {
        Ok(v) => Response {
            id,
            result: Some(v),
            error: None,
            ..Default::default()
        },
        Err(e) => {
            tracing::error!(cmd, error = %e, "serialization failed for debug result");
            Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                    message: format!("serialization failed for {cmd}: {e}"),
                }),
                ..Default::default()
            }
        }
    }
}

/// Serialize each item of `items` into a JSON array, returning an
/// `INTERNAL_ERROR` `Response` (as `Err`) if any element fails. This surfaces
/// serialization failures instead of silently dropping items
/// (`filter_map(...ok())`) or emitting empty objects (`unwrap_or_default()`).
fn serialize_each<T: Serialize>(
    id: u64,
    items: impl IntoIterator<Item = T>,
    cmd: &str,
) -> Result<Vec<serde_json::Value>, Response> {
    items
        .into_iter()
        .map(|item| serde_json::to_value(&item))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            tracing::error!(cmd, error = %e, "serialization failed for debug item");
            Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                    message: format!("serialization failed for {cmd}: {e}"),
                }),
                ..Default::default()
            }
        })
}

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
    // BC object IDs are i32; reject (return None) rather than silently wrap an
    // out-of-range cached i64 so breakpoints never land on the wrong object.
    let id = i32::try_from(info.id?).ok()?;
    Some((kind_to_object_type(&info.kind), id))
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
                        Ok(verified) => match serialize_each(id, verified, "breakpoint") {
                            Ok(bp_json) => Response {
                                id,
                                result: Some(serde_json::json!({
                                    "cmd": "breakpoint",
                                    "breakpoints": bp_json,
                                })),
                                error: None,
                                ..Default::default()
                            },
                            Err(err_response) => err_response,
                        },
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
                    Ok(state) => state_response(id, &state, "state"),
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
                    Ok(state) => state_response(id, &state, "continue"),
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
                    Ok(state) => state_response(id, &state, "step"),
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
                    let history = session.history(var_filter.as_deref());
                    match serialize_each(id, history, "history") {
                        Ok(hits) => Response {
                            id,
                            result: Some(serde_json::json!({
                                "cmd": "history",
                                "hits": hits,
                            })),
                            error: None,
                            ..Default::default()
                        },
                        Err(err_response) => err_response,
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

    #[test]
    fn returns_none_for_out_of_range_cached_id() {
        // Negative regression: a cached object id beyond the i32 range must
        // make the helper return None rather than silently wrapping via
        // `as i32` and routing a breakpoint to the wrong BC object.
        use crate::file_index::CachedObjectInfo;
        let ws = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/Overflow.al");
        let zero = tree_sitter::Point { row: 0, column: 0 };
        ws.file_index.object_info.insert(
            path.clone(),
            CachedObjectInfo {
                kind: "codeunit".to_string(),
                id: Some(i64::from(i32::MAX) + 1),
                name: "Overflow".to_string(),
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 0,
                    start_point: zero,
                    end_point: zero,
                },
            },
        );
        assert!(
            resolve_object_metadata(&ws, "/tmp/Overflow.al").is_none(),
            "out-of-range cached id must not be silently truncated"
        );
    }
}

#[cfg(test)]
mod dispatch_debug_tests {
    //! In-process coverage for [`dispatch_debug`]'s param-validation and
    //! no-session error paths. A fresh `Workspace` starts with
    //! `debug_session == None`, so every "no active session" branch and every
    //! argument-validation branch is reachable without spawning a real BC
    //! debugger.
    use super::dispatch_debug;
    use crate::workspace::Workspace;
    use al_protocol::jsonrpc::error_codes;
    use serde_json::json;

    fn err_code(r: &al_protocol::jsonrpc::Response) -> i32 {
        r.error.as_ref().expect("error expected").code
    }

    fn err_msg(r: &al_protocol::jsonrpc::Response) -> String {
        r.error.as_ref().expect("error expected").message.clone()
    }

    #[tokio::test]
    async fn missing_cmd_is_invalid_params() {
        // No `cmd` key at all → INVALID_PARAMS, not a panic or success.
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 1, &json!({})).await;
        assert_eq!(r.id, 1);
        assert!(r.result.is_none());
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("Missing 'cmd'"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn non_string_cmd_is_invalid_params() {
        // `cmd` present but not a string → as_str() is None → same branch.
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 2, &json!({"cmd": 42})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn unknown_cmd_is_invalid_params_and_echoes_name() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 3, &json!({"cmd": "frobnicate"})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(
            err_msg(&r).contains("frobnicate"),
            "unknown cmd name must be echoed: {}",
            err_msg(&r)
        );
    }

    #[tokio::test]
    async fn breakpoint_missing_file_is_invalid_params() {
        // The `file` guard fires before any session lock is taken.
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 4, &json!({"cmd": "breakpoint", "line": 5})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("file"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn breakpoint_missing_line_is_invalid_params() {
        // The `line` guard rejects a missing line rather than defaulting to 0.
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 5, &json!({"cmd": "breakpoint", "file": "/tmp/Foo.al"})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("line"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn breakpoint_out_of_range_line_is_invalid_params() {
        // A line beyond u32::MAX must be rejected, not wrapped.
        let ws = Workspace::new();
        let big = u64::from(u32::MAX) + 1;
        let r = dispatch_debug(
            &ws,
            6,
            &json!({"cmd": "breakpoint", "file": "/tmp/Foo.al", "line": big}),
        )
        .await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("line"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn breakpoint_unresolvable_metadata_is_invalid_params() {
        // file + valid line present, but the file isn't indexed and the caller
        // supplied neither objectType nor objectId → metadata cannot be
        // resolved → INVALID_PARAMS (NOT the no-session error, which would
        // only be reached after metadata resolves).
        let ws = Workspace::new();
        let r = dispatch_debug(
            &ws,
            7,
            &json!({"cmd": "breakpoint", "file": "/nope/Foo.al", "line": 3}),
        )
        .await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("object metadata"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn breakpoint_with_caller_metadata_but_no_session_is_no_session() {
        // Caller supplies objectType + objectId so metadata resolves; we then
        // hit the session lock and find None → INTERNAL_ERROR "No active
        // debug session". This proves the metadata path can fall through.
        let ws = Workspace::new();
        let r = dispatch_debug(
            &ws,
            8,
            &json!({
                "cmd": "breakpoint",
                "file": "/nope/Foo.al",
                "line": 3,
                "objectType": 5,
                "objectId": 50100
            }),
        )
        .await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(
            err_msg(&r).contains("No active debug session"),
            "{}",
            err_msg(&r)
        );
    }

    #[tokio::test]
    async fn state_without_session_is_no_session() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 9, &json!({"cmd": "state"})).await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&r).contains("No active debug session"));
    }

    #[tokio::test]
    async fn eval_missing_expr_is_invalid_params() {
        // The expr guard fires before the session lock.
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 10, &json!({"cmd": "eval"})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("expr"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn eval_with_expr_but_no_session_is_no_session() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 11, &json!({"cmd": "eval", "expr": "x + 1"})).await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&r).contains("No active debug session"));
    }

    #[tokio::test]
    async fn continue_without_session_is_no_session() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 12, &json!({"cmd": "continue"})).await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&r).contains("No active debug session"));
    }

    #[tokio::test]
    async fn step_without_session_is_no_session() {
        // step defaults stepType to "over" and still requires a session.
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 13, &json!({"cmd": "step"})).await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&r).contains("No active debug session"));
    }

    #[tokio::test]
    async fn history_without_session_is_no_session() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 14, &json!({"cmd": "history"})).await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&r).contains("No active debug session"));
    }

    #[tokio::test]
    async fn stop_without_session_is_idempotent_success() {
        // Stopping when nothing is running is NOT an error: it returns a
        // success result reporting "stopped". This is the one no-session
        // branch that intentionally succeeds.
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 15, &json!({"cmd": "stop"})).await;
        assert_eq!(r.id, 15);
        assert!(r.error.is_none(), "stop with no session must not error");
        let result = r.result.expect("stop must return a result");
        assert_eq!(result["status"], "stopped");
        assert_eq!(result["cmd"], "stop");
    }

    #[tokio::test]
    async fn start_without_project_or_config_is_invalid_params() {
        // No `server` key and no project → resolve_debug_config errors with
        // "No active project", surfaced as INVALID_PARAMS (not a panic).
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 16, &json!({"cmd": "start"})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("No active project"), "{}", err_msg(&r));
    }
}

#[cfg(test)]
mod serialization_helper_tests {
    use super::{serialize_each, state_response};
    use serde::Serialize;

    #[derive(Serialize)]
    struct Ok {
        a: i32,
    }

    // serde_json cannot serialize a map with non-string keys → forces an error
    // without relying on panics, so we can exercise the failure branch.
    #[derive(Serialize)]
    struct Bad {
        m: std::collections::HashMap<Vec<u8>, i32>,
    }

    fn bad() -> Bad {
        let mut m = std::collections::HashMap::new();
        m.insert(vec![1u8, 2], 3);
        Bad { m }
    }

    #[test]
    fn state_response_ok_has_result_no_error() {
        let r = state_response(7, &Ok { a: 1 }, "state");
        assert_eq!(r.id, 7);
        assert!(r.result.is_some());
        assert!(r.error.is_none());
    }

    #[test]
    fn state_response_failure_returns_error_not_null_result() {
        // JSON-RPC §5.1: exactly one of result/error must be present. A
        // serialization failure must produce an error, never a response with
        // both fields None.
        let r = state_response(9, &bad(), "state");
        assert_eq!(r.id, 9);
        assert!(r.result.is_none());
        let err = r.error.expect("must surface error, not null result");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INTERNAL_ERROR);
    }

    #[test]
    fn serialize_each_ok_collects_all() {
        let items = vec![Ok { a: 1 }, Ok { a: 2 }];
        let out = serialize_each(1, items, "breakpoint").expect("all serialize");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn serialize_each_failure_returns_error_response_not_silent_drop() {
        // The old code used unwrap_or_default() (empty object) or
        // filter_map(...ok()) (silent drop). The helper must instead surface
        // an INTERNAL_ERROR so the client knows the response is incomplete.
        let items = vec![bad()];
        let err = serialize_each(3, items, "history").expect_err("must error");
        assert_eq!(err.id, 3);
        assert!(err.result.is_none());
        assert_eq!(
            err.error.expect("error present").code,
            al_protocol::jsonrpc::error_codes::INTERNAL_ERROR
        );
    }
}
