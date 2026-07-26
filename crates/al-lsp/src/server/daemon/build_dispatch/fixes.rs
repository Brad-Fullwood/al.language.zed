//! Analysis/fix dispatchers — lint, format, fix, rules, parse, arch-lint,
//! and field-annotation fixers.

use super::super::{
    ensure_document, file_not_found, file_uri_from_params, invalid_params, optional_bool_param,
    require_document_text, require_project_root, rpc_error,
};
use super::build::write_al_file_and_refresh;
use super::serialized_response;
use al_protocol::jsonrpc::{error_codes, Response};
use al_workspace::Workspace;

pub(in crate::server::daemon) async fn dispatch_lint(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let all = match optional_bool_param(params, "all", false) {
        Ok(all) => all,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    if all {
        // Run the same syntax + file-local + symbol-aware workspace rules used
        // by editor diagnostics. Clean files remain present so "scanned all,
        // clean" is distinguishable from "scanned nothing".
        let mut results: Vec<serde_json::Value> = Vec::new();
        let config = workspace.config.read().await.clone();
        let project_root = workspace
            .project
            .read()
            .await
            .as_ref()
            .map(|project| project.root.clone());
        let diagnostics =
            match al_analysis::queries::diagnostics::workspace_syntax_diagnostics_at_root(
                workspace,
                &config,
                project_root.as_deref(),
            ) {
                Ok(diagnostics) => diagnostics,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("workspace lint could not analyze every indexed source: {error}"),
                    );
                }
            };
        for (path, diagnostics) in diagnostics {
            let diags: Vec<serde_json::Value> =
                diagnostics.iter().map(syntax_diag_to_json).collect();
            results.push(serde_json::json!({
                "file": path.display().to_string(),
                "diagnostics": diags,
            }));
        }
        return Response {
            id,
            result: Some(serde_json::json!(results)),
            error: None,
            ..Default::default()
        };
    }

    let uri = match file_uri_from_params(params) {
        Ok(Some(uri)) => uri,
        Ok(None) => return invalid_params(id),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let _text = match require_document_text(workspace, &uri, id).await {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    let config = workspace.config.read().await.clone();
    let project_root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let diagnostics = al_analysis::queries::diagnostics::syntax_diagnostics_at_root(
        workspace,
        &uri,
        &config,
        project_root.as_deref(),
    );
    let diags: Vec<serde_json::Value> = diagnostics.iter().map(syntax_diag_to_json).collect();
    Response {
        id,
        result: Some(serde_json::json!(diags)),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) async fn dispatch_format(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let check = match optional_bool_param(params, "check", false) {
        Ok(check) => check,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    let file_uri = match file_uri_from_params(params) {
        Ok(file_uri) => file_uri,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if params.get("content").is_some() && file_uri.is_some() {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "'content' cannot be combined with 'uri' or 'file'",
        );
    }

    let content = match params.get("content") {
        Some(value) => match value.as_str() {
            Some(text) => text.to_string(),
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'content' must be a string when supplied",
                );
            }
        },
        None => {
            let Some(uri) = file_uri.as_ref() else {
                return invalid_params(id);
            };
            if let Err(response) = ensure_document(workspace, uri, id) {
                return response;
            }
            match workspace.documents.get_text(uri) {
                Some(text) => text,
                None => return file_not_found(id),
            }
        }
    };

    let project_root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let options = match project_root {
        Some(root) => {
            match al_analysis::queries::format::AlFormatConfig::load_options_strict(&root) {
                Ok(options) => options,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("invalid formatter configuration: {error}"),
                    );
                }
            }
        }
        None => Default::default(),
    };
    let formatted = al_syntax::format_al(&content, &options);
    let changed = formatted != content;

    if check {
        Response {
            id,
            result: Some(serde_json::json!({ "changed": changed })),
            error: None,
            ..Default::default()
        }
    } else {
        // If a file was specified, write it through
        // write_al_file_and_refresh so the document store, file index,
        // and insight graph all see the update.
        if let Some(uri) = file_uri {
            let path = match uri.to_file_path() {
                Ok(path) => path,
                Err(()) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        "validated file URI could not be converted back to a path",
                    );
                }
            };
            if changed {
                if let Err(e) = tokio::task::block_in_place(|| {
                    write_al_file_and_refresh(workspace, &path, formatted.clone())
                }) {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("Failed to write formatted file: {e}"),
                    );
                }
            }
        }
        Response {
            id,
            result: Some(serde_json::json!({
                "formatted": formatted,
                "changed": changed,
            })),
            error: None,
            ..Default::default()
        }
    }
}
pub(in crate::server::daemon) fn dispatch_fix(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dry_run = match optional_bool_param(params, "dryRun", false) {
        Ok(dry_run) => dry_run,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let rule_filter = match params.get("rule") {
        None => None,
        Some(value) => match value.as_str().filter(|rule| !rule.trim().is_empty()) {
            Some(rule) => Some(rule),
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'rule' must be a non-empty string when supplied",
                );
            }
        },
    };
    if let Some(rule) = rule_filter {
        if !al_syntax::lint_rules()
            .iter()
            .any(|registered| registered.code.eq_ignore_ascii_case(rule))
        {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("Unknown native lint rule '{rule}'"),
            );
        }
    }

    let file_uri = match file_uri_from_params(params) {
        Ok(uri) => uri,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let targets = if let Some(uri) = file_uri {
        let path = match uri.to_file_path() {
            Ok(path) => path,
            Err(()) => return invalid_params(id),
        };
        vec![(path, uri)]
    } else {
        let root = match require_project_root(workspace, id) {
            Ok(root) => root,
            Err(response) => return response,
        };
        let paths = match al_analysis::queries::bulk_fix::collect_al_files(&root) {
            Ok(paths) => paths,
            Err(error) => return rpc_error(id, error_codes::INTERNAL_ERROR, &error),
        };
        let mut targets = Vec::with_capacity(paths.len());
        for path in paths {
            let Ok(uri) = url::Url::from_file_path(&path) else {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("Cannot convert project file to URI: {}", path.display()),
                );
            };
            targets.push((path, uri));
        }
        targets
    };

    let mut diagnostics_count = 0usize;
    let mut fixable_count = 0usize;
    let mut edits_json = Vec::new();
    let mut pending_writes = Vec::new();
    for (path, uri) in &targets {
        if let Err(response) = ensure_document(workspace, uri, id) {
            return response;
        }
        let Some(text) = workspace.documents.get_text(uri) else {
            return file_not_found(id);
        };
        let result = al_syntax::AlParser::parse_quick(&text);
        let diagnostics = al_syntax::lint(&result.tree, &text);
        let mut file_edits = Vec::new();
        for diagnostic in diagnostics.iter().filter(|diagnostic| {
            rule_filter.is_none_or(|rule| diagnostic.code.eq_ignore_ascii_case(rule))
        }) {
            diagnostics_count += 1;
            let range = lint_range(diagnostic.range, &text);
            let info = al_analysis::queries::code_actions::DiagnosticInfo {
                range,
                message: diagnostic.message.clone(),
                code: Some(diagnostic.code.clone()),
            };
            let Some(action) = al_analysis::queries::code_actions::quick_fix_for_diagnostic(
                workspace, uri, &text, &info,
            ) else {
                continue;
            };
            let Some(edit) = action.edit else {
                continue;
            };
            let action_edits: Vec<_> = edit
                .changes
                .into_iter()
                .filter(|(edit_uri, _)| edit_uri == uri)
                .flat_map(|(_, edits)| edits)
                .collect();
            if action_edits.is_empty() {
                continue;
            }
            fixable_count += 1;
            for edit in &action_edits {
                edits_json.push(serde_json::json!({
                    "file": path.display().to_string(),
                    "rule": diagnostic.code,
                    "title": action.title,
                    "range": edit.range,
                    "newText": edit.new_text,
                }));
            }
            file_edits.extend(action_edits);
        }
        if !file_edits.is_empty() {
            let updated = match apply_text_edits(&text, &file_edits) {
                Ok(updated) => updated,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("Cannot apply fixes to {}: {error}", path.display()),
                    );
                }
            };
            if updated != text {
                pending_writes.push((path.clone(), updated));
            }
        }
    }

    if !dry_run {
        for (path, updated) in &pending_writes {
            if let Err(error) = write_al_file_and_refresh(workspace, path, updated.clone()) {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("Failed to write fixes to {}: {error}", path.display()),
                );
            }
        }
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "diagnostics": diagnostics_count,
            "fixes": fixable_count,
            "unfixable": diagnostics_count.saturating_sub(fixable_count),
            "filesScanned": targets.len(),
            "filesChanged": pending_writes.len(),
            "dryRun": dry_run,
            "edits": edits_json,
        })),
        error: None,
        ..Default::default()
    }
}

