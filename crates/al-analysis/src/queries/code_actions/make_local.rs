//! Make-procedure-local source action.

use url::Url;

use super::single_edit_ws;
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

/// Scan other workspace files for a call-site identifier matching `proc_name`.
/// Returns on the first match. Any other file containing the literal
/// identifier (followed by `(` or whitespace, case-insensitive) counts as
/// an external caller, including same-name procedures in unrelated objects
/// — promoting to `local` is non-reversible by tooling, so false positives
/// are preferable to silently breaking call sites.
fn external_caller_exists(workspace: &Workspace, current_uri: &Url, proc_name: &str) -> bool {
    let current_path = current_uri.to_file_path().ok();
    let needle = proc_name.as_bytes();
    for entry in workspace.file_index.files.iter() {
        if current_path.as_ref().is_some_and(|p| entry.key() == p) {
            continue;
        }
        if contains_identifier_ignore_ascii_case(entry.value().as_bytes(), needle) {
            return true;
        }
    }
    false
}

/// Whether `haystack` contains `needle` as a whole identifier, comparing
/// ASCII case-insensitively.
///
/// Called for every indexed file on every `textDocument/codeAction` request,
/// which editors fire as the cursor moves. Lowercasing each file first — as
/// this used to — allocated a fresh copy of the whole workspace source per
/// cursor move. This reads the indexed bytes in place.
///
/// A match must not be part of a longer identifier (`Foo` inside `FooBar`).
/// It deliberately does *not* also require a following `(`: AL lets a
/// parameterless procedure be called bare (`Helper;`), and demanding parens
/// missed those callers, letting "make local" break them. Over-detecting only
/// withholds the refactor, which is the safe direction for a guard.
fn contains_identifier_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let Some(&first) = needle.first() else {
        return false;
    };
    let first = first.to_ascii_lowercase();
    let Some(last_start) = haystack.len().checked_sub(needle.len()) else {
        return false;
    };
    for start in 0..=last_start {
        if haystack[start].to_ascii_lowercase() != first {
            continue;
        }
        if start > 0 && is_ident(haystack[start - 1]) {
            continue;
        }
        let end = start + needle.len();
        if haystack.get(end).copied().is_some_and(is_ident) {
            continue;
        }
        if haystack[start..end]
            .iter()
            .zip(needle)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            return true;
        }
    }
    false
}

/// The declaration's member modifiers and its `procedure`/`function` keyword.
///
/// The grammar puts `repeat($.attribute)` inside `procedure_declaration`, so
/// the node starts at the first attribute, which may sit lines above the
/// signature and may itself contain the word "procedure" — an event name such
/// as `'OnAfterPostProcedure'`. Reading the keyword node instead of searching
/// the declaration's first line for a substring is the only way to get the
/// span that actually holds the keyword.
///
/// `None` for an `event procedure` declaration: `local` cannot be spliced in
/// front of `event` and this action has no rule for where it would go.
fn signature_keywords(
    declaration: tree_sitter::Node<'_>,
) -> Option<(
    Vec<(&'static str, tree_sitter::Node<'_>)>,
    tree_sitter::Node<'_>,
)> {
    let mut inner = declaration;
    let mut cursor = declaration.walk();
    if let Some(event) = declaration
        .children(&mut cursor)
        .find(|child| child.kind() == "event_procedure_declaration")
    {
        inner = event;
    }

    let mut modifiers = Vec::new();
    let mut cursor = inner.walk();
    for child in inner.children(&mut cursor) {
        let kind = match child.kind() {
            "member_modifier" => child.child(0).map_or("member_modifier", |k| k.kind()),
            other => other,
        };
        match kind {
            "kw_local"
            | "kw_internal"
            | "kw_protected"
            | "kw_withevents"
            | "kw_runonclient"
            | "kw_securityfiltering"
            | "kw_suppressdispose" => modifiers.push((kind, child)),
            "kw_event" => return None,
            "kw_procedure" | "kw_function" => return Some((modifiers, child)),
            _ => {}
        }
    }
    None
}

