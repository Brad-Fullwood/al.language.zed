//! LSP method dispatchers — hover, definition, references, completions, etc.

use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response, RpcError};
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
            ..Default::default()
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
                ..Default::default()
            }
        }
    }
}

/// `ok_response` variant for `Option<T>` results. `None` is encoded as a
/// JSON-RPC `null` result (no error). `Some(v)` defers to `ok_response`.
fn ok_response_opt<T: Serialize>(id: u64, value: Option<T>, method: &str) -> Response {
    match value {
        Some(v) => ok_response(id, &v, method),
        None => Response {
            id,
            result: None,
            error: None,
            ..Default::default()
        },
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
    let result = crate::queries::hover::hover_full(workspace, &uri, position).await;
    ok_response_opt(id, result, "textDocument/hover")
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
    let result = crate::queries::definition::definition(workspace, &uri, position);
    match result {
        Some(locations) => ok_response(id, &locations, "textDocument/definition"),
        None => Response {
            id,
            result: None,
            error: None,
            ..Default::default()
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
        crate::queries::references::references(workspace, &uri, position, include_declaration);
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
    let locations = crate::queries::implementation::find_implementations(workspace, &uri, position);
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
    let entries = crate::queries::completions::completions_full(workspace, &uri, position).await;
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
    let result = crate::queries::signature::signature_help(workspace, &uri, position);
    ok_response_opt(id, result, "textDocument/signatureHelp")
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
    let result = crate::queries::rename::rename(workspace, &uri, position, new_name);
    match result {
        Some(we) => ok_response(id, &we, "textDocument/rename"),
        None => Response {
            id,
            result: None,
            error: None,
            ..Default::default()
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
    let result = crate::queries::symbols::document_symbols(workspace, &uri);
    ok_response_opt(id, result, "textDocument/documentSymbol")
}

pub(super) fn dispatch_folding_ranges(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let result = crate::queries::folding::folding_ranges(workspace, &uri);
    ok_response_opt(id, result, "textDocument/foldingRange")
}

pub(super) fn dispatch_semantic_tokens(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = extract_uri(params) else {
        return invalid_params(id);
    };
    let tokens = crate::queries::semantic_tokens::semantic_tokens_full(workspace, &uri);
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
    // Cap to u32::MAX to prevent silent truncation of attacker-controlled
    // line values (matches extract_position in mod.rs). An out-of-range
    // startLine/endLine is rejected with INVALID_PARAMS rather than wrapping
    // to a nonsensical line number.
    let start_line = match params.get("startLine") {
        None => 0,
        Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) => n,
            None => return invalid_params(id),
        },
    };
    let end_line = match params.get("endLine") {
        None => u32::MAX,
        Some(v) => match v.as_u64().and_then(|n| u32::try_from(n).ok()) {
            Some(n) => n,
            None => return invalid_params(id),
        },
    };
    let range = crate::queries::Range {
        start: crate::queries::Position {
            line: start_line,
            character: 0,
        },
        end: crate::queries::Position {
            line: end_line,
            character: u32::MAX,
        },
    };
    let hints =
        crate::queries::inlay_hints::inlay_hints(workspace, &uri, range).unwrap_or_default();
    match serde_json::to_value(&hints) {
        Ok(v) => Response {
            id,
            result: Some(v),
            error: None,
            ..Default::default()
        },
        Err(e) => {
            tracing::error!(method = "textDocument/inlayHint", error = %e, "serialization failed");
            Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("serialization failed for textDocument/inlayHint: {e}"),
                }),
                ..Default::default()
            }
        }
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
    let range = crate::queries::Range {
        start: position,
        end: position,
    };
    let actions = crate::queries::code_actions::source_actions(workspace, &uri, range);
    ok_response(id, &actions, "textDocument/codeAction")
}

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
    // FB-1: `summary: true` strips member arrays (methods/fields/controls/
    // enum values/keys/properties/variables) from the response. A full dump
    // of a real workspace is ~60 MB of JSON and took seconds at every TUI
    // start; the browser list only needs identity fields and fetches
    // members lazily per selected object.
    let summary = params
        .get("summary")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let results = workspace.symbols.search(query, limit);
    let mut value: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| {
            if summary {
                Some(serde_json::json!({
                    "kind": e.kind,
                    "id": e.id,
                    "name": e.name,
                    "package": e.package,
                    "extends": e.extends,
                }))
            } else {
                serde_json::to_value(e.as_ref()).ok() // SILENT: serialization of valid structs should not fail
            }
        })
        .collect();
    // Workspace file objects — use al-core search to avoid duplicating the filter logic.
    let remaining = limit.saturating_sub(value.len());
    let ws_results = crate::queries::search::workspace_search(workspace, query, remaining);
    for r in ws_results {
        value.push(workspace_object_to_json(&r.info));
    }
    dedup_objects_by_identity(&mut value);
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}

