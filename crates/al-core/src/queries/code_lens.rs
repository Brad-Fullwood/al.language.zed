//! CodeLens query — reference count lenses on procedure/method/event declarations.

use std::collections::HashSet;

use tower_lsp::lsp_types::{Range, SymbolKind};
use url::Url;

use crate::workspace::Workspace;

/// A transport-agnostic CodeLens entry.
pub struct CodeLensEntry {
    pub range: Range,
    pub title: String,
}

/// Return CodeLens entries for all referenceable symbols in the document.
///
/// For each procedure, trigger, event, field, or variable declaration the lens
/// shows the number of references found across the whole workspace.
pub fn code_lens(workspace: &Workspace, uri: &Url) -> Vec<CodeLensEntry> {
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return vec![];
    };

    let symbols = al_syntax::extract_document_symbols(&tree, &text);
    let mut lenses = Vec::new();

    for sym in &symbols {
        // Top-level symbols (objects) — recurse into children
        if let Some(children) = &sym.children {
            for child in children {
                if is_referenceable(child.kind) {
                    let count = count_references_by_name(workspace, uri, &child.name);
                    let title = reference_label(count);
                    lenses.push(CodeLensEntry {
                        range: child.selection_range,
                        title,
                    });
                }
            }
        }
        // Also include top-level referenceable symbols (rare in AL, but complete)
        if is_referenceable(sym.kind) {
            let count = count_references_by_name(workspace, uri, &sym.name);
            let title = reference_label(count);
            lenses.push(CodeLensEntry {
                range: sym.selection_range,
                title,
            });
        }
    }

    lenses
}

fn reference_label(count: usize) -> String {
    if count == 1 {
        "1 reference".to_string()
    } else {
        format!("{} references", count)
    }
}

fn is_referenceable(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::FUNCTION
            | SymbolKind::METHOD
            | SymbolKind::EVENT
            | SymbolKind::FIELD
            | SymbolKind::VARIABLE
    )
}

/// Count workspace-wide references to `name`, deduplicating by (uri, line, col).
fn count_references_by_name(workspace: &Workspace, current_uri: &Url, name: &str) -> usize {
    let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
    let clean_name = name.trim_matches('"');

    // Search the current document
    if let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, current_uri) {
        let source_bytes = text.as_bytes();
        let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
        for r in &refs {
            let range = al_syntax::ts_range_to_lsp(r, source_bytes);
            let key = (
                current_uri.to_string(),
                range.start.line,
                range.start.character,
            );
            seen.insert(key);
        }
    }

    // Search all other workspace files
    let current_path = current_uri.to_file_path().ok();
    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().clone();
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        let file_source_bytes = file_text.as_bytes();
        let refs = al_syntax::find_variable_references(&file_tree, &file_text, clean_name);
        for r in &refs {
            if let Ok(file_uri) = Url::from_file_path(&file_path) {
                let range = al_syntax::ts_range_to_lsp(r, file_source_bytes);
                let key = (
                    file_uri.to_string(),
                    range.start.line,
                    range.start.character,
                );
                seen.insert(key);
            }
        }
    }

    seen.len()
}
