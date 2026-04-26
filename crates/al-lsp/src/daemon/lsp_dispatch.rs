//! LSP method dispatchers — hover, definition, references, completions, etc.

use al_core::workspace::Workspace;
use al_daemon_client::jsonrpc::{error_codes, Response, RpcError};
use serde::Serialize;

use super::{extract_position, extract_uri, invalid_params};

/// Sentinel package name for workspace-local objects (not from .app packages).
const WORKSPACE_PACKAGE: &str = "(workspace)";

/// Build a `Response` whose `result` is `value` serialised to JSON. On
/// serialisation failure log the error and return an `RpcError` so the
/// client surfaces the problem instead of silently receiving `null`.
fn ok_response<T: Serialize>(id: u64, value: &T, method: &str) -> Response {
    match serde_json::to_value(value) {
        Ok(v) => Response {
            id,
            result: Some(v),
            error: None,
        },
        Err(e) => {
            tracing::error!(method, error = %e, "serialization failed for LSP result");
            Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("serialization failed for {method}: {e}"),
                }),
            }
        }
    }
}

pub(super) async fn dispatch_hover(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let result = al_core::queries::hover::hover_full(workspace, &uri, position).await;
    let value = result.and_then(|r| serde_json::to_value(&r).ok()); // SILENT: serialization of valid structs should not fail
    Response {
        id,
        result: value,
        error: None,
    }
}

pub(super) fn dispatch_definition(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let result = al_core::queries::definition::definition(workspace, &uri, position);
    match result {
        Some(locations) => ok_response(id, &locations, "textDocument/definition"),
        None => Response {
            id,
            result: None,
            error: None,
        },
    }
}

pub(super) fn dispatch_references(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let include_declaration = params
        .get("includeDeclaration")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let locations =
        al_core::queries::references::references(workspace, &uri, position, include_declaration);
    ok_response(id, &locations, "textDocument/references")
}

pub(super) fn dispatch_implementations(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let locations =
        al_core::queries::implementation::find_implementations(workspace, &uri, position);
    ok_response(id, &locations, "textDocument/implementation")
}

pub(super) async fn dispatch_completions(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let entries = al_core::queries::completions::completions_full(workspace, &uri, position).await;
    ok_response(id, &entries, "textDocument/completion")
}

pub(super) fn dispatch_signature_help(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let result = al_core::queries::signature::signature_help(workspace, &uri, position);
    let value = result.and_then(|sh| serde_json::to_value(&sh).ok()); // SILENT: serialization of valid structs should not fail
    Response {
        id,
        result: value,
        error: None,
    }
}

pub(super) fn dispatch_rename(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let Some(new_name) = params.get("newName").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let result = al_core::queries::rename::rename(workspace, &uri, position, new_name);
    match result {
        Some(we) => ok_response(id, &we, "textDocument/rename"),
        None => Response {
            id,
            result: None,
            error: None,
        },
    }
}

pub(super) fn dispatch_document_symbols(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    // Serialize the transport-agnostic AlDocumentSymbol vec directly. The daemon
    // returns JSON, so there is no need to round-trip through tower_lsp types.
    let result = al_core::queries::symbols::document_symbols(workspace, &uri);
    let value = result.and_then(|r| serde_json::to_value(r).ok()); // SILENT: serialization of valid structs should not fail
    Response {
        id,
        result: value,
        error: None,
    }
}

pub(super) fn dispatch_folding_ranges(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let result = al_core::queries::folding::folding_ranges(workspace, &uri).map(|ranges| {
        ranges
            .into_iter()
            .map(tower_lsp::lsp_types::FoldingRange::from)
            .collect::<Vec<_>>()
    });
    let value = result.and_then(|r| serde_json::to_value(r).ok()); // SILENT: serialization of valid structs should not fail
    Response {
        id,
        result: value,
        error: None,
    }
}

pub(super) fn dispatch_semantic_tokens(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let tokens = al_core::queries::semantic_tokens::semantic_tokens_full(workspace, &uri);
    ok_response(id, &tokens, "textDocument/semanticTokens/full")
}

