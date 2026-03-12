//! Go-to-definition handler.
//!
//! Lookup order:
//! 1. Local definitions (variables, parameters in the same procedure)
//! 2. Cross-file definitions (procedures in workspace .al files)
//! 3. Package symbol definitions (objects from .app files — virtual file generation)

use al_symbols::SymbolEntry;
use al_syntax::AlParser;
use tower_lsp::lsp_types::*;

use crate::parsing;
use crate::resolution::{self, ResolvedMemberKind};
use crate::server::AlServer;

/// Handle textDocument/definition.
pub(crate) fn handle_definition(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    let looks_like_object_name_early = node.kind() == "quoted_identifier" || clean_name.contains(' ');

    if let Some(access) = resolution::access_path_at(&tree, &text, position) {
        if let Some(receiver) =
            resolution::resolve_expression_type(server, uri, &text, &tree, &access.receiver, position)
        {
            if let Some(member) = resolution::resolve_member(server, uri, &receiver, &access.member) {
                match member.kind {
                    ResolvedMemberKind::Variable { range: Some(range), .. }
                    | ResolvedMemberKind::Procedure { range: Some(range), .. }
                    | ResolvedMemberKind::Field { range: Some(range) }
                    | ResolvedMemberKind::EnumValue { range: Some(range) } => {
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: member.uri.unwrap_or_else(|| uri.clone()),
                            range,
                        }));
                    }
                    _ => {
                        if let Some(entry) = find_package_entry_for_type(server, &receiver.type_name, receiver.type_subtype.as_deref()) {
                            if let Some((file_uri, range)) = get_or_create_virtual_file(server, &entry, Some(&access.member)) {
                                return Some(GotoDefinitionResponse::Scalar(Location {
                                    uri: file_uri,
                                    range,
                                }));
                            }
                        }
                    }
                }
            }
        }
    }

    let looks_like_object_name = node.kind() == "quoted_identifier" || clean_name.contains(' ');
    if looks_like_object_name {
        if let Some((obj_uri, range)) = resolution::resolve_workspace_object_definition(server, clean_name) {
            return Some(GotoDefinitionResponse::Scalar(Location { uri: obj_uri, range }));
        }

        let pkg_entries = server.symbols.get_by_name(clean_name);
        if let Some(entry) = pkg_entries.into_iter().find(|e| !e.kind.is_extension()) {
            if let Some((file_uri, range)) = get_or_create_virtual_file(server, &entry, None) {
                return Some(GotoDefinitionResponse::Scalar(Location {
                    uri: file_uri,
                    range,
                }));
            }
        }
    }

    let resolver = al_syntax::TypeResolver::new(&tree, &text);
    if let Some(decl) = resolver.resolve_type(clean_name, position) {
        let def_range = al_syntax::ts_range_to_lsp(&decl.range);
        if def_range.start != position {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: def_range,
            }));
        }
    }

    // textual fallback
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if refs.len() > 1 {
        let first = &refs[0];
        let def_range = al_syntax::ts_range_to_lsp(first);
        if def_range.start != position {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: def_range,
            }));
        }
    }

    let current_path = uri.to_file_path().ok();

    if let Some(obj_path_entry) = server.workspace_objects.get(&clean_name.to_lowercase()) {
        let file_path = obj_path_entry.value().clone();
        let is_current = current_path.as_ref().is_some_and(|cp| *cp == file_path);
        if !is_current {
            if let Some(file_text_entry) = server.workspace_files.get(&file_path) {
                let file_text = file_text_entry.value();
                let result = AlParser::parse_quick(file_text);
                if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, file_text) {
                    if let Ok(file_uri) = Url::from_file_path(&file_path) {
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: file_uri,
                            range: al_syntax::ts_range_to_lsp(&obj_info.range),
                        }));
                    }
                }
            }
        }
    }

    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();
        if current_path.as_ref() == Some(file_path) { continue; }
        let result = AlParser::parse_quick(file_text);
        let doc_symbols = al_syntax::extract_document_symbols(&result.tree, file_text);
        for sym in &doc_symbols {
            if let Some(children) = &sym.children {
                for child in children {
                    if (child.kind == SymbolKind::FUNCTION || child.kind == SymbolKind::EVENT)
                        && child.name.eq_ignore_ascii_case(clean_name)
                    {
                        if let Ok(file_uri) = Url::from_file_path(file_path) {
                            return Some(GotoDefinitionResponse::Scalar(Location {
                                uri: file_uri,
                                range: child.selection_range,
                            }));
                        }
                    }
                }
            }
        }
    }

    let symbols = server.symbols.get_by_name(clean_name);
    if let Some(entry) = symbols.into_iter().find(|e| !e.kind.is_extension()) {
        if let Some((file_uri, range)) = get_or_create_virtual_file(server, &entry, None) {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: file_uri,
                range,
            }));
        }
    }

    None
}

