//! Rename symbol query.

use url::Url;

use super::{Position, Range, TextEdit, WorkspaceEdit};
use al_workspace::{Workspace, WorkspaceStateError};

pub fn prepare_rename(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
) -> Option<(Range, String)> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;

    let node = al_syntax::find_node_at_position(&tree, &text, position.into())?;
    let clean_name = super::node_clean_name(node, text.as_bytes())?;
    let clean_name = clean_name.as_str();
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

pub fn rename(
    workspace: &Workspace,
    uri: &Url,
    position: Position,
    new_name: &str,
) -> Result<Option<WorkspaceEdit>, WorkspaceStateError> {
    // Reject a new name that cannot be spelled in AL at all, and work out the
    // spelling for the rest. Without this, renaming to `` or to a name holding
    // a newline returns a WorkspaceEdit that writes syntax errors
    // workspace-wide.
    let Some(new_name) = parse_rename_name(new_name) else {
        return Ok(None);
    };
    let new_name = &new_name;

    let Some((text, tree)) = al_source::parsing::get_or_parse(&workspace.documents, uri) else {
        return Ok(None);
    };

    let Some(node) = al_syntax::find_node_at_position(&tree, &text, position.into()) else {
        return Ok(None);
    };
    let Some(clean_name) = super::node_clean_name(node, text.as_bytes()) else {
        return Ok(None);
    };
    let clean_name = clean_name.as_str();

    let mut changes: Vec<(Url, Vec<TextEdit>)> = Vec::new();
    let source_bytes = text.as_bytes();

    // Procedure-local bindings must not enter the workspace-wide rename path.
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
                    let replacement = new_name.spelled_over(matched_text);
                    Some(TextEdit {
                        range: al_syntax::ts_range_to_syntax(r, source_bytes).into(),
                        new_text: replacement,
                    })
                })
                .collect();
            if edits.is_empty() {
                return Ok(None);
            }
            return Ok(Some(WorkspaceEdit {
                changes: vec![(uri.clone(), edits)],
            }));
        }
    }

    // for non-local symbols, rename only references that BIND to the same
    // declaration as the symbol under the cursor — not every identifier that
    // happens to be spelled the same. Two objects that each declare
    // `procedure Post()` resolve to different declarations, so renaming one no
    // longer rewrites the other (or unrelated same-named fields/locals). The
    // binder is the go-to-definition query: two positions bind to the same
    // symbol iff they resolve to the same declaration location.
    let cursor_decl = node_decl_loc(workspace, uri, node, source_bytes)?;
    // Memoize binder lookups: without it every occurrence in every workspace
    // file runs a full go-to-definition query.
    let mut binder = binding::DeclLocCache::new();

    let refs = al_syntax::find_variable_references(&tree, &text, clean_name);
    if !refs.is_empty() {
        let mut edits = Vec::new();
        for r in &refs {
            if binder.decl_loc_for_reference(workspace, uri, &text, &tree, r)? != cursor_decl {
                continue;
            }
            if let Some(matched_text) = text.get(r.start_byte..r.end_byte) {
                let replacement = new_name.spelled_over(matched_text);
                edits.push(TextEdit {
                    range: al_syntax::ts_range_to_syntax(r, source_bytes).into(),
                    new_text: replacement,
                });
            }
        }
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
            let mut edits = Vec::new();
            for r in &refs {
                if binder.decl_loc_for_reference(workspace, &file_uri, &file_text, &file_tree, r)?
                    != cursor_decl
                {
                    continue;
                }
                if let Some(matched_text) = file_text.get(r.start_byte..r.end_byte) {
                    let replacement = new_name.spelled_over(matched_text);
                    edits.push(TextEdit {
                        range: al_syntax::ts_range_to_syntax(r, file_source_bytes).into(),
                        new_text: replacement,
                    });
                }
            }
            if !edits.is_empty() {
                changes.push((file_uri, edits));
            }
        }
    }

    if changes.is_empty() {
        return Ok(None);
    }

    Ok(Some(WorkspaceEdit { changes }))
}

use super::binding::{self, decl_loc, BindKey};

fn node_decl_loc(
    workspace: &Workspace,
    uri: &Url,
    node: tree_sitter::Node,
    source: &[u8],
) -> Result<BindKey, WorkspaceStateError> {
    let range: Range = al_syntax::ts_range_to_syntax(&node.range(), source).into();
    decl_loc(workspace, uri, range.start)
}