pub(super) fn dispatch_inlay_hints(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let start_line = params
        .get("startLine")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let end_line = params
        .get("endLine")
        .and_then(|v| v.as_u64())
        .unwrap_or(u32::MAX as u64) as u32;
    let range = al_core::queries::Range {
        start: al_core::queries::Position {
            line: start_line,
            character: 0,
        },
        end: al_core::queries::Position {
            line: end_line,
            character: u32::MAX,
        },
    };
    let hints = al_core::queries::inlay_hints::inlay_hints(workspace, &uri, range).map(|h| {
        h.into_iter()
            .map(tower_lsp::lsp_types::InlayHint::from)
            .collect::<Vec<_>>()
    });
    let value = hints
        .and_then(|h| serde_json::to_value(&h).ok()) // SILENT: serialization of valid structs should not fail
        .unwrap_or(serde_json::json!([]));
    Response {
        id,
        result: Some(value),
        error: None,
    }
}

pub(super) fn dispatch_code_actions(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let range = al_core::queries::Range {
        start: position,
        end: position,
    };
    let actions = al_core::queries::code_actions::source_actions(workspace, &uri, range);
    ok_response(id, &actions, "textDocument/codeAction")
}

// ---------------------------------------------------------------------------
// Symbol queries
// ---------------------------------------------------------------------------

pub(super) fn dispatch_search(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(query) = params.get("query").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
    const MAX_SEARCH_RESULTS: usize = 500_000;
    let limit = limit.min(MAX_SEARCH_RESULTS);
    // Package symbols
    let results = workspace.symbols.search(query, limit);
    let mut value: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
    // Workspace file objects — use al-core search to avoid duplicating the filter logic.
    let remaining = limit.saturating_sub(value.len());
    let ws_results = al_core::queries::search::workspace_search(workspace, query, remaining);
    for r in ws_results {
        value.push(workspace_object_to_json(&r.info));
    }
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
    }
}

pub(super) fn dispatch_object(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let kind = match super::parse_object_kind(id, kind_str) {
        Ok(k) => k,
        Err(e) => return e,
    };
    // Package symbols
    let candidates = workspace.symbols.get_by_name(name);
    let mut matches: Vec<serde_json::Value> = candidates
        .iter()
        .filter(|e| e.kind == kind)
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
    // Workspace file objects
    let name_lower = name.to_lowercase();
    let kind_lower = kind.to_string().to_lowercase();
    for entry in workspace.file_index.object_info.iter() {
        let info = entry.value();
        if info.name.eq_ignore_ascii_case(&name_lower)
            && info.kind.eq_ignore_ascii_case(&kind_lower)
        {
            matches.push(workspace_object_to_json(info));
        }
    }
    if matches.is_empty() {
        Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} named '{}'", kind, name),
            }),
        }
    } else {
        Response {
            id,
            result: Some(serde_json::json!(matches)),
            error: None,
        }
    }
}

/// Convert a workspace CachedObjectInfo to JSON matching SymbolEntry shape.
fn workspace_object_to_json(info: &al_core::file_index::CachedObjectInfo) -> serde_json::Value {
    serde_json::json!({
        "kind": info.kind,
        "id": info.id.unwrap_or(0),
        "name": info.name,
        "package": WORKSPACE_PACKAGE,
    })
}

pub(super) fn dispatch_by_id(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(obj_id) = params.get("id").and_then(|v| v.as_i64()) else {
        return invalid_params(id);
    };
    let kind = match super::parse_object_kind(id, kind_str) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let results = workspace.symbols.get_by_id(kind, obj_id as i32);
    let value: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
    if value.is_empty() {
        Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} with id {}", kind, obj_id),
            }),
        }
    } else {
        Response {
            id,
            result: Some(serde_json::json!(value)),
            error: None,
        }
    }
}