/// Resolve an object kind from a bare name: succeeds when exactly one
/// non-synthetic kind matches; returns an actionable error response
/// otherwise. Shared by `object` and `composed` when the caller (an
/// editor task with only the symbol under the cursor) omits the kind.
// Err is a ready-to-send JSON-RPC `Response` by design (callers just return it
// on a cold error path); boxing it would scatter `*` derefs across every
// dispatcher for no real benefit.
#[allow(clippy::result_large_err)]
fn resolve_unique_kind_by_name(
    workspace: &Workspace,
    id: u64,
    name: &str,
    command: &str,
) -> std::result::Result<crate::symbols::ObjectKind, Response> {
    let mut kinds: Vec<crate::symbols::ObjectKind> = workspace
        .symbols
        .get_by_name(name)
        .iter()
        .filter(|e| !e.synthetic)
        .map(|e| e.kind)
        .collect();
    kinds.sort();
    kinds.dedup();
    match kinds.as_slice() {
        [single] => Ok(*single),
        [] => Err(Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Object '{name}' not found in any package"),
            }),
            ..Default::default()
        }),
        many => {
            let list = many
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: format!(
                        "'{name}' is ambiguous — specify the kind ({list}), e.g. `{command} table \"{name}\"`"
                    ),
                }),
                ..Default::default()
            })
        }
    }
}

pub(super) fn dispatch_object(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    // `kind` is optional (audit 2026-06-12): editor tasks only have the
    // symbol under the cursor. When omitted, resolve by name — unambiguous
    // single-kind matches proceed; multi-kind matches get an actionable
    // error listing the candidates.
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some(kind_str) => match super::parse_object_kind(id, kind_str) {
            Ok(k) => k,
            Err(e) => return e,
        },
        None => match resolve_unique_kind_by_name(workspace, id, name, "object") {
            Ok(k) => k,
            Err(resp) => return resp,
        },
    };
    let candidates = workspace.symbols.get_by_name(name);
    let mut matches: Vec<serde_json::Value> = candidates
        .iter()
        .filter(|e| e.kind == kind)
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
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
    dedup_objects_by_identity(&mut matches);
    if matches.is_empty() {
        Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} named '{}'", kind, name),
            }),
            ..Default::default()
        }
    } else {
        Response {
            id,
            result: Some(serde_json::json!(matches)),
            error: None,
            ..Default::default()
        }
    }
}

/// Drop duplicate object entries that represent the SAME object surfaced by
/// both the symbol index and the workspace file index.
///
/// Workspace objects are indexed in `workspace.symbols` (package `"workspace"`)
/// AND in `file_index.object_info` (package `"(workspace)"`), so the naive
/// merge in `dispatch_search`/`dispatch_object`/`dispatch_by_id` listed each
/// workspace object twice (audit 2026-06-20). Object IDs are unique across an
/// app plus its dependencies, so `(kind, id, name)` identifies an object
/// regardless of which index produced it. The first occurrence — the richer
/// symbol-index entry, which carries members — wins.
fn dedup_objects_by_identity(objects: &mut Vec<serde_json::Value>) {
    let mut seen = std::collections::HashSet::new();
    objects.retain(|o| {
        let kind = o.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        let id = o.get("id").and_then(serde_json::Value::as_i64).unwrap_or(0);
        let name = o
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        seen.insert((kind.to_string(), id, name))
    });
}

