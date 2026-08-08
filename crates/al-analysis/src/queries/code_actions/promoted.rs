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

    // Promoted actions are valid only on pages and page extensions.
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
                let Some((property, _)) = lt.split_once('=') else {
                    continue;
                };
                match property.trim() {
                    "promoted" => {
                        if extract_property_value(lines[il]).eq_ignore_ascii_case("true") {
                            has_promoted = true;
                            remove_lines.push(il);
                        }
                    }
                    "promotedcategory" => {
                        category = Some(extract_property_value(lines[il]));
                        remove_lines.push(il);
                    }
                    // These legacy companions have no per-action equivalent in
                    // the `area(Promoted)` syntax. Leaving them behind (as this
                    // used to) orphans properties on an action that is no
                    // longer promoted.
                    "promotedonly" | "promotedisbig" | "promotedbydefault" => {
                        remove_lines.push(il);
                    }
                    _ => {}
                }
            }
            if !has_promoted {
                // Only `PromotedOnly`/`PromotedIsBig` without `Promoted = true`
                // is not a promoted action; keep those lines untouched.
                remove_lines.clear();
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

/// A `{ … }` block located by its header line.
struct BlockSpan {
    header: usize,
    /// 0-based line holding the block's closing `}`.
    close: usize,
}

fn indent_of(line: &str) -> String {
    line[..line.len() - line.trim_start().len()].to_string()
}

fn find_block_in(
    lines: &[&str],
    from: usize,
    to: usize,
    predicate: impl Fn(&str) -> bool,
) -> Option<BlockSpan> {
    for i in from..to.min(lines.len()) {
        let normalized = lines[i].trim().to_lowercase().replace(' ', "");
        if predicate(&normalized) {
            let (close, _) = find_block_extent(lines, i);
            return Some(BlockSpan { header: i, close });
        }
    }
    None
}

fn build_promoted_action_conversion(
    uri: &Url,
    text: &str,
    pa: &PromotedActionInfo,
) -> Option<CodeActionEntry> {
    let lines: Vec<&str> = text.lines().collect();

    // Every promoted action lives inside an `actions { }` block; without one
    // there is nowhere valid to put the `area(Promoted)` section, so refuse
    // rather than appending stray text at end of file.
    let actions_block = find_block_in(&lines, 0, lines.len(), |t| {
        t == "actions" || t.starts_with("actions{")
    })?;

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

    let ref_name = super::quote_al_identifier(&format!("{}_Promoted", pa.name));
    let target_name = super::quote_al_identifier(&pa.name);

    // Reuse an existing `area(Promoted)` (and, for a categorised action, its
    // category group) instead of emitting a fresh one on every conversion —
    // repeated use used to stack duplicate `area(Promoted)` blocks.
    let promoted_area = find_block_in(&lines, actions_block.header, actions_block.close + 1, |t| {
        t.starts_with("area(promoted)")
    });

    let category_group_name = pa
        .category
        .as_deref()
        .map(|category| format!("Category_{}", category));
    let category_group = match (&promoted_area, &category_group_name) {
        (Some(area), Some(group_name)) => {
            let needle = format!("group({})", group_name.to_lowercase());
            find_block_in(&lines, area.header, area.close + 1, |t| {
                t.starts_with(&needle)
            })
        }
        _ => None,
    };

    let (insert_line, new_text) = if let Some(group) = category_group {
        let indent = format!("{}    ", indent_of(lines[group.header]));
        (
            group.close as u32,
            format!("{indent}actionref({ref_name}; {target_name})\n{indent}{{\n{indent}}}\n"),
        )
    } else if let Some(area) = &promoted_area {
        let indent = format!("{}    ", indent_of(lines[area.header]));
        (
            area.close as u32,
            render_promoted_body(
                &indent,
                category_group_name.as_deref(),
                pa.category.as_deref(),
                &ref_name,
                &target_name,
            ),
        )
    } else {
        let area_indent = format!("{}    ", indent_of(lines[actions_block.header]));
        let inner_indent = format!("{area_indent}    ");
        let body = render_promoted_body(
            &inner_indent,
            category_group_name.as_deref(),
            pa.category.as_deref(),
            &ref_name,
            &target_name,
        );
        (
            actions_block.close as u32,
            format!("{area_indent}area(Promoted)\n{area_indent}{{\n{body}{area_indent}}}\n"),
        )
    };

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
        new_text,
    });

    edits.sort_by_key(|e| e.range.start.line);

    Some(CodeActionEntry {
        title: format!("Convert '{}' to actionRef", pa.name),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, edits)),
        is_preferred: false,
    })
}

