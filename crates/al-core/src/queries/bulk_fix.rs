//! Bulk fix operations for AL projects.
//!
//! Implements project-wide fixes:
//! - `add_application_area` — add ApplicationArea to page fields/actions missing it (T1603)
//! - `add_tooltips` — add ToolTip to page fields from symbol data (T1604)
//! - `add_data_classification` — add DataClassification to table fields (T1605)

use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BulkFixResult {
    /// Files that were modified (or would be modified in dry-run).
    pub modified_files: Vec<String>,
    /// Total number of changes applied (or would be applied).
    pub changes_count: usize,
    /// Whether this was a dry-run (no files were written).
    pub dry_run: bool,
}

/// Add `ApplicationArea = <value>;` to all page fields and page action items that
/// are missing it across all AL files under `project_dir`.
///
/// Respects existing `ApplicationArea` properties — only adds where absent.
pub fn add_application_area(
    project_dir: &Path,
    value: &str,
    dry_run: bool,
) -> Result<BulkFixResult, String> {
    let files = collect_al_files(project_dir);
    let mut modified_files = Vec::new();
    let mut total_changes = 0;

    for path in &files {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

        if !is_page_file(&source) {
            continue;
        }

        let (new_source, changes) = inject_application_area(&source, value);
        if changes > 0 {
            total_changes += changes;
            modified_files.push(path.display().to_string());
            if !dry_run {
                std::fs::write(path, &new_source)
                    .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
            }
        }
    }

    Ok(BulkFixResult {
        modified_files,
        changes_count: total_changes,
        dry_run,
    })
}

/// Add `ToolTip = '<value>';` to page fields that reference base-app table fields
/// and are missing a ToolTip property.
///
/// `tooltips` is a map from field name (case-insensitive) to tooltip text. These
/// would typically come from symbol data for the source table.
pub fn add_tooltips(
    project_dir: &Path,
    tooltips: &[(String, String)],
    dry_run: bool,
) -> Result<BulkFixResult, String> {
    let files = collect_al_files(project_dir);
    let mut modified_files = Vec::new();
    let mut total_changes = 0;

    for path in &files {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

        if !is_page_file(&source) {
            continue;
        }

        let (new_source, changes) = inject_tooltips(&source, tooltips);
        if changes > 0 {
            total_changes += changes;
            modified_files.push(path.display().to_string());
            if !dry_run {
                std::fs::write(path, &new_source)
                    .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
            }
        }
    }

    Ok(BulkFixResult {
        modified_files,
        changes_count: total_changes,
        dry_run,
    })
}

/// Add `DataClassification = <value>;` to all table fields missing it across all
/// AL table files under `project_dir`.
///
/// Skips FlowFields and FlowFilters (where DataClassification is not applicable).
pub fn add_data_classification(
    project_dir: &Path,
    value: &str,
    dry_run: bool,
) -> Result<BulkFixResult, String> {
    let files = collect_al_files(project_dir);
    let mut modified_files = Vec::new();
    let mut total_changes = 0;

    for path in &files {
        let source = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

        if !is_table_file(&source) {
            continue;
        }

        let (new_source, changes) = inject_data_classification(&source, value);
        if changes > 0 {
            total_changes += changes;
            modified_files.push(path.display().to_string());
            if !dry_run {
                std::fs::write(path, &new_source)
                    .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
            }
        }
    }

    Ok(BulkFixResult {
        modified_files,
        changes_count: total_changes,
        dry_run,
    })
}

fn collect_al_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    // Iterative directory walk via an explicit stack. Avoids stack overflow on
    // deeply nested directory trees and the (real-world rare but possible)
    // pathological symlink junction cycles, both of which would overflow a
    // recursive walker.
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if !name.starts_with('.') && name != "target" {
                    stack.push(path);
                }
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("al"))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Read the first whitespace-delimited token of `source`'s first non-blank,
/// non-comment line and resolve it via `LanguageData` (T013/T042: was a
/// hardcoded prefix list — silently dropped reportextension, requestpage
/// variations, etc.). Returns the canonical lowercase keyword.
fn detect_object_keyword(source: &str) -> Option<&'static str> {
    let first = source
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("//"))?;
    let token = first.split(|c: char| c.is_whitespace()).next()?;
    crate::syntax::language_data::object_type_by_keyword(token).map(|ot| ot.keyword.as_str())
}

fn is_page_file(source: &str) -> bool {
    matches!(
        detect_object_keyword(source),
        Some("page")
            | Some("pageextension")
            | Some("report")
            | Some("reportextension")
            | Some("requestpage")
            | Some("pagecustomization")
    )
}

