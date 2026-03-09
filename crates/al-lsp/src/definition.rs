//! Go-to-definition handler.
//!
//! Lookup order:
//! 1. Local definitions (variables, parameters in the same procedure)
//! 2. Cross-file definitions (procedures in workspace .al files)
//! 3. Package symbol definitions (objects from .app files — no location, just name match)

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

    tracing::debug!(name = %clean_name, node_kind = %node.kind(), "definition: looking up");

    if let Some(access) = resolution::access_path_at(&tree, &text, position) {
        if let Some(receiver) =
            resolution::resolve_expression_type(server, uri, &text, &tree, &access.receiver, position)
        {
            if let Some(member) = resolution::resolve_member(server, uri, &receiver, &access.member) {
                match member.kind {
                    ResolvedMemberKind::Variable { range: Some(range), .. }
                    | ResolvedMemberKind::Procedure { range: Some(range), .. } => {
                        return Some(GotoDefinitionResponse::Scalar(Location {
                            uri: member.uri.unwrap_or_else(|| uri.clone()),
                            range,
                        }));
                    }
                    _ => {}
                }
            }
        }
    }

    let looks_like_object_name = node.kind() == "quoted_identifier" || clean_name.contains(' ');
    if looks_like_object_name {
        if let Some((obj_uri, range)) = resolution::resolve_workspace_object_definition(server, clean_name) {
            return Some(GotoDefinitionResponse::Scalar(Location { uri: obj_uri, range }));
        }

        if server
            .symbols
            .get_by_name(clean_name)
            .into_iter()
            .any(|entry| {
                matches!(
                    entry.kind,
                    al_symbols::ObjectKind::Table
                        | al_symbols::ObjectKind::Page
                        | al_symbols::ObjectKind::Codeunit
                        | al_symbols::ObjectKind::Report
                        | al_symbols::ObjectKind::Query
                        | al_symbols::ObjectKind::XmlPort
                        | al_symbols::ObjectKind::Enum
                        | al_symbols::ObjectKind::Interface
                        | al_symbols::ObjectKind::PermissionSet
                        | al_symbols::ObjectKind::Profile
                        | al_symbols::ObjectKind::PageCustomization
                        | al_symbols::ObjectKind::ControlAddIn
                        | al_symbols::ObjectKind::Entitlement
                )
            })
        {
            tracing::debug!(
                name = %clean_name,
                "definition: package object found but no file location is available"
            );
            return None;
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

    // 1. Local textual fallback
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

    // Pre-resolve the current file path once for all workspace lookups
    let current_path = uri.to_file_path().ok();

    // 2. Cross-file definitions: check workspace object name index first
    if let Some(obj_path_entry) = server.workspace_objects.get(&clean_name.to_lowercase()) {
        let file_path = obj_path_entry.value().clone();
        // Skip the current file
        let is_current = current_path.as_ref().map_or(false, |cp| *cp == file_path);
        if !is_current {
            if let Some(file_text_entry) = server.workspace_files.get(&file_path) {
                let file_text = file_text_entry.value();
                let mut parser = server.parser.lock().unwrap();
                let result = parser.parse(file_text);
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

    // 2b. Search workspace files for matching procedures (not in the name index)
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file (already handled above)
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(file_text);

        // Check procedures in other files
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

    // 3. Package symbols — no file location available, but we can note the package
    let symbols = server.symbols.get_by_name(clean_name);
    if !symbols.is_empty() {
        tracing::debug!(
            name = %clean_name,
            count = symbols.len(),
            packages = ?symbols.iter().map(|s| s.package.as_str()).collect::<Vec<_>>(),
            "definition: found in packages but no file location"
        );
    } else {
        tracing::debug!(
            name = %clean_name,
            index_size = server.symbols.len(),
            workspace_files = server.workspace_files.len(),
            workspace_objects = server.workspace_objects.len(),
            "definition: symbol not found anywhere"
        );
    }

    None
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

    // Find references in the current file
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

    // Find references in workspace files
    let current_path = uri.to_file_path().ok();
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(file_text);
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

/// Build the replacement text for a rename, preserving quoted identifiers.
///
/// If the original node was a quoted_identifier (e.g. `"My Procedure"`), wrap
/// the new name in quotes. Otherwise return it as-is.
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

/// Handle textDocument/rename.
///
/// Renames the identifier at the cursor across the current file AND all
/// workspace files, returning a `WorkspaceEdit` with changes grouped by URI.
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

    // --- Current file ---
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let edits: Vec<TextEdit> = refs
            .iter()
            .map(|r| {
                // Determine per-reference whether the matched node is quoted
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

    // --- Workspace files (cross-file rename) ---
    let current_path = uri.to_file_path().ok();
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file — already handled above
        if current_path.as_ref() == Some(file_path) {
            continue;
        }

        let file_uri = match Url::from_file_path(file_path) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(file_text);
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
///
/// Validates that the cursor is on a renameable identifier and returns the
/// range together with a placeholder (the unquoted name).
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

    // Only allow renaming identifiers
    if !matches!(
        node.kind(),
        "identifier" | "quoted_identifier" | "name" | "name_or_keyword"
    ) {
        return None;
    }

    // Return range + placeholder so the editor pre-fills the current name
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: al_syntax::ts_range_to_lsp(&node.range()),
        placeholder: clean_name.to_string(),
    })
}
