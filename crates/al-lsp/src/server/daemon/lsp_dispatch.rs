//! LSP method dispatchers — hover, definition, references, completions, etc.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};
use al_workspace::Workspace;
use serde::Serialize;

use super::{
    extract_position, invalid_params, optional_bool_param, optional_bounded_usize_param,
    read_document_from_params, rpc_error, serialized_response,
};

/// Sentinel package name for workspace-local objects (not from .app packages).
const WORKSPACE_PACKAGE: &str = "(workspace)";

/// [`serialized_response`] for `Option<T>` results. `None` is encoded as an
/// explicit `result: null`, not as an absent `result`: both `result` and
/// `error` carry `skip_serializing_if`, so `Response { result: None, error:
/// None }` serialises to `{"jsonrpc":"2.0","id":7}`, which JSON-RPC 2.0 §5
/// forbids.
fn ok_response_opt<T: Serialize>(id: u64, value: Option<T>, method: &str) -> Response {
    match value {
        Some(v) => serialized_response(id, &v, method),
        None => Response::null(id),
    }
}

pub(super) async fn dispatch_hover(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let result = match al_analysis::queries::hover::hover_full(workspace, &uri, position).await {
        Ok(result) => result,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("hover query failed: {error}"),
            );
        }
    };
    ok_response_opt(id, result, "textDocument/hover")
}

pub(super) fn dispatch_definition(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let result = al_analysis::queries::definition::definition(workspace, &uri, position);
    match result {
        Ok(Some(locations)) => serialized_response(id, &locations, "textDocument/definition"),
        Ok(None) => Response::null(id),
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("definition query failed: {error}"),
        ),
    }
}

pub(super) fn dispatch_references(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let include_declaration = match optional_bool_param(params, "includeDeclaration", true) {
        Ok(value) => value,
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error),
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let locations = match al_analysis::queries::references::references(
        workspace,
        &uri,
        position,
        include_declaration,
    ) {
        Ok(locations) => locations,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("references query failed: {error}"),
            );
        }
    };
    serialized_response(id, &locations, "textDocument/references")
}

pub(super) fn dispatch_implementations(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    // `None` (document not loaded) and an empty list are both an empty array
    // on the wire: the daemon already rejected an unknown document above.
    let locations =
        al_analysis::queries::implementation::find_implementations(workspace, &uri, position);
    ok_response_opt(id, locations, "textDocument/implementation")
}

pub(super) async fn dispatch_completions(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let entries = match al_analysis::queries::completions::completions_full(
        workspace, &uri, position,
    )
    .await
    {
        Ok(entries) => entries,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("completion query failed: {error}"),
            );
        }
    };
    serialized_response(id, &entries, "textDocument/completion")
}

pub(super) fn dispatch_signature_help(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let result = al_analysis::queries::signature::signature_help(workspace, &uri, position);
    match result {
        Ok(result) => ok_response_opt(id, result, "textDocument/signatureHelp"),
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("signature-help query failed: {error}"),
        ),
    }
}

pub(super) fn dispatch_rename(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let Some(new_name) = params.get("newName").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let result = al_analysis::queries::rename::rename(workspace, &uri, position, new_name);
    match result {
        Ok(Some(we)) => serialized_response(id, &we, "textDocument/rename"),
        Ok(None) => Response::null(id),
        Err(error @ al_analysis::queries::rename::RenameError::Collision { .. }) => {
            rpc_error(id, error_codes::INVALID_PARAMS, &error.to_string())
        }
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("rename query failed: {error}"),
        ),
    }
}

pub(super) fn dispatch_document_symbols(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    // Serialize the transport-agnostic AlDocumentSymbol vec directly. The daemon
    // returns JSON, so there is no need to round-trip through tower_lsp types.
    let result = al_analysis::queries::symbols::document_symbols(workspace, &uri);
    ok_response_opt(id, result, "textDocument/documentSymbol")
}

pub(super) fn dispatch_folding_ranges(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let result = al_analysis::queries::folding::folding_ranges(workspace, &uri);
    ok_response_opt(id, result, "textDocument/foldingRange")
}

pub(super) fn dispatch_semantic_tokens(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let tokens = al_analysis::queries::semantic_tokens::semantic_tokens_full(workspace, &uri);
    ok_response_opt(id, tokens, "textDocument/semanticTokens/full")
}

pub(super) fn dispatch_inlay_hints(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
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
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let range = al_analysis::queries::Range {
        start: al_analysis::queries::Position {
            line: start_line,
            character: 0,
        },
        end: al_analysis::queries::Position {
            line: end_line,
            character: u32::MAX,
        },
    };
    let hints = match al_analysis::queries::inlay_hints::inlay_hints(workspace, &uri, range) {
        Ok(Some(hints)) => hints,
        Ok(None) => Vec::new(),
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("inlay-hint query failed: {error}"),
            );
        }
    };
    serialized_response(id, &hints, "textDocument/inlayHint")
}

pub(super) fn dispatch_code_actions(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(position) = extract_position(params) else {
        return invalid_params(id);
    };
    let (uri, _supplied) = match read_document_from_params(workspace, params, id) {
        Ok(document) => document,
        Err(response) => return response,
    };
    let range = al_analysis::queries::Range {
        start: position,
        end: position,
    };
    let actions = al_analysis::queries::code_actions::source_actions(workspace, &uri, range);
    serialized_response(id, &actions, "textDocument/codeAction")
}

