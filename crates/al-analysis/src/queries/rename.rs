//! Rename symbol query.

use url::Url;

use super::{Position, Range, TextEdit, WorkspaceEdit};
use al_workspace::Workspace;

pub fn prepare_rename(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<(Range, String)> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, &text, position.into())?;
    let clean_name = super::node_clean_name(node, text.as_bytes())?;
    if !matches!(
        node.kind(),
        "identifier" | "quoted_identifier" | "name" | "name_or_keyword"
    ) {
        return None;
    }
    Some((
        al_syntax::ts_range_to_syntax(&node.range(), text.as_bytes()).into(),
        clean_name.to_string(),
    ))
}

#[must_use]
pub fn rename(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    // Reject a new name that would splice invalid AL into every touched file.
    // Without this, renaming to `my var`, `2Start`, `` or a reserved keyword
    // returns a WorkspaceEdit that writes syntax errors workspace-wide (C19).
    if !is_valid_rename_target(new_name) {
        return None;
    }

    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, &text, position.into())?;
    let clean_name = super::node_clean_name(node, text.as_bytes())?;

    let mut changes: Vec<(Url, Vec<TextEdit>)> = Vec::new();
    let source_bytes = text.as_bytes();

    // F-038: when the symbol at the cursor binds locally to a procedure
    // (parameter or `var`-declared local), restrict the rename to that
    // procedure's source range. Workspace-wide lexical rename of a local
    // would silently edit every other procedure / object that happens to
    // use the same name. Until proper symbol-aware rename exists this
    // scope-local fast path is the safe default for locals; non-local
    // identifiers (cross-file procedures, fields, types) still get the
    // workspace-wide pass below.
    if let Some(proc) = al_syntax::find_procedure_at(&tree, &text, position.into()) {
        let is_local_binding = proc
            .parameters
            .iter()
            .any(|p| p.name.eq_ignore_ascii_case(clean_name))
            || {
                let resolver = al_syntax::type_resolver::TypeResolver::new(&tree, &text);
                resolver
                    .resolve_type(clean_name, position.into())
                    .map(|d| {
                        matches!(
                            d.scope,
                            al_syntax::type_resolver::VariableScope::Local
                                | al_syntax::type_resolver::VariableScope::Parameter
                        )
                    })
                    .unwrap_or(false)
            };
        if is_local_binding {
            let proc_start = proc.range.start_byte;
            let proc_end = proc.range.end_byte;
            let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
            let edits: Vec<TextEdit> = refs
                .iter()
                .filter(|r| r.start_byte >= proc_start && r.end_byte <= proc_end)
                .filter_map(|r| {
                    let matched_text = text.get(r.start_byte..r.end_byte)?;
                    let replacement = make_rename_text(node.kind(), matched_text, new_name);
                    Some(TextEdit {
                        range: al_syntax::ts_range_to_syntax(r, source_bytes).into(),
                        new_text: replacement,
                    })
                })
                .collect();
            if edits.is_empty() {
                return None;
            }
            return Some(WorkspaceEdit {
                changes: vec![(uri.clone(), edits)],
            });
        }
    }

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let edits: Vec<TextEdit> = refs
            .iter()
            .filter_map(|r| {
                let matched_text = text.get(r.start_byte..r.end_byte)?;
                let replacement = make_rename_text(node.kind(), matched_text, new_name);
                Some(TextEdit {
                    range: al_syntax::ts_range_to_syntax(r, source_bytes).into(),
                    new_text: replacement,
                })
            })
            .collect();
        changes.push((uri.clone(), edits));
    }

    let current_path = uri.to_file_path().ok();
    for entry in workspace.file_index.files.iter() {
        let file_path = entry.key().clone();
        if current_path.as_ref() == Some(&file_path) {
            continue;
        }
        let file_uri = match Url::from_file_path(&file_path) {
            Ok(u) => u,
            Err(_) => continue,
        };
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
                        range: al_syntax::ts_range_to_syntax(r, file_source_bytes).into(),
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

/// Whether `new_name` can be spliced into AL source as a rename target without
/// producing invalid code. Accepts a plain identifier
/// (`[A-Za-z_][A-Za-z0-9_]*` that is not a reserved keyword) or an
/// already-quoted identifier (`"…"` with a non-empty, quote-free interior).
/// Rejects empty names, names containing spaces or other characters that would
/// require quoting, and bare keywords (C19).
fn is_valid_rename_target(new_name: &str) -> bool {
    let name = new_name.trim();
    if name.is_empty() {
        return false;
    }
    // Already-quoted identifier: quotable names (fields, objects) may be passed
    // pre-quoted. Require a non-empty interior with no embedded quote.
    if name.len() >= 2 && name.starts_with('"') && name.ends_with('"') {
        let inner = &name[1..name.len() - 1];
        return !inner.is_empty() && !inner.contains('"');
    }
    // Plain identifier grammar.
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    // A bare reserved keyword is not a legal unquoted identifier.
    !al_syntax::language_data::is_keyword(name)
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
    use al_workspace::Workspace;

    fn test_uri() -> Url {
        Url::parse("file:///test/src/Test.al").unwrap()
    }

    fn open_doc(ws: &Workspace, uri: &Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string());
    }

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
        };
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
        };
        let result = prepare_rename(&ws, &uri, pos);
        assert!(result.is_none(), "keywords should not be renameable");
    }

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

    /// F-038 positive: when two procedures each declare a local with the
    /// same name (`Status`), renaming the local in procedure A must NOT
    /// touch procedure B's same-named local.
    #[test]
    fn rename_local_var_does_not_touch_other_procedure_with_same_name() {
        let ws = Workspace::new();
        let uri = test_uri();
        let src = r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        Status: Integer;
    begin
        Status := 1;
    end;

    procedure Bar()
    var
        Status: Integer;
    begin
        Status := 2;
    end;
}
"#;
        open_doc(&ws, &uri, src);

        let pos = Position {
            line: 6,
            character: 8,
        };
        let result = rename(&ws, &uri, pos, "Phase").expect("rename should produce edits");
        let (edit_uri, edits) = &result.changes[0];
        assert_eq!(edit_uri, &uri);

        for e in edits {
            assert!(
                e.range.start.line <= 7,
                "F-038: leaked edit at line {} into Bar's procedure",
                e.range.start.line
            );
        }
        assert!(
            edits.len() >= 2,
            "expected at least 2 edits in Foo, got {}",
            edits.len()
        );
    }

    /// F-038 negative: a non-local identifier (the procedure name itself,
    /// which IS workspace-visible) should still be renamed across the
    /// workspace — only locals get the scope-restricted treatment.
    #[test]
    fn rename_procedure_name_still_workspace_wide() {
        let ws = Workspace::new();
        let uri = test_uri();
        let src = r#"codeunit 50100 "Test"
{
    procedure Foo()
    begin
    end;

    procedure Bar()
    begin
        Foo();
    end;
}
"#;
        open_doc(&ws, &uri, src);
        let pos = Position {
            line: 2,
            character: 14,
        };
        let result = rename(&ws, &uri, pos, "Baz").expect("rename should produce edits");
        let (_uri, edits) = &result.changes[0];
        let touched_lines: Vec<u32> = edits.iter().map(|e| e.range.start.line).collect();
        assert!(
            touched_lines.contains(&2) && touched_lines.contains(&8),
            "expected rename to span declaration (line 2) AND call (line 8); got {touched_lines:?}"
        );
    }

    #[test]
    fn rename_returns_none_when_no_refs() {
        let ws = Workspace::new();
        let uri = test_uri();
        open_doc(&ws, &uri, "codeunit 50100 \"X\" { }");
        let pos = Position {
            line: 0,
            character: 0,
        };
        let result = rename(&ws, &uri, pos, "Y");
        let _ = result;
    }

    #[test]
    fn c19_invalid_new_names_rejected() {
        assert!(!is_valid_rename_target(""));
        assert!(!is_valid_rename_target("   "));
        assert!(!is_valid_rename_target("my var with spaces"));
        assert!(!is_valid_rename_target("2Start"));
        assert!(!is_valid_rename_target("has-dash"));
        assert!(!is_valid_rename_target("bad!name"));
        assert!(!is_valid_rename_target("begin")); // reserved keyword
        assert!(!is_valid_rename_target("\"\"")); // empty quoted
        assert!(!is_valid_rename_target("\"bad\"quote\"")); // embedded quote
    }

    #[test]
    fn c19_valid_new_names_accepted() {
        assert!(is_valid_rename_target("NewVar"));
        assert!(is_valid_rename_target("_leading"));
        assert!(is_valid_rename_target("Var123"));
        assert!(is_valid_rename_target("\"My Field\"")); // pre-quoted quotable name
    }

    #[test]
    fn c19_rename_to_invalid_name_produces_no_edit() {
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
        // Space-containing name would splice broken code — must be rejected.
        assert!(
            rename(&ws, &uri, pos, "my var with spaces").is_none(),
            "rename to a space-containing name must not produce edits"
        );
        assert!(rename(&ws, &uri, pos, "").is_none());
        assert!(rename(&ws, &uri, pos, "begin").is_none());
        // A valid name still works (control).
        assert!(rename(&ws, &uri, pos, "NewVar").is_some());
    }

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
        assert_eq!(make_rename_text("", "\"Quoted\"", "Renamed"), "\"Renamed\"");
    }
}
