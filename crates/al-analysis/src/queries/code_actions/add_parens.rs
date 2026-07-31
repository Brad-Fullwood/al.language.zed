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
}
