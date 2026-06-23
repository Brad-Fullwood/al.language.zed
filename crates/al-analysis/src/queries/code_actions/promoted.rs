//! Promoted-actions conversion, ApplicationArea, and report-layout source actions.

use url::Url;

use super::{detect_indent, detect_object_kind, single_edit_ws};
use super::{AlObjectKind, CodeActionEntry, CodeActionKind, Range, TextEdit};

/// Detect page actions that use old-style `Promoted = true` / `PromotedCategory` properties
/// and offer to convert them to the new `actionRef` syntax inside `area(Promoted)`.
///
/// The action is offered when the cursor is within an `action(...)` declaration that
/// has a `Promoted = true` property in its body.
///
/// The generated edit:
///   1. Removes `Promoted = true` and `PromotedCategory = X` from the existing action body.
///   2. Inserts an `area(Promoted)` section (before the closing `}` of `actions { }`) with
///      an `actionref` pointing to the action.
pub(super) fn source_action_convert_promoted_actions(
    uri: &Url,
    text: &str,
    range: Range,
) -> Vec<CodeActionEntry> {
    let cursor_line = range.start.line as usize;

    // Must be inside a page or pageextension object (F-045: previously
    // matched `Page | Other`, which included tables, queries, enums, and
    // every other non-special-cased object kind).
    let obj_kind = detect_object_kind(text);
    match obj_kind {
        Some(AlObjectKind::Page) | Some(AlObjectKind::PageExtension) => {}
        _ => return Vec::new(),
    }

    let promoted_actions = collect_promoted_actions(text);
    if promoted_actions.is_empty() {
        return Vec::new();
    }

    let active: Vec<_> = promoted_actions
        .iter()
        .filter(|a| cursor_line >= a.decl_line && cursor_line <= a.body_end_line)
        .collect();

    if active.is_empty() {
        return Vec::new();
    }

    let mut actions = Vec::new();

    for pa in &active {
        if let Some(edit) = build_promoted_action_conversion(uri, text, pa) {
            actions.push(edit);
        }
    }

    actions
}

/// Information about a promoted action found by text scanning.
struct PromotedActionInfo {
    /// Name of the action (unquoted if in `"..."`)
    name: String,
    /// PromotedCategory value, e.g. "Process"
    category: Option<String>,
    /// 0-based line of `action(...)` declaration
    decl_line: usize,
    /// 0-based last line of the action body (closing `}`)
    body_end_line: usize,
    /// Lines to remove (0-based): the `Promoted = true` and `PromotedCategory = X` lines
    remove_lines: Vec<usize>,
}

fn collect_promoted_actions(text: &str) -> Vec<PromotedActionInfo> {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim().to_lowercase();
        if trimmed.starts_with("action(") {
            let raw = lines[i].trim();
            let name = match extract_action_name(raw) {
                Some(n) => n,
                None => {
                    i += 1;
                    continue;
                }
            };

            let (body_end, inner_lines) = find_block_extent(&lines, i);

            let mut has_promoted = false;
            let mut category: Option<String> = None;
            let mut remove_lines = Vec::new();

            for &il in &inner_lines {
                let lt = lines[il].trim().to_lowercase();
                if lt.starts_with("promoted") && lt.contains('=') {
                    if !lt.contains("category") && !lt.contains("actiontype") {
                        if lt.contains("true") {
                            has_promoted = true;
                            remove_lines.push(il);
                        }
                    } else if lt.contains("promotedcategory") {
                        let val = extract_property_value(lines[il]);
                        category = Some(val);
                        remove_lines.push(il);
                    }
                }
            }

            if has_promoted && !name.is_empty() {
                result.push(PromotedActionInfo {
                    name,
                    category,
                    decl_line: i,
                    body_end_line: body_end,
                    remove_lines,
                });
            }

            i = body_end + 1;
            continue;
        }
        i += 1;
    }

    result
}