/// Convert a workspace CachedObjectInfo to JSON matching SymbolEntry shape.
///
/// `info.kind` is the tree-sitter node kind (lowercase, e.g. "table"). The wire
/// schema for SymbolEntry uses the crate::symbols ObjectKind enum, whose serde
/// representation is PascalCase. Normalize via `ObjectKind::from_str` so the
/// payload deserializes cleanly on al-cli / al-explorer.
fn workspace_object_to_json(info: &crate::file_index::CachedObjectInfo) -> serde_json::Value {
    let kind_value = info
        .kind
        .parse::<crate::symbols::ObjectKind>()
        .ok()
        .and_then(|k| serde_json::to_value(k).ok())
        .unwrap_or_else(|| serde_json::Value::String(info.kind.clone()));
    serde_json::json!({
        "kind": kind_value,
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
    // Reject overflowing object IDs (>2³¹-1) instead of silently wrapping to a
    // negative i32 — the resulting `get_by_id` would either miss legitimate
    // objects or hit unintended ones.
    let Some(obj_id) = super::extract_i32(params, "id") else {
        return invalid_params(id);
    };
    let kind = match super::parse_object_kind(id, kind_str) {
        Ok(k) => k,
        Err(e) => return e,
    };
    let results = workspace.symbols.get_by_id(kind, obj_id);
    let mut value: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|e| serde_json::to_value(e.as_ref()).ok()) // SILENT: serialization of valid structs should not fail
        .collect();
    // Workspace file objects (F-OPEN-268) — same merge as dispatch_object.
    let kind_lower = kind.to_string().to_lowercase();
    for entry in workspace.file_index.object_info.iter() {
        let info = entry.value();
        if info.id == Some(i64::from(obj_id)) && info.kind.eq_ignore_ascii_case(&kind_lower) {
            value.push(workspace_object_to_json(info));
        }
    }
    dedup_objects_by_identity(&mut value);
    if value.is_empty() {
        Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} with id {}", kind, obj_id),
            }),
            ..Default::default()
        }
    } else {
        Response {
            id,
            result: Some(serde_json::json!(value)),
            error: None,
            ..Default::default()
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
    // F-OPEN-268: workspace methods (with their [IntegrationEvent]/
    // [EventSubscriber] attributes) only enter the SymbolIndex via the
    // call-graph enrichment pass — trigger the cached build before querying
    // so the user's own publishers are visible, not just package symbols.
    let _ = workspace.get_or_build_call_graph();
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
        ..Default::default()
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
    // F-OPEN-268: see dispatch_events — workspace subscribers need the
    // enrichment pass too.
    let _ = workspace.get_or_build_call_graph();
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
        ..Default::default()
    }
}

pub(super) fn dispatch_composed(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    // `kind` is optional (audit 2026-06-12): the Zed task only has the
    // symbol under the cursor.
    let kind = match params.get("kind").and_then(|v| v.as_str()) {
        Some(kind_str) => match super::parse_object_kind(id, kind_str) {
            Ok(k) => k,
            Err(e) => return e,
        },
        None => match resolve_unique_kind_by_name(workspace, id, name, "composed") {
            Ok(k) => k,
            Err(resp) => return resp,
        },
    };
    // F-OPEN-268: composition must see workspace extensions/bases as well —
    // they enter the SymbolIndex via the enrichment pass.
    let _ = workspace.get_or_build_call_graph();
    match workspace.symbols.get_composed_cached(kind, name) {
        Some(composed) => {
            let value = serde_json::to_value(composed.as_ref()).unwrap_or(serde_json::Value::Null);
            Response {
                id,
                result: Some(value),
                error: None,
                ..Default::default()
            }
        }
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("No {} named '{}' or no extensions found", kind, name),
            }),
            ..Default::default()
        },
    }
}

