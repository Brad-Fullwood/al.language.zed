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
    for info in workspace_file_objects(workspace) {
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
    for info in workspace_file_objects(workspace) {
        if info.name.eq_ignore_ascii_case(&name_lower)
            && info.kind.eq_ignore_ascii_case(&kind_lower)
        {
            match workspace_object_to_json(&info) {
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
/// AND in `file_index.object_infos` (package `"(workspace)"`), so the naive
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

/// Every object the workspace's files declare, one row per object.
///
/// A file can declare several objects. `object_info` holds only a file's
/// first, so a codeunit declared after a table in the same file was missing
/// from `object`, `byId` and bare-name kind resolution.
fn workspace_file_objects(workspace: &Workspace) -> Vec<al_source::file_index::CachedObjectInfo> {
    al_insight::calls::indexed_objects(&workspace.file_index)
        .into_iter()
        .map(|(_, info)| info)
        .collect()
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
    for info in workspace_file_objects(workspace) {
        let (workspace_kind, workspace_id) = match workspace_object_identity(&info) {
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
            match workspace_object_to_json(&info) {
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
    if workspace_file_objects(workspace)
        .iter()
        .any(|info| info.name.eq_ignore_ascii_case(object_name))
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
#[path = "lsp_dispatch_tests.rs"]
mod lsp_dispatch_tests;