fn lint_range(range: tree_sitter::Range, text: &str) -> al_analysis::queries::Range {
    let start_line = text.lines().nth(range.start_point.row).unwrap_or("");
    let end_line = text.lines().nth(range.end_point.row).unwrap_or("");
    al_analysis::queries::Range {
        start: al_analysis::queries::Position {
            line: range.start_point.row as u32,
            character: al_syntax::byte_col_to_utf16_col(start_line, range.start_point.column),
        },
        end: al_analysis::queries::Position {
            line: range.end_point.row as u32,
            character: al_syntax::byte_col_to_utf16_col(end_line, range.end_point.column),
        },
    }
}

fn apply_text_edits(
    text: &str,
    edits: &[al_analysis::queries::TextEdit],
) -> Result<String, String> {
    fn offset(text: &str, position: al_analysis::queries::Position) -> Result<usize, String> {
        let mut line_start = 0usize;
        let mut selected = None;
        for (line_index, line) in text.split('\n').enumerate() {
            if line_index == position.line as usize {
                selected = Some((line_start, line));
                break;
            }
            line_start += line.len() + 1;
        }
        let Some((line_start, line)) = selected else {
            return Err(format!("line {} is past end of file", position.line));
        };

        let mut remaining = position.character as usize;
        for (byte, ch) in line.char_indices() {
            if remaining == 0 {
                return Ok(line_start + byte);
            }
            let width = ch.len_utf16();
            if remaining < width {
                return Err(format!(
                    "character {} splits a UTF-16 surrogate pair on line {}",
                    position.character, position.line
                ));
            }
            remaining -= width;
        }
        if remaining == 0 {
            Ok(line_start + line.len())
        } else {
            Err(format!(
                "character {} is past end of line {}",
                position.character, position.line
            ))
        }
    }

    let mut ranges = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = offset(text, edit.range.start)?;
        let end = offset(text, edit.range.end)?;
        if start > end {
            return Err("edit starts after it ends".to_string());
        }
        ranges.push((start, end, edit.new_text.as_str()));
    }
    ranges.sort_by_key(|(start, end, _)| (*start, *end));
    for pair in ranges.windows(2) {
        let (left_start, left_end, _) = pair[0];
        let (right_start, _, _) = pair[1];
        if left_end > right_start || (left_start == left_end && left_start == right_start) {
            return Err("quick-fix edits overlap or share an insertion point".to_string());
        }
    }

    let mut updated = text.to_string();
    for (start, end, replacement) in ranges.into_iter().rev() {
        updated.replace_range(start..end, replacement);
    }
    Ok(updated)
}
pub(in crate::server::daemon) fn dispatch_rules(id: u64) -> Response {
    let mut value: Vec<serde_json::Value> = al_syntax::lint_rules()
        .iter()
        .map(|r| {
            serde_json::json!({
                "code": r.code,
                "name": r.name,
                "severity": r.severity.to_string(),
                "description": r.description,
            })
        })
        .collect();
    value.extend(
        al_analysis::queries::transaction_lint::transaction_lint_rules()
            .iter()
            .map(|rule| {
                serde_json::json!({
                    "code": rule.code,
                    "name": rule.name,
                    "severity": workspace_severity_label(rule.severity),
                    "description": rule.description,
                })
            }),
    );
    value.extend(
        al_analysis::queries::native_check::native_check_rules()
            .iter()
            .map(|rule| {
                serde_json::json!({
                    "code": rule.code,
                    "name": rule.name,
                    "severity": match rule.severity {
                        al_analysis::queries::native_check::NativeSeverity::Error => "error",
                        al_analysis::queries::native_check::NativeSeverity::Warning => "warning",
                    },
                    "description": rule.description,
                })
            }),
    );
    value.extend(
        al_analysis::queries::diagnostics::native_workspace_lint_rules()
            .iter()
            .map(|rule| {
                serde_json::json!({
                    "code": rule.code,
                    "name": rule.name,
                    "severity": match rule.severity {
                        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Error => "error",
                        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Warning => "warning",
                        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Info => "info",
                        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Hint => "hint",
                    },
                    "description": rule.description,
                })
            }),
    );
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}

