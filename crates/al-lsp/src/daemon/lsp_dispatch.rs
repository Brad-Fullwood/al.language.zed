//! LSP method dispatchers — hover, definition, references, completions, etc.

use al_core::workspace::Workspace;
use al_core::jsonrpc::{error_codes, Response, RpcError};

use super::{extract_uri, extract_position, invalid_params};

pub(super) fn dispatch_hover(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let result = al_core::queries::hover::hover(workspace, &uri, position);
    let value = result.map(|r| serde_json::json!({
        "contents": r.contents,
        "range": r.range.map(|rng| serde_json::json!({
            "start": { "line": rng.start.line, "character": rng.start.character },
            "end": { "line": rng.end.line, "character": rng.end.character },
        })),
    }));
    Response { id, result: value, error: None }
}

pub(super) fn dispatch_definition(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let result = al_core::queries::definition::definition(workspace, &uri, position);
    let value = result.map(|locations| {
        serde_json::json!(locations.iter().map(|l| serde_json::json!({
            "uri": l.uri.as_str(),
            "range": {
                "start": { "line": l.range.start.line, "character": l.range.start.character },
                "end": { "line": l.range.end.line, "character": l.range.end.character },
            }
        })).collect::<Vec<_>>())
    });
    Response { id, result: value, error: None }
}