fn is_table_file(source: &str) -> bool {
    matches!(
        detect_object_keyword(source),
        Some("table") | Some("tableextension")
    )
}

/// Inject `ApplicationArea = <value>;` after field/action blocks that lack it.
///
/// Strategy: line-by-line state machine. When inside a `field(...)` or `action(...)`
/// block, track brace depth. Before the closing `}`, if no `ApplicationArea` was
/// seen, insert the property.
fn inject_application_area(source: &str, value: &str) -> (String, usize) {
    let lines: Vec<&str> = source.lines().collect();
    let mut output = Vec::with_capacity(lines.len() + 16);
    let mut changes = 0;

    // Stack of (is_field_or_action, has_application_area, indent)
    let mut stack: Vec<(bool, bool, String)> = Vec::new();
    // Track if the previous non-empty line was a bare field/action declaration
    // (without a `{` on the same line), so the next standalone `{` is for it.
    let mut pending_control_start = false;

    for line in &lines {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        let is_control_decl = lower.starts_with("field(") || lower.starts_with("action(");
        let is_control_start = is_control_decl && trimmed.contains('{');

        let brace_open_count = trimmed.chars().filter(|&c| c == '{').count();
        let brace_close_count = trimmed.chars().filter(|&c| c == '}').count();

        // If a standalone `{` appears after a field/action line, treat it as a control start
        let effective_control_start = if !is_control_start
            && pending_control_start
            && brace_open_count > 0
            && brace_close_count == 0
        {
            true
        } else {
            is_control_start
        };

        let has_area = lower.contains("applicationarea");

        if brace_close_count > 0 && !stack.is_empty() {
            let closes = brace_close_count.saturating_sub(brace_open_count);
            for _ in 0..closes {
                if let Some((is_field_action, had_area, indent)) = stack.pop() {
                    if is_field_action && !had_area {
                        output.push(format!("{indent}    ApplicationArea = {value};"));
                        changes += 1;
                    }
                }
            }
        }

        output.push(line.to_string());

        if has_area {
            if let Some(top) = stack.last_mut() {
                top.1 = true;
            }
        }

        if brace_open_count > 0 {
            let opens = brace_open_count.saturating_sub(brace_close_count);
            for _ in 0..opens {
                let indent = leading_whitespace(line);
                stack.push((effective_control_start, false, indent.to_string()));
            }
        }

        if !trimmed.is_empty() {
            pending_control_start = is_control_decl && !trimmed.contains('{');
        }
    }

    let result = if source.ends_with('\n') {
        output.join("\n") + "\n"
    } else {
        output.join("\n")
    };

    (result, changes)
}

fn inject_tooltips(source: &str, tooltips: &[(String, String)]) -> (String, usize) {
    let lines: Vec<&str> = source.lines().collect();
    let mut output = Vec::with_capacity(lines.len() + 16);
    let mut changes = 0;

    struct FieldCtx {
        field_name: String,
        has_tooltip: bool,
        indent: String,
    }

    let mut stack: Vec<FieldCtx> = Vec::new();
    // Name of a field/action that was declared on the previous line without `{`
    let mut pending_field_name: Option<String> = None;

    for line in &lines {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        let brace_open = trimmed.chars().filter(|&c| c == '{').count();
        let brace_close = trimmed.chars().filter(|&c| c == '}').count();

        // Detect `field(<var>; Rec."<Name>")` or `field(<var>; "<Name>")`
        let field_name = if lower.starts_with("field(") {
            extract_field_source_name(trimmed)
        } else {
            None
        };

        // Determine effective field name for block pushes: inline `{` on field line,
        // or a standalone `{` line following a bare field declaration.
        let effective_field_name = if field_name.is_some() && trimmed.contains('{') {
            field_name.clone()
        } else if field_name.is_none()
            && brace_open > 0
            && brace_close == 0
            && pending_field_name.is_some()
        {
            pending_field_name.clone()
        } else {
            None
        };

        if brace_close > 0 && !stack.is_empty() {
            let closes = brace_close.saturating_sub(brace_open);
            for _ in 0..closes {
                if let Some(ctx) = stack.pop() {
                    if !ctx.has_tooltip && !ctx.field_name.is_empty() {
                        if let Some(tooltip) = find_tooltip(&ctx.field_name, tooltips) {
                            let escaped = tooltip.replace('\'', "''");
                            output.push(format!(
                                "{}    ToolTip = 'Specifies {}';",
                                ctx.indent, escaped
                            ));
                            changes += 1;
                        }
                    }
                }
            }
        }

        output.push(line.to_string());

        if lower.contains("tooltip") {
            if let Some(top) = stack.last_mut() {
                top.has_tooltip = true;
            }
        }

        if brace_open > 0 {
            let opens = brace_open.saturating_sub(brace_close);
            for i in 0..opens {
                let is_field_block = i == 0 && effective_field_name.is_some();
                let indent = leading_whitespace(line).to_string();
                stack.push(FieldCtx {
                    field_name: if is_field_block {
                        effective_field_name.clone().unwrap_or_default()
                    } else {
                        String::new()
                    },
                    has_tooltip: false,
                    indent,
                });
            }
        }

        if !trimmed.is_empty() {
            pending_field_name = if lower.starts_with("field(") && !trimmed.contains('{') {
                field_name.clone()
            } else {
                None
            };
        }
    }

    let result = if source.ends_with('\n') {
        output.join("\n") + "\n"
    } else {
        output.join("\n")
    };

    (result, changes)
}