fn syntax_diag_to_json(
    diagnostic: &al_analysis::queries::diagnostics::SyntaxDiagnostic,
) -> serde_json::Value {
    let severity = match diagnostic.severity {
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Error => "error",
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Warning => "warning",
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Info => "info",
        al_analysis::queries::diagnostics::SyntaxDiagnosticSeverity::Hint => "hint",
    };
    serde_json::json!({
        "code": diagnostic.code,
        "message": diagnostic.message,
        "severity": severity,
        "line": diagnostic.range.start.line + 1,
        "column": diagnostic.range.start.character + 1,
        "endLine": diagnostic.range.end.line + 1,
        "endColumn": diagnostic.range.end.character + 1,
    })
}

fn workspace_severity_label(
    severity: al_analysis::queries::transaction_lint::WorkspaceLintSeverity,
) -> &'static str {
    use al_analysis::queries::transaction_lint::WorkspaceLintSeverity;
    match severity {
        WorkspaceLintSeverity::Error => "error",
        WorkspaceLintSeverity::Warning => "warning",
        WorkspaceLintSeverity::Info => "info",
        WorkspaceLintSeverity::Hint => "hint",
    }
}
pub(in crate::server::daemon) fn dispatch_parse(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let uri = match file_uri_from_params(params) {
        Ok(Some(uri)) => uri,
        Ok(None) => return invalid_params(id),
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if let Err(response) = ensure_document(workspace, &uri, id) {
        return response;
    }

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let start = std::time::Instant::now();
    let result = al_syntax::AlParser::parse_quick(&text);
    let elapsed = start.elapsed();

    let node_count = al_source::parsing::count_nodes(&result.tree);

    Response {
        id,
        result: Some(serde_json::json!({
            "errors": result.errors.len(),
            "nodeCount": node_count,
            "parseTimeMs": elapsed.as_secs_f64() * 1000.0,
            "parseErrors": result.errors.iter().map(|e| serde_json::json!({
                "message": e.message,
                "line": e.range.start_point.row + 1,
                "column": e.range.start_point.column + 1,
            })).collect::<Vec<_>>(),
        })),
        error: None,
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_fix_application_area(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let value = match property_value_param(params, "value", "All") {
        Ok(value) => value,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let dry_run = match optional_bool_param(params, "dryRun", false) {
        Ok(dry_run) => dry_run,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let project_root = match require_project_root(workspace, id) {
        Ok(root) => root,
        Err(response) => return response,
    };

    let plan = match al_analysis::queries::bulk_fix::plan_application_area(&project_root, value) {
        Ok(plan) => plan,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!("application-area fix planning failed: {error}"),
            );
        }
    };
    apply_bulk_fix_plan(workspace, id, plan, dry_run, "application-area")
}
pub(in crate::server::daemon) fn dispatch_fix_tooltips(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dry_run = match optional_bool_param(params, "dryRun", false) {
        Ok(dry_run) => dry_run,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };

    let table_name = match params.get("fromTable") {
        Some(value) => match value.as_str().filter(|name| !name.trim().is_empty()) {
            Some(name) => name,
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "'fromTable' must be a non-empty string",
                );
            }
        },
        None => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing required 'fromTable' parameter",
            );
        }
    };
    let project_root = match require_project_root(workspace, id) {
        Ok(root) => root,
        Err(response) => return response,
    };
    let tables = workspace
        .symbols
        .get_by_name(table_name)
        .into_iter()
        .filter(|entry| entry.kind == al_symbols::ObjectKind::Table)
        .collect::<Vec<_>>();
    let table = match tables.as_slice() {
        [table] => table,
        [] => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("Table '{table_name}' was not found in loaded symbols"),
            );
        }
        _ => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("Table '{table_name}' is ambiguous across loaded packages"),
            );
        }
    };
    let tooltips = table
        .fields
        .iter()
        .filter_map(|field| {
            let tooltip = field
                .properties
                .iter()
                .find(|property| property.name.eq_ignore_ascii_case("ToolTip"))
                .map(|property| property.value.clone())?;
            Some((field.name.clone(), tooltip))
        })
        .collect::<Vec<_>>();

    let plan = match al_analysis::queries::bulk_fix::plan_tooltips(&project_root, &tooltips) {
        Ok(plan) => plan,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!("tooltip fix planning failed: {error}"),
            );
        }
    };
    apply_bulk_fix_plan(workspace, id, plan, dry_run, "tooltip")
}
pub(in crate::server::daemon) fn dispatch_fix_data_classification(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let value = match data_classification_param(params) {
        Ok(value) => value,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let dry_run = match optional_bool_param(params, "dryRun", false) {
        Ok(dry_run) => dry_run,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    let project_root = match require_project_root(workspace, id) {
        Ok(root) => root,
        Err(response) => return response,
    };

    let plan = match al_analysis::queries::bulk_fix::plan_data_classification(&project_root, value)
    {
        Ok(plan) => plan,
        Err(error) => {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!("data-classification fix planning failed: {error}"),
            );
        }
    };
    apply_bulk_fix_plan(workspace, id, plan, dry_run, "data-classification")
}

fn apply_bulk_fix_plan(
    workspace: &Workspace,
    id: u64,
    plan: al_analysis::queries::bulk_fix::BulkFixPlan,
    dry_run: bool,
    label: &str,
) -> Response {
    let result = plan.result(dry_run);
    if dry_run {
        return serialized_response(id, &format!("{label} fix result"), &result);
    }

    for change in &plan.changes {
        let uri = match url::Url::from_file_path(&change.path) {
            Ok(uri) => uri,
            Err(()) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!(
                        "planned bulk-fix path cannot be represented as a file URI: {}",
                        change.path.display()
                    ),
                );
            }
        };
        if let Err(response) = ensure_document(workspace, &uri, id) {
            return response;
        }
        let Some(indexed) = workspace.documents.get_text(&uri) else {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "bulk-fix document loader completed without publishing the source",
            );
        };
        if indexed != change.original {
            return rpc_error(
                id,
                error_codes::CODE_ANALYSIS_ERROR,
                &format!(
                    "bulk fix refused to overwrite changed or unsaved source '{}'; save or refresh and retry",
                    change.path.display()
                ),
            );
        }
    }

    let mut applied: Vec<(std::path::PathBuf, String)> = Vec::new();
    for change in &plan.changes {
        if let Err(error) =
            write_al_file_and_refresh(workspace, &change.path, change.updated.clone())
        {
            let mut rollback_errors = Vec::new();
            for (path, original) in applied.iter().rev() {
                if let Err(rollback_error) =
                    write_al_file_and_refresh(workspace, path, original.clone())
                {
                    rollback_errors.push(format!("{}: {rollback_error}", path.display()));
                }
            }
            let suffix = if rollback_errors.is_empty() {
                String::new()
            } else {
                format!("; rollback also failed: {}", rollback_errors.join("; "))
            };
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!(
                    "{label} bulk fix failed while updating '{}': {error}{suffix}",
                    change.path.display()
                ),
            );
        }
        applied.push((change.path.clone(), change.original.clone()));
    }
    serialized_response(id, &format!("{label} fix result"), &result)
}

