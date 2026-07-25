//! Object/source lookup dispatchers used by al-explorer and go-to-definition:
//! resolve an object name (or file+line) to a concrete source location.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;

use crate::server::daemon::{invalid_params, rpc_error};

fn optional_non_empty_string<'a>(
    params: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a str>, String> {
    let Some(value) = params.get(key) else {
        return Ok(None);
    };
    let Some(value) = value.as_str() else {
        return Err(format!("'{key}' must be a non-empty string"));
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("'{key}' must be a non-empty string"));
    }
    Ok(Some(value))
}

/// Return the absolute file path (and line 1) for a workspace object by name.
/// Used by al-explorer to open objects in Zed via the `zed://file/path:line:col` URL scheme.
pub(in crate::server::daemon) fn dispatch_location(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let name = match optional_non_empty_string(params, "name") {
        Ok(Some(name)) => name,
        Ok(None) => return invalid_params(id),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let kind_filter = match optional_non_empty_string(params, "kind") {
        Ok(Some(kind)) => match kind.parse::<al_symbols::ObjectKind>() {
            Ok(kind) => Some(kind),
            Err(_) => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Unknown AL object kind '{kind}'"),
                );
            }
        },
        Ok(None) => None,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let package_filter = match optional_non_empty_string(params, "package") {
        Ok(package) => package,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let id_filter = match params.get("id") {
        None => None,
        Some(value) => match value.as_i64() {
            Some(value) => Some(value),
            None => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "'id' must be an integer");
            }
        },
    };

    let workspace_requested = package_filter.is_none_or(|package| {
        package.eq_ignore_ascii_case("workspace") || package.eq_ignore_ascii_case("(workspace)")
    });
    let mut workspace_candidates = if workspace_requested {
        workspace
            .file_index
            .object_paths(name)
            .into_iter()
            .filter(|path| {
                workspace
                    .file_index
                    .object_info
                    .get(path)
                    .is_some_and(|info| {
                        kind_filter
                            .is_none_or(|kind| info.kind.eq_ignore_ascii_case(&kind.to_string()))
                            && id_filter.is_none_or(|object_id| info.id == Some(object_id))
                    })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    workspace_candidates.sort();
    if workspace_candidates.len() > 1 {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!("Location lookup for '{name}' is ambiguous; specify 'kind' and/or 'id'"),
        );
    }
    if let Some(path) = workspace_candidates.first() {
        return Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "line": 1,
                "source_availability": al_symbols::SourceAvailability::WorkspaceSource,
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
    let mut candidates = workspace.symbols.get_by_name(name);
    if let Some(kind) = kind_filter {
        candidates.retain(|e| e.kind == kind);
    }
    if let Some(package) = package_filter {
        candidates.retain(|entry| entry.package.eq_ignore_ascii_case(package));
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
    candidates.sort_by(|left, right| {
        (
            left.package.to_ascii_lowercase(),
            left.kind.to_string(),
            left.id,
        )
            .cmp(&(
                right.package.to_ascii_lowercase(),
                right.kind.to_string(),
                right.id,
            ))
    });
    if candidates.len() > 1 {
        let matches = candidates
            .iter()
            .map(|entry| format!("{} {} ({})", entry.kind, entry.id, entry.package))
            .collect::<Vec<_>>()
            .join(", ");
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!(
                "Location lookup for '{name}' is ambiguous; specify 'kind', 'id', and/or \
                 'package'. Matches: {matches}"
            ),
        );
    }

    if let Some(entry) = candidates.first() {
        let app_path = workspace.symbols.app_path(&entry.package);
        match al_symbols::virtual_file::get_or_create_with_availability(entry, app_path.as_deref())
        {
            Ok(materialized) => {
                return Response {
                    id,
                    result: Some(serde_json::json!({
                        "path": materialized.path.to_string_lossy(),
                        "line": 1,
                        "virtual": true,
                        "source_availability": materialized.availability,
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
    let name = match optional_non_empty_string(params, "name") {
        Ok(Some(name)) => name,
        Ok(None) => return invalid_params(id),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    let kind_filter = match optional_non_empty_string(params, "kind") {
        Ok(None) => None,
        Ok(Some(kind)) => match kind.parse::<al_symbols::ObjectKind>() {
            Ok(kind) => Some(kind),
            Err(_) => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Unknown AL object kind '{kind}'"),
                );
            }
        },
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    let package_filter = match optional_non_empty_string(params, "package") {
        Ok(package) => package,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let proc_filter = match optional_non_empty_string(params, "proc") {
        Ok(procedure) => procedure,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let trigger_filter = match optional_non_empty_string(params, "trigger") {
        Ok(trigger) => trigger,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if proc_filter.is_some() && trigger_filter.is_some() {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'proc' and 'trigger' are mutually exclusive",
        );
    }
    let member = proc_filter
        .map(|name| al_analysis::queries::source::SourceMember {
            kind: al_analysis::queries::source::SourceMemberKind::Procedure,
            name,
        })
        .or_else(|| {
            trigger_filter.map(|name| al_analysis::queries::source::SourceMember {
                kind: al_analysis::queries::source::SourceMemberKind::Trigger,
                name,
            })
        });

    match al_analysis::queries::source::source(workspace, name, kind_filter, package_filter, member)
    {
        Ok(result) => Response {
            id,
            result: Some(
                serde_json::to_value(&result)
                    .expect("source lookup result must be JSON serializable"),
            ),
            error: None,
            ..Default::default()
        },
        Err(error) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: error.to_string(),
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
    fn location_rejects_same_name_workspace_ambiguity_and_honors_kind() {
        let ws = empty_ws();
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Customer.Table.al"),
            "table 50100 Customer { }".to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Customer.Page.al"),
            "page 50101 Customer { }".to_string(),
        );

        let ambiguous = dispatch_location(&ws, 3, &serde_json::json!({ "name": "Customer" }));
        let error = ambiguous.error.expect("same-name objects must be rejected");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
        assert!(error.message.contains("ambiguous"));

        let table = dispatch_location(
            &ws,
            4,
            &serde_json::json!({ "name": "Customer", "kind": "table" }),
        );
        assert!(table.error.is_none(), "kind should disambiguate location");
        assert_eq!(
            table.result.expect("location")["path"],
            "/project/Customer.Table.al"
        );
    }

    #[test]
    fn location_rejects_malformed_optional_identity() {
        let ws = empty_ws();
        let response = dispatch_location(
            &ws,
            5,
            &serde_json::json!({ "name": "Customer", "package": 42 }),
        );
        let error = response.error.expect("non-string package must be rejected");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
        assert!(error.message.contains("package"));
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

    #[test]
    fn source_rejects_invalid_kind_instead_of_ignoring_it() {
        let ws = empty_ws();
        let resp = dispatch_source(
            &ws,
            3,
            &serde_json::json!({ "name": "Customer", "kind": "tabl" }),
        );
        let err = resp.error.expect("invalid kind must be rejected");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("tabl"));
    }

    #[test]
    fn source_rejects_conflicting_member_selectors() {
        let ws = empty_ws();
        let resp = dispatch_source(
            &ws,
            4,
            &serde_json::json!({
                "name": "Customer",
                "proc": "DoWork",
                "trigger": "OnInsert"
            }),
        );
        let err = resp.error.expect("conflicting selectors must be rejected");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("mutually exclusive"));
    }

    #[test]
    fn source_rejects_malformed_or_empty_optional_selectors() {
        let ws = empty_ws();
        for params in [
            serde_json::json!({ "name": "Customer", "package": 42 }),
            serde_json::json!({ "name": "Customer", "proc": "" }),
            serde_json::json!({ "name": "Customer", "trigger": false }),
        ] {
            let response = dispatch_source(&ws, 5, &params);
            let error = response.error.expect("malformed selector must be rejected");
            assert_eq!(error.code, error_codes::INVALID_PARAMS);
            assert!(error.message.contains("must be a non-empty string"));
        }
    }

    #[test]
    fn source_returns_unopened_workspace_source_with_provenance() {
        let ws = empty_ws();
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Customer.al"),
            "table 50100 Customer { procedure DoWork() begin end; }".to_string(),
        );
        let resp = dispatch_source(
            &ws,
            6,
            &serde_json::json!({
                "name": "Customer",
                "kind": "table",
                "proc": "DoWork"
            }),
        );
        assert!(
            resp.error.is_none(),
            "source request failed: {:?}",
            resp.error
        );
        let result = resp.result.expect("source result");
        assert_eq!(result["src"], "workspace");
        assert_eq!(result["source_availability"], "workspace_source");
        assert!(result["code"]
            .as_str()
            .is_some_and(|code| code.contains("procedure DoWork")));
    }
}
