//! Add-parentheses source action.

use url::Url;

use super::single_edit_ws;
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

pub(super) fn source_action_add_parens(
    _workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let line_idx = range.start.line as usize;
    let line = text.lines().nth(line_idx)?;
    let trimmed = line.trim();

    // Must end with semicolon and look like a bare identifier call: word chars only, then ';'
    // e.g. "Commit;" or "MyHelper;" — not "Commit();" or "x := Commit;"
    let is_bare_call = {
        let s = trimmed;
        if let Some(body) = s.strip_suffix(';') {
            let body = body.trim_end();
            !body.is_empty()
                && !body.contains('(')
                && !body.contains(":=")
                && body
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '"')
                && is_callable_identifier_path(body)
        } else {
            false
        }
    };

    if !is_bare_call {
        return None;
    }

    // rfind returns a byte index; convert to UTF-16 code unit offset for LSP.
    let semicolon_byte = line.rfind(';')?;
    let semicolon_col = line[..semicolon_byte].encode_utf16().count() as u32;

    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: line_idx as u32,
                character: semicolon_col,
            },
            end: super::Position {
                line: line_idx as u32,
                character: semicolon_col,
            },
        },
        new_text: "()".to_string(),
    };

    Some(CodeActionEntry {
        title: "Add parentheses to method call".to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, vec![edit])),
        is_preferred: false,
    })
}

/// Statement keywords that terminate with a bare `;` and are *not* calls.
///
/// `is_keyword` (grammar-derived) already covers `end`, `begin`, `break`,
/// `exit`, `var`, … but the AL grammar models a few statement words as
/// built-ins or contextual tokens rather than keywords. Appending `()` to any
/// of them produces code the compiler rejects, so they are listed explicitly.
const NON_CALL_STATEMENT_WORDS: &[&str] = &[
    "skip",
    "quit",
    "trap",
    "rec",
    "xrec",
    "currpage",
    "currreport",
    "currxmlport",
];

/// Whether `body` (the text between the statement start and its `;`) is an
/// identifier path that could name a parameterless method.
///
/// The previous heuristic accepted any word-only line, so placing the cursor on
/// `end;` or `break;` offered an "add parentheses" action whose edit produced
/// `end();` / `break();` and corrupted the file. Every dot-separated segment
/// must therefore be a plain identifier — not an AL keyword and not one of the
/// bare statement words above.
fn is_callable_identifier_path(body: &str) -> bool {
    let mut segments = 0usize;
    for segment in body.split('.') {
        let segment = segment.trim().trim_matches('"');
        if segment.is_empty() {
            // A trailing/leading dot or an empty quoted segment is not an
            // identifier path (e.g. the AL field name `"No."` splits this way);
            // refuse rather than guess.
            return false;
        }
        let lower = segment.to_ascii_lowercase();
        if al_syntax::language_data::is_keyword(&lower)
            || NON_CALL_STATEMENT_WORDS.contains(&lower.as_str())
        {
            return false;
        }
        segments += 1;
    }
    segments > 0
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
    fn add_parens_offered_for_call_without_parens() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure Caller()
    begin
        Commit;
    end;
}
"#;
        let uri = Url::parse("file:///test/AddParens.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 4,
                character: 8,
            },
            end: super::super::Position {
                line: 4,
                character: 8,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let paren_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("parentheses") || a.title.contains("()"))
            .collect();

        assert!(
            !paren_actions.is_empty(),
            "Should offer 'Add parentheses' action"
        );
    }

    #[test]
    fn add_parens_not_offered_when_already_has_parens() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure Caller()
    begin
        Commit();
    end;
}
"#;
        let uri = Url::parse("file:///test/AddParens2.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 4,
                character: 8,
            },
            end: super::super::Position {
                line: 4,
                character: 8,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let paren_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("parentheses") || a.title.contains("()"))
            .collect();

        assert!(
            paren_actions.is_empty(),
            "Should NOT offer when already has ()"
        );
    }

    /// The bare-call heuristic used to accept any word-only line ending in
    /// `;`, so a cursor on `end;` / `break;` / `exit;` offered an action whose
    /// edit produced `end();` and corrupted the file.
    #[test]
    fn add_parens_not_offered_for_keyword_statements() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure Caller()
    var
        i: Integer;
    begin
        for i := 1 to 10 do begin
            if i = 3 then
                break;
            if i = 4 then
                exit;
        end;
    end;
}
"#;
        let uri = Url::parse("file:///test/AddParensKeywords.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // `break;` (line 8), `exit;` (line 10), `end;` (lines 11/12/13)
        for line in [8u32, 10, 11, 12, 13] {
            let range = Range {
                start: super::super::Position {
                    line,
                    character: 12,
                },
                end: super::super::Position {
                    line,
                    character: 12,
                },
            };
            let actions = source_actions(&ws, &uri, range);
            let paren_actions: Vec<_> = actions
                .iter()
                .filter(|a| a.title.contains("parentheses"))
                .collect();
            assert!(
                paren_actions.is_empty(),
                "keyword statement on line {line} ({:?}) must not be offered parentheses",
                al_code.lines().nth(line as usize)
            );
        }
    }

    #[test]
    fn add_parens_edit_reparses_cleanly() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure Caller()
    begin
        Commit;
    end;
}
"#;
        let uri = Url::parse("file:///test/AddParensApply.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 4,
                character: 8,
            },
            end: super::super::Position {
                line: 4,
                character: 8,
            },
        };
        let actions = source_actions(&ws, &uri, range);
        let action = actions
            .iter()
            .find(|a| a.title.contains("parentheses"))
            .expect("add-parens action");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            action,
            "add_parens",
        );
        assert!(updated.contains("Commit();"), "got: {updated}");
    }
}