pub(super) fn dispatch_packages(workspace: &Workspace, id: u64) -> Response {
    // F-OPEN-069: recover from a poisoned lock via the workspace.rs-wide
    // pattern (`unwrap_or_else(|e| e.into_inner())`). A single poisoned
    // lock no longer permanently bricks this endpoint; the stale data
    // visible after recovery is the same data the panicking writer was
    // about to commit, so reads remain consistent with the rest of the
    // workspace.
    let pkgs = workspace
        .package_info
        .read()
        .unwrap_or_else(|e| e.into_inner());
    let value = serde_json::to_value(pkgs.as_slice()).unwrap_or(serde_json::json!([]));
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
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
                ..Default::default()
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
                ..Default::default()
            }
        }
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "No project loaded".to_string(),
            }),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_objects_by_identity_drops_same_object_from_two_indices() {
        // Regression (audit 2026-06-20): workspace objects appear in both the
        // symbol index (package "workspace") and the file index (package
        // "(workspace)"), so the merged search/object/by-id result listed each
        // one twice. (kind, id, name) identifies an object regardless of which
        // index produced it.
        let mut objects = vec![
            serde_json::json!({"kind": "Table", "id": 50100, "name": "Customer", "package": "workspace"}),
            serde_json::json!({"kind": "Table", "id": 50100, "name": "Customer", "package": "(workspace)"}),
            serde_json::json!({"kind": "Codeunit", "id": 50100, "name": "Mgmt", "package": "workspace"}),
        ];
        dedup_objects_by_identity(&mut objects);
        assert_eq!(objects.len(), 2, "duplicate table must collapse to one");
        // The first (richer symbol-index) entry wins.
        assert_eq!(objects[0]["package"], "workspace");
        assert_eq!(objects[1]["name"], "Mgmt");
    }

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

    /// Workspace-object payloads must deserialize as `crate::symbols::SymbolEntry` so
    /// downstream daemon clients (al-cli, al-explorer) accept them. The `kind`
    /// field arrives from tree-sitter as a lowercase string but the wire schema
    /// is the PascalCase `ObjectKind` enum — regression test for the
    /// daemon→explorer launch failure observed in cycle 4.
    #[test]
    fn workspace_object_to_json_round_trips_through_symbol_entry() {
        let info = crate::file_index::CachedObjectInfo {
            kind: "table".to_string(),
            id: Some(50_000),
            name: "Customer".to_string(),
            range: tree_sitter::Range {
                start_byte: 0,
                end_byte: 0,
                start_point: tree_sitter::Point { row: 0, column: 0 },
                end_point: tree_sitter::Point { row: 0, column: 0 },
            },
        };
        let json = workspace_object_to_json(&info);
        let entry: crate::symbols::SymbolEntry =
            serde_json::from_value(json).expect("workspace object must deserialize as SymbolEntry");
        assert_eq!(entry.kind, crate::symbols::ObjectKind::Table);
        assert_eq!(entry.name, "Customer");
        assert_eq!(entry.id, 50_000);
    }

    #[test]
    fn workspace_object_to_json_handles_all_object_kinds() {
        // Data-driven: iterate every object kind language_data knows about, so a
        // newly-extracted AL object type is exercised automatically instead of
        // being silently omitted (the old static list had drifted — it was missing
        // profileextension and dotnet for exactly this reason). Fires before users
        // hit a launch-time crash if the ObjectKind normaliser ever misses a kind.
        //
        // `value` (kw_value) is the one language_data entry that is NOT a top-level
        // object — it has no ObjectKind and find_object_declaration never emits it.
        // A NEW non-object pseudo-entry would fail here, prompting either an
        // ObjectKind addition or an explicit exclusion — the correct prompt.
        const NON_OBJECT_KEYWORDS: &[&str] = &["value"];
        let mut covered = 0;
        for ot in crate::syntax::language_data::object_types() {
            let k = ot.keyword.as_str();
            if NON_OBJECT_KEYWORDS.contains(&k) {
                continue;
            }
            covered += 1;
            let info = crate::file_index::CachedObjectInfo {
                kind: k.to_string(),
                id: Some(1),
                name: "X".to_string(),
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 0,
                    start_point: tree_sitter::Point { row: 0, column: 0 },
                    end_point: tree_sitter::Point { row: 0, column: 0 },
                },
            };
            let json = workspace_object_to_json(&info);
            let _: crate::symbols::SymbolEntry = serde_json::from_value(json)
                .unwrap_or_else(|e| panic!("kind {k:?} must deserialize: {e}"));
        }
        // Floor so the test can't silently degrade to covering nothing if the
        // data source or exclusion list changes (20 ObjectKind variants today).
        assert!(
            covered >= 20,
            "expected >= 20 object kinds covered, only {covered}"
        );
    }

    /// F-OPEN-268: `by-id` must find workspace source objects, not only .app
    /// package symbols. `object` (lookup by name) already merges the workspace
    /// file index; `by-id codeunit 50100` returned "No Codeunit with id 50100"
    /// for an object that `object codeunit "Hello World"` found.
    #[test]
    fn dispatch_by_id_finds_workspace_objects() {
        let ws = crate::workspace::Workspace::new();
        ws.file_index.object_info.insert(
            std::path::PathBuf::from("/proj/src/HelloWorld.al"),
            crate::file_index::CachedObjectInfo {
                kind: "codeunit".to_string(),
                id: Some(50_100),
                name: "Hello World".to_string(),
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 0,
                    start_point: tree_sitter::Point { row: 0, column: 0 },
                    end_point: tree_sitter::Point { row: 0, column: 0 },
                },
            },
        );
        let resp = dispatch_by_id(
            &ws,
            1,
            &serde_json::json!({"kind": "codeunit", "id": 50_100}),
        );
        assert!(
            resp.error.is_none(),
            "by-id must find workspace objects: {:?}",
            resp.error
        );
        let value = resp.result.expect("result");
        let arr = value.as_array().expect("array result");
        assert_eq!(arr.len(), 1, "exactly the one workspace object: {arr:?}");
        assert_eq!(arr[0]["name"], "Hello World");
        assert_eq!(arr[0]["package"], WORKSPACE_PACKAGE);
    }

    /// F-OPEN-268: `events` must surface WORKSPACE event publishers, not only
    /// .app package symbols. Workspace methods only enter the SymbolIndex via
    /// the call-graph enrichment pass — which nothing on the events path
    /// triggered, so `al-explorer events OnBeforeProcess` missed publishers
    /// defined in the user's own project.
    #[test]
    fn dispatch_events_finds_workspace_publishers() {
        let ws = crate::workspace::Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/Pub.al"),
            r#"codeunit 50101 "Test Event Publisher"
{
    [IntegrationEvent(false, false)]
    procedure OnBeforeProcess(var InputValue: Text; var IsHandled: Boolean)
    begin
    end;
}
"#
            .to_string(),
        );
        let resp = dispatch_events(&ws, 1, &serde_json::json!({"name": "OnBeforeProcess"}));
        assert!(resp.error.is_none(), "events errored: {:?}", resp.error);
        let value = resp.result.expect("result");
        let arr = value.as_array().expect("array");
        assert!(
            arr.iter().any(|p| p["objectName"] == "Test Event Publisher"
                && p["methodName"] == "OnBeforeProcess"),
            "workspace publisher must be listed; got: {arr:?}"
        );
    }

    /// F-OPEN-268: `composed` must merge a WORKSPACE base table with its
    /// WORKSPACE extension — previously it returned "No Table named ... or no
    /// extensions found" because neither object was in the SymbolIndex.
    /// Audit 2026-06-12: the Zed task only has the symbol under the cursor,
    /// so `composed` must resolve the kind from a bare name when it is
    /// unambiguous, and explain itself when it is not.
    #[test]
    fn dispatch_composed_resolves_kind_from_bare_name() {
        let ws = crate::workspace::Workspace::new();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base".to_string(),
            ..Default::default()
        }]);
        // No "kind" param — unique name resolves.
        let resp = dispatch_composed(&ws, 7, &serde_json::json!({ "name": "Customer" }));
        assert!(
            resp.error.is_none(),
            "unique bare name must resolve: {:?}",
            resp.error
        );

        // Unknown name → actionable not-found error.
        let resp = dispatch_composed(&ws, 8, &serde_json::json!({ "name": "Nope" }));
        let err = resp.error.expect("unknown name must error");
        assert!(err.message.contains("not found"), "got: {}", err.message);

        // Two kinds sharing the name → ambiguity error listing kinds.
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Page,
            id: 21,
            name: "Customer".to_string(),
            package: "Base".to_string(),
            ..Default::default()
        }]);
        let resp = dispatch_composed(&ws, 9, &serde_json::json!({ "name": "Customer" }));
        let err = resp.error.expect("ambiguous name must error");
        assert!(
            err.message.contains("ambiguous") && err.message.contains("Table"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_composed_merges_workspace_table_and_extension() {
        let ws = crate::workspace::Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/TestCustomer.Table.al"),
            r#"table 50100 "Test Customer"
{
    fields
    {
        field(1; "No."; Code[20])
        {
        }
    }
}
"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/TestCustomerExt.TableExt.al"),
            r#"tableextension 50100 "Test Customer Ext" extends "Test Customer"
{
    fields
    {
        field(50100; "Custom Field"; Text[50])
        {
        }
    }
}
"#
            .to_string(),
        );
        let resp = dispatch_composed(
            &ws,
            1,
            &serde_json::json!({"kind": "table", "name": "Test Customer"}),
        );
        assert!(
            resp.error.is_none(),
            "composed must find the workspace base + extension: {:?}",
            resp.error
        );
        let text = resp.result.expect("result").to_string();
        assert!(
            text.contains("Custom Field"),
            "composed view must include the extension's field: {text}"
        );
    }

    /// F-OPEN-268 guard: an id that matches nothing still errors.
    #[test]
    fn dispatch_by_id_unknown_id_still_errors() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_by_id(&ws, 1, &serde_json::json!({"kind": "codeunit", "id": 1}));
        assert!(resp.error.is_some(), "unknown id must keep erroring");
    }

    /// An out-of-range `startLine` (> u32::MAX) must be rejected with
    /// INVALID_PARAMS rather than silently truncated via `as u32`. Mirrors the
    /// hardening verified by `extract_position_rejects_overflow` in mod.rs.
    #[test]
    fn dispatch_inlay_hints_rejects_overflow_start_line() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_inlay_hints(
            &ws,
            7,
            &serde_json::json!({
                "uri": "file:///tmp/x.al",
                "startLine": (u32::MAX as u64) + 1,
                "endLine": 10
            }),
        );
        let err = resp
            .error
            .expect("expected INVALID_PARAMS for overflow start");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(resp.result.is_none());
    }

    /// An out-of-range `endLine` must likewise be rejected, not wrapped.
    #[test]
    fn dispatch_inlay_hints_rejects_overflow_end_line() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_inlay_hints(
            &ws,
            8,
            &serde_json::json!({
                "uri": "file:///tmp/x.al",
                "startLine": 0,
                "endLine": (u32::MAX as u64) + 5
            }),
        );
        let err = resp
            .error
            .expect("expected INVALID_PARAMS for overflow end");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(resp.result.is_none());
    }

    /// Absent line params default to the full-document range (0..u32::MAX) and
    /// succeed — no document is open so the hints list is simply empty.
    #[test]
    fn dispatch_inlay_hints_defaults_lines_when_absent() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_inlay_hints(&ws, 9, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert!(
            resp.error.is_none(),
            "absent lines must default, not error: {:?}",
            resp.error
        );
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    #[test]
    fn ok_response_opt_none_yields_null_result_no_error() {
        let resp = ok_response_opt::<Vec<u8>>(3, None, "test/method");
        assert_eq!(resp.id, 3);
        assert!(resp.result.is_none(), "None must map to absent result");
        assert!(resp.error.is_none(), "None is not an error");
    }

    #[test]
    fn ok_response_opt_some_serializes_value() {
        let resp = ok_response_opt(4, Some(vec![1u8, 2, 3]), "test/method");
        assert_eq!(resp.id, 4);
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([1, 2, 3])));
    }

    fn assert_invalid_params(resp: &Response, id: u64) {
        assert_eq!(resp.id, id);
        assert!(
            resp.result.is_none(),
            "invalid params must not carry a result"
        );
        let err = resp.error.as_ref().expect("expected an RpcError");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn dispatch_definition_rejects_missing_uri() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_definition(&ws, 1, &serde_json::json!({ "line": 0, "character": 0 }));
        assert_invalid_params(&resp, 1);
    }

    #[test]
    fn dispatch_definition_rejects_missing_position() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_definition(&ws, 2, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 2);
    }

    #[test]
    fn dispatch_references_rejects_missing_position() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_references(&ws, 5, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 5);
    }

    #[test]
    fn dispatch_implementations_rejects_missing_uri() {
        let ws = crate::workspace::Workspace::new();
        let resp =
            dispatch_implementations(&ws, 6, &serde_json::json!({ "line": 0, "character": 0 }));
        assert_invalid_params(&resp, 6);
    }

    #[test]
    fn dispatch_signature_help_rejects_missing_position() {
        let ws = crate::workspace::Workspace::new();
        let resp =
            dispatch_signature_help(&ws, 7, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 7);
    }

    #[test]
    fn dispatch_document_symbols_rejects_missing_uri() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_document_symbols(&ws, 8, &serde_json::json!({}));
        assert_invalid_params(&resp, 8);
    }

    #[test]
    fn dispatch_code_actions_rejects_missing_position() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_code_actions(&ws, 9, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 9);
    }

    /// rename has an extra required `newName` parameter beyond uri/position.
    #[test]
    fn dispatch_rename_rejects_missing_new_name() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_rename(
            &ws,
            10,
            &serde_json::json!({ "uri": "file:///tmp/x.al", "line": 0, "character": 0 }),
        );
        assert_invalid_params(&resp, 10);
    }

    #[test]
    fn dispatch_search_rejects_missing_query() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_search(&ws, 11, &serde_json::json!({ "limit": 5 }));
        assert_invalid_params(&resp, 11);
    }

    #[test]
    fn dispatch_search_empty_workspace_returns_empty_array() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_search(&ws, 12, &serde_json::json!({ "query": "Customer" }));
        assert!(
            resp.error.is_none(),
            "search must not error: {:?}",
            resp.error
        );
        assert_eq!(
            resp.result,
            Some(serde_json::json!([])),
            "empty workspace yields no matches"
        );
    }

    /// An absurdly large `limit` must be clamped to MAX_SEARCH_RESULTS, not
    /// passed through verbatim (a u64 → usize that could exhaust memory).
    /// We can observe the clamp indirectly: the call succeeds and does not
    /// hang/allocate unboundedly. The value handed to the index is the
    /// min(limit, 500_000); we assert the request completes with an empty
    /// array on an empty workspace.
    #[test]
    fn dispatch_search_clamps_oversized_limit() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_search(
            &ws,
            13,
            &serde_json::json!({ "query": "x", "limit": u64::MAX }),
        );
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    #[test]
    fn dispatch_object_rejects_unknown_kind() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_object(
            &ws,
            14,
            &serde_json::json!({ "kind": "frobnicator", "name": "Foo" }),
        );
        let err = resp.error.expect("unknown kind must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(
            err.message.contains("frobnicator"),
            "message should name the bad kind: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_object_rejects_missing_name() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_object(&ws, 15, &serde_json::json!({ "kind": "table" }));
        assert_invalid_params(&resp, 15);
    }

    /// A valid kind+name with no matching object yields an INVALID_PARAMS error
    /// whose message names the object, not a silent empty success.
    #[test]
    fn dispatch_object_not_found_returns_error() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_object(
            &ws,
            16,
            &serde_json::json!({ "kind": "table", "name": "NoSuchTable" }),
        );
        assert!(resp.result.is_none());
        let err = resp.error.expect("not-found must be an error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(
            err.message.contains("NoSuchTable"),
            "message should name the missing object: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_by_id_rejects_overflowing_id() {
        let ws = crate::workspace::Workspace::new();
        // (i32::MAX as i64) + 1 must be rejected by extract_i32, not wrapped.
        let resp = dispatch_by_id(
            &ws,
            17,
            &serde_json::json!({ "kind": "table", "id": (i32::MAX as i64) + 1 }),
        );
        assert_invalid_params(&resp, 17);
    }

    #[test]
    fn dispatch_by_id_not_found_returns_error() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_by_id(
            &ws,
            18,
            &serde_json::json!({ "kind": "table", "id": 50000 }),
        );
        assert!(resp.result.is_none());
        let err = resp.error.expect("not-found must be an error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(
            err.message.contains("50000"),
            "message should name the missing id: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_composed_not_found_returns_error() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_composed(
            &ws,
            19,
            &serde_json::json!({ "kind": "table", "name": "Ghost" }),
        );
        assert!(resp.result.is_none());
        let err = resp.error.expect("not-found must be an error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Ghost"));
    }

    #[test]
    fn dispatch_events_rejects_missing_name() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_events(&ws, 20, &serde_json::json!({}));
        assert_invalid_params(&resp, 20);
    }

    #[test]
    fn dispatch_events_empty_returns_empty_array() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_events(&ws, 21, &serde_json::json!({ "name": "OnAfterPost" }));
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    #[test]
    fn dispatch_subscribers_rejects_missing_event() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_subscribers(&ws, 22, &serde_json::json!({}));
        assert_invalid_params(&resp, 22);
    }

    #[test]
    fn dispatch_subscribers_empty_returns_empty_array() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_subscribers(&ws, 23, &serde_json::json!({ "event": "OnAfterPost" }));
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    #[test]
    fn dispatch_packages_empty_returns_empty_array() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_packages(&ws, 24);
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    /// With no project loaded, deps reports an INTERNAL_ERROR rather than a
    /// bogus empty success — clients must distinguish "no project" from
    /// "project with zero dependencies".
    #[test]
    fn dispatch_deps_no_project_returns_internal_error() {
        let ws = crate::workspace::Workspace::new();
        let resp = dispatch_deps(&ws, 25);
        assert!(resp.result.is_none());
        let err = resp.error.expect("no project must be an error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("No project loaded"),
            "message: {}",
            err.message
        );
    }
}
