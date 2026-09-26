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

/// Return the absolute file path and the 1-based declaration line of a
/// workspace object by name.
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
    // One candidate per declaration named `name`: a file can declare several
    // objects, so the filters apply to that declaration, not to the file's
    // first object.
    let mut workspace_candidates = if workspace_requested {
        let mut paths = workspace.file_index.object_paths(name);
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .flat_map(|path| {
                workspace
                    .file_index
                    .object_infos_in(&path)
                    .into_iter()
                    .filter(|info| {
                        info.name.eq_ignore_ascii_case(name)
                            && kind_filter.is_none_or(|kind| {
                                info.kind.eq_ignore_ascii_case(&kind.to_string())
                            })
                            && id_filter.is_none_or(|object_id| info.id == Some(object_id))
                    })
                    .map(move |info| (path.clone(), info.range.start_point.row + 1))
                    .collect::<Vec<_>>()
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
    if let Some((path, line)) = workspace_candidates.first() {
        return Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "line": line,
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
/// The parameters `source` reads, and those any request may carry.
const SOURCE_PARAMS: &[&str] = &[
    "name",
    "kind",
    "package",
    "listProcedures",
    "proc",
    "procedure",
    "trigger",
];
const PROJECTION_PARAMS: &[&str] = &["limit", "offset", "fields", "scope"];

pub(in crate::server::daemon) fn dispatch_source(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    // An unknown key was ignored, so `procedure` (the CLI's flag name)
    // through `al_call` returned the whole 173 KB codeunit instead of one
    // procedure. `procedure` is accepted now, and other names are refused.
    if let Some(unknown) = params.as_object().and_then(|object| {
        object.keys().find(|key| {
            !SOURCE_PARAMS.contains(&key.as_str()) && !PROJECTION_PARAMS.contains(&key.as_str())
        })
    }) {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            &format!(
                "source does not take '{unknown}'; it takes name, kind, package, listProcedures, \
                 proc (or procedure) and trigger"
            ),
        );
    }
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
    // `listProcedures` answers "what can I ask for" without paying for the
    // bodies: `source "Sales-Post"` was 837 KB, of which the agent needed one
    // procedure name.
    let list_members = match params.get("listProcedures") {
        None => false,
        Some(value) => match value.as_bool() {
            Some(flag) => flag,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'listProcedures' must be a boolean",
                );
            }
        },
    };
    if list_members {
        return match al_analysis::queries::source::list_members(
            workspace,
            name,
            kind_filter,
            package_filter,
        ) {
            Ok(result) => Response {
                id,
                result: Some(
                    serde_json::to_value(&result).expect("member list must be JSON serializable"),
                ),
                error: None,
                ..Default::default()
            },
            Err(error) => rpc_error(id, error_codes::INVALID_PARAMS, &error.to_string()),
        };
    }

    let proc_filter = match optional_non_empty_string(params, "proc").and_then(|proc| match proc {
        Some(proc) => Ok(Some(proc)),
        None => optional_non_empty_string(params, "procedure"),
    }) {
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
    // `event_source` falls back to reading the file from disk when it is not
    // in the parse cache, so the path is contained like any other.
    let file = match crate::server::daemon::containment::resolve_within_project(
        workspace,
        std::path::Path::new(file),
    ) {
        Ok(file) => file,
        Err(message) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("'file' {message}"),
            )
        }
    };
    match al_analysis::queries::source::event_source(
        workspace,
        &file,
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

    /// The codeunit is the second object of its file. The kind filter was
    /// applied to the file's first object, a table, and the line was always 1.
    #[test]
    fn location_finds_the_second_object_of_a_file_and_its_line() {
        let ws = empty_ws();
        ws.file_index.add_file(
            std::path::PathBuf::from("/project/Posting.al"),
            "table 50200 \"Posting Buffer\"\n{\n}\n\ncodeunit 50100 \"Posting Mgt\"\n{\n}\n"
                .to_string(),
        );
        let response = dispatch_location(
            &ws,
            6,
            &serde_json::json!({ "name": "Posting Mgt", "kind": "codeunit" }),
        );
        assert!(response.error.is_none(), "{:?}", response.error);
        let location = response.result.expect("location");
        assert_eq!(location["path"], "/project/Posting.al");
        assert_eq!(location["line"], 5, "the codeunit's declaration line");
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

    /// `al_call source` with `procedure` (the CLI flag's name) returned the
    /// whole codeunit: the key was ignored.
    #[test]
    fn source_refuses_a_parameter_it_does_not_take() {
        let ws = al_workspace::Workspace::new();
        let response = dispatch_source(
            &ws,
            1,
            &serde_json::json!({ "name": "Sales-Post", "procedre": "PostItemLine" }),
        );
        let error = response.error.expect("unknown key refused");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
        assert!(error.message.contains("'procedre'"), "{}", error.message);

        // `procedure` is accepted as `proc`: the lookup gets as far as the
        // missing object.
        let response = dispatch_source(
            &ws,
            2,
            &serde_json::json!({ "name": "Sales-Post", "procedure": "PostItemLine" }),
        );
        let message = response.error.expect("no such object here").message;
        assert!(!message.contains("does not take"), "{message}");
    }
}