/// The identifier a rename request names, plus how AL has to spell it.
///
/// A client sends `newName` as free text and the two spellings of one AL name
/// both arrive: an editor that prefilled its box from [`prepare_rename`] sends
/// the placeholder back unquoted (`Posting Date`), while a client that echoes
/// the source sends `"Posting Date"`. Both name the same field, so the request
/// is reduced to the identifier and the spelling is derived from that
/// identifier rather than from whichever form arrived.
pub(crate) struct RenameName {
    /// The identifier, with the surrounding quotes and `""` escapes removed.
    clean: String,
    /// Whether AL needs double quotes to write `clean`.
    needs_quoting: bool,
}

impl RenameName {
    /// `clean` as a quoted AL identifier, re-doubling any embedded `"`.
    fn quoted(&self) -> String {
        format!("\"{}\"", self.clean.replace('"', "\"\""))
    }

    /// The text to splice over an occurrence currently spelled
    /// `original_text`. An occurrence already written with quotes keeps them,
    /// so a rename does not reflow the file's existing style.
    fn spelled_over(&self, original_text: &str) -> String {
        if self.needs_quoting || is_quoted(original_text) {
            self.quoted()
        } else {
            self.clean.clone()
        }
    }

    /// `clean` inside an AL string literal, for the `[EventSubscriber]`
    /// arguments that name an event or element by string.
    fn as_string_literal(&self) -> String {
        format!("'{}'", self.clean.replace('\'', "''"))
    }
}

/// Parse a client's `newName` into the identifier it names, or `None` when no
/// AL spelling of it exists.
///
/// Rejects the empty name, an interior whose `"` are not all doubled escapes,
/// and any control character: an interior holding a newline would split the
/// identifier across two lines in every file the rename touches, and the LSP
/// specification puts no constraint on `newName`, so the daemon path passes
/// whatever arrives on the wire.
pub(crate) fn parse_rename_name(new_name: &str) -> Option<RenameName> {
    let raw = new_name.trim();
    let clean = if is_quoted(raw) {
        let inner = &raw[1..raw.len() - 1];
        if !quotes_are_all_doubled(inner) {
            return None;
        }
        inner.replace("\"\"", "\"")
    } else {
        raw.to_string()
    };
    if clean.is_empty() || clean.trim() != clean {
        return None;
    }
    if clean.chars().any(char::is_control) {
        return None;
    }
    Some(RenameName {
        needs_quoting: needs_quoting(&clean),
        clean,
    })
}

fn is_quoted(text: &str) -> bool {
    text.len() >= 2 && text.starts_with('"') && text.ends_with('"')
}

/// Whether every `"` in a quoted identifier's interior is half of a `""` escape.
fn quotes_are_all_doubled(inner: &str) -> bool {
    let bytes = inner.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        if bytes.get(i + 1) != Some(&b'"') {
            return false;
        }
        i += 2;
    }
    true
}

/// Whether AL requires double quotes around `name`.
///
/// Compiler error AL0107 states the rule: a bare name holds letters, digits and
/// underscores, does not start with a digit, and is not a reserved keyword;
/// anything else goes in double quotes. Microsoft does not publish the reserved
/// set, so every word in the grammar's keyword table is quoted. Quoting a name
/// that would also have been legal bare still names the same identifier, while
/// leaving a reserved word bare is AL0107.
fn needs_quoting(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return true;
    };
    if !is_identifier_start(first) || !chars.all(is_identifier_continue) {
        return true;
    }
    al_syntax::language_data::is_keyword(name)
}

/// The grammar's `identifier` rule: `[A-Za-z_\u{80}-\u{FFFF}]`.
fn is_identifier_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || ('\u{80}'..='\u{FFFF}').contains(&c)
}