pub(super) fn dispatch_search(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(query) = params.get("query").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    const MAX_SEARCH_RESULTS: usize = 500_000;
    let limit = match optional_bounded_usize_param(params, "limit", 20, MAX_SEARCH_RESULTS) {
        Ok(limit) => limit,
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error),
    };
    // `summary: true` strips member arrays (methods/fields/controls/
    // enum values/keys/properties/variables) from the response. A full dump
    // of a real workspace is ~60 MB of JSON and took seconds at every TUI
    // start; the browser list only needs identity fields and fetches
    // members lazily per selected object.
    let summary = match optional_bool_param(params, "summary", false) {
        Ok(summary) => summary,
        Err(error) => return rpc_error(id, error_codes::INVALID_PARAMS, &error),
    };
    // `limit` and `offset` also page the result (projection.rs), which
    // needs every match to report `total` and `truncated`: capping the
    // search at `limit` told a caller asking for 3 of 8 matches that 3 was
    // all there was, and `offset 3` returned nothing. So a paged search
    // collects every match, and serializes in full only the rows that can
    // land in the page (with a margin for de-duplication below); the rest
    // are summaries the projection drops.
    const MAX_PAGED_MATCHES: usize = 10_000;
    let paged = params.get("limit").is_some() || params.get("offset").is_some();
    let offset = params
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(0);
    let (limit, full_rows) = if paged {
        (
            MAX_PAGED_MATCHES.max(limit),
            offset.saturating_add(limit).saturating_add(64),
        )
    } else {
        (limit, usize::MAX)
    };
    let results = workspace.symbols.search(query, limit);
    let mut value: Vec<serde_json::Value> = match results
        .iter()
        .enumerate()
        .map(|(row, entry)| symbol_entry_to_json(workspace, entry, summary || row >= full_rows))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("workspace search response serialization failed: {error}"),
            );
        }
    };
    // Use the shared search implementation for workspace file objects.
    let remaining = limit.saturating_sub(value.len());
    let ws_results = al_analysis::queries::search::workspace_search(workspace, query, remaining);
    for r in ws_results {
        match workspace_object_to_json(&r.info) {
            Ok(object) => value.push(object),
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("workspace search found invalid object metadata: {error}"),
                );
            }
        }
    }
    if let Err(error) = dedup_objects_by_identity(&mut value) {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("workspace search produced invalid object metadata: {error}"),
        );
    }
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}

fn symbol_entry_to_json(
    workspace: &Workspace,
    entry: &al_symbols::SymbolEntry,
    summary: bool,
) -> Result<serde_json::Value, String> {
    let mut value = if summary {
        serde_json::json!({
            "kind": entry.kind,
            "id": entry.id,
            "name": entry.name,
            "package": entry.package,
            "extends": entry.extends,
        })
    } else {
        serde_json::to_value(entry)
            .map_err(|error| format!("symbol entry is not JSON serializable: {error}"))?
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| "serialized symbol entry is not an object".to_string())?;
    object.insert(
        "source_availability".into(),
        source_availability_to_json(workspace, entry)?,
    );
    Ok(value)
}

fn source_availability_to_json(
    workspace: &Workspace,
    entry: &al_symbols::SymbolEntry,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(workspace.symbols.source_availability(entry))
        .map_err(|error| format!("source availability is not JSON serializable: {error}"))
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
) -> std::result::Result<al_symbols::ObjectKind, Response> {
    let mut kinds: Vec<al_symbols::ObjectKind> = workspace
        .symbols
        .get_by_name(name)
        .iter()
        .filter(|e| !e.synthetic)
        .map(|e| e.kind)
        .collect();
    for info in workspace.file_index.object_info.iter() {
        if !info.name.eq_ignore_ascii_case(name) {
            continue;
        }
        match workspace_object_identity(&info) {
            Ok((kind, _)) => kinds.push(kind),
            Err(error) => {
                return Err(rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("workspace object metadata is invalid: {error}"),
                ));
            }
        }
    }
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

/// Why the workspace objects in an `object` or `byId` answer have no fields
/// or methods, or `None` when they have them.
///
/// Workspace members enter the symbol index only when the call graph is
/// built, and the build waits for the dependency source index: a first
/// `by-id table 18` on a fresh daemon took 52.6 s on the medium benchmark
/// project while `search` answered in milliseconds. So a lookup uses the graph
/// when it is already built and builds it only for `waitForMembers: true`. A
/// build that fails still leaves the lookup answering with the object's
/// identity.
fn workspace_members_missing(workspace: &Workspace, method: &str, wait: bool) -> Option<String> {
    if !wait {
        return (!workspace.call_graph_is_current()).then(|| {
            "fields and methods of workspace objects load with the call graph, which is not \
             built for the current files yet. Ask again with waitForMembers: true \
             (al-explorer --wait-for-members) to wait for it."
                .to_string()
        });
    }
    match workspace.get_or_build_call_graph() {
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(
                method,
                %error,
                "workspace members are unavailable for this lookup; answering with object identity"
            );
            Some(format!(
                "fields and methods of workspace objects are missing because the call graph did \
                 not build: {error}"
            ))
        }
    }
}

/// Mark a workspace object answered without its members, the way
/// `suggestEvent` marks an answer that leaves something out.
fn mark_members_missing(object: &mut serde_json::Value, reason: &str) {
    if let Some(object) = object.as_object_mut() {
        object.insert("partial".into(), serde_json::Value::Bool(true));
        object.insert("partial_reason".into(), reason.into());
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
    let signatures = match optional_bool_param(params, "signatures", false) {
        Ok(signatures) => signatures,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let wait_for_members = match optional_bool_param(params, "waitForMembers", false) {
        Ok(wait) => wait,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    // `kind` is optional: editor tasks only have the
    // symbol under the cursor. When omitted, resolve by name — unambiguous
    // single-kind matches proceed; multi-kind matches get an actionable
    // error listing the candidates.
    let kind = match params.get("kind") {
        None => match resolve_unique_kind_by_name(workspace, id, name, "object") {
            Ok(k) => k,
            Err(resp) => return resp,
        },
        Some(value) => match value.as_str() {
            Some(kind_str) => match super::parse_object_kind(id, kind_str) {
                Ok(k) => k,
                Err(e) => return e,
            },
            None => return invalid_params(id),
        },
    };
    let name_lower = name.to_lowercase();
    let kind_lower = kind.to_string().to_lowercase();
    let mut workspace_objects = Vec::new();
    for entry in workspace.file_index.object_info.iter() {
        let info = entry.value();
        if info.name.eq_ignore_ascii_case(&name_lower)
            && info.kind.eq_ignore_ascii_case(&kind_lower)
        {
            match workspace_object_to_json(info) {
                Ok(object) => workspace_objects.push(object),
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("workspace object metadata is invalid: {error}"),
                    );
                }
            }
        }
    }
    let is_indexed_workspace_object = |entry: &al_symbols::SymbolEntry| {
        entry.kind == kind && al_symbols::source_availability::is_workspace_package(&entry.package)
    };
    let members_missing = if workspace_objects.is_empty()
        && !workspace
            .symbols
            .get_by_name(name)
            .iter()
            .any(|entry| is_indexed_workspace_object(entry))
    {
        None
    } else {
        workspace_members_missing(workspace, "object", wait_for_members)
    };
    let candidates = workspace.symbols.get_by_name(name);
    let mut matches: Vec<serde_json::Value> = match candidates
        .iter()
        .filter(|e| e.kind == kind)
        .filter(|e| members_missing.is_none() || !is_indexed_workspace_object(e))
        .map(|entry| symbol_entry_to_json(workspace, entry, false))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(matches) => matches,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("object response serialization failed: {error}"),
            );
        }
    };
    if let Some(reason) = &members_missing {
        for object in &mut workspace_objects {
            mark_members_missing(object, reason);
        }
    }
    matches.extend(workspace_objects);
    if let Err(error) = dedup_objects_by_identity(&mut matches) {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("object lookup produced invalid metadata: {error}"),
        );
    }
    if signatures {
        matches.iter_mut().for_each(member_signatures);
    }
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