/// Extract the action name from a line like `action("My Action")` or `action(MyAction)`.
fn extract_action_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let lower = trimmed.to_lowercase();
    let start = lower.find("action(")?;
    let after = &trimmed[start + "action(".len()..];
    let close = after.find(')')?;
    let inner = after[..close].trim();
    if inner.is_empty() {
        return None;
    }
    Some(inner.trim_matches('"').to_string())
}

fn find_block_extent(lines: &[&str], start: usize) -> (usize, Vec<usize>) {
    let mut depth = 0i32;
    let mut inner = Vec::new();
    let mut block_started = false;

    for (i, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    block_started = true;
                }
                '}' => {
                    depth -= 1;
                }
                _ => {}
            }
        }
        if block_started && depth > 0 && i > start {
            inner.push(i);
        }
        if block_started && depth == 0 {
            return (i, inner);
        }
    }
    (lines.len().saturating_sub(1), inner)
}

/// Extract the value portion of a property line like `PromotedCategory = Process;` → `"Process"`.
fn extract_property_value(line: &str) -> String {
    if let Some(eq_pos) = line.find('=') {
        let val = line[eq_pos + 1..].trim().trim_end_matches(';').trim();
        val.to_string()
    } else {
        String::new()
    }
}

fn build_promoted_action_conversion(
    uri: &Url,
    text: &str,
    pa: &PromotedActionInfo,
) -> Option<CodeActionEntry> {
    let mut edits: Vec<TextEdit> = Vec::new();

    for &line_no in &pa.remove_lines {
        edits.push(TextEdit {
            range: Range {
                start: super::Position {
                    line: line_no as u32,
                    character: 0,
                },
                end: super::Position {
                    line: (line_no + 1) as u32,
                    character: 0,
                },
            },
            new_text: String::new(),
        });
    }

    // Find where to insert the actionRef — before the closing `}` of `actions { }` block.
    let actions_end_line = find_actions_block_end(text);
    let category = pa.category.as_deref().unwrap_or("Process");

    let decl_line_text = text.lines().nth(pa.decl_line).unwrap_or("        ");
    let action_indent_len = decl_line_text.len() - decl_line_text.trim_start().len();
    let action_indent = &decl_line_text[..action_indent_len];
    // The area indent is one level up (remove 4 spaces)
    let area_indent = if action_indent_len >= 4 {
        &action_indent[..action_indent_len - 4]
    } else {
        action_indent
    };

    let insert_line = actions_end_line.unwrap_or_else(|| text.lines().count() as u32);

    let action_ref_text = format!(
        "{ind}area(Promoted)\n{ind}{{\n{ind}    actionref({name}_Promoted; {name})\n{ind}    {{\n{ind}        Caption = '{cat}';\n{ind}    }}\n{ind}}}\n",
        ind = area_indent,
        name = pa.name,
        cat = category
    );

    edits.push(TextEdit {
        range: Range {
            start: super::Position {
                line: insert_line,
                character: 0,
            },
            end: super::Position {
                line: insert_line,
                character: 0,
            },
        },
        new_text: action_ref_text,
    });

    edits.sort_by_key(|e| e.range.start.line);

    Some(CodeActionEntry {
        title: format!("Convert '{}' to actionRef (T1205)", pa.name),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, edits)),
        is_preferred: false,
    })
}

fn find_actions_block_end(text: &str) -> Option<u32> {
    let lines: Vec<&str> = text.lines().collect();
    let mut depth = 0i32;
    let mut in_actions = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim().to_lowercase();
        if !in_actions
            && (trimmed == "actions"
                || trimmed.starts_with("actions ")
                || trimmed.starts_with("actions{"))
        {
            in_actions = true;
        }
        if in_actions {
            for ch in line.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if in_actions && depth == 0 && i > 0 {
                return Some(i as u32);
            }
        }
    }
    None
}

