//! Debug session dispatcher.

use al_protocol::jsonrpc::{Response, RpcError};
use al_workspace::Workspace;
use serde::Serialize;

use super::serialized_response;

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

/// The object a breakpoint on 1-based `line` of `path` belongs to.
///
/// A file can declare several objects, and BC routes a breakpoint by object
/// type and ID, so a breakpoint inside a file's second object set on the
/// first never hit. The object is the last one declared at or above the
/// line; a line above every declaration belongs to the first.
pub(in crate::server::daemon) fn object_at_line(
    workspace: &Workspace,
    path: &std::path::Path,
    line: u32,
) -> Option<al_source::file_index::CachedObjectInfo> {
    let row = usize::try_from(line.saturating_sub(1)).ok()?;
    let mut objects = workspace.file_index.object_infos_in(path).into_iter();
    let first = objects.next()?;
    Some(
        objects
            .rfind(|info| info.range.start_point.row <= row)
            .unwrap_or(first),
    )
}

/// Resolve `(objectType, objectId)` from the workspace file index for the
/// object around 1-based `line` of a file, so daemon breakpoints land at the
/// correct BC object instead of `(0, 0)`. Returns `None` when the file isn't
/// indexed yet — the caller must then either supply the metadata explicitly
/// or surface an error.
fn resolve_object_metadata(workspace: &Workspace, file: &str, line: u32) -> Option<(i32, i32)> {
    use al_dap::dap::native_dap::kind_to_object_type;
    // The CLI sends `file` as a `file://` URI (via `file_to_uri`); internal
    // callers may pass a plain filesystem path. The file index is keyed by
    // plain paths, so `PathBuf::from("file:///…")` never matched and every
    // CLI breakpoint failed with "file is not indexed". Accept both forms.
    let path = url::Url::parse(file)
        .ok()
        .filter(|u| u.scheme() == "file")
        .and_then(|u| u.to_file_path().ok())
        .unwrap_or_else(|| std::path::PathBuf::from(file));
    let info = object_at_line(workspace, &path, line)?;
    // BC object IDs are i32; reject (return None) rather than silently wrap an
    // out-of-range cached i64 so breakpoints never land on the wrong object.
    let id = i32::try_from(info.id?).ok()?;
    Some((kind_to_object_type(&info.kind), id))
}

pub(super) use al_bc::launch::pick_config as pick_named_config;

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

/// Where the daemon may spend the user's cached Business Central credential
/// for this request, and whether TLS verification may be turned off.
///
/// One rule, applied to every debug configuration regardless of where it came
/// from: `al_project::trust::authorize_cached_credential`. Both the inline
/// configuration an MCP caller passes and the named one that comes out of the
/// repository's own launch file go through it, because both are written by
/// someone other than the person whose token it is.
pub(super) fn authorize_debug_target(
    workspace: &Workspace,
    config: &al_dap::dap::bc_debug::BcDebugConfig,
    source: al_project::trust::TargetSource,
) -> Result<al_project::trust::CredentialAuthorization, String> {
    let Some(project_root) = workspace
        .project
        .try_read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|project| project.root.clone()))
    else {
        return Err(
            "No active project, so no Business Central target can be authorised".to_string(),
        );
    };
    let target = al_project::trust::BcTarget::from_debug(
        &config.environment_type,
        config.server.as_deref(),
        config.port,
    );
    al_project::trust::authorize_cached_credential(
        &project_root,
        &target,
        al_project::trust::CredentialKind::Bearer,
        source,
    )
}

/// The same decision for a launch configuration used outside the debugger:
/// symbol download from a BC server, and publish.
pub(in crate::server) fn authorize_launch_target(
    workspace: &Workspace,
    config: &al_bc::launch::BcServerConfig,
) -> Result<al_project::trust::CredentialAuthorization, String> {
    let Some(project_root) = workspace
        .project
        .try_read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|project| project.root.clone()))
    else {
        return Err(
            "No active project, so no Business Central target can be authorised".to_string(),
        );
    };
    let kind = match config.authentication {
        al_bc::launch::AuthMethod::AAD => al_project::trust::CredentialKind::Bearer,
        _ => al_project::trust::CredentialKind::Basic,
    };
    al_project::trust::authorize_cached_credential(
        &project_root,
        &al_project::trust::BcTarget::from_launch(config),
        kind,
        al_project::trust::TargetSource::Repository,
    )
}