/// Render an object's members as one line each, for `signatures: true`.
///
/// Base Application's Customer table was 113 KB as JSON, 110 KB of it
/// fields with every property (tooltips included) and methods with their
/// parameters as objects. An agent asking what Customer has needs the
/// names and types: `1 "No.": Code[20]`,
/// `AssistEdit(OldCust: Record "Customer"): Boolean`.
fn member_signatures(object: &mut serde_json::Value) {
    fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
        value.get(key).and_then(|v| v.as_str()).unwrap_or("")
    }
    fn quoted(name: &str) -> String {
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            name.to_string()
        } else {
            format!("\"{name}\"")
        }
    }
    fn property<'a>(member: &'a serde_json::Value, name: &str) -> Option<&'a str> {
        member
            .get("properties")?
            .as_array()?
            .iter()
            .find(|p| text(p, "name").eq_ignore_ascii_case(name))
            .map(|p| text(p, "value"))
    }
    let Some(object) = object.as_object_mut() else {
        return;
    };
    if let Some(serde_json::Value::Array(fields)) = object.get_mut("fields") {
        for field in fields.iter_mut() {
            let mut line = format!(
                "{} {}: {}",
                field.get("id").map(|id| id.to_string()).unwrap_or_default(),
                quoted(text(field, "name")),
                text(field, "type_name")
            );
            if let Some(class) =
                property(field, "FieldClass").filter(|c| !c.eq_ignore_ascii_case("Normal"))
            {
                line.push_str(&format!(" ({class})"));
            }
            if let Some(state) =
                property(field, "ObsoleteState").filter(|s| !s.eq_ignore_ascii_case("No"))
            {
                line.push_str(&format!(" (obsolete: {state})"));
            }
            *field = serde_json::Value::String(line);
        }
    }
    if let Some(serde_json::Value::Array(methods)) = object.get_mut("methods") {
        for method in methods.iter_mut() {
            let mut line = String::new();
            for attribute in method
                .get("attributes")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
            {
                // Arguments kept: an Obsolete attribute's reason names the
                // replacement.
                let arguments: Vec<String> = attribute
                    .get("arguments")
                    .and_then(|a| a.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|a| a.as_str())
                    .map(|a| format!("'{a}'"))
                    .collect();
                if arguments.is_empty() {
                    line.push_str(&format!("[{}] ", text(attribute, "name")));
                } else {
                    line.push_str(&format!(
                        "[{}({})] ",
                        text(attribute, "name"),
                        arguments.join(", ")
                    ));
                }
            }
            if method.get("is_local").and_then(|v| v.as_bool()) == Some(true) {
                line.push_str("local ");
            }
            let parameters: Vec<String> = method
                .get("parameters")
                .and_then(|p| p.as_array())
                .into_iter()
                .flatten()
                .map(|parameter| {
                    let var = if parameter.get("is_var").and_then(|v| v.as_bool()) == Some(true) {
                        "var "
                    } else {
                        ""
                    };
                    format!(
                        "{var}{}: {}",
                        text(parameter, "name"),
                        text(parameter, "type_name")
                    )
                })
                .collect();
            line.push_str(&format!(
                "{}({})",
                text(method, "name"),
                parameters.join("; ")
            ));
            if let Some(ret) = method.get("return_type").and_then(|v| v.as_str()) {
                line.push_str(&format!(": {ret}"));
            }
            *method = serde_json::Value::String(line);
        }
    }
    if let Some(serde_json::Value::Array(variables)) = object.get_mut("variables") {
        for variable in variables.iter_mut() {
            let line = format!(
                "{}: {}",
                text(variable, "name"),
                text(variable, "type_name")
            );
            *variable = serde_json::Value::String(line);
        }
    }
}

/// Drop duplicate object entries that represent the SAME object surfaced by
/// both the symbol index and the workspace file index.
///
/// Workspace objects are indexed in `workspace.symbols` (package `"workspace"`)
/// AND in `file_index.object_info` (package `"(workspace)"`), so the naive
/// merge in `dispatch_search`/`dispatch_object`/`dispatch_by_id` listed each
/// workspace object twice. Object IDs are unique across an
/// app plus its dependencies, so `(kind, id, name)` identifies an object
/// regardless of which index produced it. The first occurrence — the richer
/// symbol-index entry, which carries members — wins.
fn dedup_objects_by_identity(objects: &mut Vec<serde_json::Value>) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    let mut deduplicated = Vec::with_capacity(objects.len());
    for object in objects.drain(..) {
        let kind = object
            .get("kind")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "object is missing string field 'kind'".to_string())?;
        let id = object
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| "object is missing integer field 'id'".to_string())?;
        let name = object
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| "object is missing string field 'name'".to_string())?
            .to_lowercase();
        if seen.insert((kind.to_string(), id, name)) {
            deduplicated.push(object);
        }
    }
    *objects = deduplicated;
    Ok(())
}

/// Convert a workspace CachedObjectInfo to JSON matching SymbolEntry shape.
///
/// `info.kind` is the tree-sitter node kind (lowercase, e.g. "table"). The wire
/// schema for SymbolEntry uses the al_symbols ObjectKind enum, whose serde
/// representation is PascalCase. Normalize via `ObjectKind::from_str` so the
/// payload deserializes cleanly on al-explorer.
fn workspace_object_identity(
    info: &al_source::file_index::CachedObjectInfo,
) -> Result<(al_symbols::ObjectKind, i32), String> {
    let kind = info
        .kind
        .parse::<al_symbols::ObjectKind>()
        .map_err(|error| format!("object '{}': {error}", info.name))?;
    let id = kind
        .normalize_declaration_id(info.id)
        .map_err(|error| format!("object '{}': {error}", info.name))?;
    Ok((kind, id))
}

