//! Find references query.

use al_syntax::AlParser;
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
    let node_text = node.utf8_text(text.as_bytes()).unwrap_or("");
    let clean_name = node_text.trim_matches('"');
    if clean_name.is_empty() {
        return Vec::new();
    }

    let mut locations = Vec::new();

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    for r in &refs {
        let range: Range = al_syntax::ts_range_to_lsp(r).into();
        if !include_declaration && range.start == position {
            continue;
        }
        locations.push(Location { uri: uri.clone(), range });
    }

    let current_path = uri.to_file_path().ok(); // SILENT: non-file URIs legitimately have no path
    for entry in workspace.file_index.files.iter() {
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
                    range: al_syntax::ts_range_to_lsp(r).into(),
                });
            }
        }
    }

    locations
}
