//! Event-subscriber conversion and tooltip-move source actions.

use url::Url;

use super::{annotation_edit, detect_object_kind, single_edit_ws};
use super::{AlObjectKind, CodeActionEntry, CodeActionKind, Range, TextEdit, WorkspaceEdit};
use al_workspace::Workspace;

/// Move a page field's `ToolTip` down to the underlying table field.
///
/// The action deletes the page's `ToolTip` line **and** inserts the same
/// property into the table field it displays. When the underlying table (or its
/// field) cannot be resolved in the workspace, the action is *not offered* —
/// the previous implementation only ever emitted the delete half, so applying
/// it silently destroyed the tooltip text.
pub(super) fn source_action_move_tooltip(
    workspace: &Workspace,
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
    // Only a complete single-line property can be relocated verbatim; a
    // continued/multi-line declaration would be truncated by the delete half.
    if !trimmed.ends_with(';') {
        return None;
    }

    // Verify we're inside a page object (not a table).
    let object_kind = detect_object_kind(text);
    if object_kind != Some(AlObjectKind::Page) {
        return None;
    }

    // The enclosing member must be a page *field* control. The old backward
    // scan matched any earlier `field(` line (its brace-depth counter was
    // computed and thrown away), so a ToolTip inside a page `action(...)`
    // wrongly received the action too.
    let member = enclosing_member(text, cursor_line)?;
    if member.keyword != "field" {
        return None;
    }

    let table_field = table_field_name(&member.args)?;
    let source_table = page_source_table(text)?;

    // Resolve the table's file; without it there is nowhere to put the tooltip.
    let table_path = workspace
        .file_index
        .find_by_object_name(&source_table.to_lowercase())?;
    let table_text = workspace
        .file_index
        .files
        .get(&table_path)
        .map(|entry| entry.value().clone())?;
    let table_uri = Url::from_file_path(&table_path).ok()?;

    let field_decl_line = find_table_field_declaration(&table_text, &table_field)?;
    if table_field_block_has_tooltip(&table_text, field_decl_line) {
        return None;
    }
    let insert_edit = annotation_edit(&table_text, field_decl_line, trimmed)?;

    // Delete from start of line to start of next line. Use checked conversion so
    // a pathological cursor_line near u32::MAX cannot wrap the next-line index.
    let delete_edit = TextEdit {
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

    let changes = if table_uri == *uri {
        // Page and table in one file: both edits must live under a single URI.
        vec![(uri.clone(), vec![delete_edit, insert_edit])]
    } else {
        vec![
            (uri.clone(), vec![delete_edit]),
            (table_uri, vec![insert_edit]),
        ]
    };

    Some(CodeActionEntry {
        title: format!("Move ToolTip to table field '{}'", table_field),
        kind: CodeActionKind::Refactor,
        edit: Some(WorkspaceEdit { changes }),
        is_preferred: false,
    })
}

/// A page/table member declaration such as `field(Name; Rec.Name)` or
/// `action("Do It")`.
struct MemberHead {
    /// Lower-cased declaration keyword (`field`, `action`, `group`, …).
    keyword: String,
    /// Raw text between the parentheses.
    args: String,
}

/// Declaration keywords that open a named member block in a page/table body.
const MEMBER_KEYWORDS: &[&str] = &[
    "field",
    "action",
    "actionref",
    "group",
    "part",
    "systempart",
    "usercontrol",
    "label",
    "repeater",
    "cuegroup",
    "fixed",
    "grid",
    "area",
    "dataitem",
    "column",
    "key",
    "fieldgroup",
];

/// Innermost member block containing `cursor_line`, using real brace nesting.
fn enclosing_member(text: &str, cursor_line: usize) -> Option<MemberHead> {
    let mut stack: Vec<Option<MemberHead>> = Vec::new();
    let mut pending: Option<MemberHead> = None;

    for (index, line) in text.lines().enumerate() {
        if index >= cursor_line {
            break;
        }
        let code = strip_literals_and_comment(line);
        if let Some(head) = parse_member_head(code.trim()) {
            pending = Some(head);
        }
        for ch in code.chars() {
            match ch {
                '{' => stack.push(pending.take()),
                '}' => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }

    stack.into_iter().rev().flatten().next()
}

/// Parse `keyword(args)` at the start of a trimmed line.
fn parse_member_head(trimmed: &str) -> Option<MemberHead> {
    let open = trimmed.find('(')?;
    let keyword = trimmed[..open].trim().to_lowercase();
    if !MEMBER_KEYWORDS.contains(&keyword.as_str()) {
        return None;
    }
    let close = trimmed.rfind(')')?;
    if close < open {
        return None;
    }
    Some(MemberHead {
        keyword,
        args: trimmed[open + 1..close].to_string(),
    })
}

/// Blank out single/double-quoted spans and drop a trailing line comment so
/// braces inside AL captions do not corrupt the nesting count.
fn strip_literals_and_comment(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    let mut in_single = false;
    let mut in_double = false;
    while let Some((_, ch)) = chars.next() {
        if !in_single && !in_double && ch == '/' && chars.peek().is_some_and(|(_, n)| *n == '/') {
            break;
        }
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(ch);
            }
            _ if in_single => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// Table field name displayed by a page control's `field(Name; Source)` args.
///
/// `field(Name; Rec.Name)` → `Name`; `field("My Ctl"; Rec."No.")` → `No.`.
/// Returns `None` for an expression source (e.g. `Rec.Amount * 2`) since the
/// tooltip has no single owning field then.
fn table_field_name(args: &str) -> Option<String> {
    let mut parts = args.splitn(2, ';');
    let first = parts.next()?.trim();
    let source = parts.next().map(str::trim).unwrap_or(first);
    if source.is_empty() {
        return None;
    }
    // Strip a leading record-variable qualifier (`Rec.`, `xRec.`, `"My Rec".`).
    let bare = match strip_receiver(source) {
        Some(rest) => rest,
        None => source,
    };
    let bare = bare.trim();
    let name = bare.trim_matches('"').trim();
    if name.is_empty() {
        return None;
    }
    // Reject anything that is not a plain identifier reference.
    if !bare.starts_with('"')
        && !bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
    {
        return None;
    }
    Some(name.to_string())
}

/// Split `Receiver.Rest` into `Rest`, honouring a quoted receiver.
fn strip_receiver(source: &str) -> Option<&str> {
    if let Some(after_quote) = source.strip_prefix('"') {
        let close = after_quote.find('"')?;
        let rest = &after_quote[close + 1..];
        return rest.strip_prefix('.');
    }
    let dot = source.find('.')?;
    Some(&source[dot + 1..])
}

/// The page's `SourceTable` property value (unquoted).
fn page_source_table(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        if lower.starts_with("layout") || lower.starts_with("actions") {
            break;
        }
        if !lower.starts_with("sourcetable") {
            continue;
        }
        let value = trimmed
            .split_once('=')?
            .1
            .trim()
            .trim_end_matches(';')
            .trim();
        let value = value.trim_matches('"').trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// 0-based line of `field(<id>; <name>; <type>)` declaring `field_name`.
fn find_table_field_declaration(table_text: &str, field_name: &str) -> Option<u32> {
    for (index, line) in table_text.lines().enumerate() {
        let code = strip_literals_and_comment(line);
        let Some(head) = parse_member_head(code.trim()) else {
            continue;
        };
        if head.keyword != "field" {
            continue;
        }
        let parts: Vec<&str> = head.args.split(';').collect();
        // Table fields are `field(id; Name; Type)`.
        let Some(name_part) = parts.get(1) else {
            continue;
        };
        if name_part
            .trim()
            .trim_matches('"')
            .eq_ignore_ascii_case(field_name)
        {
            return Some(index as u32);
        }
    }
    None
}

/// Whether the table field block starting at `decl_line` already declares a
/// `ToolTip` (in which case there is nothing safe to move).
fn table_field_block_has_tooltip(table_text: &str, decl_line: u32) -> bool {
    let mut depth = 0i32;
    let mut started = false;
    for line in table_text.lines().skip(decl_line as usize) {
        let code = strip_literals_and_comment(line);
        if started && code.trim().to_lowercase().starts_with("tooltip") {
            return true;
        }
        for ch in code.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    started = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        if started && depth <= 0 {
            break;
        }
    }
    false
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
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
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

    const PAGE_WITH_TOOLTIP: &str = r#"page 50100 "My Page"
{
    SourceTable = "My Table";

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
    actions
    {
        area(Processing)
        {
            action(DoIt)
            {
                ApplicationArea = All;
                ToolTip = 'Runs the thing.';
            }
        }
    }
}
"#;

    const TABLE_WITHOUT_TOOLTIP: &str = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            DataClassification = CustomerContent;
        }
    }
}
"#;

    fn add_indexed(ws: &Workspace, uri: &Url, code: &str) {
        open_doc(ws, uri, code);
        ws.file_index
            .add_file(uri.to_file_path().unwrap(), code.to_string());
    }

    fn tooltip_cursor(line: u32) -> Range {
        Range {
            start: super::super::Position {
                line,
                character: 16,
            },
            end: super::super::Position {
                line,
                character: 16,
            },
        }
    }

    #[test]
    fn tooltip_on_page_field_is_moved_into_the_table_field() {
        let ws = Workspace::new();
        let page_uri = Url::parse("file:///test/MyPage.al").unwrap();
        let table_uri = Url::parse("file:///test/MyTable.al").unwrap();
        add_indexed(&ws, &page_uri, PAGE_WITH_TOOLTIP);
        add_indexed(&ws, &table_uri, TABLE_WITHOUT_TOOLTIP);

        let action =
            source_action_move_tooltip(&ws, &page_uri, PAGE_WITH_TOOLTIP, tooltip_cursor(11))
                .expect("should offer move-tooltip on a page field");
        let edit = action.edit.as_ref().expect("has edit");
        assert_eq!(
            edit.changes.len(),
            2,
            "must edit both the page and the table: {:?}",
            edit.changes
        );

        let page_edits = edit
            .changes
            .iter()
            .find(|(u, _)| u == &page_uri)
            .map(|(_, e)| e.clone())
            .expect("page edit");
        let table_edits = edit
            .changes
            .iter()
            .find(|(u, _)| u == &table_uri)
            .map(|(_, e)| e.clone())
            .expect("table edit");

        let updated_page =
            super::super::test_support::apply_text_edits(PAGE_WITH_TOOLTIP, &page_edits);
        let updated_table =
            super::super::test_support::apply_text_edits(TABLE_WITHOUT_TOOLTIP, &table_edits);

        assert!(
            !updated_page.contains("Specifies the name."),
            "page tooltip removed: {updated_page}"
        );
        assert!(
            updated_table.contains("ToolTip = 'Specifies the name.';"),
            "tooltip must land in the table field: {updated_table}"
        );
        for (source, updated, label) in [
            (PAGE_WITH_TOOLTIP, &updated_page, "page"),
            (TABLE_WITHOUT_TOOLTIP, &updated_table, "table"),
        ] {
            assert!(
                !al_syntax::AlParser::parse_quick(source)
                    .tree
                    .root_node()
                    .has_error(),
                "{label} fixture must parse"
            );
            assert!(
                !al_syntax::AlParser::parse_quick(updated)
                    .tree
                    .root_node()
                    .has_error(),
                "{label} must still parse after the move:\n{updated}"
            );
        }
    }

    /// Never offer an action that would only delete the tooltip.
    #[test]
    fn tooltip_not_offered_when_table_is_not_resolvable() {
        let ws = Workspace::new();
        let page_uri = Url::parse("file:///test/MyPage.al").unwrap();
        add_indexed(&ws, &page_uri, PAGE_WITH_TOOLTIP);

        assert!(
            source_action_move_tooltip(&ws, &page_uri, PAGE_WITH_TOOLTIP, tooltip_cursor(11))
                .is_none(),
            "no table in the workspace must suppress the action instead of deleting text"
        );
    }

    /// The backward `field(` scan matched any earlier field line, so a ToolTip
    /// inside a page *action* wrongly received the move-to-table action.
    #[test]
    fn tooltip_inside_page_action_is_not_offered() {
        let ws = Workspace::new();
        let page_uri = Url::parse("file:///test/MyPage.al").unwrap();
        let table_uri = Url::parse("file:///test/MyTable.al").unwrap();
        add_indexed(&ws, &page_uri, PAGE_WITH_TOOLTIP);
        add_indexed(&ws, &table_uri, TABLE_WITHOUT_TOOLTIP);

        // Line 22 is the ToolTip inside `action(DoIt)`.
        let line = PAGE_WITH_TOOLTIP
            .lines()
            .position(|l| l.contains("Runs the thing."))
            .expect("action tooltip present") as u32;
        assert!(
            source_action_move_tooltip(&ws, &page_uri, PAGE_WITH_TOOLTIP, tooltip_cursor(line))
                .is_none(),
            "a ToolTip inside an action block must not be offered the move-to-table action"
        );
    }

    #[test]
    fn tooltip_not_offered_when_table_field_already_has_one() {
        let ws = Workspace::new();
        let page_uri = Url::parse("file:///test/MyPage.al").unwrap();
        let table_uri = Url::parse("file:///test/MyTable.al").unwrap();
        let table = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            ToolTip = 'Already documented.';
        }
    }
}
"#;
        add_indexed(&ws, &page_uri, PAGE_WITH_TOOLTIP);
        add_indexed(&ws, &table_uri, table);

        assert!(
            source_action_move_tooltip(&ws, &page_uri, PAGE_WITH_TOOLTIP, tooltip_cursor(11))
                .is_none()
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

    #[test]
    fn table_field_name_reads_the_source_expression() {
        assert_eq!(
            table_field_name("Name; Rec.Name").as_deref(),
            Some("Name"),
            "unqualified field"
        );
        assert_eq!(
            table_field_name("\"My Ctl\"; Rec.\"No.\"").as_deref(),
            Some("No.")
        );
        assert_eq!(table_field_name("Amount").as_deref(), Some("Amount"));
        assert!(
            table_field_name("Total; Rec.Amount * 2").is_none(),
            "an expression source has no single owning field"
        );
    }

    #[test]
    fn enclosing_member_respects_brace_nesting() {
        let member = enclosing_member(PAGE_WITH_TOOLTIP, 11).expect("field control");
        assert_eq!(member.keyword, "field");
        let action_line = PAGE_WITH_TOOLTIP
            .lines()
            .position(|l| l.contains("Runs the thing."))
            .unwrap();
        let member = enclosing_member(PAGE_WITH_TOOLTIP, action_line).expect("action");
        assert_eq!(member.keyword, "action");
    }

    // if_to_case UTF-16 column vs byte offset
    // When a non-ASCII character appears before the cursor on the same line,
    // tree-sitter Point::column must be a byte offset, not a UTF-16 code unit.
    // The two differ for characters with len_utf16 > 1 (e.g. emoji, surrogate pairs)
    // but we can also test with a 2-byte UTF-8 sequence (1 UTF-16 unit = still 2 bytes).
    // A simpler but valid test: verify the action is still offered when the procedure
    // contains a non-ASCII comment, ensuring we use utf16_col_to_byte_offset.
}
