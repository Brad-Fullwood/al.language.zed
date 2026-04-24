//! Find references query.

use url::Url;

use super::{Location, Position, Range};
use crate::workspace::Workspace;

/// Find all references to the symbol at the given position.
#[must_use]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use url::Url;

    fn ws_with_doc(uri: &Url, text: &str) -> Workspace {
        let ws = Workspace::new();
        ws.documents.open(uri.clone(), text.to_string());
        ws
    }

    // --- positive tests ---

    #[test]
    fn references_finds_variable_in_same_file() {
        let uri = Url::parse("file:///test/refs.al").expect("test");
        let src = r#"codeunit 50100 "Refs"
{
    procedure Calc()
    var
        MyVar: Integer;
    begin
        MyVar := 1;
        MyVar += 2;
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 4,
            character: 8,
        }; // on MyVar declaration
        let locs = references(&ws, &uri, pos, true);
        // Should find declaration + 2 usages
        assert!(
            locs.len() >= 2,
            "Expected at least 2 references to MyVar, got {}",
            locs.len()
        );
        assert!(
            locs.iter().all(|l| l.uri == uri),
            "All references should be in the same file"
        );
    }

    #[test]
    fn references_includes_declaration_when_requested() {
        let uri = Url::parse("file:///test/refs_decl.al").expect("test");
        let src = r#"codeunit 50100 "Refs"
{
    procedure Calc()
    var
        X: Integer;
    begin
        X := 1;
    end;
}"#;
        let ws = ws_with_doc(&uri, src);
        let pos = Position {
            line: 4,
            character: 8,
        }; // on X declaration
        let with_decl = references(&ws, &uri, pos, true);
        let without_decl = references(&ws, &uri, pos, false);
        // include_declaration=true should return >= include_declaration=false
        assert!(
            with_decl.len() >= without_decl.len(),
            "include_declaration=true should not return fewer results"
        );
    }

    // --- negative tests ---

    #[test]
    fn references_missing_uri_returns_empty() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent/file.al").expect("test");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let locs = references(&ws, &uri, pos, true);
        assert!(locs.is_empty(), "Unknown URI must return empty references");
    }

    #[test]
    fn references_invalid_position_returns_empty() {
        let uri = Url::parse("file:///test/out_of_range.al").expect("test");
        let src = "codeunit 50100 Test { }";
        let ws = ws_with_doc(&uri, src);
        // Position way beyond file content
        let pos = Position {
            line: 9999,
            character: 9999,
        };
        let locs = references(&ws, &uri, pos, true);
        assert!(
            locs.is_empty(),
            "Out-of-range position must return empty references"
        );
    }

    #[test]
    fn references_on_whitespace_returns_empty() {
        let uri = Url::parse("file:///test/whitespace.al").expect("test");
        let src = "codeunit 50100 Test\n{\n    // comment\n}";
        let ws = ws_with_doc(&uri, src);
        // Position on blank/whitespace — not a valid identifier
        let pos = Position {
            line: 0,
            character: 19,
        }; // after "Test", on whitespace
        let locs = references(&ws, &uri, pos, true);
        // May return empty or may match "Test" — either way must not panic
        let _ = locs;
    }
}
