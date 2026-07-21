//! Event-subscriber conversion and tooltip-move source actions.

use url::Url;

use super::{detect_object_kind, single_edit_ws};
use super::{AlObjectKind, CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

pub(super) fn source_action_move_tooltip(
    _workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let cursor_line = range.start.line as usize;
    let line = text.lines().nth(cursor_line)?;
    let trimmed = line.trim();

    let lower_trimmed = trimmed.to_lowercase();
    if !lower_trimmed.starts_with("tooltip") {
        return None;
    }

    // Verify we're inside a page object (not a table). Walk up looking for
    // the object type declaration. A simple heuristic: scan backwards for
    // lines matching `^page ` or `^table ` at the start of the file.
    let object_kind = detect_object_kind(text);
    if object_kind != Some(AlObjectKind::Page) {
        return None;
    }

    // Verify we're inside a field control (not a table fieldgroup or other context).
    // Scan backwards from cursor for "field(" pattern — page field controls start with `field(`
    let in_page_field = {
        let mut found = false;
        let mut depth = 0i32;
        for (i, l) in text
            .lines()
            .enumerate()
            .take(cursor_line + 1)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let lt = l.trim().to_lowercase();
            for ch in l.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if lt.starts_with("field(") && i < cursor_line {
                found = true;
                break;
            }
            // If we've crossed a page/layout/area keyword going backwards, stop
            if lt == "layout" || lt == "area(content)" || lt == "area(factboxes)" {
                break;
            }
            let _ = depth; // suppress warning
        }
        found
    };

    if !in_page_field {
        return None;
    }

    // Delete from start of line to start of next line. Use checked conversion so
    // a pathological cursor_line near u32::MAX cannot wrap the next-line index.
    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: u32::try_from(cursor_line).unwrap_or(u32::MAX),
                character: 0,
            },
            end: super::Position {
                line: u32::try_from(cursor_line.saturating_add(1)).unwrap_or(u32::MAX),
                character: 0,
            },
        },
        new_text: String::new(),
    };

    Some(CodeActionEntry {
        title: "Move ToolTip to table field (remove from page)".to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, vec![edit])),
        is_preferred: false,
    })
}

