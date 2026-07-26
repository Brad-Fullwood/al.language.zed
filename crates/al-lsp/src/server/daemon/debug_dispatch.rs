//! Debug session dispatcher.

use al_protocol::jsonrpc::{Response, RpcError};
use al_workspace::Workspace;
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
// Err is a ready-to-send JSON-RPC `Response` (cold path); boxing it would only
// scatter `*` derefs across callers.
#[allow(clippy::result_large_err)]
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

fn optional_non_empty_string<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a str>, String> {
    match params.get(key) {
        None => Ok(None),
        Some(value) => {
            let value = value
                .as_str()
                .ok_or_else(|| format!("'{key}' must be a string when supplied"))?
                .trim();
            if value.is_empty() {
                Err(format!("'{key}' must not be empty"))
            } else {
                Ok(Some(value))
            }
        }
    }
}

fn optional_frame_id(params: &serde_json::Value) -> Result<i64, String> {
    match params.get("frameId") {
        None => Ok(0),
        Some(value) => value
            .as_i64()
            .ok_or_else(|| "'frameId' must be an integer when supplied".to_string()),
    }
}

fn optional_i32_param(params: &serde_json::Value, key: &str) -> Result<Option<i32>, String> {
    match params.get(key) {
        None => Ok(None),
        Some(value) => {
            let value = value
                .as_i64()
                .ok_or_else(|| format!("'{key}' must be an integer when supplied"))?;
            i32::try_from(value)
                .map(Some)
                .map_err(|_| format!("'{key}' is outside the supported i32 range"))
        }
    }
}

/// Resolve `(objectType, objectId)` from the workspace file index for
/// a given file path, so daemon breakpoints land at the correct BC object
/// instead of `(0, 0)`. Returns `None` when the file isn't indexed yet — the
/// caller must then either supply the metadata explicitly or surface an error.
fn resolve_object_metadata(workspace: &Workspace, file: &str) -> Option<(i32, i32)> {
    use al_dap::dap::native_dap::kind_to_object_type;
    // The CLI sends `file` as a `file://` URI (via `file_to_uri`); internal
    // callers may pass a plain filesystem path. `file_index.object_info` is
    // keyed by plain paths, so `PathBuf::from("file:///…")` never matched and
    // every CLI breakpoint failed with "file is not indexed". Accept both forms.
    let path = url::Url::parse(file)
        .ok()
        .filter(|u| u.scheme() == "file")
        .and_then(|u| u.to_file_path().ok())
        .unwrap_or_else(|| std::path::PathBuf::from(file));
    let entry = workspace.file_index.object_info.get(&path)?;
    let info = entry.value();
    // BC object IDs are i32; reject (return None) rather than silently wrap an
    // out-of-range cached i64 so breakpoints never land on the wrong object.
    let id = i32::try_from(info.id?).ok()?;
    Some((kind_to_object_type(&info.kind), id))
}

/// When a config name is supplied, it must match exactly. Falling
/// back to the first config silently masks typos (and could route to the
/// wrong BC environment). Only fall back to the first config when no name
/// was supplied. Extracted for unit-testability — the surrounding
/// `resolve_debug_config` adds project + file IO that is hard to mock.
pub(super) fn pick_named_config<'a>(
    configs: &'a [al_bc::launch::BcServerConfig],
    requested_name: Option<&str>,
) -> Result<&'a al_bc::launch::BcServerConfig, String> {
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

pub(super) fn resolve_debug_config(
    workspace: &Workspace,
    params: &serde_json::Value,
) -> Result<al_dap::dap::bc_debug::BcDebugConfig, String> {
    use al_bc::launch::find_launch_config;

    let project_root = workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .ok_or_else(|| "No active project".to_string())?;

    let debug_file = find_launch_config(&project_root)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            "No debug configuration found in project (.zed/debug.json or .vscode/launch.json)"
                .to_string()
        })?;

    let config_name = optional_non_empty_string(params, "config")?;
    let bc_cfg = pick_named_config(&debug_file.configs, config_name)?;

    Ok(debug_config_from_server_config(bc_cfg))
}