/// Authorise a live Business Central test run against the launch
/// configuration it picked, and keep `acceptInvalidCerts` only where the
/// authorisation grants it.
///
/// The test runner sends `BC_ACCESS_TOKEN`, or `BC_USERNAME` and `BC_PASSWORD`,
/// from the environment. The environment is the user's, but the server comes
/// from a launch file the repository carries, which is the case `publish` is
/// authorised for. `tests.run*` and the Run Test code lens both call this.
pub(crate) fn authorize_live_test_target(
    project_root: &std::path::Path,
    config: &mut al_bc::launch::BcServerConfig,
) -> Result<(), String> {
    let authorization = al_project::trust::authorize_cached_credential(
        project_root,
        &al_project::trust::BcTarget::from_launch(config),
        al_project::trust::CredentialKind::Environment,
        al_project::trust::TargetSource::Repository,
    )?;
    config.accept_invalid_certs &= authorization.may_accept_invalid_certs;
    Ok(())
}

/// The bearer token a debug configuration authenticates with, and the
/// authorisation that decides whether it may be spent on that target.
///
/// Both halves live here so a caller cannot have one without the other.
/// `tests.snapshot_capture` repeated the acquire sequence
/// (`debug_uses_oauth` -> `access_token_from_env` -> `acquire_token`) and left
/// the authorisation out, which handed the user's cached token to whatever
/// server a cloned repository's launch file named.
///
/// A caller that brings its own token in `supplied` spends its own credential,
/// so only the cached paths are authorised. `accept_invalid_certs` is checked
/// either way, because turning off TLS verification is the target's decision
/// rather than the token's.
///
/// `Err` carries the JSON-RPC code the dispatcher should answer with.
pub(super) async fn acquire_bc_token(
    workspace: &Workspace,
    config: &al_dap::dap::bc_debug::BcDebugConfig,
    supplied: &str,
    source: al_project::trust::TargetSource,
) -> Result<String, (i32, String)> {
    use al_protocol::jsonrpc::error_codes;

    let spends_cached_credential = supplied.is_empty() && debug_uses_oauth(config);
    if spends_cached_credential || config.accept_invalid_certs {
        let authorization = authorize_debug_target(workspace, config, source)
            .map_err(|message| (error_codes::INVALID_PARAMS, message))?;
        if config.accept_invalid_certs && !authorization.may_accept_invalid_certs {
            return Err((
                error_codes::INVALID_PARAMS,
                "Refusing to disable TLS verification for this target: set acceptInvalidCerts in \
                 the project's own debug configuration, trust the project, and pass 'config'."
                    .to_string(),
            ));
        }
    }
    if !spends_cached_credential {
        return Ok(supplied.to_string());
    }

    // MCP/CLI callers normally authenticate through the shared OAuth cache
    // (`authenticate login`). Requiring them to extract that bearer token and
    // pass it back defeats the purpose of the cache and made the first real MCP
    // call negotiate with an empty bearer token.
    match al_bc::http_auth::access_token_from_env() {
        Ok(Some(token)) => Ok(token),
        Ok(None) => {
            let client = reqwest::Client::new();
            al_symbols::oauth::acquire_token(&client, &config.tenant, |message| {
                tracing::info!("Business Central authentication: {message}");
            })
            .await
            .map_err(|error| {
                (
                    error_codes::INTERNAL_ERROR,
                    format!("Business Central authentication failed: {error}"),
                )
            })
        }
        Err(error) => Err((
            error_codes::INVALID_PARAMS,
            format!("Invalid bearer-token environment: {error}"),
        )),
    }
}

