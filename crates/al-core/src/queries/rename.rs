//! Rename symbol query.

use al_syntax::AlParser;
use url::Url;

use super::{Position, Range, TextEdit, WorkspaceEdit};
use crate::workspace::Workspace;

/// Prepare rename: check if the symbol at position can be renamed.
/// Returns the range of the symbol and its current name.
pub fn prepare_rename(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<(Range, String)> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, lsp_pos)?;
    let node_text = node.utf8_text(text.as_bytes()).unwrap_or("");
    let clean_name = node_text.trim_matches('"');
    if clean_name.is_empty() {
        return None;
    }
    if !matches!(node.kind(), "identifier" | "quoted_identifier" | "name" | "name_or_keyword") {
        return None;
    }
    Some((al_syntax::ts_range_to_lsp(&node.range()).into(), clean_name.to_string()))
}

/// Rename the symbol at the given position to `new_name`.
pub fn rename(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, lsp_pos)?;
    let node_text = node.utf8_text(text.as_bytes()).unwrap_or("");
    let clean_name = node_text.trim_matches('"');
    if clean_name.is_empty() {
        return None;
    }

    let mut changes: Vec<(Url, Vec<TextEdit>)> = Vec::new();

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let edits: Vec<TextEdit> = refs
            .iter()
            .filter_map(|r| {
                let matched_text = text.get(r.start_byte..r.end_byte)?;
                let replacement = make_rename_text(node.kind(), matched_text, new_name);
                Some(TextEdit {
                    range: al_syntax::ts_range_to_lsp(r).into(),
                    new_text: replacement,
                })
            })
            .collect();
        changes.push((uri.clone(), edits));
    }

    let current_path = uri.to_file_path().ok(); // non-file URIs have no path
    for entry in workspace.file_index.files.iter() {
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
                .filter_map(|r| {
                    let matched_text = file_text.get(r.start_byte..r.end_byte)?;
                    let replacement = make_rename_text("", matched_text, new_name);
                    Some(TextEdit {
                        range: al_syntax::ts_range_to_lsp(r).into(),
                        new_text: replacement,
                    })
                })
                .collect();
            changes.push((file_uri, edits));
        }
    }

    if changes.is_empty() {
        return None;
    }

    Some(WorkspaceEdit { changes })
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
