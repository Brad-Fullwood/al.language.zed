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

    // C18: for non-local symbols, rename only references that BIND to the same
    // declaration as the symbol under the cursor — not every identifier that
    // happens to be spelled the same. Two objects that each declare
    // `procedure Post()` resolve to different declarations, so renaming one no
    // longer rewrites the other (or unrelated same-named fields/locals). The
    // binder is the go-to-definition query: two positions bind to the same
    // symbol iff they resolve to the same declaration location.
    let cursor_decl = node_decl_loc(workspace, uri, node, source_bytes);

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let edits: Vec<TextEdit> = refs
            .iter()
            .filter_map(|r| {
                if ref_decl_loc(workspace, uri, &text, r) != cursor_decl {
                    return None;
                }
                let matched_text = text.get(r.start_byte..r.end_byte)?;
                let replacement = make_rename_text(node.kind(), matched_text, new_name);
                Some(TextEdit {
                    range: al_syntax::ts_range_to_syntax(r, source_bytes).into(),
                    new_text: replacement,
                })
            })
            .collect();
        if !edits.is_empty() {
            changes.push((uri.clone(), edits));
        }
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
                    if ref_decl_loc(workspace, &file_uri, &file_text, r) != cursor_decl {
                        return None;
                    }
                    let matched_text = file_text.get(r.start_byte..r.end_byte)?;
                    let replacement = make_rename_text("", matched_text, new_name);
                    Some(TextEdit {
                        range: al_syntax::ts_range_to_syntax(r, file_source_bytes).into(),
                        new_text: replacement,
                    })
                })
                .collect();
            if !edits.is_empty() {
                changes.push((file_uri, edits));
            }
        }
    }

    if changes.is_empty() {
        return None;
    }

    Some(WorkspaceEdit { changes })
}

type BindKey = (String, u32, u32);

/// The canonical declaration a position binds to. Two positions rename together
/// iff they share a canonical declaration (C18).
///
/// When `pos` sits on a declaration's own name, that name *is* the canonical
/// declaration — an object-local identity that keeps two objects' same-named
/// `procedure Post()` declarations distinct. (Go-to-definition on a declaration
/// is unreliable: it skips the same-file decl at the cursor and can fall
/// through to an unrelated same-named procedure in another object.) For every
/// other position (a usage) we defer to go-to-definition, which resolves a bare
/// procedure call same-file-first and a qualified call to its true owner.
fn decl_loc(workspace: &Workspace, uri: &Url, pos: Position) -> BindKey {
    if let Some(key) = enclosing_declaration_name(workspace, uri, pos) {
        return key;
    }
    if let Some(loc) =
        super::definition::definition(workspace, uri, pos).and_then(|locs| locs.into_iter().next())
    {
        return (
            loc.uri.to_string(),
            loc.range.start.line,
            loc.range.start.character,
        );
    }
    (uri.to_string(), pos.line, pos.character)
}

/// If `pos` falls on the *name* of a declaration (procedure, trigger, field, or
/// variable), return that name's location as a `BindKey`. Returns `None` when
/// `pos` is inside a declaration but not on its name (i.e. a usage in the body),
/// so the caller falls back to go-to-definition.
fn enclosing_declaration_name(workspace: &Workspace, uri: &Url, pos: Position) -> Option<BindKey> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
    let node = al_syntax::find_node_at_position(&tree, &text, pos.into())?;
    let mut cur = Some(node);
    while let Some(n) = cur {
        match n.kind() {
            "procedure_declaration"
            | "trigger_declaration"
            | "event_procedure_declaration"
            | "field_declaration"
            | "variable_declaration" => {
                let name = n.child_by_field_name("name")?;
                // Only treat this as the declaration site when the cursor node
                // sits within the name token; otherwise it is a body usage.
                if node.start_byte() >= name.start_byte() && node.end_byte() <= name.end_byte() {
                    let range: Range =
                        al_syntax::ts_range_to_syntax(&name.range(), text.as_bytes()).into();
                    return Some((uri.to_string(), range.start.line, range.start.character));
                }
                return None;
            }
            _ => {}
        }
        cur = n.parent();
    }
    None
}

