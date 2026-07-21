//! Make-procedure-local source action.

use url::Url;

use super::single_edit_ws;
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

/// F-043: cheap workspace-wide scan for a call-site identifier matching
/// `proc_name`, excluding the file currently under cursor. Returns true on
/// the first match. Conservative: any other file containing the literal
/// identifier (followed by `(` or whitespace, case-insensitive) counts as
/// an external caller, including same-name procedures in unrelated objects
/// — promoting to `local` is non-reversible by tooling, so false positives
/// are preferable to silently breaking call sites.
fn external_caller_exists(workspace: &Workspace, current_uri: &Url, proc_name: &str) -> bool {
    let current_path = current_uri.to_file_path().ok();
    let needle_lower = proc_name.to_lowercase();
    for entry in workspace.file_index.files.iter() {
        if current_path.as_ref().is_some_and(|p| entry.key() == p) {
            continue;
        }
        let text_lower = entry.value().to_lowercase();
        if !text_lower.contains(&needle_lower) {
            continue;
        }
        // Require the match to be a whole identifier (not a substring of a
        // longer one like `Foo` in `FooBar`). We deliberately do NOT also
        // require a following `(`: AL lets a parameterless procedure be called
        // bare (`Helper;`), so demanding parens missed those callers and let
        // "make local" silently break them. Over-detecting here only withholds
        // the refactor conservatively, which is the safe direction for a guard.
        let mut start = 0;
        while let Some(off) = text_lower[start..].find(&needle_lower) {
            let pos = start + off;
            let end = pos + needle_lower.len();
            // Identifier-char before pos? Then it's a substring of a longer ident.
            let prev_is_ident = pos > 0
                && text_lower
                    .as_bytes()
                    .get(pos - 1)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
            let identifier_continues = text_lower
                .as_bytes()
                .get(end)
                .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
            if !prev_is_ident && !identifier_continues {
                return true;
            }
            start = end;
        }
    }
    false
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

    let proc_line = node.start_position().row;
    let line_text = text.lines().nth(proc_line)?;
    let lower = line_text.to_lowercase();
    if lower.contains("local ") {
        return None;
    }

    // F-043: don't offer the refactor when external callers exist —
    // promoting to `local` would silently break those callers. Resolve the
    // procedure name from the parsed `name` field, then scan every other
    // file in the workspace for a call-site identifier matching that name.
    // The check is conservative (any same-name identifier in another file
    // suppresses the action) — a name collision across two unrelated
    // codeunits would also suppress, which is the safe direction.
    let proc_name = node
        .child_by_field_name("name")
        .and_then(|n| n.utf8_text(text.as_bytes()).ok())
        .map(|s| s.trim_matches('"').to_string())?;
    if external_caller_exists(workspace, uri, &proc_name) {
        return None;
    }

    // F-042: `find` returns a BYTE offset; LSP `Position.character` is a UTF-16 code
    // unit count. Convert before using, otherwise a multi-byte character earlier on
    // the line shifts the edit to the wrong column.
    let proc_col_bytes = lower.find("procedure")?;
    let proc_end_col_bytes = proc_col_bytes + "procedure".len();
    let proc_col_utf16 = al_syntax::byte_col_to_utf16_col(line_text, proc_col_bytes);
    let proc_end_col_utf16 = al_syntax::byte_col_to_utf16_col(line_text, proc_end_col_bytes);

    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: proc_line as u32,
                character: proc_col_utf16,
            },
            end: super::Position {
                line: proc_line as u32,
                character: proc_end_col_utf16,
            },
        },
        new_text: "local procedure".to_string(),
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
        ws.documents.open(uri.clone(), al_code.to_string());
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

    /// F-043 positive (no external callers): the procedure is only used in
    /// the same file, so the refactor is still offered.
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

    /// F-043 negative: an external caller in a SEPARATE workspace file
    /// must suppress the refactor — promoting to `local` would break the
    /// caller silently.
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
            "external caller in another file must suppress (F-043)"
        );
    }
}