/// Skips fields that have `FieldClass = FlowField` or `FieldClass = FlowFilter`.
fn inject_data_classification(source: &str, value: &str) -> (String, usize) {
    let lines: Vec<&str> = source.lines().collect();
    let mut output = Vec::with_capacity(lines.len() + 16);
    let mut changes = 0;

    struct FieldCtx {
        is_field: bool,
        is_flow: bool,
        has_classification: bool,
        indent: String,
    }

    let mut stack: Vec<FieldCtx> = Vec::new();
    let mut pending_field_block = false;

    for line in &lines {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();

        let brace_open = trimmed.chars().filter(|&c| c == '{').count();
        let brace_close = trimmed.chars().filter(|&c| c == '}').count();

        let is_field_decl = lower.starts_with("field(");
        let is_field_start_inline = is_field_decl && trimmed.contains('{');

        if brace_close > 0 && !stack.is_empty() {
            let closes = brace_close.saturating_sub(brace_open);
            for _ in 0..closes {
                if let Some(ctx) = stack.pop() {
                    if ctx.is_field && !ctx.is_flow && !ctx.has_classification {
                        output.push(format!("{}    DataClassification = {value};", ctx.indent));
                        changes += 1;
                    }
                }
            }
        }

        output.push(line.to_string());

        if lower.contains("dataclassification") {
            if let Some(top) = stack.last_mut() {
                top.has_classification = true;
            }
        }
        if lower.contains("fieldclass")
            && (lower.contains("flowfield") || lower.contains("flowfilter"))
        {
            if let Some(top) = stack.last_mut() {
                top.is_flow = true;
            }
        }

        if brace_open > 0 {
            let opens = brace_open.saturating_sub(brace_close);
            for i in 0..opens {
                // A field block starts either inline or when we see a standalone `{`
                // after a bare field declaration on the previous line.
                let is_field = i == 0
                    && (is_field_start_inline
                        || (!is_field_decl && brace_close == 0 && pending_field_block));
                let indent = leading_whitespace(line).to_string();
                stack.push(FieldCtx {
                    is_field,
                    is_flow: false,
                    has_classification: false,
                    indent,
                });
            }
        }

        if !trimmed.is_empty() {
            pending_field_block = is_field_decl && !trimmed.contains('{');
        }
    }

    let result = if source.ends_with('\n') {
        output.join("\n") + "\n"
    } else {
        output.join("\n")
    };

    (result, changes)
}

fn leading_whitespace(line: &str) -> &str {
    let len = line.len() - line.trim_start().len();
    &line[..len]
}

/// Extract the source field name from `field(varName; Rec."FieldName")` or `field(varName; "FieldName")`.
fn extract_field_source_name(line: &str) -> Option<String> {
    let s = line.trim_start();
    let after_field = s.strip_prefix("field(")?;
    let close_paren = after_field.rfind(')')?;
    let inner = after_field[..close_paren].trim();

    let parts: Vec<&str> = inner.splitn(2, ';').collect();
    if parts.len() < 2 {
        return None;
    }
    let source = parts[1].trim();
    let source = source.strip_prefix("Rec.").unwrap_or(source);
    let source = source.strip_prefix("rec.").unwrap_or(source);
    let source = source.trim_matches('"').trim();
    if source.is_empty() {
        None
    } else {
        Some(source.to_string())
    }
}