/// Render the `actionref` (wrapped in its category group when the legacy
/// action declared a `PromotedCategory`).
///
/// `PromotedCategory = Process` maps to a `group(Category_Process)` inside
/// `area(Promoted)` — it is *not* the action's caption, which is what the
/// previous implementation emitted.
fn render_promoted_body(
    indent: &str,
    category_group_name: Option<&str>,
    category: Option<&str>,
    ref_name: &str,
    target_name: &str,
) -> String {
    match (category_group_name, category) {
        (Some(group_name), Some(category)) => {
            let inner = format!("{indent}    ");
            format!(
                "{indent}group({group_name})\n{indent}{{\n\
                 {inner}Caption = '{caption}';\n\
                 {inner}actionref({ref_name}; {target_name})\n{inner}{{\n{inner}}}\n\
                 {indent}}}\n",
                caption = category.replace('\'', "''")
            )
        }
        _ => format!("{indent}actionref({ref_name}; {target_name})\n{indent}{{\n{indent}}}\n"),
    }
}

/// Offer to add an object-level `ApplicationArea` property to a page or report
/// and remove redundant field-level properties that duplicate the object default.
///
/// The action is only offered when:
///   - The object is a `page`, `pageextension`, or `report`.
///   - The object does NOT yet have a top-level `ApplicationArea` property.
///   - At least one field/column control has `ApplicationArea = All`.
///
/// **Hardcoded AL property names:** `ApplicationArea` /
/// `PromotedCategory` / `Promoted` / `tooltip` appear as string literals
/// throughout the code-actions emitter because they are *generated output* —
/// AL source the user can run through alc. Emitters must know the specific AL
/// syntax they produce; routing output tokens through `LanguageData` would be
/// circular because it reads property names from the grammar at runtime.
pub(super) fn source_action_set_application_area(
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let cursor_line = range.start.line as usize;
    let _ = cursor_line; // offered throughout the file

    // Action categories are valid only on page/report objects and extensions.
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
        title: "Set ApplicationArea = All on object and remove redundant field overrides"
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
        title: "Convert legacy layout properties to rendering section".to_string(),
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
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
    }

    #[test]
    fn promoted_action_conversion_offered_on_promoted_line() {
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
            .filter(|a| a.title.contains("actionRef") || a.title.contains("actionref"))
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
    fn promoted_action_preserves_category() {
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
            .filter(|a| a.title.to_lowercase().contains("actionref"))
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
    fn promoted_action_not_offered_on_non_page_objects() {
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
            .filter(|a| a.title.to_lowercase().contains("actionref"))
            .collect();

        assert!(
            conv_actions.is_empty(),
            "Should NOT offer promoted action conversion on table"
        );
    }

    #[test]
    fn promoted_action_not_offered_without_promoted_property() {
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
            .filter(|a| a.title.to_lowercase().contains("actionref"))
            .collect();

        assert!(
            conv_actions.is_empty(),
            "Should NOT offer conversion when no Promoted = true"
        );
    }

    #[test]
    fn application_area_action_offered_on_page_with_field_override() {
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
            .filter(|a| a.title.to_lowercase().contains("applicationarea"))
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
    fn application_area_action_not_offered_when_object_already_has_property() {
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
            .filter(|a| a.title.to_lowercase().contains("applicationarea"))
            .collect();

        assert!(
            aa_actions.is_empty(),
            "Should NOT offer action when object already has ApplicationArea"
        );
    }

    #[test]
    fn application_area_action_not_offered_without_field_override() {
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
            .filter(|a| a.title.to_lowercase().contains("applicationarea"))
            .collect();

        assert!(
            aa_actions.is_empty(),
            "Should NOT offer action when no field-level ApplicationArea"
        );
    }

    #[test]
    fn application_area_action_not_offered_on_table_objects() {
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
            .filter(|a| a.title.to_lowercase().contains("applicationarea"))
            .collect();

        assert!(
            aa_actions.is_empty(),
            "Should NOT offer action on table objects"
        );
    }

    #[test]
    fn report_layout_conversion_offered_for_rdlc_layout() {
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
            .filter(|a| a.title.to_lowercase().contains("rendering"))
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
    fn report_layout_conversion_offered_for_word_layout() {
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
            .filter(|a| a.title.to_lowercase().contains("rendering"))
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
    fn report_layout_conversion_not_offered_on_non_report_objects() {
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
            .filter(|a| a.title.to_lowercase().contains("rendering"))
            .collect();

        assert!(
            layout_actions.is_empty(),
            "Should NOT offer rendering conversion on page objects"
        );
    }

    #[test]
    fn report_layout_conversion_not_offered_away_from_layout_property() {
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
            .filter(|a| a.title.to_lowercase().contains("rendering"))
            .collect();

        assert!(
            layout_actions.is_empty(),
            "Should NOT offer conversion when cursor is far from layout property"
        );
    }

    #[test]
    fn report_layout_conversion_preserves_layout_path() {
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
            .filter(|a| a.title.to_lowercase().contains("rendering"))
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

    fn promoted_cursor(line: u32) -> Range {
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

    fn convert_promoted(al_code: &str, uri_str: &str, line: u32) -> (CodeActionEntry, String) {
        let ws = Workspace::new();
        let uri = Url::parse(uri_str).unwrap();
        open_doc(&ws, &uri, al_code);
        let actions = source_action_convert_promoted_actions(&uri, al_code, promoted_cursor(line));
        let action = actions
            .into_iter()
            .next()
            .expect("promoted conversion should be offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "promoted conversion",
        );
        (action, updated)
    }

    /// `actionref(My Action_Promoted; My Action)` is not valid AL.
    #[test]
    fn promoted_conversion_quotes_action_names_that_need_quoting() {
        let al_code = r#"page 50100 "My Page"
{
    actions
    {
        area(processing)
        {
            action("My Action")
            {
                Caption = 'Do Something';
                ApplicationArea = All;
                Promoted = true;
            }
        }
    }
}
"#;
        let (_, updated) = convert_promoted(al_code, "file:///test/PromotedQuoted.al", 9);
        assert!(
            updated.contains("actionref(\"My Action_Promoted\"; \"My Action\")"),
            "quoted action names must stay quoted: {updated}"
        );
    }

    /// `PromotedCategory` maps to a `group(Category_X)` inside `area(Promoted)`,
    /// not to the actionref's `Caption`.
    #[test]
    fn promoted_conversion_maps_category_to_a_group_not_a_caption() {
        let al_code = r#"page 50100 "My Page"
{
    actions
    {
        area(processing)
        {
            action(PostAction)
            {
                Caption = 'Post';
                ApplicationArea = All;
                Promoted = true;
                PromotedCategory = Process;
            }
        }
    }
}
"#;
        let (action, updated) = convert_promoted(al_code, "file:///test/PromotedCat.al", 10);
        let generated: String = action.edit.as_ref().unwrap().changes[0]
            .1
            .iter()
            .map(|e| e.new_text.as_str())
            .collect();
        assert!(
            generated.contains("group(Category_Process)"),
            "category must become a group: {generated}"
        );
        assert!(
            !generated.contains("Caption = 'Process';\n")
                || generated.contains("group(Category_Process)"),
            "category caption belongs to the group: {generated}"
        );
        assert!(updated.contains("area(Promoted)"), "{updated}");
        assert!(
            !updated.contains("PromotedCategory"),
            "legacy property must be removed: {updated}"
        );
    }

    /// `PromotedOnly` / `PromotedIsBig` have no equivalent and used to be left
    /// behind on an action that is no longer promoted.
    #[test]
    fn promoted_conversion_removes_orphaned_companion_properties() {
        let al_code = r#"page 50100 "My Page"
{
    actions
    {
        area(processing)
        {
            action(MyAction)
            {
                ApplicationArea = All;
                Promoted = true;
                PromotedOnly = true;
                PromotedIsBig = true;
            }
        }
    }
}
"#;
        let (_, updated) = convert_promoted(al_code, "file:///test/PromotedOnly.al", 9);
        assert!(!updated.contains("PromotedOnly"), "{updated}");
        assert!(!updated.contains("PromotedIsBig"), "{updated}");
        assert!(!updated.contains("Promoted = true"), "{updated}");
    }

    /// Converting a second action must reuse the `area(Promoted)` the first one
    /// created instead of stacking a duplicate.
    #[test]
    fn promoted_conversion_reuses_an_existing_promoted_area() {
        let al_code = r#"page 50100 "My Page"
{
    actions
    {
        area(processing)
        {
            action(Second)
            {
                ApplicationArea = All;
                Promoted = true;
            }
        }
        area(Promoted)
        {
            actionref(First_Promoted; First)
            {
            }
        }
    }
}
"#;
        let (_, updated) = convert_promoted(al_code, "file:///test/PromotedReuse.al", 9);
        assert_eq!(
            updated.matches("area(Promoted)").count(),
            1,
            "must not add a second area(Promoted): {updated}"
        );
        assert!(
            updated.contains("actionref(Second_Promoted; Second)"),
            "{updated}"
        );
    }

    /// Without an `actions { }` block there is nowhere valid to put the area.
    #[test]
    fn promoted_conversion_not_offered_without_an_actions_block() {
        let al_code = r#"page 50100 "My Page"
{
    layout
    {
        area(Content)
        {
            field(Name; Rec.Name)
            {
                ApplicationArea = All;
                Promoted = true;
            }
        }
    }
}
"#;
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/PromotedNoActions.al").unwrap();
        open_doc(&ws, &uri, al_code);
        assert!(
            source_action_convert_promoted_actions(&uri, al_code, promoted_cursor(9)).is_empty()
        );
    }
}
