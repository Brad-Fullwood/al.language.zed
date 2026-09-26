//! Add-parentheses source action.

use al_syntax::IdentifierText;
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

/// Statement words that terminate with a bare `;` and can never appear in a
/// call at all.
///
/// `is_keyword` (grammar-derived) already covers `end`, `begin`, `break`,
/// `exit`, `var`, … but the AL grammar models these as built-ins or contextual
/// tokens rather than keywords. Appending `()` to any of them produces code
/// the compiler rejects.
const BARE_STATEMENT_WORDS: &[&str] = &["skip", "quit", "trap"];

/// Words that are not a call on their own but are the standard receivers of
/// one: `CurrPage.Update;` and `Rec.Modify;` are exactly the bare calls
/// AL0604 asks you to parenthesise.
const RECEIVER_WORDS: &[&str] = &["rec", "xrec", "currpage", "currreport", "currxmlport"];

/// Whether `body` (the text between the statement start and its `;`) is an
/// identifier path that could name a parameterless method.
///
/// A word-only line is not enough: a cursor on `end;` or `break;` used to be
/// offered an action whose edit produced `end();` and corrupted the file.
///
/// The segments are not all alike. The last one is the member being called and
/// lives in its own namespace — `Rec.Modify` and `Rec.Count` are method calls
/// even though `modify` and `count` are grammar keywords — so only the
/// receiver chain is keyword-checked. A lone segment has no receiver, so it
/// must be neither a keyword nor a receiver word.
fn is_callable_identifier_path(body: &str) -> bool {
    // A trailing/leading dot or an empty quoted segment is not an identifier
    // path (the AL field name `"No."` splits this way); refuse rather than
    // guess.
    let segments: Vec<String> = body
        .split('.')
        .map(|segment| segment.unquote_identifier().to_ascii_lowercase())
        .collect();
    if segments.iter().any(String::is_empty) {
        return false;
    }
    let Some((member, receivers)) = segments.split_last() else {
        return false;
    };
    if segments
        .iter()
        .any(|segment| BARE_STATEMENT_WORDS.contains(&segment.as_str()))
    {
        return false;
    }
    if receivers.is_empty() {
        return !al_syntax::language_data::is_keyword(member)
            && !RECEIVER_WORDS.contains(&member.as_str());
    }
    receivers.iter().all(|segment| {
        RECEIVER_WORDS.contains(&segment.as_str()) || !al_syntax::language_data::is_keyword(segment)
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

    /// `Rec`/`CurrPage` are not calls on their own, but they are the receivers
    /// of the most common AL0604 targets. Rejecting a whole path because one
    /// segment is on that list suppressed the action exactly where it is
    /// wanted.
    #[test]
    fn add_parens_offered_for_member_calls_on_rec_and_currpage() {
        for (call, member) in [
            ("CurrPage.Update", "CurrPage.Update()"),
            ("Rec.Modify", "Rec.Modify()"),
            ("Rec.Insert", "Rec.Insert()"),
            ("xRec.Delete", "xRec.Delete()"),
            ("Rec.Count", "Rec.Count()"),
        ] {
            let ws = Workspace::new();
            let al_code = format!(
                "page 50100 \"My Page\"\n{{\n    procedure Caller()\n    begin\n        {call};\n    end;\n}}\n"
            );
            let uri = Url::parse(&format!(
                "file:///test/AddParens{}.al",
                call.replace('.', "")
            ))
            .unwrap();
            open_doc(&ws, &uri, &al_code);

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
                .unwrap_or_else(|| panic!("{call}; must be offered parentheses"));
            let updated =
                super::super::test_support::assert_action_applies_cleanly(&al_code, action, call);
            assert!(updated.contains(&format!("{member};")), "{updated}");
        }
    }

    /// The receiver words are still not calls when they stand alone.
    #[test]
    fn add_parens_not_offered_for_a_bare_receiver_word() {
        for word in ["Rec", "CurrPage", "skip", "quit"] {
            let ws = Workspace::new();
            let al_code = format!(
                "page 50100 \"My Page\"\n{{\n    procedure Caller()\n    begin\n        {word};\n    end;\n}}\n"
            );
            let uri = Url::parse(&format!("file:///test/AddParensBare{word}.al")).unwrap();
            open_doc(&ws, &uri, &al_code);

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
            assert!(
                !actions.iter().any(|a| a.title.contains("parentheses")),
                "`{word};` alone is not a call"
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
