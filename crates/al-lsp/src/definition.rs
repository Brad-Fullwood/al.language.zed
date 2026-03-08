//! Go-to-definition handler.
//!
//! Lookup order:
//! 1. Local definitions (variables, parameters in the same procedure)
//! 2. Cross-file definitions (procedures in workspace .al files)
//! 3. Package symbol definitions (objects from .app files — no location, just name match)

use tower_lsp::lsp_types::*;

use crate::parsing;
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

    // 1. Local definitions: look for variable/parameter declarations in the same procedure
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if refs.len() > 1 {
        // The first reference is typically the declaration
        let first = &refs[0];
        let def_range = al_syntax::ts_range_to_lsp(first);
        // Only return if the definition is NOT the same position we're on
        if def_range.start != position {
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri.clone(),
                range: def_range,
            }));
        }
    }

    // 2. Cross-file definitions: check workspace object name index first
    if let Some(obj_path_entry) = server.workspace_objects.get(&clean_name.to_lowercase()) {
        let file_path = obj_path_entry.value().clone();
        // Skip the current file
        let is_current = uri.to_file_path().map_or(false, |cp| cp == file_path);
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
        if let Ok(current_path) = uri.to_file_path() {
            if *file_path == current_path {
                continue;
            }
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
        // Package symbols don't have file locations, so we can't jump to them.
        // Return None to indicate no definition found.
        // In the future, we could synthesize a virtual document showing the symbol's API.
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
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file
        if let Ok(current_path) = uri.to_file_path() {
            if *file_path == current_path {
                continue;
            }
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
    for entry in server.workspace_files.iter() {
        let file_path = entry.key();
        let file_text = entry.value();

        // Skip the current file — already handled above
        if let Ok(current_path) = uri.to_file_path() {
            if *file_path == current_path {
                continue;
            }
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
