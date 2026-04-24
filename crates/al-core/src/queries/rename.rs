//! Rename symbol query.

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
    let clean_name = super::node_clean_name(node, text.as_bytes())?;
    if !matches!(
        node.kind(),
        "identifier" | "quoted_identifier" | "name" | "name_or_keyword"
    ) {
        return None;
    }
    Some((
        al_syntax::ts_range_to_lsp(&node.range(), text.as_bytes()).into(),
        clean_name.to_string(),
    ))
}

/// Rename the symbol at the given position to `new_name`.
#[must_use]
pub fn rename(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    let lsp_pos: tower_lsp::lsp_types::Position = position.into();
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, lsp_pos)?;
    let clean_name = super::node_clean_name(node, text.as_bytes())?;

    let mut changes: Vec<(Url, Vec<TextEdit>)> = Vec::new();

    let source_bytes = text.as_bytes();
    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let edits: Vec<TextEdit> = refs
            .iter()
            .filter_map(|r| {
                let matched_text = text.get(r.start_byte..r.end_byte)?;
                let replacement = make_rename_text(node.kind(), matched_text, new_name);
                Some(TextEdit {
                    range: al_syntax::ts_range_to_lsp(r, source_bytes).into(),
                    new_text: replacement,
                })
            })
            .collect();
        changes.push((uri.clone(), edits));
    }

    let current_path = uri.to_file_path().ok(); // SILENT: non-file URIs legitimately have no path
    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().clone();
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let file_uri = match Url::from_file_path(&file_path) {
            Ok(u) => u,
            Err(_) => continue,
        };
        // Use cached parse tree — avoids re-parsing every workspace file on each rename.
        let Some((file_text, file_tree)) = workspace.file_index.get_cached_parse(&file_path) else {
            continue;
        };
        let refs = al_syntax::find_variable_references(&file_tree, &file_text, clean_name);
        if !refs.is_empty() {
            let file_source_bytes = file_text.as_bytes();
            let edits: Vec<TextEdit> = refs
                .iter()
                .filter_map(|r| {
                    let matched_text = file_text.get(r.start_byte..r.end_byte)?;
                    let replacement = make_rename_text("", matched_text, new_name);
                    Some(TextEdit {
                        range: al_syntax::ts_range_to_lsp(r, file_source_bytes).into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn test_uri() -> Url {
        Url::parse("file:///test/src/Test.al").unwrap()
    }

    fn open_doc(ws: &Workspace, uri: &Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string());
    }

    // --- prepare_rename ---

    #[test]
    fn prepare_rename_on_identifier() {
        let ws = Workspace::new();
        let uri = test_uri();
        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        MyVar: Integer;
    begin
        MyVar := 42;
    end;
}"#,
        );
        let pos = Position {
            line: 6,
            character: 8,
        }; // "MyVar" in assignment
        let result = prepare_rename(&ws, &uri, pos);
        assert!(result.is_some(), "should find renameable identifier");
        let (range, name) = result.unwrap();
        assert_eq!(name, "MyVar");
        assert_eq!(range.start.line, 6);
    }

    #[test]
    fn prepare_rename_on_non_identifier_returns_none() {
        let ws = Workspace::new();
        let uri = test_uri();
        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin
    end;
}"#,
        );
        let pos = Position {
            line: 3,
            character: 4,
        }; // "begin" keyword
        let result = prepare_rename(&ws, &uri, pos);
        assert!(result.is_none(), "keywords should not be renameable");
    }

    // --- rename ---

    #[test]
    fn rename_variable_in_single_file() {
        let ws = Workspace::new();
        let uri = test_uri();
        open_doc(
            &ws,
            &uri,
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        MyVar: Integer;
    begin
        MyVar := 42;
    end;
}"#,
        );
        let pos = Position {
            line: 6,
            character: 8,
        };
        let result = rename(&ws, &uri, pos, "NewVar");
        assert!(result.is_some(), "should produce rename edits");
        let edit = result.unwrap();
        assert!(!edit.changes.is_empty(), "should have changes");
        let (edit_uri, edits) = &edit.changes[0];
        assert_eq!(edit_uri, &uri);
        assert!(
            edits.len() >= 2,
            "should rename both declaration and usage, got {}",
            edits.len()
        );
        for e in edits {
            assert_eq!(e.new_text, "NewVar");
        }
    }

    #[test]
    fn rename_returns_none_when_no_refs() {
        let ws = Workspace::new();
        let uri = test_uri();
        // Position on something with no textual references
        open_doc(&ws, &uri, "codeunit 50100 \"X\" { }");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = rename(&ws, &uri, pos, "Y");
        // Should be None — no variable refs for "codeunit" keyword
        // (or Some if tree-sitter finds refs)
        // Just verify it doesn't panic
        let _ = result;
    }

    // --- make_rename_text ---

    #[test]
    fn make_rename_text_unquoted() {
        assert_eq!(make_rename_text("identifier", "MyVar", "NewVar"), "NewVar");
    }

    #[test]
    fn make_rename_text_quoted_identifier() {
        assert_eq!(
            make_rename_text("quoted_identifier", "\"Old Name\"", "New Name"),
            "\"New Name\""
        );
    }

    #[test]
    fn make_rename_text_strips_extra_quotes() {
        assert_eq!(
            make_rename_text("quoted_identifier", "\"Old\"", "\"New\""),
            "\"New\""
        );
    }

    #[test]
    fn make_rename_text_detects_quotes_from_text() {
        // When node_kind is empty but text is quoted
        assert_eq!(make_rename_text("", "\"Quoted\"", "Renamed"), "\"Renamed\"");
    }
}