/// Offer to add an object-level `ApplicationArea` property to a page or report
/// and remove redundant field-level properties that duplicate the object default.
///
/// The action is only offered when:
///   - The object is a `page`, `pageextension`, or `report`.
///   - The object does NOT yet have a top-level `ApplicationArea` property.
///   - At least one field/column control has `ApplicationArea = All`.
///
/// **Hardcoded AL property names (F-OPEN-039):** `ApplicationArea` /
/// `PromotedCategory` / `Promoted` / `tooltip` appear as string literals
/// throughout the code-actions emitter because they are *generated output* —
/// AL source the user can run through alc. The CLAUDE.md no-hardcoded-AL-
/// values rule targets the *validation/lookup* surface that drifts with BC
/// releases; an emitter that produces specific AL syntax must by definition
/// know that syntax. Routing these through `LanguageData` would be circular
/// (LanguageData reads property names from the grammar/symbols at runtime;
/// the emitter encodes the AL Sample that uses those names).
pub(super) fn source_action_set_application_area(
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let cursor_line = range.start.line as usize;
    let _ = cursor_line; // offered throughout the file

    // Must be inside a page, report, or one of their extensions (F-045:
    // previously matched `Page | Report | Other`, which included queries,
    // enums, and every other non-special-cased kind).
    let obj_kind = detect_object_kind(text)?;
    match obj_kind {
        AlObjectKind::Page
        | AlObjectKind::PageExtension
        | AlObjectKind::Report
        | AlObjectKind::ReportExtension => {}
        _ => return None,
    }

    if object_has_application_area(text) {
        return None;
    }

    let field_aa_lines = collect_field_application_area_lines(text, "All");
    if field_aa_lines.is_empty() {
        return None;
    }

    let insert_line = find_object_properties_insert_line(text)?;

    let indent = detect_indent(text, insert_line.saturating_sub(1));

    let mut edits: Vec<TextEdit> = Vec::new();

    edits.push(TextEdit {
        range: Range {
            start: super::Position {
                line: insert_line,
                character: 0,
            },
            end: super::Position {
                line: insert_line,
                character: 0,
            },
        },
        new_text: format!("{}ApplicationArea = All;\n", indent),
    });

    let mut sorted_lines = field_aa_lines.clone();
    sorted_lines.sort_unstable_by(|a, b| b.cmp(a));
    for ln in &sorted_lines {
        edits.push(TextEdit {
            range: Range {
                start: super::Position {
                    line: *ln as u32,
                    character: 0,
                },
                end: super::Position {
                    line: (*ln + 1) as u32,
                    character: 0,
                },
            },
            new_text: String::new(),
        });
    }

    edits.sort_by_key(|e| e.range.start.line);

    Some(CodeActionEntry {
        title: "Set ApplicationArea = All on object and remove redundant field overrides (T1206)"
            .to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, edits)),
        is_preferred: false,
    })
}

fn object_has_application_area(text: &str) -> bool {
    for line in text.lines() {
        let trimmed = line.trim().to_lowercase();
        // Stop scanning once we hit major section keywords
        if trimmed == "layout"
            || trimmed == "actions"
            || trimmed == "requestpage"
            || trimmed == "rendering"
        {
            break;
        }
        if trimmed.starts_with("applicationarea") && trimmed.contains('=') {
            return true;
        }
    }
    false
}

/// Collect 0-based line numbers where `ApplicationArea = <value>` appears,
/// only returning lines where the value matches `value` (case-insensitive).
fn collect_field_application_area_lines(text: &str, value: &str) -> Vec<usize> {
    let value_lower = value.to_lowercase();
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim().to_lowercase();
            if trimmed.starts_with("applicationarea") && trimmed.contains('=') {
                if let Some(eq) = trimmed.find('=') {
                    let val = trimmed[eq + 1..]
                        .trim()
                        .trim_end_matches(';')
                        .trim()
                        .to_lowercase();
                    if val == value_lower {
                        return Some(i);
                    }
                }
            }
            None
        })
        .collect()
}

