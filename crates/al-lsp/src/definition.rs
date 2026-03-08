//! Go-to-definition handler.
//!
//! Lookup order:
//! 1. Local definitions (variables, parameters in the same procedure)
//! 2. Cross-file definitions (procedures in workspace .al files)
//! 3. Package symbol definitions (objects from .app files — no location, just name match)

use tower_lsp::lsp_types::*;

use crate::server::AlServer;

/// Handle textDocument/definition.
pub(crate) fn handle_definition(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

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

    // 2. Cross-file definitions: search workspace files for matching procedures/objects
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

        // Check object declaration name
        if let Some(obj_info) = al_syntax::find_object_declaration(&result.tree, file_text) {
            if obj_info.name.eq_ignore_ascii_case(clean_name) {
                if let Ok(file_uri) = Url::from_file_path(file_path) {
                    return Some(GotoDefinitionResponse::Scalar(Location {
                        uri: file_uri,
                        range: al_syntax::ts_range_to_lsp(&obj_info.range),
                    }));
                }
            }
        }

        // Check procedures in other files
        let refs = al_syntax::find_variable_references(&result.tree, file_text, clean_name);
        if !refs.is_empty() {
            // Look for procedure declarations
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
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

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

/// Handle textDocument/rename.
pub(crate) fn handle_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
    new_name: String,
) -> Option<WorkspaceEdit> {
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

    let node = al_syntax::find_node_at_position(&tree, position)?;
    let source = text.as_bytes();
    let node_text = node.utf8_text(source).ok()?;
    let clean_name = node_text.trim_matches('"');

    if clean_name.is_empty() {
        return None;
    }

    // Find all references in the current file
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if refs.is_empty() {
        return None;
    }

    let edits: Vec<TextEdit> = refs
        .iter()
        .map(|r| TextEdit {
            range: al_syntax::ts_range_to_lsp(r),
            new_text: new_name.clone(),
        })
        .collect();

    let mut changes = std::collections::HashMap::new();
    changes.insert(uri.clone(), edits);

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
    let text = server.documents.get_text(uri)?;

    let tree = {
        let mut parser = server.parser.lock().unwrap();
        let result = parser.parse(&text);
        result.tree
    };

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

    Some(PrepareRenameResponse::Range(al_syntax::ts_range_to_lsp(
        &node.range(),
    )))
}