fn workspace_object_to_json(
    info: &al_source::file_index::CachedObjectInfo,
) -> Result<serde_json::Value, String> {
    let (kind, id) = workspace_object_identity(info)?;
    Ok(serde_json::json!({
        "kind": kind,
        "id": id,
        "name": info.name,
        "package": WORKSPACE_PACKAGE,
        "source_availability": al_symbols::SourceAvailability::WorkspaceSource,
    }))
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
    let signatures = match optional_bool_param(params, "signatures", false) {
        Ok(signatures) => signatures,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let wait_for_members = match optional_bool_param(params, "waitForMembers", false) {
        Ok(wait) => wait,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let kind = match super::parse_object_kind(id, kind_str) {
        Ok(k) => k,
        Err(e) => return e,
    };
    // Workspace file objects, merged after the symbol index's as in
    // dispatch_object.
    let kind_lower = kind.to_string().to_lowercase();
    let mut workspace_objects = Vec::new();
    for entry in workspace.file_index.object_info.iter() {
        let info = entry.value();
        let (workspace_kind, workspace_id) = match workspace_object_identity(info) {
            Ok(identity) => identity,
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("workspace object metadata is invalid: {error}"),
                );
            }
        };
        if workspace_id == obj_id
            && workspace_kind == kind
            && info.kind.eq_ignore_ascii_case(&kind_lower)
        {
            match workspace_object_to_json(info) {
                Ok(object) => workspace_objects.push(object),
                Err(error) => {
                    return rpc_error(id, error_codes::INTERNAL_ERROR, &error);
                }
            }
        }
    }
    let is_indexed_workspace_object = |entry: &al_symbols::SymbolEntry| {
        al_symbols::source_availability::is_workspace_package(&entry.package)
    };
    let members_missing = if workspace_objects.is_empty()
        && !workspace
            .symbols
            .get_by_id(kind, obj_id)
            .iter()
            .any(|entry| is_indexed_workspace_object(entry))
    {
        None
    } else {
        workspace_members_missing(workspace, "byId", wait_for_members)
    };
    let results = workspace.symbols.get_by_id(kind, obj_id);
    let mut value: Vec<serde_json::Value> = match results
        .iter()
        .filter(|e| members_missing.is_none() || !is_indexed_workspace_object(e))
        .map(|entry| symbol_entry_to_json(workspace, entry, false))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("object-by-id response serialization failed: {error}"),
            );
        }
    };
    if let Some(reason) = &members_missing {
        for object in &mut workspace_objects {
            mark_members_missing(object, reason);
        }
    }
    value.extend(workspace_objects);
    if let Err(error) = dedup_objects_by_identity(&mut value) {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("object ID lookup produced invalid metadata: {error}"),
        );
    }
    if signatures {
        value.iter_mut().for_each(member_signatures);
    }
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
    // workspace methods (with their [IntegrationEvent]/
    // [EventSubscriber] attributes) only enter the SymbolIndex via the
    // call-graph enrichment pass — trigger the cached build before querying
    // so the user's own publishers are visible, not just package symbols.
    if let Err(error) = workspace.get_or_build_call_graph() {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("event query could not build a complete workspace graph: {error}"),
        );
    }
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
                "source_availability": workspace.symbols.source_availability(&p.object),
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
    // see dispatch_events — workspace subscribers need the
    // enrichment pass too.
    let (graph, _cg_guard) = match workspace.get_or_build_call_graph() {
        Ok(graph) => graph,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("subscriber query could not build a complete workspace graph: {error}"),
            );
        }
    };
    let results = workspace.symbols.get_events(event);
    let mut subscribers: Vec<serde_json::Value> = results
        .subscribers
        .iter()
        .map(|s| {
            serde_json::json!({
                "objectKind": s.object.kind.to_string(),
                "objectName": s.object.name,
                "methodName": s.method.name,
                "targetObjectType": s.target_object_type,
                "targetObjectName": s.target_object_name,
                "targetEventName": s.target_event_name,
                "package": s.object.package,
                "resolved": true,
                "source_availability": workspace.symbols.source_availability(&s.object),
            })
        })
        .collect();

    // Microsoft symbol packages carry no EventSubscriber attribute, so the
    // symbol index sees workspace subscribers only. The insight graph also
    // holds the subscribers parsed out of each package's extracted AL source,
    // which is the set `trace` was reporting while this method returned [].
    for matched in al_insight::discovery::subscribers_of(&graph, event) {
        subscribers.push(serde_json::json!({
            "objectKind": matched.object_kind,
            "objectName": matched.object_name,
            "methodName": matched.method_name,
            "targetObjectType": "",
            "targetObjectName": matched.target_object,
            "targetEventName": matched.target_event,
            "package": package_of_object(workspace, &matched.object_name),
            "resolved": matched.resolved,
        }));
    }
    dedup_subscribers(&mut subscribers);

    Response {
        id,
        result: Some(serde_json::json!(subscribers)),
        error: None,
        ..Default::default()
    }
}