fn find_object_properties_insert_line(text: &str) -> Option<u32> {
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "{" || (trimmed.ends_with('{') && !trimmed.starts_with("//")) {
            return Some((i + 1) as u32);
        }
    }
    None
}

/// Convert legacy `RDLCLayout`/`WordLayout` properties to the new
/// `rendering { layout(...) { ... } }` section syntax.
///
/// The action is offered when the cursor is within a few lines of an
/// `RDLCLayout` or `WordLayout` property inside a `report` object.
pub(super) fn source_action_fix_report_layout(
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let cursor_line = range.start.line as usize;

    let obj_kind = detect_object_kind(text)?;
    if obj_kind != AlObjectKind::Report {
        return None;
    }

    let legacy = collect_legacy_layout_properties(text);
    if legacy.is_empty() {
        return None;
    }

    // Offer when cursor is within 3 lines of any legacy layout property
    let is_near = legacy.iter().any(|lp| cursor_line.abs_diff(lp.line) <= 3);

    if !is_near {
        return None;
    }

    build_report_layout_conversion(uri, text, &legacy)
}

/// Information about a legacy layout property.
struct LegacyLayoutProp {
    /// 0-based line number
    line: usize,
    /// Layout type: "RDLC" or "Word"
    layout_type: String,
    path: String,
}

fn collect_legacy_layout_properties(text: &str) -> Vec<LegacyLayoutProp> {
    let mut result = Vec::new();

    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim().to_lowercase();
        if trimmed.starts_with("rdlclayout") && trimmed.contains('=') {
            let path = extract_property_value(line.trim())
                .trim_matches('\'')
                .to_string();
            result.push(LegacyLayoutProp {
                line: i,
                layout_type: "RDLC".to_string(),
                path,
            });
        } else if trimmed.starts_with("wordlayout") && trimmed.contains('=') {
            let path = extract_property_value(line.trim())
                .trim_matches('\'')
                .to_string();
            result.push(LegacyLayoutProp {
                line: i,
                layout_type: "Word".to_string(),
                path,
            });
        }
    }

    result
}

fn build_report_layout_conversion(
    uri: &Url,
    text: &str,
    legacy: &[LegacyLayoutProp],
) -> Option<CodeActionEntry> {
    let mut edits: Vec<TextEdit> = Vec::new();

    for lp in legacy {
        edits.push(TextEdit {
            range: Range {
                start: super::Position {
                    line: lp.line as u32,
                    character: 0,
                },
                end: super::Position {
                    line: (lp.line + 1) as u32,
                    character: 0,
                },
            },
            new_text: String::new(),
        });
    }

    let insert_line = find_rendering_insert_line(text);

    let indent = detect_indent(text, insert_line.saturating_sub(1));

    let mut rendering_text = format!("{}rendering\n{}{{\n", indent, indent);
    for (idx, lp) in legacy.iter().enumerate() {
        let layout_name = if legacy.len() == 1 {
            format!("{}Layout", lp.layout_type)
        } else {
            format!("{}Layout{}", lp.layout_type, idx + 1)
        };
        let mime = match lp.layout_type.as_str() {
            "RDLC" => "RDLC",
            "Word" => "Word",
            _ => "RDLC",
        };
        rendering_text.push_str(&format!(
            "{}    layout({})\n{}    {{\n{}        Type = {};\n{}        LayoutFile = '{}';\n{}    }}\n",
            indent, layout_name,
            indent,
            indent, mime,
            indent, lp.path,
            indent
        ));
    }
    rendering_text.push_str(&format!("{}}}\n", indent));

    edits.push(TextEdit {
        range: Range {
            start: super::Position {
                line: insert_line,
                character: 0,
            },
            end: super::Position {
                line: insert_line,
                character: 0,
            },
        },
        new_text: rendering_text,
    });

    edits.sort_by_key(|e| e.range.start.line);

    Some(CodeActionEntry {
        title: "Convert legacy layout properties to rendering section (T1207)".to_string(),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, edits)),
        is_preferred: false,
    })
}