pub(super) fn dispatch_references(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let include_declaration = params.get("includeDeclaration").and_then(|v| v.as_bool()).unwrap_or(true);
    let locations = al_core::queries::references::references(workspace, &uri, position, include_declaration);
    let value = serde_json::json!(locations.iter().map(|l| serde_json::json!({
        "uri": l.uri.as_str(),
        "range": {
            "start": { "line": l.range.start.line, "character": l.range.start.character },
            "end": { "line": l.range.end.line, "character": l.range.end.character },
        }
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

pub(super) fn dispatch_implementations(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let locations = al_core::queries::implementation::find_implementations(workspace, &uri, position);
    let value = serde_json::json!(locations.iter().map(|l| serde_json::json!({
        "uri": l.uri.as_str(),
        "range": {
            "start": { "line": l.range.start.line, "character": l.range.start.character },
            "end": { "line": l.range.end.line, "character": l.range.end.character },
        }
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

pub(super) fn dispatch_completions(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let entries = al_core::queries::completions::completions(workspace, &uri, position);
    let value = serde_json::json!(entries.iter().map(|e| serde_json::json!({
        "label": e.label,
        "kind": format!("{:?}", e.kind),
        "detail": e.detail,
        "sortText": e.sort_text,
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

pub(super) fn dispatch_signature_help(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let result = al_core::queries::signature::signature_help(workspace, &uri, position);
    let value = result.map(|sh| serde_json::json!({
        "signatures": sh.signatures.iter().map(|s| serde_json::json!({
            "label": s.label,
            "documentation": s.documentation,
            "parameters": s.parameters.iter().map(|p| serde_json::json!({
                "label": p.label,
            })).collect::<Vec<_>>(),
            "activeParameter": s.active_parameter,
        })).collect::<Vec<_>>(),
        "activeSignature": sh.active_signature,
        "activeParameter": sh.active_parameter,
    }));
    Response { id, result: value, error: None }
}

pub(super) fn dispatch_rename(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let Some(new_name) = params.get("newName").and_then(|v| v.as_str()) else { return invalid_params(id); };
    let result = al_core::queries::rename::rename(workspace, &uri, position, new_name);
    let value = result.map(|we| {
        let changes: serde_json::Map<String, serde_json::Value> = we.changes.iter().map(|(uri, edits)| {
            (uri.as_str().to_string(), serde_json::json!(edits.iter().map(|e| serde_json::json!({
                "range": {
                    "start": { "line": e.range.start.line, "character": e.range.start.character },
                    "end": { "line": e.range.end.line, "character": e.range.end.character },
                },
                "newText": e.new_text,
            })).collect::<Vec<_>>()))
        }).collect();
        serde_json::json!({ "changes": changes })
    });
    Response { id, result: value, error: None }
}

pub(super) fn dispatch_document_symbols(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let result = al_core::queries::symbols::document_symbols(workspace, &uri);
    let value = result.and_then(|r| serde_json::to_value(r).ok()); // SILENT: serialization of valid structs should not fail
    Response { id, result: value, error: None }
}

pub(super) fn dispatch_folding_ranges(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let result = al_core::queries::folding::folding_ranges(workspace, &uri);
    let value = result.and_then(|r| serde_json::to_value(r).ok()); // SILENT: serialization of valid structs should not fail
    Response { id, result: value, error: None }
}

pub(super) fn dispatch_semantic_tokens(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let tokens = al_core::queries::semantic_tokens::semantic_tokens_full(workspace, &uri);
    let value = serde_json::json!(tokens.iter().map(|t| serde_json::json!({
        "deltaLine": t.delta_line,
        "deltaStart": t.delta_start,
        "length": t.length,
        "tokenType": t.token_type,
        "tokenModifiers": t.token_modifiers,
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

pub(super) fn dispatch_inlay_hints(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let start_line = params.get("startLine").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let end_line = params.get("endLine").and_then(|v| v.as_u64()).unwrap_or(u32::MAX as u64) as u32;
    let range = tower_lsp::lsp_types::Range {
        start: tower_lsp::lsp_types::Position::new(start_line, 0),
        end: tower_lsp::lsp_types::Position::new(end_line, u32::MAX),
    };
    let hints = al_core::queries::inlay_hints::inlay_hints(workspace, &uri, range);
    let value = hints
        .and_then(|h| serde_json::to_value(&h).ok()) // SILENT: serialization of valid structs should not fail
        .unwrap_or(serde_json::json!([]));
    Response { id, result: Some(value), error: None }
}

pub(super) fn dispatch_code_actions(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(uri) = extract_uri(params) else { return invalid_params(id); };
    let Some(position) = extract_position(params) else { return invalid_params(id); };
    let range = al_core::queries::Range { start: position, end: position };
    let actions = al_core::queries::code_actions::source_actions(workspace, &uri, range);
    let value = serde_json::json!(actions.iter().map(|a| serde_json::json!({
        "title": a.title,
        "kind": format!("{:?}", a.kind),
    })).collect::<Vec<_>>());
    Response { id, result: Some(value), error: None }
}

// ---------------------------------------------------------------------------
// Symbol queries
// ---------------------------------------------------------------------------

pub(super) fn dispatch_search(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
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
    // Workspace file objects
    let query_lower = query.to_lowercase();
    for entry in workspace.file_index.object_info.iter() {
        if value.len() >= limit {
            break;
        }
        let info = entry.value();
        if !query.is_empty() && !info.name.to_lowercase().contains(&query_lower) {
            continue;
        }
        value.push(workspace_object_to_json(info));
    }
    Response { id, result: Some(serde_json::json!(value)), error: None }
}

pub(super) fn dispatch_object(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Ok(kind) = kind_str.parse::<al_core::symbols::ObjectKind>() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown object kind: {}", kind_str),
            }),
        };
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
    for entry in workspace.file_index.object_info.iter() {
        let info = entry.value();
        if info.name.to_lowercase() == name_lower
            && info.kind.to_lowercase() == kind.to_string().to_lowercase()
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
        Response { id, result: Some(serde_json::json!(matches)), error: None }
    }
}

/// Convert a workspace CachedObjectInfo to JSON matching SymbolEntry shape.
fn workspace_object_to_json(info: &al_core::file_index::CachedObjectInfo) -> serde_json::Value {
    serde_json::json!({
        "kind": info.kind,
        "id": info.id.unwrap_or(0),
        "name": info.name,
        "package": "(workspace)",
    })
}

pub(super) fn dispatch_by_id(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(obj_id) = params.get("id").and_then(|v| v.as_i64()) else {
        return invalid_params(id);
    };
    let Ok(kind) = kind_str.parse::<al_core::symbols::ObjectKind>() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown object kind: {}", kind_str),
            }),
        };
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
        Response { id, result: Some(serde_json::json!(value)), error: None }
    }
}

pub(super) fn dispatch_events(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
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
    Response { id, result: Some(serde_json::json!(publishers)), error: None }
}

pub(super) fn dispatch_subscribers(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
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
    Response { id, result: Some(serde_json::json!(subscribers)), error: None }
}

pub(super) fn dispatch_composed(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let Some(kind_str) = params.get("kind").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Ok(kind) = kind_str.parse::<al_core::symbols::ObjectKind>() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown object kind: {}", kind_str),
            }),
        };
    };
    match workspace.symbols.get_composed_cached(kind, name) {
        Some(composed) => {
            let value = serde_json::to_value(composed.as_ref()).unwrap_or(serde_json::Value::Null);
            Response { id, result: Some(value), error: None }
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
    Response { id, result: Some(value), error: None }
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
