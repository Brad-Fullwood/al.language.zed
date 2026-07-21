//! Object/source lookup dispatchers used by al-explorer and go-to-definition:
//! resolve an object name (or file+line) to a concrete source location.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

use crate::server::daemon::invalid_params;

/// Return the absolute file path (and line 1) for a workspace object by name.
/// Used by al-explorer to open objects in Zed via the `zed://file/path:line:col` URL scheme.
pub(in crate::server::daemon) fn dispatch_location(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return invalid_params(id),
    };
    if let Some(path) = workspace.file_index.find_by_object_name(name) {
        return Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "line": 1,
            })),
            error: None,
            ..Default::default()
        };
    }

    // not a workspace file — fall back to the symbol index and
    // materialise the package object's source as a virtual .al file, the
    // same mechanism go-to-definition uses. Without this, double-clicking
    // any object from a symbol package (i.e. almost everything in the
    // browser) silently did nothing.
    let kind_filter = params
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<al_symbols::ObjectKind>().ok());
    let id_filter = params.get("id").and_then(|v| v.as_i64());

    let mut candidates = workspace.symbols.get_by_name(name);
    if let Some(kind) = kind_filter {
        candidates.retain(|e| e.kind == kind);
    }
    if let Some(obj_id) = id_filter {
        // Only narrow by ID when it is a real object ID — ID-less kinds
        // (interfaces & co.) carry sentinel/hash values that callers may
        // forward verbatim.
        if obj_id > 0 {
            let has_exact = candidates.iter().any(|e| i64::from(e.id) == obj_id);
            candidates.retain(|e| {
                if has_exact {
                    i64::from(e.id) == obj_id
                } else {
                    e.id <= 0
                }
            });
        }
    }

    if let Some(entry) = candidates.first() {
        let app_path = workspace.symbols.app_path(&entry.package);
        match al_symbols::virtual_file::get_or_create(entry, app_path.as_deref()) {
            Ok(path) => {
                return Response {
                    id,
                    result: Some(serde_json::json!({
                        "path": path.to_string_lossy(),
                        "line": 1,
                        "virtual": true,
                    })),
                    error: None,
                    ..Default::default()
                };
            }
            Err(e) => {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!(
                            "Object '{}' found in package '{}' but its source could not \
                             be materialised: {}",
                            name, entry.package, e
                        ),
                    }),
                    ..Default::default()
                };
            }
        }
    }

    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INVALID_PARAMS,
            message: format!(
                "Object '{}' not found in workspace or symbol packages",
                name
            ),
        }),
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_source(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return invalid_params(id),
    };

    let kind_filter = params
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<al_symbols::ObjectKind>().ok());

    let proc_filter = params.get("proc").and_then(|v| v.as_str());
    let trigger_filter = params.get("trigger").and_then(|v| v.as_str());

    match al_analysis::queries::source::source(
        workspace,
        name,
        kind_filter,
        proc_filter,
        trigger_filter,
    ) {
        Some(result) => Response {
            id,
            result: Some(
                serde_json::to_value(&result)
                    .expect("source lookup result must be JSON serializable"),
            ),
            error: None,
            ..Default::default()
        },
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Object '{}' not found", name),
            }),
            ..Default::default()
        },
    }
}
pub(in crate::server::daemon) fn dispatch_event_source(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(file) = params.get("file").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(line) = params.get("line").and_then(|v| v.as_u64()) else {
        return invalid_params(id);
    };
    match al_analysis::queries::source::event_source(
        workspace,
        std::path::Path::new(file),
        line.min(u64::from(u32::MAX)) as u32,
    ) {
        Ok(result) => Response {
            id,
            result: Some(
                serde_json::to_value(&result)
                    .expect("source lookup result must be JSON serializable"),
            ),
            error: None,
            ..Default::default()
        },
        Err(msg) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: msg,
            }),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_protocol::jsonrpc::error_codes;
    use al_workspace::Workspace;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    #[test]
    fn location_missing_name_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_location(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn location_unknown_object_is_invalid_params_with_name() {
        let ws = empty_ws();
        let resp = dispatch_location(&ws, 2, &serde_json::json!({ "name": "Nope" }));
        let err = resp.error.expect("unknown object must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Nope"), "error names the object");
    }

    #[test]
    fn source_missing_name_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_source(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn source_unknown_object_returns_not_found() {
        let ws = empty_ws();
        let resp = dispatch_source(&ws, 2, &serde_json::json!({ "name": "GhostObject" }));
        let err = resp.error.expect("unknown object must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("GhostObject"));
    }
}
