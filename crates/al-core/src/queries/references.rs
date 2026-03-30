//! Find references query.

use url::Url;

use super::{Location, Position, Range};
use crate::workspace::Workspace;

/// Find all references to the symbol at the given position.
pub fn references(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Vec<Location> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let Some((text, tree)) = crate::parsing::get_or_parse(&workspace.documents, uri) else {
        return Vec::new();
    };

    let Some(node) = al_syntax::find_node_at_position(&tree, lsp_pos) else {
        return Vec::new();
    };
    let Some(clean_name) = super::node_clean_name(node, text.as_bytes()) else {
        return Vec::new();
    };

    let mut locations = Vec::new();

    let source_bytes = text.as_bytes();
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    for r in &refs {
        let range: Range = al_syntax::ts_range_to_lsp(r, source_bytes).into();
        if !include_declaration && range.start == position {
            continue;
        }
        locations.push(Location {
            uri: uri.clone(),
            range,
        });
    }

    let current_path = uri.to_file_path().ok(); // SILENT: non-file URIs legitimately have no path
    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().clone();
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        // Use cached parse tree — avoids re-parsing every workspace file on each request.
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        let refs = al_syntax::find_variable_references(&file_tree, &file_text, clean_name);
        let file_source_bytes = file_text.as_bytes();
        for r in &refs {
            if let Ok(file_uri) = Url::from_file_path(&file_path) {
                locations.push(Location {
                    uri: file_uri,
                    range: al_syntax::ts_range_to_lsp(r, file_source_bytes).into(),
                });
            }
        }
    }

    locations
}