fn is_identifier_continue(c: char) -> bool {
    is_identifier_start(c) || c.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;

    fn rename(
        workspace: &Workspace,
        uri: &Url,
        position: Position,
        new_name: &str,
    ) -> Option<WorkspaceEdit> {
        super::rename(workspace, uri, position, new_name).unwrap()
    }

    fn test_uri() -> Url {
        Url::parse("file:///test/src/Test.al").unwrap()
    }

    fn open_doc(ws: &Workspace, uri: &Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
    }

    /// Index `al_code` as well as opening it, so the cross-file loop sees it.
    fn open_and_index(ws: &Workspace, uri: &Url, al_code: &str) {
        open_doc(ws, uri, al_code);
        ws.file_index
            .add_file(uri.to_file_path().unwrap(), al_code.to_string());
    }

    fn byte_offset(source: &str, pos: Position) -> usize {
        let mut offset = 0usize;
        for (index, line) in source.split('\n').enumerate() {
            if index == pos.line as usize {
                return offset + al_syntax::utf16_col_to_byte_offset(line, pos.character as usize);
            }
            offset += line.len() + 1;
        }
        source.len()
    }

    fn apply_edits(source: &str, edits: &[TextEdit]) -> String {
        let mut spans: Vec<(usize, usize, &str)> = edits
            .iter()
            .map(|e| {
                (
                    byte_offset(source, e.range.start),
                    byte_offset(source, e.range.end),
                    e.new_text.as_str(),
                )
            })
            .collect();
        spans.sort_by_key(|(start, _, _)| *start);
        let mut out = String::new();
        let mut cursor = 0usize;
        for (start, end, text) in spans {
            assert!(start >= cursor, "rename produced overlapping edits");
            out.push_str(&source[cursor..start]);
            out.push_str(text);
            cursor = end;
        }
        out.push_str(&source[cursor..]);
        out
    }

    fn assert_parses(label: &str, source: &str) {
        let parsed = al_syntax::AlParser::parse_quick(source);
        assert!(
            !parsed.tree.root_node().has_error(),
            "{label} no longer parses after the rename:\n{source}"
        );
    }

    /// Drive a rename the way a client does: take the placeholder
    /// `prepare_rename` offers, hand the user's edit of it to `rename`, apply
    /// the returned edits and re-parse each touched file.
    ///
    /// Everything a real client does to a quoted name goes through here: the
    /// editor pre-fills its box with the *unquoted* placeholder, so the string
    /// that comes back carries no quotes and the validator has to accept it.
    fn client_rename(
        ws: &Workspace,
        files: &[(Url, &str)],
        uri: &Url,
        pos: Position,
        edit_placeholder: impl FnOnce(&str) -> String,
    ) -> Vec<(Url, String)> {
        let (_range, placeholder) =
            prepare_rename(ws, uri, pos).expect("prepare_rename should offer a placeholder");
        let new_name = edit_placeholder(&placeholder);
        let Some(workspace_edit) = rename(ws, uri, pos, &new_name) else {
            panic!("rename to {new_name:?} produced no edits");
        };
        let mut renamed = Vec::new();
        for (file_uri, edits) in &workspace_edit.changes {
            let source = files
                .iter()
                .find(|(u, _)| u == file_uri)
                .unwrap_or_else(|| panic!("rename edited an unknown file {file_uri}"))
                .1;
            let after = apply_edits(source, edits);
            assert_parses(file_uri.as_str(), &after);
            renamed.push((file_uri.clone(), after));
        }
        renamed
    }

    fn text_for<'a>(renamed: &'a [(Url, String)], uri: &Url) -> &'a str {
        renamed
            .iter()
            .find(|(u, _)| u == uri)
            .unwrap_or_else(|| panic!("no edits landed in {uri}"))
            .1
            .as_str()
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
                "rename leaked into Bar at line {}",
                e.range.start.line
            );
        }
        assert!(
            edits.len() >= 2,
            "expected at least 2 edits in Foo, got {}",
            edits.len()
        );
    }

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

    /// two separate objects each declare `procedure Post()`. These are
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

    /// Same-named fields in different tables are distinct declarations.
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
    fn unspellable_new_names_rejected() {
        assert!(parse_rename_name("").is_none());
        assert!(parse_rename_name("   ").is_none());
        assert!(parse_rename_name("\"\"").is_none()); // empty quoted
        assert!(parse_rename_name("\"bad\"quote\"").is_none()); // undoubled quote
        assert!(parse_rename_name("\"My\nField\"").is_none()); // splits across lines
        assert!(parse_rename_name("My\tField").is_none());
        assert!(parse_rename_name("\" Padded \"").is_none());
    }

    #[test]
    fn a_name_that_needs_quoting_gets_quoted() {
        let quoted = |name: &str| parse_rename_name(name).expect("spellable").quoted();
        assert_eq!(
            parse_rename_name("my var with spaces")
                .unwrap()
                .spelled_over("MyVar"),
            "\"my var with spaces\""
        );
        assert_eq!(
            parse_rename_name("2Start").unwrap().spelled_over("MyVar"),
            "\"2Start\""
        );
        assert_eq!(
            parse_rename_name("has-dash").unwrap().spelled_over("MyVar"),
            "\"has-dash\""
        );
        // A reserved keyword is a legal name once quoted (AL0107).
        assert_eq!(
            parse_rename_name("begin").unwrap().spelled_over("MyVar"),
            "\"begin\""
        );
        // An embedded quote is re-doubled on the way out.
        assert_eq!(quoted("Cust \"Main\" Rec"), "\"Cust \"\"Main\"\" Rec\"");
        assert_eq!(
            parse_rename_name("\"Cust \"\"Main\"\" Rec\"")
                .unwrap()
                .clean,
            "Cust \"Main\" Rec"
        );
    }

    #[test]
    fn a_plain_name_stays_unquoted_unless_the_occurrence_is_quoted() {
        let plain = parse_rename_name("NewVar").unwrap();
        assert!(!plain.needs_quoting);
        assert_eq!(plain.spelled_over("MyVar"), "NewVar");
        assert_eq!(plain.spelled_over("\"My Var\""), "\"NewVar\"");
        assert!(!parse_rename_name("_leading").unwrap().needs_quoting);
        assert!(!parse_rename_name("Var123").unwrap().needs_quoting);
        // Unicode identifiers are plain per the grammar's `identifier` rule.
        assert!(!parse_rename_name("Ørnamental").unwrap().needs_quoting);
        // A pre-quoted plain name is still the same identifier.
        let prequoted = parse_rename_name("\"NewVar\"").unwrap();
        assert_eq!(prequoted.clean, "NewVar");
        assert_eq!(prequoted.spelled_over("MyVar"), "NewVar");
    }

    #[test]
    fn rename_to_an_unspellable_name_produces_no_edit() {
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
        assert!(rename(&ws, &uri, pos, "").is_none());
        assert!(
            rename(&ws, &uri, pos, "\"My\nField\"").is_none(),
            "a newline in the new name must not be spliced into the file"
        );
        assert!(rename(&ws, &uri, pos, "NewVar").is_some());
    }

    /// The round trip an editor makes on a quoted field: `prepare_rename`
    /// hands back the unquoted placeholder, the user edits it, and the edited
    /// text comes back with no quotes. The rename has to re-quote it.
    #[test]
    fn rename_quoted_field_through_the_client_round_trip() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/src/Shipment.al").unwrap();
        let src = r#"table 50100 "Shipment"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Posting Date"; Date) { }
    }

    procedure Stamp()
    begin
        Rec."Posting Date" := Today();
    end;
}
"#;
        open_and_index(&ws, &uri, src);

        let pos = Position {
            line: 5,
            character: 18,
        };
        let (_range, placeholder) = prepare_rename(&ws, &uri, pos).expect("placeholder");
        assert_eq!(placeholder, "Posting Date");

        let renamed = client_rename(&ws, &[(uri.clone(), src)], &uri, pos, |placeholder| {
            placeholder.replace("Posting", "Posted")
        });
        assert_eq!(
            text_for(&renamed, &uri),
            r#"table 50100 "Shipment"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; "Posted Date"; Date) { }
    }

    procedure Stamp()
    begin
        Rec."Posted Date" := Today();
    end;
}
"#
        );
    }

    /// `"No."` cannot be written without quotes at all: the `.` is not an
    /// identifier character. Renaming it to another dotted name has to keep the
    /// quotes on both the declaration and the use.
    #[test]
    fn rename_dotted_quoted_field_keeps_its_quotes() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/src/Dotted.al").unwrap();
        let src = r#"table 50100 "Shipment"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }

    procedure Stamp()
    begin
        Rec."No." := 'X';
    end;
}
"#;
        open_and_index(&ws, &uri, src);
        let pos = Position {
            line: 4,
            character: 18,
        };
        let renamed = client_rename(&ws, &[(uri.clone(), src)], &uri, pos, |placeholder| {
            assert_eq!(placeholder, "No.");
            "Doc. No.".to_string()
        });
        let after = text_for(&renamed, &uri);
        assert!(
            after.contains(r#"field(1; "Doc. No."; Code[20])"#),
            "declaration not re-quoted: {after}"
        );
        assert!(
            after.contains(r#"        Rec."Doc. No." := 'X';"#),
            "use not re-quoted: {after}"
        );
    }

    /// A name whose embedded `"` is escaped by doubling survives the round
    /// trip: `prepare_rename` hands back the unescaped identifier and the
    /// rename re-escapes it.
    #[test]
    fn rename_field_whose_name_contains_doubled_quotes() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/src/Doubled.al").unwrap();
        let src = r#"table 50100 "Shipment"
{
    fields
    {
        field(1; "Cust ""Main"" Rec"; Code[20]) { }
    }

    procedure Stamp()
    begin
        Rec."Cust ""Main"" Rec" := 'X';
    end;
}
"#;
        open_and_index(&ws, &uri, src);
        let pos = Position {
            line: 4,
            character: 20,
        };
        let renamed = client_rename(&ws, &[(uri.clone(), src)], &uri, pos, |placeholder| {
            assert_eq!(placeholder, r#"Cust "Main" Rec"#);
            r#"Cust "Head" Rec"#.to_string()
        });
        let after = text_for(&renamed, &uri);
        assert!(
            after.contains(r#"field(1; "Cust ""Head"" Rec"; Code[20])"#),
            "declaration not re-escaped: {after}"
        );
        assert!(
            after.contains(r#"        Rec."Cust ""Head"" Rec" := 'X';"#),
            "use not re-escaped: {after}"
        );
    }

    /// A rename to a name AL cannot write bare has to quote it, rather than
    /// returning no edits and leaving the editor silent.
    #[test]
    fn rename_plain_variable_to_a_quotable_name() {
        let ws = Workspace::new();
        let uri = test_uri();
        let src = r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        MyVar: Integer;
    begin
        MyVar := 42;
    end;
}
"#;
        open_and_index(&ws, &uri, src);
        let pos = Position {
            line: 6,
            character: 8,
        };
        let renamed = client_rename(&ws, &[(uri.clone(), src)], &uri, pos, |_| {
            "Total (LCY)".to_string()
        });
        assert_eq!(
            text_for(&renamed, &uri),
            r#"codeunit 50100 "Test"
{
    procedure Foo()
    var
        "Total (LCY)": Integer;
    begin
        "Total (LCY)" := 42;
    end;
}
"#
        );
    }

    /// The cross-file loop has to produce edits, not just withhold them: a
    /// codeunit referencing a table's field is renamed along with the field.
    #[test]
    fn rename_field_edits_the_referencing_codeunit() {
        let ws = Workspace::new();
        let table_uri = Url::parse("file:///test/src/Tab50100.al").unwrap();
        let codeunit_uri = Url::parse("file:///test/src/Cod50100.al").unwrap();
        let table_src = r#"table 50100 "Shipment"
{
    fields
    {
        field(1; "No."; Code[20]) { }
        field(2; Amount; Decimal) { }
    }
}
"#;
        let codeunit_src = r#"codeunit 50100 "Shipment Mgt."
{
    procedure Total(var Shipment: Record "Shipment"): Decimal
    begin
        exit(Shipment.Amount);
    end;
}
"#;
        open_and_index(&ws, &table_uri, table_src);
        open_and_index(&ws, &codeunit_uri, codeunit_src);

        let pos = Position {
            line: 5,
            character: 18,
        };
        let renamed = client_rename(
            &ws,
            &[
                (table_uri.clone(), table_src),
                (codeunit_uri.clone(), codeunit_src),
            ],
            &table_uri,
            pos,
            |placeholder| {
                assert_eq!(placeholder, "Amount");
                "Total Amount".to_string()
            },
        );
        assert!(
            text_for(&renamed, &table_uri).contains(r#"field(2; "Total Amount"; Decimal)"#),
            "table not renamed: {}",
            text_for(&renamed, &table_uri)
        );
        let codeunit_after = text_for(&renamed, &codeunit_uri);
        assert!(
            codeunit_after.contains(r#"exit(Shipment."Total Amount");"#),
            "cross-file reference not renamed: {codeunit_after}"
        );
    }
}