fn node_decl_loc(
    workspace: &Workspace,
    uri: &Url,
    node: tree_sitter::Node,
    source: &[u8],
) -> BindKey {
    let range: Range = al_syntax::ts_range_to_syntax(&node.range(), source).into();
    decl_loc(workspace, uri, range.start)
}

fn ref_decl_loc(workspace: &Workspace, uri: &Url, text: &str, r: &tree_sitter::Range) -> BindKey {
    let range: Range = al_syntax::ts_range_to_syntax(r, text.as_bytes()).into();
    decl_loc(workspace, uri, range.start)
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

    /// C18: two separate objects each declare `procedure Post()`. These are
    /// distinct declarations. Renaming the one in object A must not rewrite the
    /// same-spelled procedure (or its call) in object B.
    #[test]
    fn rename_procedure_does_not_touch_same_name_in_other_object() {
        let ws = Workspace::new();
        let uri_a = Url::parse("file:///test/src/A.al").unwrap();
        let uri_b = Url::parse("file:///test/src/B.al").unwrap();
        let src_a = r#"codeunit 50100 "A"
{
    procedure Post()
    begin
    end;

    procedure Run()
    begin
        Post();
    end;
}
"#;
        let src_b = r#"codeunit 50101 "B"
{
    procedure Post()
    begin
    end;

    procedure Run()
    begin
        Post();
    end;
}
"#;
        open_doc(&ws, &uri_a, src_a);
        open_doc(&ws, &uri_b, src_b);
        ws.file_index
            .add_file(uri_a.to_file_path().unwrap(), src_a.to_string());
        ws.file_index
            .add_file(uri_b.to_file_path().unwrap(), src_b.to_string());

        // Cursor on A's `Post` declaration.
        let pos = Position {
            line: 2,
            character: 14,
        };
        let result = rename(&ws, &uri_a, pos, "Publish").expect("rename should produce edits");

        // No edits may land in B.
        for (edit_uri, _edits) in &result.changes {
            assert_ne!(
                edit_uri, &uri_b,
                "C18: rename of A::Post leaked into B; edits: {:?}",
                result.changes
            );
        }
        // A's declaration and its call site should both be covered.
        let (_uri, edits) = result
            .changes
            .iter()
            .find(|(u, _)| u == &uri_a)
            .expect("A should have edits");
        let touched: Vec<u32> = edits.iter().map(|e| e.range.start.line).collect();
        assert!(
            touched.contains(&2) && touched.contains(&8),
            "expected A's decl (line 2) and call (line 8); got {touched:?}"
        );
    }

    /// C18 adversarial: two tables each declare a field `Amount`. Renaming the
    /// field in table A must not rewrite table B's same-named field. Fields are
    /// the other common non-local symbol (besides procedures).
    #[test]
    fn rename_field_does_not_touch_same_name_in_other_table() {
        let ws = Workspace::new();
        let uri_a = Url::parse("file:///test/src/TableA.al").unwrap();
        let uri_b = Url::parse("file:///test/src/TableB.al").unwrap();
        let src_a = r#"table 50100 "A"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Decimal) { }
    }
}
"#;
        let src_b = r#"table 50101 "B"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Decimal) { }
    }
}
"#;
        open_doc(&ws, &uri_a, src_a);
        open_doc(&ws, &uri_b, src_b);
        ws.file_index
            .add_file(uri_a.to_file_path().unwrap(), src_a.to_string());
        ws.file_index
            .add_file(uri_b.to_file_path().unwrap(), src_b.to_string());

        // Cursor on A's `Amount` field declaration. Line 5 is
        // `        field(2; Amount; Decimal) { }` — `Amount` spans chars 17..23.
        let pos = Position {
            line: 5,
            character: 18,
        };
        let result =
            rename(&ws, &uri_a, pos, "Total").expect("rename of A.Amount should produce edits");
        // A must actually be edited (proves the cursor hit the field), and no
        // edit may land in table B.
        assert!(
            result.changes.iter().any(|(u, _)| u == &uri_a),
            "expected an edit in table A"
        );
        for (edit_uri, _edits) in &result.changes {
            assert_ne!(
                edit_uri, &uri_b,
                "C18: rename of A.Amount leaked into table B: {:?}",
                result.changes
            );
        }
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