async fn debug_start(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_dap::dap::bc_debug::BcDebugConfig;
    use al_dap::native_debug::NativeDebugSession;
    use al_protocol::jsonrpc::error_codes;

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
    let inline = params.get("config").is_none() && has_inline_debug_config(params);
    let config = if inline {
        BcDebugConfig::from_dap_args(params)
    } else {
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
    };
    if let Err(message) = config.validate_native() {
        return Response::error(id, error_codes::INVALID_PARAMS, message);
    }

    let source = if inline {
        al_project::trust::TargetSource::Inline
    } else {
        al_project::trust::TargetSource::Repository
    };
    let access_token =
        match acquire_bc_token(workspace, &config, &supplied_access_token, source).await {
            Ok(token) => token,
            Err((code, message)) => return Response::error(id, code, message),
        };

    let onprem_web_base =
        if config.launch_browser && config.environment_type.eq_ignore_ascii_case("OnPrem") {
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

async fn debug_breakpoint(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

    let file = match optional_non_empty_string(params, "file") {
        Ok(Some(file)) => file.to_string(),
        Ok(None) => {
            return Response::error(id, error_codes::INVALID_PARAMS, "Missing 'file' parameter");
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
    let resolved = resolve_object_metadata(workspace, &file, line);
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

async fn debug_state(workspace: &Workspace, id: u64, _params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

    let mut guard = workspace.debug_session.lock().await;
    match guard.as_mut() {
        None => no_session(id),
        Some(session) => match session.state().await {
            Ok(state) => serialized_response(id, &state, "state"),
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

async fn debug_stack(workspace: &Workspace, id: u64, _params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

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

async fn debug_variables(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
    cmd: &str,
) -> Response {
    use al_protocol::jsonrpc::error_codes;

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

async fn debug_expand(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

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

async fn debug_eval(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

    let expr = match optional_non_empty_string(params, "expr") {
        Ok(Some(expression)) => expression.to_string(),
        Ok(None) => {
            return Response::error(id, error_codes::INVALID_PARAMS, "Missing 'expr' parameter");
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

async fn debug_continue(workspace: &Workspace, id: u64, _params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

    let mut guard = workspace.debug_session.lock().await;
    match guard.as_mut() {
        None => no_session(id),
        Some(session) => match session.continue_exec().await {
            Ok(state) => serialized_response(id, &state, "continue"),
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

async fn debug_step(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

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
            Ok(state) => serialized_response(id, &state, "step"),
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

async fn debug_history(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

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

async fn debug_stop(workspace: &Workspace, id: u64, _params: &serde_json::Value) -> Response {
    use al_protocol::jsonrpc::error_codes;

    let mut guard = workspace.debug_session.lock().await;
    match guard.as_mut() {
        // No active session: say so instead of claiming a stop
        // happened (`debug stop` printed
        // "Debug session stopped." on a machine with no session).
        None => Response {
            id,
            result: Some(serde_json::json!({"cmd": "stop", "status": "no active debug session"})),
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

/// Route `debug` to the function for its `cmd`.
///
/// Each command is its own function above. They used to be inline arms of a
/// 629-line match, which is how the twenty lines that decide whether a cached
/// Business Central credential may be spent ended up buried in the `start`
/// arm, and how `tests.snapshot_capture` came to repeat the acquisition
/// without them. That decision lives in [`acquire_bc_token`] now.
pub(super) async fn dispatch_debug(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use al_protocol::jsonrpc::error_codes;

    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return missing_cmd(id, "Missing 'cmd' in debug params"),
    };

    match cmd {
        "start" => debug_start(workspace, id, params).await,
        "breakpoint" => debug_breakpoint(workspace, id, params).await,
        "state" => debug_state(workspace, id, params).await,
        "stack" => debug_stack(workspace, id, params).await,
        "variables" | "globals" => debug_variables(workspace, id, params, cmd).await,
        "expand" => debug_expand(workspace, id, params).await,
        "eval" => debug_eval(workspace, id, params).await,
        "continue" => debug_continue(workspace, id, params).await,
        "step" => debug_step(workspace, id, params).await,
        "history" => debug_history(workspace, id, params).await,
        "stop" => debug_stop(workspace, id, params).await,

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

/// The gate that decides whether a cached Business Central token may be spent.
///
/// Every case here refuses before any network call, which is what makes the
/// tests safe to run offline: a passing assertion is also the evidence that no
/// token was acquired.
#[cfg(test)]
mod acquire_bc_token_tests {
    use super::acquire_bc_token;
    use al_dap::dap::bc_debug::BcDebugConfig;
    use al_workspace::Workspace;

    /// A project whose own launch file names an on-premises server, and which
    /// nobody has trusted.
    fn untrusted_project() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join(".vscode")).unwrap();
        std::fs::write(
            root.join(".vscode/launch.json"),
            serde_json::json!({
                "configurations": [{
                    "name": "Local",
                    "type": "al",
                    "request": "launch",
                    "environmentType": "OnPrem",
                    "server": "https://collector.example.test",
                    "serverInstance": "BC",
                    "authentication": "AAD",
                    "tenant": "tenant-id",
                }]
            })
            .to_string(),
        )
        .unwrap();
        let workspace = Workspace::new();
        crate::server::daemon::set_test_project_root(&workspace, &root);
        (dir, workspace)
    }

    fn repository_config() -> BcDebugConfig {
        BcDebugConfig {
            server: Some("https://collector.example.test".to_string()),
            server_instance: Some("BC".to_string()),
            environment_type: "OnPrem".to_string(),
            authentication: "AAD".to_string(),
            tenant: "tenant-id".to_string(),
            ..BcDebugConfig::default()
        }
    }

    #[tokio::test]
    async fn an_untrusted_repository_target_gets_no_cached_token() {
        let (_dir, workspace) = untrusted_project();
        let (_code, message) = acquire_bc_token(
            &workspace,
            &repository_config(),
            "",
            al_project::trust::TargetSource::Repository,
        )
        .await
        .expect_err("an untrusted repository target must not receive the cached token");
        assert!(message.contains("not trusted"), "{message}");
    }

    #[tokio::test]
    async fn a_caller_supplied_token_is_returned_unchanged() {
        let (_dir, workspace) = untrusted_project();
        let token = acquire_bc_token(
            &workspace,
            &repository_config(),
            "caller-token",
            al_project::trust::TargetSource::Repository,
        )
        .await
        .expect("a caller spending its own credential needs no project trust");
        assert_eq!(token, "caller-token");
    }

    #[tokio::test]
    async fn accept_invalid_certs_is_refused_even_for_a_caller_supplied_token() {
        let (_dir, workspace) = untrusted_project();
        let mut config = repository_config();
        config.accept_invalid_certs = true;
        let (_code, message) = acquire_bc_token(
            &workspace,
            &config,
            "caller-token",
            al_project::trust::TargetSource::Repository,
        )
        .await
        .expect_err("TLS verification is the target's decision, not the token's");
        assert!(message.contains("not trusted"), "{message}");
    }

    /// Business Central online has a fixed endpoint, so a repository cannot
    /// redirect the token and trust is not required. The acquisition itself is
    /// not reached here: `AL_BC_ACCESS_TOKEN` short-circuits it.
    #[tokio::test]
    async fn a_business_central_online_target_needs_no_trust() {
        let (_dir, workspace) = untrusted_project();
        let config = BcDebugConfig {
            environment_type: "Sandbox".to_string(),
            environment_name: Some("SANDBOX".to_string()),
            ..BcDebugConfig::default()
        };
        let token = acquire_bc_token(
            &workspace,
            &config,
            "caller-token",
            al_project::trust::TargetSource::Repository,
        )
        .await
        .expect("a fixed Microsoft endpoint is always authorised");
        assert_eq!(token, "caller-token");
    }
}

#[cfg(test)]
mod resolve_object_metadata_tests {
    use super::{object_at_line, resolve_object_metadata};
    use al_workspace::Workspace;

    /// `object_at_line` is what a snapshot capture's breakpoints are routed
    /// by as well: each line belongs to the object declared at or above it.
    #[test]
    fn object_at_line_picks_the_object_declared_at_or_above_the_line() {
        let ws = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/ObjectAtLine.al");
        let src = "// header\ntable 50200 \"Posting Buffer\"\n{\n}\n\ncodeunit 50100 \"Posting Mgt\"\n{\n}\n\n";
        ws.file_index.add_file(path.clone(), src.to_string());
        let name_at = |line| object_at_line(&ws, &path, line).map(|info| info.name);

        assert_eq!(
            name_at(1).as_deref(),
            Some("Posting Buffer"),
            "above every object"
        );
        assert_eq!(name_at(3).as_deref(), Some("Posting Buffer"));
        assert_eq!(
            name_at(5).as_deref(),
            Some("Posting Buffer"),
            "between objects"
        );
        assert_eq!(name_at(6).as_deref(), Some("Posting Mgt"));
        assert_eq!(
            name_at(10).as_deref(),
            Some("Posting Mgt"),
            "after the last"
        );
        assert!(object_at_line(&ws, std::path::Path::new("/tmp/None.al"), 1).is_none());
    }

    #[test]
    fn resolves_indexed_codeunit_to_object_type_and_id() {
        let ws = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/SomeCodeunit.al");
        let src = "codeunit 50100 \"Some Codeunit\"\n{\n}\n".to_string();
        ws.file_index.add_file(path.clone(), src);

        let (obj_type, obj_id) = resolve_object_metadata(&ws, "/tmp/SomeCodeunit.al", 1)
            .expect("indexed file should resolve");
        // codeunit kind → bc_object_type::CODEUNIT (don't pin the exact int —
        // assert it's non-zero, which is the invariant).
        assert!(
            obj_type > 0,
            "object_type should be non-zero, got {obj_type}"
        );
        assert_eq!(obj_id, 50100);
    }

    /// A breakpoint inside the second object of a file routes to that
    /// object, not the file's first.
    #[test]
    fn resolves_the_object_around_the_breakpoint_line() {
        let ws = Workspace::new();
        let path = std::path::PathBuf::from("/tmp/Posting.al");
        let src = "table 50200 \"Posting Buffer\"\n{\n}\n\ncodeunit 50100 \"Posting Mgt\"\n{\n    procedure Post()\n    begin\n    end;\n}\n";
        ws.file_index.add_file(path, src.to_string());

        let (table_type, table_id) =
            resolve_object_metadata(&ws, "/tmp/Posting.al", 2).expect("the table resolves");
        assert_eq!(table_id, 50200);
        let (codeunit_type, codeunit_id) =
            resolve_object_metadata(&ws, "/tmp/Posting.al", 8).expect("the codeunit resolves");
        assert_eq!(codeunit_id, 50100, "line 8 is inside the codeunit");
        assert_ne!(codeunit_type, table_type);
    }

    #[test]
    fn returns_none_for_unindexed_file() {
        let ws = Workspace::new();
        assert!(resolve_object_metadata(&ws, "/nonexistent/Foo.al", 1).is_none());
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
            resolve_object_metadata(&ws, "/tmp/Overflow.al", 1).is_none(),
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
    async fn start_refuses_to_mint_a_token_for_a_caller_supplied_server() {
        let ws = Workspace::new();
        let response = dispatch_debug(
            &ws,
            74,
            &json!({
                "cmd": "start",
                "server": "https://attacker.example",
                "serverInstance": "BC",
                "environmentType": "OnPrem",
                "authentication": "AAD",
                "tenant": "contoso.onmicrosoft.com",
                "launchBrowser": true,
            }),
        )
        .await;
        assert_eq!(err_code(&response), error_codes::INVALID_PARAMS);
        // With no project loaded there is nothing to authorise the target
        // against, so the refusal names that rather than the host.
        assert!(
            err_msg(&response).contains("authorised"),
            "{}",
            err_msg(&response)
        );
    }

    #[tokio::test]
    async fn start_refuses_inline_accept_invalid_certs() {
        let ws = Workspace::new();
        let response = dispatch_debug(
            &ws,
            75,
            &json!({
                "cmd": "start",
                "server": "https://erp.example.com",
                "serverInstance": "BC",
                "environmentType": "OnPrem",
                "authentication": "AAD",
                "tenant": "contoso.onmicrosoft.com",
                "accessToken": "caller-supplied",
                "acceptInvalidCerts": true,
            }),
        )
        .await;
        assert_eq!(err_code(&response), error_codes::INVALID_PARAMS);
        assert!(
            err_msg(&response).contains("authorised"),
            "{}",
            err_msg(&response)
        );
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
    use super::{serialize_each, serialized_response};
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
    fn serialized_response_ok_has_result_no_error() {
        let r = serialized_response(7, &Ok { a: 1 }, "state");
        assert_eq!(r.id, 7);
        assert!(r.result.is_some());
        assert!(r.error.is_none());
    }

    #[test]
    fn serialized_response_failure_returns_error_not_null_result() {
        // JSON-RPC §5.1: exactly one of result/error must be present. A
        // serialization failure must produce an error, never a response with
        // both fields None.
        let r = serialized_response(9, &bad(), "state");
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