fn property_value_param<'a>(
    params: &'a serde_json::Value,
    key: &str,
    default: &'a str,
) -> Result<&'a str, String> {
    let value = match params.get(key) {
        None => default,
        Some(value) => value
            .as_str()
            .ok_or_else(|| format!("'{key}' must be a string when supplied"))?,
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(format!("'{key}' must not be empty"));
    }
    if value
        .chars()
        .any(|character| matches!(character, ';' | '{' | '}' | '=' | '\r' | '\n'))
    {
        return Err(format!(
            "'{key}' contains characters that cannot appear in a property value"
        ));
    }
    Ok(value)
}

fn data_classification_param(params: &serde_json::Value) -> Result<&str, String> {
    let value = property_value_param(params, "value", "CustomerContent")?;
    const VALUES: &[&str] = &[
        "AccountData",
        "CustomerContent",
        "EndUserIdentifiableInformation",
        "EndUserPseudonymousIdentifiers",
        "OrganizationIdentifiableInformation",
        "SystemMetadata",
        "ToBeClassified",
    ];
    VALUES
        .iter()
        .copied()
        .find(|candidate| candidate.eq_ignore_ascii_case(value))
        .ok_or_else(|| {
            format!("'value' must be a supported DataClassification value; got '{value}'")
        })
}
pub(in crate::server::daemon) async fn dispatch_arch_lint(
    workspace: &Workspace,
    id: u64,
) -> Response {
    let root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone());
    let config = if let Some(root) = root {
        let path = root.join(".alarch.json");
        match tokio::fs::read_to_string(&path).await {
            Ok(json) => match al_analysis::queries::arch_lint::ArchConfig::from_json(&json) {
                Ok(config) => config,
                Err(error) => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("Invalid {}: {error}", path.display()),
                    );
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(error) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("Failed to read {}: {error}", path.display()),
                );
            }
        }
    } else {
        Default::default()
    };
    let violations = match al_analysis::queries::arch_lint::arch_lint(workspace, &config) {
        Ok(violations) => violations,
        Err(error) => {
            return rpc_error(id, error_codes::INTERNAL_ERROR, &error.to_string());
        }
    };
    match serde_json::to_value(&violations) {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err(error) => rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("serialize architecture lint results failed: {error}"),
        ),
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

    /// Open a real `.al` file on disk and return its `file://` URI string,
    /// suitable for the `{ "file": ... }` param shape the dispatchers accept.
    /// The file must exist because `file_uri_from_params` canonicalises it.
    fn write_al(tmp: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = tmp.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.canonicalize().unwrap().to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn lint_missing_file_param_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 1, &serde_json::json!({})).await;
        let err = resp.error.expect("missing file/uri must be invalid params");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn lint_single_file_reports_parse_errors() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // Deliberately malformed AL — unterminated object.
        let file = write_al(&tmp, "Bad.al", "codeunit 50100 \"Bad\" { procedure X( ");
        let resp = dispatch_lint(&ws, 2, &serde_json::json!({ "file": file })).await;
        assert!(
            resp.error.is_none(),
            "lint should succeed: {:?}",
            resp.error
        );
        let diags = resp
            .result
            .as_ref()
            .and_then(|v| v.as_array())
            .expect("diagnostics array");
        assert!(
            diags
                .iter()
                .any(|d| d.get("code") == Some(&serde_json::json!("syntax"))),
            "malformed AL must produce a syntax diagnostic; got {diags:?}"
        );
    }

    #[tokio::test]
    async fn lint_all_mode_returns_array_for_empty_workspace() {
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 3, &serde_json::json!({ "all": true })).await;
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        assert!(arr.is_empty(), "empty workspace → no per-file lint entries");
    }

    #[tokio::test]
    async fn lint_all_rejects_an_indexed_file_without_a_cached_parse() {
        let ws = empty_ws();
        ws.file_index.files.insert(
            std::path::PathBuf::from("/proj/MissingCache.al"),
            r#"codeunit 50100 "Missing Cache" { }"#.to_string(),
        );

        let resp = dispatch_lint(&ws, 4, &serde_json::json!({ "all": true })).await;
        let error = resp
            .error
            .expect("an incomplete workspace must not return a partial lint array");
        assert_eq!(error.code, error_codes::INTERNAL_ERROR);
        assert!(error.message.contains("MissingCache.al"));
    }

    #[tokio::test]
    async fn arch_lint_rejects_malformed_configuration() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".alarch.json"),
            r#"{"rules":[{"id":"CX","description":"complexity","kind":"maxComplexity","values":["not-a-number"]}]}"#,
        )
        .unwrap();
        let workspace = Workspace::new();
        *workspace.project.write().await = Some(al_project::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: al_project::project::AppManifest {
                id: "test".to_string(),
                name: "Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
        });

        let response = dispatch_arch_lint(&workspace, 4).await;
        let error = response
            .error
            .expect("malformed config must be an RPC error");
        assert_eq!(error.code, error_codes::INVALID_PARAMS);
        assert!(error.message.contains(".alarch.json"));
        assert!(error.message.contains("complexity threshold"));
    }

    #[tokio::test]
    async fn format_content_returns_formatted_text() {
        let ws = empty_ws();
        let resp = dispatch_format(
            &ws,
            1,
            &serde_json::json!({ "content": "codeunit 50100 \"X\"\n{\n}" }),
        )
        .await;
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        assert!(r.get("formatted").and_then(|v| v.as_str()).is_some());
        assert!(r.get("changed").and_then(|v| v.as_bool()).is_some());
    }

    #[tokio::test]
    async fn format_check_mode_only_reports_changed_flag() {
        let ws = empty_ws();
        let resp = dispatch_format(
            &ws,
            2,
            &serde_json::json!({ "content": "codeunit 50100 X {}", "check": true }),
        )
        .await;
        let r = resp.result.expect("result");
        assert!(r.get("changed").is_some(), "check mode must report changed");
        assert!(
            r.get("formatted").is_none(),
            "check mode must NOT include formatted text"
        );
    }

    #[tokio::test]
    async fn format_missing_content_and_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_format(&ws, 3, &serde_json::json!({})).await;
        let err = resp.error.expect("no content and no file → invalid params");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn format_and_lint_reject_wrong_typed_boolean_params() {
        let ws = empty_ws();
        let format = dispatch_format(
            &ws,
            10,
            &serde_json::json!({ "content": "codeunit 50100 X {}", "check": "yes" }),
        )
        .await;
        assert_eq!(
            format.error.expect("wrong check type").code,
            error_codes::INVALID_PARAMS
        );

        let lint = dispatch_lint(&ws, 11, &serde_json::json!({ "all": "yes" })).await;
        assert_eq!(
            lint.error.expect("wrong all type").code,
            error_codes::INVALID_PARAMS
        );
    }

    #[tokio::test]
    async fn format_rejects_wrong_typed_content() {
        let ws = empty_ws();
        let response = dispatch_format(&ws, 12, &serde_json::json!({ "content": 42 })).await;
        assert_eq!(
            response.error.expect("wrong content type").code,
            error_codes::INVALID_PARAMS
        );
    }

    #[test]
    fn bulk_fix_dispatchers_reject_malformed_params_before_project_lookup() {
        let ws = empty_ws();
        for response in [
            dispatch_fix(&ws, 20, &serde_json::json!({ "dryRun": "yes" })),
            dispatch_fix(&ws, 21, &serde_json::json!({ "rule": 7 })),
            dispatch_fix_application_area(
                &ws,
                22,
                &serde_json::json!({ "value": "All; Caption = 'injected'" }),
            ),
            dispatch_fix_tooltips(&ws, 23, &serde_json::json!({})),
            dispatch_fix_data_classification(
                &ws,
                24,
                &serde_json::json!({ "value": "NotAClassification" }),
            ),
        ] {
            assert_eq!(
                response.error.expect("malformed params must fail").code,
                error_codes::INVALID_PARAMS
            );
        }
    }

    #[test]
    fn fix_without_file_requires_a_loaded_project() {
        let ws = empty_ws();
        let resp = dispatch_fix(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INTERNAL_ERROR);
    }

    #[test]
    fn fix_reports_zero_fixes_for_clean_file() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "Ok.al", "codeunit 50100 \"Ok\"\n{\n}\n");
        let resp = dispatch_fix(&ws, 2, &serde_json::json!({ "file": file, "dryRun": true }));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["fixes"], serde_json::json!(0));
        assert_eq!(r["dryRun"], serde_json::json!(true));
    }

    #[test]
    fn fix_dry_run_reports_edit_and_apply_updates_workspace_and_disk() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let source = r#"table 50100 "Fix Me"
{
    fields
    {
        field(1; Name; Text[20])
        {
        }
    }
}
"#;
        let file = write_al(&tmp, "Fix.al", source);

        let preview = dispatch_fix(
            &ws,
            3,
            &serde_json::json!({ "file": file, "dryRun": true, "rule": "AL-NL002" }),
        );
        assert!(preview.error.is_none(), "{:?}", preview.error);
        let result = preview.result.unwrap();
        assert_eq!(result["diagnostics"], 1);
        assert_eq!(result["fixes"], 1);
        assert_eq!(result["filesChanged"], 1);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), source);

        let applied = dispatch_fix(
            &ws,
            4,
            &serde_json::json!({ "file": file, "rule": "AL-NL002" }),
        );
        assert!(applied.error.is_none(), "{:?}", applied.error);
        let updated = std::fs::read_to_string(&file).unwrap();
        assert!(updated.contains("DataClassification = CustomerContent;"));
        let uri = url::Url::from_file_path(&file).unwrap();
        assert_eq!(
            ws.documents.get_text(&uri).as_deref(),
            Some(updated.as_str())
        );

        let idempotent = dispatch_fix(
            &ws,
            5,
            &serde_json::json!({ "file": file, "dryRun": true, "rule": "AL-NL002" }),
        );
        assert_eq!(idempotent.result.unwrap()["fixes"], 0);
    }

    #[test]
    fn fix_rejects_unknown_rule_instead_of_reporting_false_zero() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "Fix.al", "codeunit 50100 X { }\n");
        let response = dispatch_fix(
            &ws,
            6,
            &serde_json::json!({ "file": file, "rule": "NOT-A-RULE" }),
        );
        assert_eq!(
            response.error.expect("unknown rule must fail").code,
            error_codes::INVALID_PARAMS
        );
    }

    #[test]
    fn rules_returns_json_array_mirroring_registry() {
        let resp = dispatch_rules(1);
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        let expected = al_syntax::lint_rules().len()
            + al_analysis::queries::transaction_lint::transaction_lint_rules().len()
            + al_analysis::queries::native_check::native_check_rules().len()
            + al_analysis::queries::diagnostics::native_workspace_lint_rules().len();
        assert_eq!(
            arr.len(),
            expected,
            "dispatch_rules must list every registry"
        );
    }

    #[test]
    fn parse_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_parse(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_reports_node_count_and_errors() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "P.al", "codeunit 50100 \"P\"\n{\n}\n");
        let resp = dispatch_parse(&ws, 2, &serde_json::json!({ "file": file }));
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        let nodes = r["nodeCount"].as_u64().expect("nodeCount");
        assert!(nodes > 0, "a non-empty file must parse to >0 nodes");
        assert!(r.get("parseErrors").is_some());
    }
}