fn debug_config_from_server_config(
    bc_cfg: &al_bc::launch::BcServerConfig,
) -> al_dap::dap::bc_debug::BcDebugConfig {
    use al_bc::launch::{AuthMethod, EnvironmentType};
    use al_dap::dap::bc_debug::BcDebugConfig;

    let mut config = BcDebugConfig::from_dap_args(&bc_cfg.debug_args);
    config.server = bc_cfg.server.clone();
    config.server_instance = bc_cfg.server_instance.clone();
    if let Some(port) = bc_cfg.port {
        config.port = port;
    }
    if let Some(tenant) = &bc_cfg.tenant {
        config.tenant = tenant.clone();
    }
    config.environment_type = match bc_cfg.environment_type {
        EnvironmentType::OnPrem => "OnPrem".to_string(),
        EnvironmentType::Sandbox => "Sandbox".to_string(),
        EnvironmentType::Production => "Production".to_string(),
    };
    config.environment_name = bc_cfg.environment_name.clone();
    if bc_cfg.debug_args.get("authentication").is_none() {
        config.authentication = match bc_cfg.authentication {
            AuthMethod::Windows => "Windows".to_string(),
            AuthMethod::UserPassword => "UserPassword".to_string(),
            AuthMethod::AAD => "AAD".to_string(),
        };
    }
    if bc_cfg.debug_args.get("validateServerCertificate").is_none()
        && bc_cfg.debug_args.get("acceptInvalidCerts").is_none()
    {
        config.accept_invalid_certs = bc_cfg.accept_invalid_certs;
    }
    config
}

pub(super) fn debug_uses_oauth(config: &al_dap::dap::bc_debug::BcDebugConfig) -> bool {
    !config.environment_type.eq_ignore_ascii_case("OnPrem")
        || config.authentication.eq_ignore_ascii_case("AAD")
        || config
            .authentication
            .eq_ignore_ascii_case("MicrosoftEntraID")
}

fn has_inline_debug_config(params: &serde_json::Value) -> bool {
    // `server` identifies an on-prem target; `tenant` / `environmentName`
    // identify a BC online target. Do not require a meaningless `server`
    // placeholder merely to select the inline cloud configuration path.
    ["server", "tenant", "environmentName"]
        .iter()
        .any(|field| params.get(field).is_some())
}