fn get_or_create_virtual_file(
    server: &AlServer,
    entry: &SymbolEntry,
    member_name: Option<&str>,
) -> Option<(Url, Range)> {
    let app_path = server.symbols.app_path(&entry.package);
    let allow_fallback = server
        .outline_fallback_approved
        .load(std::sync::atomic::Ordering::Relaxed);
    match al_symbols::virtual_file::get_or_create(entry, app_path.as_deref(), allow_fallback) {
        Ok(path) => {
            let uri = Url::from_file_path(&path).ok()?;
            let range = member_name
                .and_then(|name| find_member_range_in_file(&path, name))
                .unwrap_or_default();
            Some((uri, range))
        }
        Err(_) => None
    }
}

fn find_member_range_in_file(path: &std::path::Path, member_name: &str) -> Option<Range> {
    let range = al_symbols::virtual_file::find_member_range(
        path,
        member_name,
        al_symbols::virtual_file::MemberKind::Unknown,
    )?;
    Some(Range::new(
        Position::new(range.line, range.col_start),
        Position::new(range.line, range.col_end),
    ))
}

/// Handle textDocument/references.
pub(crate) fn handle_references(
    server: &AlServer,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    let mut locations = Vec::new();

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    for r in &refs {
        let range = al_syntax::ts_range_to_lsp(r);
        if !include_declaration && range.start == position {
            continue;
        }
        locations.push(Location {
            uri: uri.clone(),
            range,
        });
    }

    let current_path = uri.to_file_path().ok();
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let result = AlParser::parse_quick(file_text);
        let refs = al_syntax::find_variable_references(&result.tree, file_text, clean_name);

        for r in &refs {
            if let Ok(file_uri) = Url::from_file_path(file_path) {
                locations.push(Location {
                    uri: file_uri,
                    range: al_syntax::ts_range_to_lsp(r),
                });
            }
        }
    }

    if locations.is_empty() {
        None
    } else {
        Some(locations)
    }
}

/// Handle textDocument/rename.
pub(crate) fn handle_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
    new_name: String,
) -> Option<WorkspaceEdit> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    let mut changes = std::collections::HashMap::new();

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let edits: Vec<TextEdit> = refs
            .iter()
            .map(|r| {
                let matched_text = &text[r.start_byte..r.end_byte];
                let replacement = make_rename_text(node.kind(), matched_text, &new_name);
                TextEdit {
                    range: al_syntax::ts_range_to_lsp(r),
                    new_text: replacement,
                }
            })
            .collect();
        changes.insert(uri.clone(), edits);
    }

    let current_path = uri.to_file_path().ok();
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let file_uri = match Url::from_file_path(file_path) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let result = AlParser::parse_quick(file_text);
        let refs = al_syntax::find_variable_references(&result.tree, file_text, clean_name);

        if !refs.is_empty() {
            let edits: Vec<TextEdit> = refs
                .iter()
                .map(|r| {
                    let matched_text = &file_text[r.start_byte..r.end_byte];
                    let replacement = make_rename_text("", matched_text, &new_name);
                    TextEdit {
                        range: al_syntax::ts_range_to_lsp(r),
                        new_text: replacement,
                    }
                })
                .collect();
            changes.insert(file_uri, edits);
        }
    }

    if changes.is_empty() {
        return None;
    }

    Some(WorkspaceEdit {
        changes: Some(changes),
        ..Default::default()
    })
}

/// Handle textDocument/prepareRename.
pub(crate) fn handle_prepare_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let (text, tree) = parsing::get_or_parse(server, uri)?;

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    if !matches!(
        node.kind(),
        "identifier" | "quoted_identifier" | "name" | "name_or_keyword"
    ) {
        return None;
    }

    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: al_syntax::ts_range_to_lsp(&node.range()),
        placeholder: clean_name.to_string(),
    })
}

fn make_rename_text(node_kind: &str, original_text: &str, new_name: &str) -> String {
    let is_quoted = node_kind == "quoted_identifier"
        || (original_text.starts_with('"') && original_text.ends_with('"'));
    if is_quoted {
        let clean = new_name.trim_matches('"');
        format!("\"{}\"", clean)
    } else {
        new_name.to_string()
    }
}

fn find_package_entry_for_type(
    server: &AlServer,
    type_name: &str,
    subtype: Option<&str>,
) -> Option<std::sync::Arc<SymbolEntry>> {
    let obj_name = subtype.or({
        if !matches!(type_name, "Record" | "Page" | "Codeunit" | "Report" | "Query" | "Xmlport" | "Enum") {
            Some(type_name)
        } else {
            None
        }
    })?;

    server
        .symbols
        .get_by_name(obj_name)
        .into_iter()
        .find(|e| !e.kind.is_extension())
}