/// The package that owns an object name, for rows whose source carries no
/// package of its own (insight-graph nodes). Workspace objects win over a
/// same-named package object because the workspace copy is the one a developer
/// can change.
fn package_of_object(workspace: &Workspace, object_name: &str) -> String {
    if workspace
        .file_index
        .object_info
        .iter()
        .any(|entry| entry.value().name.eq_ignore_ascii_case(object_name))
    {
        return WORKSPACE_PACKAGE.to_string();
    }
    workspace
        .symbols
        .get_by_name(object_name)
        .first()
        .map(|entry| entry.package.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Collapse rows that name the same handler. The symbol index and the insight
/// graph both see a workspace subscriber, so the merge above lists it twice.
fn dedup_subscribers(subscribers: &mut Vec<serde_json::Value>) {
    let mut seen = std::collections::HashSet::new();
    let mut unique = Vec::with_capacity(subscribers.len());
    for row in subscribers.drain(..) {
        let key = (
            row.get("objectName")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase(),
            row.get("methodName")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase(),
            row.get("targetEventName")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase(),
        );
        if seen.insert(key) {
            unique.push(row);
        }
    }
    *subscribers = unique;
}

pub(super) fn dispatch_composed(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    // `kind` is optional: the Zed task only has the
    // symbol under the cursor.
    let kind = match params.get("kind") {
        None => match resolve_unique_kind_by_name(workspace, id, name, "composed") {
            Ok(k) => k,
            Err(resp) => return resp,
        },
        Some(value) => match value.as_str() {
            Some(kind_str) => match super::parse_object_kind(id, kind_str) {
                Ok(k) => k,
                Err(e) => return e,
            },
            None => return invalid_params(id),
        },
    };
    // composition must see workspace extensions/bases as well —
    // they enter the SymbolIndex via the enrichment pass.
    if let Err(error) = workspace.get_or_build_call_graph() {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("composition query could not build a complete workspace graph: {error}"),
        );
    }
    match workspace.symbols.get_composed_cached(kind, name) {
        Some(composed) => {
            let mut value = match serde_json::to_value(composed.as_ref()) {
                Ok(value) => value,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("composed-symbol response serialization failed: {error}"),
                    );
                }
            };
            let Some(base) = value
                .get_mut("base")
                .and_then(serde_json::Value::as_object_mut)
            else {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "composed-symbol response has no object-shaped base",
                );
            };
            let base_availability = match source_availability_to_json(workspace, &composed.base) {
                Ok(availability) => availability,
                Err(error) => {
                    return rpc_error(
                            id,
                            error_codes::INTERNAL_ERROR,
                            &format!(
                                "composed-symbol base source availability serialization failed: {error}"
                            ),
                        );
                }
            };
            base.insert("source_availability".into(), base_availability);

            let Some(extensions) = value
                .get_mut("extensions")
                .and_then(serde_json::Value::as_array_mut)
            else {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "composed-symbol response has no extensions array",
                );
            };
            if extensions.len() != composed.extensions.len() {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    "composed-symbol response extension count is inconsistent",
                );
            }
            for (index, (extension, entry)) in
                extensions.iter_mut().zip(&composed.extensions).enumerate()
            {
                let Some(object) = extension.as_object_mut() else {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("composed-symbol extension {index} is not an object"),
                    );
                };
                let availability = match source_availability_to_json(workspace, entry) {
                    Ok(availability) => availability,
                    Err(error) => {
                        return rpc_error(
                            id,
                            error_codes::INTERNAL_ERROR,
                            &format!(
                                "composed-symbol extension {index} source availability serialization failed: {error}"
                            ),
                        );
                    }
                };
                object.insert("source_availability".into(), availability);
            }
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
    let pkgs = match workspace.package_info.read() {
        Ok(packages) => packages.clone(),
        Err(_) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "package inventory lock is poisoned; refusing to report potentially inconsistent data",
            );
        }
    };
    let value: Vec<_> = match pkgs
        .iter()
        .enumerate()
        .map(|(index, package)| {
            let mut value = serde_json::to_value(package).map_err(|error| {
                format!("package {index} information is not JSON serializable: {error}")
            })?;
            let object = value
                .as_object_mut()
                .ok_or_else(|| format!("serialized package {index} is not an object"))?;
            let availability = serde_json::to_value(
                workspace
                    .symbols
                    .package_source_availability_for(&package.app_id, &package.name),
            )
            .map_err(|error| {
                format!("package {index} source availability is not JSON serializable: {error}")
            })?;
            object.insert("source_availability".into(), availability);
            Ok::<_, String>(value)
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(value) => value,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("package response serialization failed: {error}"),
            );
        }
    };
    Response {
        id,
        result: Some(serde_json::Value::Array(value)),
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
    fn member_signatures_render_one_line_per_member() {
        let mut object = serde_json::json!({
            "kind": "Table",
            "name": "Customer",
            "fields": [
                {"id": 1, "name": "No.", "type_name": "Code[20]",
                 "properties": [{"name": "ToolTip", "value": "long text"}]},
                {"id": 59, "name": "Balance", "type_name": "Decimal",
                 "properties": [{"name": "FieldClass", "value": "FlowField"}]},
                {"id": 7, "name": "Old", "type_name": "Text[30]",
                 "properties": [{"name": "ObsoleteState", "value": "Removed"}]}
            ],
            "methods": [
                {"name": "LookupCustomer",
                 "parameters": [{"name": "Customer", "type_name": "Record \"Customer\"", "is_var": true}],
                 "return_type": "Boolean",
                 "attributes": [{"name": "Obsolete", "arguments": ["Use SelectCustomer instead.", "24.0"]}],
                 "is_local": false}
            ],
            "variables": [{"name": "SalesSetup", "type_name": "Record \"Sales & Receivables Setup\""}]
        });
        member_signatures(&mut object);
        assert_eq!(
            object["fields"],
            serde_json::json!([
                "1 \"No.\": Code[20]",
                "59 Balance: Decimal (FlowField)",
                "7 Old: Text[30] (obsolete: Removed)"
            ])
        );
        assert_eq!(
            object["methods"][0],
            "[Obsolete('Use SelectCustomer instead.', '24.0')] LookupCustomer(var Customer: Record \"Customer\"): Boolean"
        );
        assert_eq!(
            object["variables"][0],
            "SalesSetup: Record \"Sales & Receivables Setup\""
        );
    }

    /// JSON-RPC 2.0 §5: every response carries exactly one of `result` or
    /// `error`. `Response { result: None, error: None }` serialises to
    /// `{"jsonrpc":"2.0","id":7}` because both fields skip when absent, which
    /// a conforming third-party client rejects.
    #[tokio::test]
    async fn empty_results_serialise_as_an_explicit_null_result() {
        let workspace = Workspace::new();
        let uri = url::Url::parse("file:///proj/Foo.Codeunit.al").unwrap();
        workspace
            .documents
            .open(uri.clone(), "codeunit 50100 Foo\n{\n}\n".to_string())
            .unwrap();
        let position = serde_json::json!({
            "uri": uri.as_str(),
            "line": 1,
            "character": 0,
        });

        let mut rename = position.clone();
        rename["newName"] = serde_json::json!("Bar");
        for response in [
            dispatch_definition(&workspace, 7, &position),
            dispatch_rename(&workspace, 8, &rename),
            dispatch_hover(&workspace, 9, &position).await,
        ] {
            let frame = serde_json::to_value(&response).expect("response serialises");
            assert!(
                frame.get("result").is_some() != frame.get("error").is_some(),
                "exactly one of result/error must be present: {frame}"
            );
        }
    }

    #[test]
    fn dedup_objects_by_identity_drops_same_object_from_two_indices() {
        // Regression: workspace objects appear in both the
        // symbol index (package "workspace") and the file index (package
        // "(workspace)"), so the merged search/object/by-id result listed each
        // one twice. (kind, id, name) identifies an object regardless of which
        // index produced it.
        let mut objects = vec![
            serde_json::json!({"kind": "Table", "id": 50100, "name": "Customer", "package": "workspace"}),
            serde_json::json!({"kind": "Table", "id": 50100, "name": "Customer", "package": "(workspace)"}),
            serde_json::json!({"kind": "Codeunit", "id": 50100, "name": "Mgmt", "package": "workspace"}),
        ];
        dedup_objects_by_identity(&mut objects).unwrap();
        assert_eq!(objects.len(), 2, "duplicate table must collapse to one");
        // The first (richer symbol-index) entry wins.
        assert_eq!(objects[0]["package"], "workspace");
        assert_eq!(objects[1]["name"], "Mgmt");
    }

    #[test]
    fn serialized_response_serializes_value() {
        let resp = serialized_response(7, &vec!["a", "b"], "test/method");
        assert_eq!(resp.id, 7);
        assert!(resp.error.is_none());
        assert_eq!(
            resp.result,
            Some(serde_json::json!(["a", "b"])),
            "expected serialised array"
        );
    }

    struct AlwaysFails;

    impl serde::Serialize for AlwaysFails {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("intentional failure"))
        }
    }

    #[test]
    fn serialized_response_returns_rpc_error_on_serialization_failure() {
        let resp = serialized_response(11, &AlwaysFails, "test/method");
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

    #[test]
    fn workspace_object_to_json_round_trips_through_symbol_entry() {
        let info = al_source::file_index::CachedObjectInfo {
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
        let json = workspace_object_to_json(&info).unwrap();
        assert_eq!(json["source_availability"], "workspace_source");
        let entry: al_symbols::SymbolEntry =
            serde_json::from_value(json).expect("workspace object must deserialize as SymbolEntry");
        assert_eq!(entry.kind, al_symbols::ObjectKind::Table);
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
        for ot in al_syntax::language_data::object_types() {
            let k = ot.keyword.as_str();
            if NON_OBJECT_KEYWORDS.contains(&k) {
                continue;
            }
            covered += 1;
            let kind = k
                .parse::<al_symbols::ObjectKind>()
                .unwrap_or_else(|error| panic!("kind {k:?} must normalize: {error}"));
            let info = al_source::file_index::CachedObjectInfo {
                kind: k.to_string(),
                id: kind.requires_numeric_id().then_some(1),
                name: "X".to_string(),
                range: tree_sitter::Range {
                    start_byte: 0,
                    end_byte: 0,
                    start_point: tree_sitter::Point { row: 0, column: 0 },
                    end_point: tree_sitter::Point { row: 0, column: 0 },
                },
            };
            let json = workspace_object_to_json(&info).unwrap();
            let _: al_symbols::SymbolEntry = serde_json::from_value(json)
                .unwrap_or_else(|e| panic!("kind {k:?} must deserialize: {e}"));
        }
        // Floor so the test can't silently degrade to covering nothing if the
        // data source or exclusion list changes (20 ObjectKind variants today).
        assert!(
            covered >= 20,
            "expected >= 20 object kinds covered, only {covered}"
        );
    }

    #[test]
    fn dispatch_by_id_finds_workspace_objects() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.object_info.insert(
            std::path::PathBuf::from("/proj/src/HelloWorld.al"),
            al_source::file_index::CachedObjectInfo {
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

    #[test]
    fn dispatch_events_finds_workspace_publishers() {
        let ws = al_workspace::Workspace::new();
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

    #[test]
    fn dispatch_composed_resolves_kind_from_bare_name() {
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
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
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Page,
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
        let ws = al_workspace::Workspace::new();
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

    /// A first `by-id table 18` on a fresh daemon waited 52.6 s for the call
    /// graph, which adds nothing to a package object.
    #[test]
    fn a_package_object_lookup_does_not_build_the_call_graph() {
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base Application".to_string(),
            ..Default::default()
        }]);

        let by_id = dispatch_by_id(&ws, 1, &serde_json::json!({"kind": "table", "id": 18}));
        let by_name = dispatch_object(
            &ws,
            2,
            &serde_json::json!({"kind": "table", "name": "Customer"}),
        );

        for response in [by_id, by_name] {
            let result = response.result.expect("the package object is found");
            assert_eq!(result[0]["name"], "Customer");
            assert!(result[0].get("partial").is_none(), "{result}");
        }
        assert_eq!(ws.call_graph_build_count(), 0);
    }

    fn workspace_with_codeunit() -> al_workspace::Workspace {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/src/Greeter.Codeunit.al"),
            "codeunit 50130 Greeter\n{\n    procedure Greet()\n    begin\n    end;\n}\n"
                .to_string(),
        );
        ws
    }

    fn method_names(result: &serde_json::Value) -> Vec<String> {
        result[0]["methods"]
            .as_array()
            .map(|methods| {
                methods
                    .iter()
                    .filter_map(|method| method["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Without a built call graph a workspace object answers at once with its
    /// identity and says its members are missing, and `waitForMembers` builds
    /// the graph to fill them in.
    #[test]
    fn a_workspace_object_is_partial_until_the_call_graph_is_built() {
        let ws = workspace_with_codeunit();
        let lookups: [(&str, serde_json::Value); 2] = [
            ("byId", serde_json::json!({"kind": "codeunit", "id": 50130})),
            (
                "object",
                serde_json::json!({"kind": "codeunit", "name": "Greeter"}),
            ),
        ];
        let lookup = |method: &str, params: &serde_json::Value| {
            let response = match method {
                "byId" => dispatch_by_id(&ws, 1, params),
                _ => dispatch_object(&ws, 1, params),
            };
            response.result.expect("the workspace object is found")
        };

        for (method, params) in &lookups {
            let result = lookup(method, params);
            assert_eq!(result.as_array().map(Vec::len), Some(1), "{result}");
            assert_eq!(result[0]["partial"], true, "{method}: {result}");
            assert!(
                result[0]["partial_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("waitForMembers")),
                "{method}: {result}"
            );
        }
        assert_eq!(
            ws.call_graph_build_count(),
            0,
            "nothing waited on the graph"
        );

        let mut waiting = lookups[0].1.clone();
        waiting["waitForMembers"] = serde_json::json!(true);
        let result = lookup("byId", &waiting);
        assert_eq!(method_names(&result), ["Greet"], "{result}");
        assert!(result[0].get("partial").is_none(), "{result}");
        assert_eq!(ws.call_graph_build_count(), 1);

        for (method, params) in &lookups {
            let result = lookup(method, params);
            assert_eq!(method_names(&result), ["Greet"], "{method}: {result}");
            assert!(result[0].get("partial").is_none(), "{method}: {result}");
        }
        assert_eq!(ws.call_graph_build_count(), 1, "a built graph is reused");

        // An edit drops the graph. The members the symbol index still holds
        // are from before the edit, so they are left out again.
        ws.invalidate_insight_graph();
        let result = lookup("object", &lookups[1].1);
        assert_eq!(result[0]["partial"], true, "{result}");
        assert!(method_names(&result).is_empty(), "{result}");
    }

    #[test]
    fn dispatch_by_id_unknown_id_still_errors() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_by_id(&ws, 1, &serde_json::json!({"kind": "codeunit", "id": 1}));
        assert!(resp.error.is_some(), "unknown id must keep erroring");
    }

    #[test]
    fn dispatch_inlay_hints_rejects_overflow_start_line() {
        let ws = al_workspace::Workspace::new();
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

    #[test]
    fn dispatch_inlay_hints_rejects_overflow_end_line() {
        let ws = al_workspace::Workspace::new();
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

    #[test]
    fn dispatch_inlay_hints_defaults_lines_when_absent() {
        let ws = al_workspace::Workspace::new();
        let uri = url::Url::parse("file:///tmp/x.al").unwrap();
        ws.documents
            .open(uri.clone(), r#"codeunit 50100 "Hints" { }"#.to_string())
            .unwrap();
        let resp = dispatch_inlay_hints(&ws, 9, &serde_json::json!({ "uri": uri.as_str() }));
        assert!(
            resp.error.is_none(),
            "absent lines must default, not error: {:?}",
            resp.error
        );
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    #[test]
    fn document_queries_do_not_turn_missing_files_into_empty_results() {
        let ws = al_workspace::Workspace::new();
        let uri = "file:///definitely/not/existing/al-language-zed-missing.al";
        for response in [
            dispatch_definition(
                &ws,
                10,
                &serde_json::json!({ "uri": uri, "line": 0, "character": 0 }),
            ),
            dispatch_inlay_hints(&ws, 10, &serde_json::json!({ "uri": uri })),
            dispatch_document_symbols(&ws, 10, &serde_json::json!({ "uri": uri })),
        ] {
            assert!(
                response.result.is_none(),
                "missing source must not become an empty successful result"
            );
            assert!(response.error.is_some());
        }
    }

    #[test]
    fn ok_response_opt_none_yields_an_explicit_null_result_no_error() {
        let resp = ok_response_opt::<Vec<u8>>(3, None, "test/method");
        assert_eq!(resp.id, 3);
        assert_eq!(
            resp.result,
            Some(serde_json::Value::Null),
            "an absent result would serialise to a frame with neither result nor error"
        );
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
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_definition(&ws, 1, &serde_json::json!({ "line": 0, "character": 0 }));
        assert_invalid_params(&resp, 1);
    }

    #[test]
    fn dispatch_definition_rejects_missing_position() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_definition(&ws, 2, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 2);
    }

    #[test]
    fn dispatch_references_rejects_missing_position() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_references(&ws, 5, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 5);
    }

    #[test]
    fn dispatch_implementations_rejects_missing_uri() {
        let ws = al_workspace::Workspace::new();
        let resp =
            dispatch_implementations(&ws, 6, &serde_json::json!({ "line": 0, "character": 0 }));
        assert_invalid_params(&resp, 6);
    }

    #[test]
    fn dispatch_signature_help_rejects_missing_position() {
        let ws = al_workspace::Workspace::new();
        let resp =
            dispatch_signature_help(&ws, 7, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 7);
    }

    #[test]
    fn dispatch_document_symbols_rejects_missing_uri() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_document_symbols(&ws, 8, &serde_json::json!({}));
        assert_invalid_params(&resp, 8);
    }

    #[test]
    fn dispatch_code_actions_rejects_missing_position() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_code_actions(&ws, 9, &serde_json::json!({ "uri": "file:///tmp/x.al" }));
        assert_invalid_params(&resp, 9);
    }

    #[test]
    fn dispatch_rename_rejects_missing_new_name() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_rename(
            &ws,
            10,
            &serde_json::json!({ "uri": "file:///tmp/x.al", "line": 0, "character": 0 }),
        );
        assert_invalid_params(&resp, 10);
    }

    /// `limit` pages the result, so the search itself must not stop at it:
    /// the page is cut, and `total` counted, from every match.
    #[test]
    fn a_paged_search_returns_every_match_for_the_projection_to_page() {
        let ws = al_workspace::Workspace::new();
        let entries: Vec<al_symbols::SymbolEntry> = (0..5)
            .map(|index| al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Codeunit,
                id: 80 + index,
                name: format!("Sales-Post {index}"),
                package: "Base Application".to_string(),
                ..Default::default()
            })
            .collect();
        ws.symbols.add_entries(&entries);

        let params = serde_json::json!({ "query": "Sales-Post", "limit": 2, "offset": 2 });
        let resp = dispatch_search(&ws, 13, &params);
        let rows = resp
            .result
            .as_ref()
            .and_then(|r| r.as_array())
            .unwrap()
            .len();
        assert_eq!(rows, 5);

        let paged = super::super::projection::apply("search", &params, resp);
        let page = paged.result.unwrap();
        assert_eq!(page["total"], 5, "{page}");
        assert_eq!(page["returned"], 2, "{page}");
        assert_eq!(page["truncated"], true, "{page}");
    }

    #[test]
    fn dispatch_search_rejects_missing_query() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_search(&ws, 11, &serde_json::json!({ "limit": 5 }));
        assert_invalid_params(&resp, 11);
    }

    #[test]
    fn dispatch_search_empty_workspace_returns_empty_array() {
        let ws = al_workspace::Workspace::new();
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

    #[test]
    fn dispatch_search_rejects_invalid_optional_parameters() {
        let ws = al_workspace::Workspace::new();
        for params in [
            serde_json::json!({ "query": "x", "limit": u64::MAX }),
            serde_json::json!({ "query": "x", "limit": "20" }),
            serde_json::json!({ "query": "x", "summary": "yes" }),
        ] {
            let resp = dispatch_search(&ws, 13, &params);
            assert_invalid_params(&resp, 13);
        }
    }

    #[test]
    fn dispatch_references_rejects_non_boolean_include_declaration() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_references(
            &ws,
            14,
            &serde_json::json!({
                "uri": "file:///tmp/x.al",
                "line": 0,
                "character": 0,
                "includeDeclaration": "yes"
            }),
        );
        assert_invalid_params(&resp, 14);
    }

    #[test]
    fn optional_object_kind_must_be_a_string_when_present() {
        let ws = al_workspace::Workspace::new();
        for response in [
            dispatch_object(
                &ws,
                15,
                &serde_json::json!({ "name": "Customer", "kind": 42 }),
            ),
            dispatch_composed(
                &ws,
                15,
                &serde_json::json!({ "name": "Customer", "kind": 42 }),
            ),
        ] {
            assert_invalid_params(&response, 15);
        }
    }

    #[test]
    fn dispatch_search_reports_cached_source_availability() {
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            package: "Base Application".to_string(),
            fields: vec![al_symbols::FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code[20]".to_string(),
                properties: Vec::new(),
            }],
            ..Default::default()
        }]);
        let resp = dispatch_search(&ws, 13, &serde_json::json!({ "query": "Customer" }));
        assert!(resp.error.is_none());
        let result = resp.result.expect("search result");
        assert_eq!(result[0]["source_availability"], "generated_outline");
    }

    #[test]
    fn dispatch_object_rejects_unknown_kind() {
        let ws = al_workspace::Workspace::new();
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
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_object(&ws, 15, &serde_json::json!({ "kind": "table" }));
        assert_invalid_params(&resp, 15);
    }

    #[test]
    fn dispatch_object_not_found_returns_error() {
        let ws = al_workspace::Workspace::new();
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
        let ws = al_workspace::Workspace::new();
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
        let ws = al_workspace::Workspace::new();
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
        let ws = al_workspace::Workspace::new();
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
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_events(&ws, 20, &serde_json::json!({}));
        assert_invalid_params(&resp, 20);
    }

    #[test]
    fn dispatch_events_empty_returns_empty_array() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_events(&ws, 21, &serde_json::json!({ "name": "OnAfterPost" }));
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    #[test]
    fn dispatch_subscribers_rejects_missing_event() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_subscribers(&ws, 22, &serde_json::json!({}));
        assert_invalid_params(&resp, 22);
    }

    #[test]
    fn dispatch_subscribers_empty_returns_empty_array() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_subscribers(&ws, 23, &serde_json::json!({ "event": "OnAfterPost" }));
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    /// `subscribers` read the symbol index only, and Microsoft symbol packages
    /// carry no `EventSubscriber` attribute, so it answered `[]` for events
    /// `trace` could follow to three handlers. Both now read the same graph.
    #[test]
    fn dispatch_subscribers_agrees_with_trace() {
        let ws = al_workspace::Workspace::new();
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Publisher.Codeunit.al"),
            r#"codeunit 50100 "Test Event Publisher"
{
    [IntegrationEvent(false, false)]
    procedure OnAfterProcess()
    begin
    end;
}
"#
            .to_string(),
        );
        ws.file_index.add_file(
            std::path::PathBuf::from("/proj/Handler.Codeunit.al"),
            r#"codeunit 50101 "Work Order Subscribers"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Test Event Publisher", 'OnAfterProcess', '', false, false)]
    local procedure OnAfterProcessLogResult()
    begin
    end;
}
"#
            .to_string(),
        );

        let subscribers =
            dispatch_subscribers(&ws, 25, &serde_json::json!({ "event": "OnAfterProcess" }))
                .result
                .expect("subscribers must carry a result");
        let rows = subscribers.as_array().expect("an array of subscribers");
        assert!(
            rows.iter()
                .any(|row| row.get("objectName").and_then(|v| v.as_str())
                    == Some("Work Order Subscribers")),
            "the handler must be listed: {subscribers}"
        );
        assert_eq!(
            rows.len(),
            1,
            "the symbol-index and graph views must be merged, not doubled: {subscribers}"
        );
        let package = rows[0]
            .get("package")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(
            package.eq_ignore_ascii_case("workspace")
                || package.eq_ignore_ascii_case(WORKSPACE_PACKAGE),
            "a workspace handler must be labelled as such, got '{package}'"
        );
    }

    #[test]
    fn dispatch_packages_empty_returns_empty_array() {
        let ws = al_workspace::Workspace::new();
        let resp = dispatch_packages(&ws, 24);
        assert!(resp.error.is_none());
        assert_eq!(resp.result, Some(serde_json::json!([])));
    }

    #[test]
    fn dispatch_packages_rejects_poisoned_inventory() {
        let ws = std::sync::Arc::new(al_workspace::Workspace::new());
        let poison_target = ws.clone();
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.package_info.write().unwrap();
            panic!("poison package inventory for fail-closed test");
        })
        .join();

        let resp = dispatch_packages(&ws, 25);
        assert!(resp.result.is_none());
        let error = resp.error.expect("poisoned inventory must be explicit");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(error.message.contains("poisoned"));
    }

    #[test]
    fn dispatch_packages_summarizes_outline_and_metadata_only_objects() {
        let ws = al_workspace::Workspace::new();
        ws.symbols.add_entries(&[
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Table,
                id: 18,
                name: "Customer".to_string(),
                package: "Base Application".to_string(),
                fields: vec![al_symbols::FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code[20]".to_string(),
                    properties: Vec::new(),
                }],
                ..Default::default()
            },
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Page,
                id: 21,
                name: "Customer Card".to_string(),
                package: "Base Application".to_string(),
                ..Default::default()
            },
        ]);
        ws.package_info
            .write()
            .unwrap()
            .push(al_workspace::PackageInfo {
                app_id: String::new(),
                name: "Base Application".to_string(),
                publisher: "Microsoft".to_string(),
                version: "1.0.0.0".to_string(),
                object_count: 2,
            });

        let resp = dispatch_packages(&ws, 25);
        assert!(resp.error.is_none());
        let result = resp.result.expect("package result");
        assert_eq!(result[0]["source_availability"]["generated_outline"], 1);
        assert_eq!(result[0]["source_availability"]["metadata_only"], 1);
    }

    #[test]
    fn dispatch_deps_no_project_returns_internal_error() {
        let ws = al_workspace::Workspace::new();
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