pub(super) async fn dispatch_debug(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use al_dap::dap::bc_debug::BcDebugConfig;
    use al_dap::native_debug::NativeDebugSession;
    use al_protocol::jsonrpc::error_codes;

    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return missing_cmd(id, "Missing 'cmd' in debug params"),
    };

    match cmd {
        "start" => {
            let supplied_access_token = match params.get("accessToken") {
                None => String::new(),
                Some(value) => match value.as_str() {
                    Some(token) => token.to_string(),
                    None => {
                        return Response::error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'accessToken' must be a string when supplied",
                        );
                    }
                },
            };
            if let Err(message) = optional_non_empty_string(params, "config") {
                return Response::error(id, error_codes::INVALID_PARAMS, message);
            }

            // Look up the named config in the project's debug configuration.
            // or fall back to parsing full DAP args from params for backward compat.
            let config = if params.get("config").is_some() || !has_inline_debug_config(params) {
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
                BcDebugConfig::from_dap_args(params)
            };
            if let Err(message) = config.validate_native() {
                return Response::error(id, error_codes::INVALID_PARAMS, message);
            }

            // MCP/CLI callers normally authenticate through the shared OAuth
            // cache (`authenticate login`). Requiring them to extract that
            // bearer token and pass it back into `debug start` defeats the
            // purpose of the cache and made the first real MCP call negotiate
            // with an empty bearer token. Preserve an explicitly supplied
            // token for automation, otherwise acquire/refresh through the same
            // keyring-backed flow used by symbol download and authentication.
            let access_token = if supplied_access_token.is_empty() && debug_uses_oauth(&config) {
                match al_bc::http_auth::access_token_from_env() {
                    Ok(Some(token)) => token,
                    Ok(None) => {
                        let client = reqwest::Client::new();
                        match al_symbols::oauth::acquire_token(&client, &config.tenant, |message| {
                            tracing::info!("debug authentication: {message}");
                        })
                        .await
                        {
                            Ok(token) => token,
                            Err(e) => {
                                return Response::error(
                                    id,
                                    error_codes::INTERNAL_ERROR,
                                    format!("Debug authentication failed: {e}"),
                                );
                            }
                        }
                    }
                    Err(error) => {
                        return Response::error(
                            id,
                            error_codes::INVALID_PARAMS,
                            format!("Invalid bearer-token environment: {error}"),
                        );
                    }
                }
            } else {
                supplied_access_token
            };

            let onprem_web_base = if config.launch_browser
                && config.environment_type.eq_ignore_ascii_case("OnPrem")
            {
                let http = match reqwest::Client::builder()
                    .danger_accept_invalid_certs(config.accept_invalid_certs)
                    .build()
                {
                    Ok(client) => client,
                    Err(error) => {
                        return Response::error(
                            id,
                            error_codes::INTERNAL_ERROR,
                            format!("Cannot create the debug HTTP client: {error}"),
                        );
                    }
                };
                match al_dap::dap::bc_debug::get_web_endpoint(&http, &config, &access_token).await {
                    Ok(endpoint) => Some(endpoint),
                    Err(error) => {
                        return Response::error(
                            id,
                            error_codes::INTERNAL_ERROR,
                            format!("Cannot resolve the on-premises Web client URL: {error}"),
                        );
                    }
                }
            } else {
                None
            };
            let launch_browser = config.launch_browser;

            match NativeDebugSession::start(config, &access_token).await {
                Ok(session) => {
                    let session_id = session.session_id().to_string();
                    let browser_url = if launch_browser {
                        match al_dap::dap::native_dap::build_debug_browser_url(
                            &session.config,
                            &session_id,
                            onprem_web_base.as_deref(),
                        ) {
                            Ok(url) => Some(url),
                            Err(error) => {
                                return Response::error(
                                    id,
                                    error_codes::INTERNAL_ERROR,
                                    format!("Cannot build the Web client URL: {error}"),
                                );
                            }
                        }
                    } else {
                        None
                    };
                    *workspace.debug_session.lock().await = Some(session);
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "start",
                            "status": "running",
                            "session": session_id,
                            "browserUrl": browser_url,
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
            let file = match optional_non_empty_string(params, "file") {
                Ok(Some(file)) => file.to_string(),
                Ok(None) => {
                    return Response::error(
                        id,
                        error_codes::INVALID_PARAMS,
                        "Missing 'file' parameter",
                    );
                }
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };
            // Reject missing/overflowing `line` rather than silently defaulting
            // to line 0 — a client bug that omits the field would otherwise
            // create a phantom breakpoint at the top of the file.
            let Some(line) = params
                .get("line")
                .and_then(|v| v.as_u64())
                .and_then(|n| u32::try_from(n).ok())
                .filter(|line| *line > 0)
            else {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: "Missing or out-of-range 'line' parameter (must be 1..=u32::MAX)"
                            .to_string(),
                    }),
                    ..Default::default()
                };
            };
            let condition = match optional_non_empty_string(params, "condition") {
                Ok(condition) => condition.map(str::to_string),
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };

            // Prefer caller-supplied objectType/objectId; otherwise
            // resolve from the workspace file_index. Defaulting to (0, 0)
            // routes the breakpoint at the wrong object — BC accepts the
            // request but never hits the line.
            //
            // `as i32` would silently wrap an out-of-range value; `try_from`
            // rejects it so an upstream caller bug surfaces instead of
            // silently routing at the wrong object.
            let resolved = resolve_object_metadata(workspace, &file);
            let explicit_type = match optional_i32_param(params, "objectType") {
                Ok(value) => value,
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };
            let explicit_id = match optional_i32_param(params, "objectId") {
                Ok(value) => value,
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };
            if explicit_type.is_some() != explicit_id.is_some() {
                return Response::error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'objectType' and 'objectId' must be supplied together",
                );
            }
            let obj_type = explicit_type.or(resolved.map(|(object_type, _)| object_type));
            let obj_id = explicit_id.or(resolved.map(|(_, object_id)| object_id));

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

        "stack" => {
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.stack().await {
                    Ok(frames) => Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "stack",
                            "frames": frames,
                        })),
                        error: None,
                        ..Default::default()
                    },
                    Err(e) => Response::error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        format!("stack() failed: {e}"),
                    ),
                },
            }
        }

        "variables" | "globals" => {
            let frame_id = match optional_frame_id(params) {
                Ok(frame_id) => frame_id,
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => {
                    let values = if cmd == "globals" {
                        session.globals(frame_id).await
                    } else {
                        session.variables(frame_id).await
                    };
                    match values {
                        Ok(values) => match serialize_each(id, values, cmd) {
                            Ok(values) => Response {
                                id,
                                result: Some(serde_json::json!({
                                    "cmd": cmd,
                                    "frameId": frame_id,
                                    "variables": values,
                                })),
                                error: None,
                                ..Default::default()
                            },
                            Err(err_response) => err_response,
                        },
                        Err(e) => Response::error(
                            id,
                            error_codes::INTERNAL_ERROR,
                            format!("{cmd}() failed: {e}"),
                        ),
                    }
                }
            }
        }

        "expand" => {
            let path = match optional_non_empty_string(params, "path") {
                Ok(Some(path)) => path,
                Ok(None) => return missing_cmd(id, "Missing 'path' parameter for expand"),
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };
            let frame_id = match optional_frame_id(params) {
                Ok(frame_id) => frame_id,
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };
            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.expand(frame_id, path).await {
                    Ok(values) => match serialize_each(id, values, "expand") {
                        Ok(values) => Response {
                            id,
                            result: Some(serde_json::json!({
                                "cmd": "expand",
                                "frameId": frame_id,
                                "path": path,
                                "variables": values,
                            })),
                            error: None,
                            ..Default::default()
                        },
                        Err(err_response) => err_response,
                    },
                    Err(e) => Response::error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        format!("expand() failed: {e}"),
                    ),
                },
            }
        }

        "eval" => {
            let expr = match optional_non_empty_string(params, "expr") {
                Ok(Some(expression)) => expression.to_string(),
                Ok(None) => {
                    return Response::error(
                        id,
                        error_codes::INVALID_PARAMS,
                        "Missing 'expr' parameter",
                    );
                }
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };
            let frame_id = match optional_frame_id(params) {
                Ok(frame_id) => frame_id,
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.eval_at(frame_id, &expr).await {
                    Ok(eval_result) => Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "eval",
                            "frameId": frame_id,
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
            let step_type = match optional_non_empty_string(params, "stepType") {
                Ok(None | Some("over")) => "over",
                Ok(Some("in" | "into")) => "in",
                Ok(Some("out")) => "out",
                Ok(Some(other)) => {
                    return Response::error(
                        id,
                        error_codes::INVALID_PARAMS,
                        format!("'stepType' must be 'over', 'in'/'into', or 'out'; got '{other}'"),
                    );
                }
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };

            let mut guard = workspace.debug_session.lock().await;
            match guard.as_mut() {
                None => no_session(id),
                Some(session) => match session.step(step_type).await {
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
            let var_filter = match optional_non_empty_string(params, "var") {
                Ok(value) => value.map(str::to_string),
                Err(message) => {
                    return Response::error(id, error_codes::INVALID_PARAMS, message);
                }
            };

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
                // No active session: say so instead of claiming a stop
                // happened (`debug stop` printed
                // "Debug session stopped." on a machine with no session).
                None => Response {
                    id,
                    result: Some(
                        serde_json::json!({"cmd": "stop", "status": "no active debug session"}),
                    ),
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
    use super::{
        debug_config_from_server_config, debug_uses_oauth, has_inline_debug_config,
        pick_named_config,
    };
    use al_bc::launch::{AuthMethod, BcServerConfig, EnvironmentType};
    use al_dap::dap::bc_debug::{BcDebugConfig, BreakOnError, BreakOnRecordWrite};

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
            debug_args: serde_json::json!({}),
        }
    }

    #[test]
    fn no_name_returns_first_config() {
        let configs = vec![cfg("alpha"), cfg("beta")];
        let picked = pick_named_config(&configs, None).expect("first should win");
        assert_eq!(picked.name, "alpha");
    }

    #[test]
    fn matching_name_returns_that_config() {
        let configs = vec![cfg("alpha"), cfg("beta")];
        let picked = pick_named_config(&configs, Some("beta")).expect("beta should match");
        assert_eq!(picked.name, "beta");
    }

    #[test]
    fn unknown_name_is_not_found_no_fallback() {
        // Negative (the invariant): a typo in the config name MUST
        // surface as an error, NOT silently route to the first config.
        let configs = vec![cfg("alpha"), cfg("beta")];
        let err = pick_named_config(&configs, Some("alfa")).expect_err("typo must error");
        assert!(err.contains("\"alfa\""), "{err}");
        assert!(err.contains("alpha") && err.contains("beta"), "{err}");
    }

    #[test]
    fn named_config_preserves_every_native_debug_field() {
        let mut server = cfg("complete");
        server.server = Some("https://bc.example.test".to_string());
        server.server_instance = Some("BC240".to_string());
        server.port = Some(7050);
        server.tenant = Some("tenant.example".to_string());
        server.environment_name = Some("Production".to_string());
        server.authentication = AuthMethod::AAD;
        server.debug_args = serde_json::json!({
            "environmentType": "OnPrem",
            "authentication": "AAD",
            "breakOnError": "ExcludeTry",
            "breakOnRecordWrite": "ExcludeTemporary",
            "breakOnNext": "Background",
            "sessionId": 73,
            "startupObjectType": "Report",
            "startupObjectId": 50123,
            "startupCompany": "CRONUS UK",
            "launchBrowser": false,
            "schemaUpdateMode": "ForceSync",
            "dependencyPublishingOption": "Strict",
            "enableSqlInformationDebugger": false,
            "enableLongRunningSqlStatements": false,
            "longRunningSqlStatementsThreshold": 975,
            "numberOfSqlStatements": 37,
            "validateServerCertificate": false
        });

        let config = debug_config_from_server_config(&server);
        assert_eq!(config.server.as_deref(), Some("https://bc.example.test"));
        assert_eq!(config.server_instance.as_deref(), Some("BC240"));
        assert_eq!(config.port, 7050);
        assert_eq!(config.tenant, "tenant.example");
        assert_eq!(config.environment_type, "OnPrem");
        assert_eq!(config.environment_name.as_deref(), Some("Production"));
        assert_eq!(config.authentication, "AAD");
        assert_eq!(config.break_on_error, BreakOnError::ExcludeTry);
        assert_eq!(
            config.break_on_record_write,
            BreakOnRecordWrite::ExcludeTemporary
        );
        assert_eq!(config.break_on_next.as_deref(), Some("Background"));
        assert_eq!(config.session_id, Some(73));
        assert_eq!(config.startup_object_type, "Report");
        assert_eq!(config.startup_object_id, 50123);
        assert_eq!(config.startup_company.as_deref(), Some("CRONUS UK"));
        assert!(!config.launch_browser);
        assert_eq!(config.schema_update_mode, "ForceSync");
        assert_eq!(config.dependency_publishing_option, "Strict");
        assert!(!config.enable_sql_information_debugger);
        assert!(!config.enable_long_running_sql_statements);
        assert_eq!(config.long_running_sql_statements_threshold, 975);
        assert_eq!(config.number_of_sql_statements, 37);
        assert!(config.accept_invalid_certs);
        assert!(config.validate_native().is_ok());
    }

    #[test]
    fn named_config_without_authentication_keeps_environment_default() {
        let server = cfg("legacy-onprem-default");
        let config = debug_config_from_server_config(&server);
        assert_eq!(config.authentication, "UserPassword");
        assert!(
            config.validate_native().is_err(),
            "native DAP must reject a legacy default instead of silently switching to OAuth"
        );
    }

    #[test]
    fn empty_config_list_errors_when_no_name_given() {
        let err = pick_named_config(&[], None).expect_err("empty list must error");
        assert!(err.contains("no configs"));
    }

    #[test]
    fn cloud_debug_uses_oauth_even_when_auth_field_is_legacy_default() {
        let config = BcDebugConfig {
            environment_type: "Sandbox".to_string(),
            authentication: "UserPassword".to_string(),
            ..BcDebugConfig::default()
        };
        assert!(debug_uses_oauth(&config));
    }

    #[test]
    fn onprem_aad_uses_oauth_but_windows_does_not() {
        let aad = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            authentication: "MicrosoftEntraID".to_string(),
            ..BcDebugConfig::default()
        };
        assert!(debug_uses_oauth(&aad));

        let windows = BcDebugConfig {
            authentication: "Windows".to_string(),
            ..aad
        };
        assert!(!debug_uses_oauth(&windows));
    }

    #[test]
    fn inline_cloud_config_does_not_require_a_dummy_server() {
        assert!(has_inline_debug_config(&serde_json::json!({
            "tenant": "tenant-id",
            "environmentName": "Sandbox"
        })));
        assert!(has_inline_debug_config(&serde_json::json!({
            "server": "https://bc.example.test"
        })));
        assert!(!has_inline_debug_config(&serde_json::json!({
            "cmd": "start",
            "breakOnNext": "WebClient"
        })));
    }
}

#[cfg(test)]
mod resolve_object_metadata_tests {
    use super::resolve_object_metadata;
    use al_workspace::Workspace;

    #[test]
    fn resolves_indexed_codeunit_to_object_type_and_id() {
        let ws = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/SomeCodeunit.al");
        let src = "codeunit 50100 \"Some Codeunit\"\n{\n}\n".to_string();
        ws.file_index.add_file(path.clone(), src);

        let (obj_type, obj_id) = resolve_object_metadata(&ws, "/tmp/SomeCodeunit.al")
            .expect("indexed file should resolve");
        // codeunit kind → bc_object_type::CODEUNIT (don't pin the exact int —
        // assert it's non-zero, which is the invariant).
        assert!(
            obj_type > 0,
            "object_type should be non-zero, got {obj_type}"
        );
        assert_eq!(obj_id, 50100);
    }

    #[test]
    fn returns_none_for_unindexed_file() {
        let ws = Workspace::new();
        assert!(resolve_object_metadata(&ws, "/nonexistent/Foo.al").is_none());
    }

    #[test]
    fn returns_none_for_out_of_range_cached_id() {
        use al_source::file_index::CachedObjectInfo;
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
    use super::dispatch_debug;
    use al_protocol::jsonrpc::error_codes;
    use al_workspace::Workspace;
    use serde_json::json;

    fn err_code(r: &al_protocol::jsonrpc::Response) -> i32 {
        r.error.as_ref().expect("error expected").code
    }

    fn err_msg(r: &al_protocol::jsonrpc::Response) -> String {
        r.error.as_ref().expect("error expected").message.clone()
    }

    #[tokio::test]
    async fn missing_cmd_is_invalid_params() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 1, &json!({})).await;
        assert_eq!(r.id, 1);
        assert!(r.result.is_none());
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("Missing 'cmd'"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn non_string_cmd_is_invalid_params() {
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
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 4, &json!({"cmd": "breakpoint", "line": 5})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("file"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn breakpoint_missing_line_is_invalid_params() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 5, &json!({"cmd": "breakpoint", "file": "/tmp/Foo.al"})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("line"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn breakpoint_out_of_range_line_is_invalid_params() {
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

        let zero = dispatch_debug(
            &ws,
            61,
            &json!({"cmd": "breakpoint", "file": "/tmp/Foo.al", "line": 0}),
        )
        .await;
        assert_eq!(err_code(&zero), error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn breakpoint_unresolvable_metadata_is_invalid_params() {
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
    async fn breakpoint_rejects_malformed_optional_metadata_and_condition() {
        let ws = Workspace::new();
        for params in [
            json!({
                "cmd": "breakpoint",
                "file": "/nope/Foo.al",
                "line": 3,
                "condition": false
            }),
            json!({
                "cmd": "breakpoint",
                "file": "/nope/Foo.al",
                "line": 3,
                "objectType": "codeunit",
                "objectId": 50100
            }),
            json!({
                "cmd": "breakpoint",
                "file": "/nope/Foo.al",
                "line": 3,
                "objectType": 5
            }),
        ] {
            let response = dispatch_debug(&ws, 70, &params).await;
            assert_eq!(err_code(&response), error_codes::INVALID_PARAMS);
        }
    }

    #[tokio::test]
    async fn breakpoint_with_caller_metadata_but_no_session_is_no_session() {
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
    async fn inspection_commands_without_session_are_no_session() {
        let ws = Workspace::new();
        for (id, params) in [
            (17, json!({"cmd": "stack"})),
            (18, json!({"cmd": "variables", "frameId": 1})),
            (19, json!({"cmd": "globals", "frameId": 1})),
            (20, json!({"cmd": "expand", "frameId": 1, "path": "Rec"})),
        ] {
            let r = dispatch_debug(&ws, id, &params).await;
            assert_eq!(err_msg(&r), "No active debug session");
        }
    }

    #[tokio::test]
    async fn inspection_commands_reject_wrong_typed_frame_ids_before_session_lookup() {
        let ws = Workspace::new();
        for params in [
            json!({"cmd": "variables", "frameId": "top"}),
            json!({"cmd": "globals", "frameId": []}),
            json!({"cmd": "expand", "frameId": false, "path": "Rec"}),
            json!({"cmd": "eval", "frameId": {}, "expr": "Rec"}),
        ] {
            let response = dispatch_debug(&ws, 71, &params).await;
            assert_eq!(err_code(&response), error_codes::INVALID_PARAMS);
            assert!(err_msg(&response).contains("frameId"));
        }
    }

    #[tokio::test]
    async fn expand_requires_a_path() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 21, &json!({"cmd": "expand"})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("path"));
    }

    #[tokio::test]
    async fn eval_missing_expr_is_invalid_params() {
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
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 13, &json!({"cmd": "step"})).await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&r).contains("No active debug session"));

        let into = dispatch_debug(&ws, 131, &json!({"cmd": "step", "stepType": "into"})).await;
        assert_eq!(err_code(&into), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&into).contains("No active debug session"));
    }

    #[tokio::test]
    async fn step_and_history_reject_malformed_optional_strings() {
        let ws = Workspace::new();
        for params in [
            json!({"cmd": "step", "stepType": 1}),
            json!({"cmd": "step", "stepType": "sideways"}),
            json!({"cmd": "history", "var": false}),
        ] {
            let response = dispatch_debug(&ws, 72, &params).await;
            assert_eq!(err_code(&response), error_codes::INVALID_PARAMS);
        }
    }

    #[tokio::test]
    async fn history_without_session_is_no_session() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 14, &json!({"cmd": "history"})).await;
        assert_eq!(err_code(&r), error_codes::INTERNAL_ERROR);
        assert!(err_msg(&r).contains("No active debug session"));
    }

    #[tokio::test]
    async fn stop_without_session_is_idempotent_and_honest() {
        // Stopping when nothing is running is NOT an error — but it must
        // SAY no session was active rather than claim a stop happened
        // (`debug stop` printed "Debug session stopped."
        // on a machine that never started one).
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 15, &json!({"cmd": "stop"})).await;
        assert_eq!(r.id, 15);
        assert!(r.error.is_none(), "stop with no session must not error");
        let result = r.result.expect("stop must return a result");
        assert_eq!(result["status"], "no active debug session");
        assert_eq!(result["cmd"], "stop");
    }

    #[tokio::test]
    async fn start_without_project_or_config_is_invalid_params() {
        let ws = Workspace::new();
        let r = dispatch_debug(&ws, 16, &json!({"cmd": "start"})).await;
        assert_eq!(err_code(&r), error_codes::INVALID_PARAMS);
        assert!(err_msg(&r).contains("No active project"), "{}", err_msg(&r));
    }

    #[tokio::test]
    async fn start_rejects_malformed_config_and_access_token_before_project_lookup() {
        let ws = Workspace::new();
        for params in [
            json!({"cmd": "start", "config": null}),
            json!({"cmd": "start", "config": ""}),
            json!({"cmd": "start", "accessToken": 42}),
        ] {
            let response = dispatch_debug(&ws, 73, &params).await;
            assert_eq!(err_code(&response), error_codes::INVALID_PARAMS);
        }
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