/// Make method local — offer to add `local` keyword when procedure has no external callers.
pub(super) fn source_action_make_local(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();

    // LSP positions use UTF-16 code units; tree-sitter uses byte offsets.
    let cursor_line = text.lines().nth(range.start.line as usize).unwrap_or("");
    let col_bytes =
        crate::resolution::utf16_col_to_byte_offset(cursor_line, range.start.character as usize);
    let point = tree_sitter::Point::new(range.start.line as usize, col_bytes);
    let mut node = root.descendant_for_point_range(point, point)?;

    loop {
        if node.kind() == "procedure_declaration" {
            break;
        }
        if node.kind() == "trigger_declaration" {
            return None;
        }
        node = node.parent()?;
    }

    let (modifiers, keyword) = signature_keywords(node)?;
    if modifiers.iter().any(|(kind, _)| *kind == "kw_local") {
        return None;
    }
    // `internal`/`protected` are access modifiers that cannot coexist with
    // `local`; replace them rather than prepending.
    let replace_from = modifiers
        .iter()
        .find(|(kind, _)| matches!(*kind, "kw_internal" | "kw_protected"))
        .map_or(keyword, |(_, node)| *node);

    // Conservatively suppress the action on any same-name call in another file.
    let proc_name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(text.as_bytes()).ok())
        .map(|s| s.trim_matches('"').to_string())?;
    if external_caller_exists(workspace, uri, &proc_name) {
        return None;
    }

    // Tree-sitter columns are byte offsets; LSP `Position.character` is a
    // UTF-16 code unit count. Convert before using, otherwise a multi-byte
    // character earlier on the line shifts the edit to the wrong column.
    let start_point = replace_from.start_position();
    let end_point = keyword.end_position();
    let start_line_text = text.lines().nth(start_point.row)?;
    let end_line_text = text.lines().nth(end_point.row)?;
    // `function` is a legacy spelling the grammar still accepts; echo whatever
    // the source used instead of rewriting it to `procedure`.
    let keyword_text = keyword.utf8_text(text.as_bytes()).ok()?;

    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: start_point.row as u32,
                character: al_syntax::byte_col_to_utf16_col(start_line_text, start_point.column),
            },
            end: super::Position {
                line: end_point.row as u32,
                character: al_syntax::byte_col_to_utf16_col(end_line_text, end_point.column),
            },
        },
        new_text: format!("local {keyword_text}"),
    };

    Some(CodeActionEntry {
        title: "Make procedure local".to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, vec![edit])),
        is_preferred: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::code_actions::source_actions;
    use al_workspace::Workspace;
    use url::Url;

    fn open_doc(ws: &al_workspace::Workspace, uri: &url::Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
    }

    #[test]
    fn make_local_offered_for_procedure() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure InternalHelper()
    begin
        Message('helper');
    end;
}
"#;
        let uri = Url::parse("file:///test/MakeLocal.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 2,
                character: 4,
            },
            end: super::super::Position {
                line: 2,
                character: 4,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let local_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("local"))
            .collect();

        assert!(
            !local_actions.is_empty(),
            "Should offer 'Make local' action"
        );
        let edit = local_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        assert!(edits[0].new_text.contains("local"), "Should insert 'local'");
    }

    #[test]
    fn make_local_not_offered_when_already_local() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    local procedure InternalHelper()
    begin
        Message('helper');
    end;
}
"#;
        let uri = Url::parse("file:///test/MakeLocal.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 2,
                character: 10,
            },
            end: super::super::Position {
                line: 2,
                character: 10,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let local_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("Make procedure local"))
            .collect();

        assert!(
            local_actions.is_empty(),
            "Should NOT offer when already local"
        );
    }

    #[test]
    fn make_local_offered_when_no_external_callers() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure UniquelyNamedHelperABC()
    begin
        Message('helper');
    end;

    procedure Caller()
    begin
        UniquelyNamedHelperABC();
    end;
}
"#;
        let uri = Url::parse("file:///test/MakeLocalNoExt.al").unwrap();
        open_doc(&ws, &uri, al_code);
        // Add this same file to the file_index so external_caller_exists
        // can see it and (correctly) skip it as the current file.
        ws.file_index
            .add_file(uri.to_file_path().unwrap(), al_code.to_string());

        let range = Range {
            start: super::super::Position {
                line: 2,
                character: 4,
            },
            end: super::super::Position {
                line: 2,
                character: 4,
            },
        };
        let actions = source_actions(&ws, &uri, range);
        assert!(
            actions.iter().any(|a| a.title == "Make procedure local"),
            "should be offered when no external callers"
        );
    }

    #[test]
    fn make_local_not_offered_when_external_caller_exists() {
        let ws = Workspace::new();
        let helper_code = r#"codeunit 50100 "My Codeunit"
{
    procedure UniquelyNamedHelperXYZ()
    begin
        Message('helper');
    end;
}
"#;
        let caller_code = r#"codeunit 50101 "Other Codeunit"
{
    procedure DoStuff()
    begin
        UniquelyNamedHelperXYZ();
    end;
}
"#;
        let helper_uri = Url::parse("file:///test/Helper.al").unwrap();
        let caller_uri = Url::parse("file:///test/Caller.al").unwrap();
        open_doc(&ws, &helper_uri, helper_code);
        ws.file_index
            .add_file(helper_uri.to_file_path().unwrap(), helper_code.to_string());
        ws.file_index
            .add_file(caller_uri.to_file_path().unwrap(), caller_code.to_string());

        let range = Range {
            start: super::super::Position {
                line: 2,
                character: 4,
            },
            end: super::super::Position {
                line: 2,
                character: 4,
            },
        };
        let actions = source_actions(&ws, &helper_uri, range);
        assert!(
            !actions.iter().any(|a| a.title == "Make procedure local"),
            "external caller in another file must suppress the action"
        );
    }

    /// The scan replaced a lowercase-the-whole-file pass; it has to answer the
    /// same question, including the identifier boundaries and non-ASCII bytes
    /// it must leave alone.
    #[test]
    fn identifier_scan_matches_whole_identifiers_case_insensitively() {
        let hit = |haystack: &str, needle: &str| {
            contains_identifier_ignore_ascii_case(haystack.as_bytes(), needle.as_bytes())
        };

        assert!(hit("    Helper();", "Helper"));
        assert!(hit("    helper;", "Helper"), "bare parameterless call");
        assert!(hit("HELPER", "Helper"), "match at the very end of the text");
        assert!(hit("x := Rec.Helper();", "helper"));

        assert!(!hit("    HelperBar();", "Helper"), "longer identifier");
        assert!(!hit("    MyHelper();", "Helper"), "longer identifier");
        assert!(!hit("    My_Helper();", "Helper"), "underscore continues");
        assert!(!hit("    Help();", "Helper"), "needle longer than the text");
        assert!(!hit("", "Helper"));
        assert!(!hit("Helper", ""), "an empty needle matches nothing");
        // A multi-byte character next to the match is not an ASCII identifier
        // byte, so the occurrence still counts.
        assert!(hit("Ü Helper Ü", "Helper"));
    }

    fn make_local_action(al_code: &str, uri_str: &str, line: u32) -> Option<CodeActionEntry> {
        let ws = Workspace::new();
        let uri = Url::parse(uri_str).unwrap();
        open_doc(&ws, &uri, al_code);
        ws.file_index
            .add_file(uri.to_file_path().unwrap(), al_code.to_string());
        let range = Range {
            start: super::super::Position { line, character: 4 },
            end: super::super::Position { line, character: 4 },
        };
        source_action_make_local(&ws, &uri, al_code, range)
    }

    /// `internal local procedure` is not a valid modifier combination.
    #[test]
    fn make_local_replaces_internal_instead_of_producing_internal_local() {
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    internal procedure UniquelyNamedInternalHelperQ()
    begin
        Message('helper');
    end;
}
"#;
        let action =
            make_local_action(al_code, "file:///test/MakeLocalInternal.al", 2).expect("offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "make_local internal",
        );
        assert!(
            !updated.contains("internal local"),
            "must not emit an invalid modifier combination: {updated}"
        );
        assert!(
            updated.contains("    local procedure UniquelyNamedInternalHelperQ()"),
            "{updated}"
        );
    }

    /// A trailing comment mentioning "local " used to falsely suppress the action.
    #[test]
    fn make_local_offered_when_a_comment_mentions_local() {
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure UniquelyNamedCommentHelperQ() // local helper, not yet local
    begin
        Message('helper');
    end;
}
"#;
        let action = make_local_action(al_code, "file:///test/MakeLocalComment.al", 2)
            .expect("a comment must not suppress the action");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "make_local comment",
        );
        assert!(
            updated.contains("local procedure UniquelyNamedCommentHelperQ()"),
            "{updated}"
        );
    }

    /// The grammar puts `repeat($.attribute)` inside `procedure_declaration`,
    /// so the declaration's start row is the attribute line. Searching that
    /// line for "procedure" hits the word inside the event name.
    #[test]
    fn make_local_edits_the_procedure_line_not_an_attribute_that_contains_the_word() {
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Sales-Post", 'OnAfterPostProcedure', '', false, false)]
    procedure UniquelyNamedAttributedHelperQ()
    begin
        Message('helper');
    end;
}
"#;
        let action = make_local_action(al_code, "file:///test/MakeLocalAttr.al", 3)
            .expect("an attributed procedure must still be offered the action");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "make_local attributed",
        );
        assert!(
            updated.contains("'OnAfterPostProcedure'"),
            "the attribute must be left alone: {updated}"
        );
        assert!(
            updated.contains("    local procedure UniquelyNamedAttributedHelperQ()"),
            "{updated}"
        );
    }

    /// An attribute without the word "procedure" made the substring search
    /// return `None`, so the action was never offered.
    #[test]
    fn make_local_offered_on_a_procedure_whose_attribute_lacks_the_keyword() {
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    [NonDebuggable]
    internal procedure UniquelyNamedNonDebuggableHelperQ()
    begin
        Message('helper');
    end;
}
"#;
        let action =
            make_local_action(al_code, "file:///test/MakeLocalNonDebug.al", 3).expect("offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "make_local nondebuggable",
        );
        assert!(updated.contains("    [NonDebuggable]"), "{updated}");
        assert!(
            updated.contains("    local procedure UniquelyNamedNonDebuggableHelperQ()"),
            "{updated}"
        );
    }

    /// A `local` modifier on an attributed procedure sits on a different line
    /// from the declaration's start row.
    #[test]
    fn make_local_not_offered_on_an_attributed_procedure_that_is_already_local() {
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    [Obsolete('Use the other procedure instead', '25.0')]
    local procedure UniquelyNamedObsoleteHelperQ()
    begin
        Message('helper');
    end;
}
"#;
        assert!(
            make_local_action(al_code, "file:///test/MakeLocalObsolete.al", 3).is_none(),
            "already local"
        );
    }

    #[test]
    fn make_local_edit_reparses_cleanly() {
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure UniquelyNamedPlainHelperQ()
    begin
        Message('helper');
    end;
}
"#;
        let action =
            make_local_action(al_code, "file:///test/MakeLocalApply.al", 2).expect("offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "make_local",
        );
        assert!(updated.contains("local procedure"), "{updated}");
    }
}