fn find_tooltip<'a>(field_name: &str, tooltips: &'a [(String, String)]) -> Option<&'a str> {
    let lower = field_name.to_ascii_lowercase();
    tooltips
        .iter()
        .find(|(name, _)| name.to_ascii_lowercase() == lower)
        .map(|(_, tip)| tip.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_application_area_to_page_fields() {
        let source = r#"page 50100 "Test Page"
{
    layout
    {
        area(Content)
        {
            field(myField; Rec."No.")
            {
                Caption = 'No';
            }
        }
    }
}
"#;
        let (result, changes) = inject_application_area(source, "All");
        assert_eq!(changes, 1);
        assert!(result.contains("ApplicationArea = All;"));
    }

    #[test]
    fn add_application_area_skips_existing() {
        let source = r#"page 50100 "Test Page"
{
    layout
    {
        area(Content)
        {
            field(myField; Rec."No.")
            {
                ApplicationArea = All;
                Caption = 'No';
            }
        }
    }
}
"#;
        let (_result, changes) = inject_application_area(source, "All");
        assert_eq!(changes, 0);
    }

    #[test]
    fn add_data_classification_to_table_fields() {
        let source = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'No.';
        }
    }
}
"#;
        let (result, changes) = inject_data_classification(source, "CustomerContent");
        assert_eq!(changes, 1);
        assert!(result.contains("DataClassification = CustomerContent;"));
    }

    #[test]
    fn add_data_classification_skips_existing() {
        let source = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            DataClassification = CustomerContent;
        }
    }
}
"#;
        let (_result, changes) = inject_data_classification(source, "CustomerContent");
        assert_eq!(changes, 0);
    }

    #[test]
    fn add_data_classification_skips_flow_fields() {
        let source = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "Balance"; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = Sum("Entry"."Amount");
        }
    }
}
"#;
        let (_result, changes) = inject_data_classification(source, "CustomerContent");
        assert_eq!(changes, 0);
    }

    #[test]
    fn is_page_file_detects_page() {
        assert!(is_page_file("page 50100 \"Test\"\n{"));
        assert!(is_page_file(
            "pageextension 50100 extends \"Customer List\"\n{"
        ));
        assert!(!is_page_file("table 50100 \"Test\"\n{"));
        assert!(!is_page_file("codeunit 50100 \"Test\"\n{"));
    }

    #[test]
    fn is_table_file_detects_table() {
        assert!(is_table_file("table 50100 \"Test\"\n{"));
        assert!(is_table_file("tableextension 50100 extends Customer\n{"));
        assert!(!is_table_file("page 50100 \"Test\"\n{"));
    }

    #[test]
    fn extract_field_source_name_parses_rec_prefix() {
        assert_eq!(
            extract_field_source_name("field(no; Rec.\"No.\")"),
            Some("No.".to_string())
        );
        assert_eq!(
            extract_field_source_name("field(name; Rec.\"Name\") {"),
            Some("Name".to_string())
        );
    }

    #[test]
    fn inject_tooltips_adds_missing_tooltip() {
        let source = r#"page 50100 "Test"
{
    layout
    {
        area(Content)
        {
            field(no; Rec."No.")
            {
                ApplicationArea = All;
            }
        }
    }
}
"#;
        let tooltips = vec![("No.".to_string(), "the item number".to_string())];
        let (result, changes) = inject_tooltips(source, &tooltips);
        assert_eq!(changes, 1);
        assert!(result.contains("ToolTip"));
    }

    #[test]
    fn inject_tooltips_skips_existing() {
        let source = r#"page 50100 "Test"
{
    layout
    {
        area(Content)
        {
            field(no; Rec."No.")
            {
                ToolTip = 'My custom tooltip';
            }
        }
    }
}
"#;
        let tooltips = vec![("No.".to_string(), "the item number".to_string())];
        let (_result, changes) = inject_tooltips(source, &tooltips);
        assert_eq!(changes, 0);
    }

    #[test]
    fn add_application_area_dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let page_content = "page 50100 \"Test\"\n{\n    layout\n    {\n        area(Content)\n        {\n            field(f; Rec.\"No.\")\n            {\n            }\n        }\n    }\n}\n";
        let page_path = dir.path().join("Test.Page.al");
        std::fs::write(&page_path, page_content).unwrap();

        let result = add_application_area(dir.path(), "All", true).unwrap();
        assert!(result.dry_run);
        assert!(result.changes_count > 0);
        let content = std::fs::read_to_string(&page_path).unwrap();
        assert!(!content.contains("ApplicationArea"));
    }
}