pub(super) fn dispatch_events(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let results = workspace.symbols.get_events(name);
    let publishers: Vec<serde_json::Value> = results
        .publishers
        .iter()
        .map(|p| {
            serde_json::json!({
                "objectKind": p.object.kind.to_string(),
                "objectName": p.object.name,
                "methodName": p.method.name,
                "eventType": p.event_type.to_string(),
                "parameters": p.method.parameters.iter().map(|param| serde_json::json!({
                    "name": param.name,
                    "type_name": param.type_name,
                    "is_var": param.is_var,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!(publishers)),
        error: None,
    }
}

pub(super) fn dispatch_subscribers(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(event) = params.get("event").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let results = workspace.symbols.get_events(event);
    let subscribers: Vec<serde_json::Value> = results
        .subscribers
        .iter()
        .map(|s| {
            serde_json::json!({
                "objectName": s.object.name,
                "methodName": s.method.name,
                "targetObjectType": s.target_object_type,
                "targetObjectName": s.target_object_name,
                "targetEventName": s.target_event_name,
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!(subscribers)),
        error: None,
    }
}

pub(super) fn dispatch_composed(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let kind = match super::parse_object_kind(id, kind_str) {
        Ok(k) => k,
        Err(e) => return e,
    };
    match workspace.symbols.get_composed_cached(kind, name) {
        Some(composed) => {
            let value = serde_json::to_value(composed.as_ref()).unwrap_or(serde_json::Value::Null);
            Response {
                id,
                result: Some(value),
                error: None,
            }
        }
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} named '{}' or no extensions found", kind, name),
            }),
        },
    }
}

pub(super) fn dispatch_packages(workspace: &Workspace, id: u64) -> Response {
    let pkgs = match workspace.package_info.read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Lock poisoned".to_string(),
                }),
            };
        }
    };
    let value = serde_json::to_value(pkgs.as_slice()).unwrap_or(serde_json::json!([]));
    Response {
        id,
        result: Some(value),
        error: None,
    }
}

pub(super) fn dispatch_deps(workspace: &Workspace, id: u64) -> Response {
    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "Workspace is initializing, try again".to_string(),
                }),
            };
        }
    };
    match project.as_ref() {
        Some(p) => {
            let deps: Vec<serde_json::Value> = p
                .app_json
                .dependencies
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "id": d.id,
                        "name": d.name,
                        "publisher": d.publisher,
                        "version": d.version,
                    })
                })
                .collect();
            let all_deps: Vec<serde_json::Value> = p
                .all_dependencies()
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "id": d.id,
                        "name": d.name,
                        "publisher": d.publisher,
                        "version": d.version,
                    })
                })
                .collect();
            Response {
                id,
                result: Some(serde_json::json!({
                    "explicit": deps,
                    "all": all_deps,
                    "project": {
                        "name": p.app_json.name,
                        "publisher": p.app_json.publisher,
                        "version": p.app_json.version,
                    }
                })),
                error: None,
            }
        }
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "No project loaded".to_string(),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ok_response` succeeds and produces an Ok JSON value for serialisable input.
    #[test]
    fn ok_response_serializes_value() {
        let resp = ok_response(7, &vec!["a", "b"], "test/method");
        assert_eq!(resp.id, 7);
        assert!(resp.error.is_none());
        assert_eq!(
            resp.result,
            Some(serde_json::json!(["a", "b"])),
            "expected serialised array"
        );
    }

    /// Custom `Serialize` impl that always returns an error — used to drive the
    /// serialisation-failure path in `ok_response`.
    struct AlwaysFails;

    impl serde::Serialize for AlwaysFails {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("intentional failure"))
        }
    }

    /// `ok_response` returns an `INTERNAL_ERROR` instead of silently emitting `null`
    /// when serialisation fails.
    #[test]
    fn ok_response_returns_rpc_error_on_serialization_failure() {
        let resp = ok_response(11, &AlwaysFails, "test/method");
        assert_eq!(resp.id, 11);
        assert!(
            resp.result.is_none(),
            "expected no result on serialisation failure"
        );
        let err = resp.error.expect("expected an RpcError");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("serialization failed"),
            "expected error message to mention serialization failure, got: {}",
            err.message
        );
    }
}