pub(super) fn source_action_convert_event_subscriber(
    _workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let cursor_line = range.start.line as usize;

    // Clamp both bounds against the document length so a stale/extreme cursor
    // line cannot make `search_end - search_start` underflow below.
    let line_count = text.lines().count();
    let search_start = cursor_line.saturating_sub(2).min(line_count);
    let search_end = cursor_line.saturating_add(3).min(line_count);

    let mut attr_line_idx: Option<usize> = None;
    let mut attr_line_text = String::new();

    for (offset, line) in text
        .lines()
        .enumerate()
        .skip(search_start)
        .take(search_end - search_start)
    {
        if line.trim_start().starts_with('[') && line.to_lowercase().contains("eventsubscriber") {
            attr_line_idx = Some(offset);
            attr_line_text = line.to_string();
            break;
        }
    }

    let line_idx = attr_line_idx?;
    let line = &attr_line_text;

    // Find the third argument (index 2) in the EventSubscriber(arg0, arg1, arg2, ...) call.
    // The third argument is the event name. We detect it as a single-quoted string: 'EventName'
    let lower = line.to_lowercase();
    let es_start = lower.find("eventsubscriber(")?;
    let args_start = es_start + "eventsubscriber(".len();

    let rest = &line[args_start..];
    let mut args: Vec<(usize, usize)> = Vec::new(); // byte offsets within `rest`
    let mut depth = 0usize;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut arg_start = 0usize;

    for (i, ch) in rest.char_indices() {
        match ch {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            '(' if !in_single_quote && !in_double_quote => depth += 1,
            ')' if !in_single_quote && !in_double_quote => {
                if depth == 0 {
                    args.push((arg_start, i));
                    break;
                }
                depth -= 1;
            }
            ',' if depth == 0 && !in_single_quote && !in_double_quote => {
                args.push((arg_start, i));
                arg_start = i + 1;
            }
            _ => {}
        }
    }

    if args.len() < 3 {
        return None;
    }

    let (start_off, end_off) = args[2];
    let arg_text = rest[start_off..end_off].trim();

    if !arg_text.starts_with('\'') || !arg_text.ends_with('\'') || arg_text.len() < 2 {
        return None;
    }

    let event_name = &arg_text[1..arg_text.len() - 1];
    if event_name.is_empty() {
        return None;
    }

    // Compute character column offsets within the full line.
    // All indices so far are byte offsets; convert to UTF-16 code units for LSP.
    let abs_start = args_start + start_off;
    let trimmed_prefix_len =
        rest[start_off..end_off].len() - rest[start_off..end_off].trim_start().len();
    let quote_byte = abs_start + trimmed_prefix_len;
    let quote_end_byte = quote_byte + arg_text.len();

    let quote_col = line[..quote_byte].encode_utf16().count() as u32;
    let quote_end_col = line[..quote_end_byte].encode_utf16().count() as u32;

    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: line_idx as u32,
                character: quote_col,
            },
            end: super::Position {
                line: line_idx as u32,
                character: quote_end_col,
            },
        },
        new_text: event_name.to_string(),
    };

    Some(CodeActionEntry {
        title: "Convert event name to identifier".to_string(),
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
    fn event_subscriber_string_to_identifier_offered() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Subscriber"
{
    [EventSubscriber(ObjectType::Table, Database::"Sales Header", 'OnBeforeInsertEvent', '', false, false)]
    procedure OnSalesHeaderInsert(var Rec: Record "Sales Header"; RunTrigger: Boolean)
    begin
    end;
}
"#;
        let uri = Url::parse("file:///test/EventSub.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 2,
                character: 5,
            },
            end: super::super::Position {
                line: 2,
                character: 5,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let ev_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.to_lowercase().contains("event")
                    && a.title.to_lowercase().contains("identifier")
            })
            .collect();

        assert!(
            !ev_actions.is_empty(),
            "Should offer event subscriber conversion"
        );
        let edit = ev_actions[0].edit.as_ref().expect("has edit");
        let (_, edits) = &edit.changes[0];
        assert!(
            edits
                .iter()
                .any(|e| e.new_text.contains("OnBeforeInsertEvent") && !e.new_text.contains('\'')),
            "Should replace string literal with identifier"
        );
    }

    #[test]
    fn event_subscriber_not_offered_when_already_identifier() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Subscriber"
{
    [EventSubscriber(ObjectType::Table, Database::"Sales Header", OnBeforeInsertEvent, '', false, false)]
    procedure OnSalesHeaderInsert(var Rec: Record "Sales Header"; RunTrigger: Boolean)
    begin
    end;
}
"#;
        let uri = Url::parse("file:///test/EventSub2.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 2,
                character: 5,
            },
            end: super::super::Position {
                line: 2,
                character: 5,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let ev_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.to_lowercase().contains("event")
                    && a.title.to_lowercase().contains("identifier")
            })
            .collect();

        assert!(
            ev_actions.is_empty(),
            "Should NOT offer when already using identifier"
        );
    }

    #[test]
    fn tooltip_on_page_field_offered_for_removal() {
        let ws = Workspace::new();
        let al_code = r#"page 50100 "My Page"
{
    layout
    {
        area(Content)
        {
            field(Name; Rec.Name)
            {
                ApplicationArea = All;
                ToolTip = 'Specifies the name.';
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyPage.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 9,
                character: 16,
            },
            end: super::super::Position {
                line: 9,
                character: 16,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let tt_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.to_lowercase().contains("tooltip"))
            .collect();

        assert!(
            !tt_actions.is_empty(),
            "Should offer ToolTip action on page field"
        );
    }

    #[test]
    fn tooltip_not_offered_outside_page_field() {
        let ws = Workspace::new();
        let al_code = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            ToolTip = 'Specifies the name.';
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyTable.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 6,
                character: 12,
            },
            end: super::super::Position {
                line: 6,
                character: 12,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let tt_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.to_lowercase().contains("tooltip")
                    && a.title.to_lowercase().contains("table")
            })
            .collect();

        assert!(
            tt_actions.is_empty(),
            "Should NOT offer move-to-table action inside a table object"
        );
    }

    // if_to_case UTF-16 column vs byte offset
    // When a non-ASCII character appears before the cursor on the same line,
    // tree-sitter Point::column must be a byte offset, not a UTF-16 code unit.
    // The two differ for characters with len_utf16 > 1 (e.g. emoji, surrogate pairs)
    // but we can also test with a 2-byte UTF-8 sequence (1 UTF-16 unit = still 2 bytes).
    // A simpler but valid test: verify the action is still offered when the procedure
    // contains a non-ASCII comment, ensuring we use utf16_col_to_byte_offset.
}