/// Find the line where the `rendering { }` section should be inserted.
/// Prefers inserting before `requestpage`, otherwise before the last `}` of the report.
fn find_rendering_insert_line(text: &str) -> u32 {
    let lines: Vec<&str> = text.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim().to_lowercase();
        if trimmed == "requestpage" || trimmed.starts_with("requestpage ") {
            return i as u32;
        }
    }

    for i in (0..lines.len()).rev() {
        if lines[i].trim() == "}" {
            return i as u32;
        }
    }

    lines.len() as u32
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
    fn t1205_promoted_action_conversion_offered_on_promoted_line() {
        let ws = Workspace::new();

        let al_code = r#"page 50100 "My Page"
{
    actions
    {
        area(processing)
        {
            action(MyAction)
            {
                Caption = 'Do Something';
                Promoted = true;
                PromotedCategory = Process;
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyPage.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Cursor on the `Promoted = true;` line (line 9)
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
        let conv_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.contains("actionRef")
                    || a.title.contains("actionref")
                    || a.title.contains("T1205")
            })
            .collect();

        assert!(
            !conv_actions.is_empty(),
            "Should offer promoted action conversion"
        );

        let edit = conv_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let combined: String = edits.iter().map(|e| e.new_text.as_str()).collect();

        assert!(
            combined.to_lowercase().contains("actionref"),
            "Should contain actionref keyword, got: {:?}",
            combined
        );
        assert!(combined.contains("MyAction"), "Should reference MyAction");
        let remove_edits: Vec<_> = edits.iter().filter(|e| e.new_text.is_empty()).collect();
        assert!(
            !remove_edits.is_empty(),
            "Should have removal edits for Promoted properties"
        );
    }

    #[test]
    fn t1205_promoted_action_preserves_category() {
        let ws = Workspace::new();

        let al_code = r#"page 50100 "My Page"
{
    actions
    {
        area(processing)
        {
            action(PostAction)
            {
                Caption = 'Post';
                Promoted = true;
                PromotedCategory = New;
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyPage2.al").unwrap();
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
        let conv_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1205") || a.title.to_lowercase().contains("actionref"))
            .collect();

        assert!(!conv_actions.is_empty(), "Should offer conversion");

        let edit = conv_actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        let combined: String = edits.iter().map(|e| e.new_text.as_str()).collect();

        assert!(
            combined.contains("New"),
            "Should preserve PromotedCategory = New"
        );
    }

    #[test]
    fn t1205_not_offered_on_non_page_objects() {
        let ws = Workspace::new();

        let al_code = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            Caption = 'Name';
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyTable.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 5,
                character: 12,
            },
            end: super::super::Position {
                line: 5,
                character: 12,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let conv_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1205") || a.title.to_lowercase().contains("actionref"))
            .collect();

        assert!(
            conv_actions.is_empty(),
            "Should NOT offer promoted action conversion on table"
        );
    }

    #[test]
    fn t1205_not_offered_when_action_has_no_promoted_property() {
        let ws = Workspace::new();

        let al_code = r#"page 50100 "My Page"
{
    actions
    {
        area(processing)
        {
            action(MyAction)
            {
                Caption = 'Do Something';
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyPageNoPromote.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 7,
                character: 12,
            },
            end: super::super::Position {
                line: 7,
                character: 12,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let conv_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1205") || a.title.to_lowercase().contains("actionref"))
            .collect();

        assert!(
            conv_actions.is_empty(),
            "Should NOT offer conversion when no Promoted = true"
        );
    }

    #[test]
    fn t1206_application_area_action_offered_on_page_with_field_aa() {
        let ws = Workspace::new();

        let al_code = r#"page 50100 "My Page"
{
    layout
    {
        area(content)
        {
            field(Name; Rec.Name)
            {
                ApplicationArea = All;
                Caption = 'Name';
            }
            field(Address; Rec.Address)
            {
                ApplicationArea = All;
                Caption = 'Address';
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyPageAA.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 8,
                character: 16,
            },
            end: super::super::Position {
                line: 8,
                character: 16,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let aa_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.contains("T1206") || a.title.to_lowercase().contains("applicationarea")
            })
            .collect();

        assert!(
            !aa_actions.is_empty(),
            "Should offer ApplicationArea action"
        );

        let edit = aa_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];

        let insert_edits: Vec<_> = edits.iter().filter(|e| !e.new_text.is_empty()).collect();
        let remove_edits: Vec<_> = edits.iter().filter(|e| e.new_text.is_empty()).collect();

        assert!(
            !insert_edits.is_empty(),
            "Should insert object-level ApplicationArea"
        );
        assert!(
            !remove_edits.is_empty(),
            "Should remove field-level ApplicationArea properties"
        );

        let inserted = &insert_edits[0].new_text;
        assert!(
            inserted.to_lowercase().contains("applicationarea"),
            "Inserted text should set ApplicationArea"
        );
        assert!(inserted.contains("All"), "Should set ApplicationArea = All");
    }

    #[test]
    fn t1206_not_offered_when_object_already_has_application_area() {
        let ws = Workspace::new();

        let al_code = r#"page 50100 "My Page"
{
    ApplicationArea = All;

    layout
    {
        area(content)
        {
            field(Name; Rec.Name)
            {
                ApplicationArea = All;
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyPageAAExists.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 10,
                character: 16,
            },
            end: super::super::Position {
                line: 10,
                character: 16,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let aa_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.contains("T1206") || a.title.to_lowercase().contains("applicationarea")
            })
            .collect();

        assert!(
            aa_actions.is_empty(),
            "Should NOT offer action when object already has ApplicationArea"
        );
    }

    #[test]
    fn t1206_not_offered_when_no_field_level_application_area() {
        let ws = Workspace::new();

        let al_code = r#"page 50100 "My Page"
{
    layout
    {
        area(content)
        {
            field(Name; Rec.Name)
            {
                Caption = 'Name';
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyPageNoAA.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 8,
                character: 16,
            },
            end: super::super::Position {
                line: 8,
                character: 16,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let aa_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.contains("T1206") || a.title.to_lowercase().contains("applicationarea")
            })
            .collect();

        assert!(
            aa_actions.is_empty(),
            "Should NOT offer action when no field-level ApplicationArea"
        );
    }

    #[test]
    fn t1206_not_offered_on_table_objects() {
        let ws = Workspace::new();

        let al_code = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Name; Text[100])
        {
            ApplicationArea = All;
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyTableAA.al").unwrap();
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
        let aa_actions: Vec<_> = actions
            .iter()
            .filter(|a| {
                a.title.contains("T1206") || a.title.to_lowercase().contains("applicationarea")
            })
            .collect();

        assert!(
            aa_actions.is_empty(),
            "Should NOT offer action on table objects"
        );
    }

    #[test]
    fn t1207_report_layout_conversion_offered_for_rdlclayout() {
        let ws = Workspace::new();

        let al_code = r#"report 50100 "My Report"
{
    RDLCLayout = 'MyReport.rdlc';

    dataset
    {
    }
}
"#;
        let uri = Url::parse("file:///test/MyReport.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Cursor on the RDLCLayout line (line 2)
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
        let layout_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1207") || a.title.to_lowercase().contains("rendering"))
            .collect();

        assert!(
            !layout_actions.is_empty(),
            "Should offer rendering section conversion"
        );

        let edit = layout_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];

        let remove_edits: Vec<_> = edits.iter().filter(|e| e.new_text.is_empty()).collect();
        assert!(
            !remove_edits.is_empty(),
            "Should remove old RDLCLayout property"
        );

        let insert_edits: Vec<_> = edits.iter().filter(|e| !e.new_text.is_empty()).collect();
        assert!(
            !insert_edits.is_empty(),
            "Should insert new rendering section"
        );

        let inserted = &insert_edits[0].new_text;
        assert!(
            inserted.to_lowercase().contains("rendering"),
            "Inserted text should contain 'rendering'"
        );
        assert!(
            inserted.to_lowercase().contains("layout("),
            "Inserted text should contain 'layout('"
        );
        assert!(
            inserted.contains("MyReport.rdlc"),
            "Inserted text should preserve the layout path"
        );
        assert!(
            inserted.to_lowercase().contains("rdlc"),
            "Inserted text should set Type = RDLC"
        );
    }

    #[test]
    fn t1207_report_layout_conversion_offered_for_wordlayout() {
        let ws = Workspace::new();

        let al_code = r#"report 50100 "My Report"
{
    WordLayout = 'MyReport.docx';

    dataset
    {
    }
}
"#;
        let uri = Url::parse("file:///test/MyReportWord.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Cursor on the WordLayout line (line 2)
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
        let layout_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1207") || a.title.to_lowercase().contains("rendering"))
            .collect();

        assert!(
            !layout_actions.is_empty(),
            "Should offer rendering section conversion for WordLayout"
        );

        let edit = layout_actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        let inserted: String = edits.iter().map(|e| e.new_text.as_str()).collect();

        assert!(
            inserted.contains("MyReport.docx"),
            "Should preserve the Word layout path"
        );
        assert!(
            inserted.to_lowercase().contains("word"),
            "Should set Type = Word, got: {:?}",
            inserted
        );
    }

    #[test]
    fn t1207_not_offered_on_non_report_objects() {
        let ws = Workspace::new();

        let al_code = r#"page 50100 "My Page"
{
    RDLCLayout = 'MyPage.rdlc';
}
"#;
        let uri = Url::parse("file:///test/MyPageLayout.al").unwrap();
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
        let layout_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1207") || a.title.to_lowercase().contains("rendering"))
            .collect();

        assert!(
            layout_actions.is_empty(),
            "Should NOT offer rendering conversion on page objects"
        );
    }

    #[test]
    fn t1207_not_offered_when_cursor_far_from_layout_property() {
        let ws = Workspace::new();

        let al_code = r#"report 50100 "My Report"
{
    RDLCLayout = 'MyReport.rdlc';

    dataset
    {
        dataitem(Customer; Customer)
        {
            column(Name; Name)
            {
            }
        }
    }
}
"#;
        let uri = Url::parse("file:///test/MyReportFar.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Cursor far from the RDLCLayout line (line 9, inside dataset)
        let range = Range {
            start: super::super::Position {
                line: 9,
                character: 12,
            },
            end: super::super::Position {
                line: 9,
                character: 12,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let layout_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1207") || a.title.to_lowercase().contains("rendering"))
            .collect();

        assert!(
            layout_actions.is_empty(),
            "Should NOT offer conversion when cursor is far from layout property"
        );
    }

    #[test]
    fn t1207_conversion_does_not_drop_layout_path() {
        let ws = Workspace::new();

        let al_code = r#"report 50100 "My Report"
{
    RDLCLayout = './layouts/MyReport.rdlc';
}
"#;
        let uri = Url::parse("file:///test/MyReportPath.al").unwrap();
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
        let layout_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("T1207") || a.title.to_lowercase().contains("rendering"))
            .collect();

        assert!(!layout_actions.is_empty(), "Should offer conversion");

        let edit = layout_actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        let combined: String = edits.iter().map(|e| e.new_text.as_str()).collect();

        assert!(
            combined.contains("./layouts/MyReport.rdlc"),
            "Must not drop the layout path, got: {:?}",
            combined
        );
    }
}
