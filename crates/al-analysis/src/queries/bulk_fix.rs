//! Bulk fix operations for AL projects.
//!
//! Implements project-wide fixes:
//! - `add_application_area` — add ApplicationArea to page fields/actions missing it
//! - `add_tooltips` — add ToolTip to page fields from symbol data
//! - `add_data_classification` — add DataClassification to table fields

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

#[derive(Debug, Clone)]
pub struct BulkFixChange {
    pub path: PathBuf,
    pub original: String,
    pub updated: String,
    pub changes_count: usize,
}

#[derive(Debug, Clone)]
pub struct BulkFixPlan {
    pub changes: Vec<BulkFixChange>,
}

impl BulkFixPlan {
    #[must_use]
    pub fn result(&self, dry_run: bool) -> BulkFixResult {
        BulkFixResult {
            modified_files: self
                .changes
                .iter()
                .map(|change| change.path.display().to_string())
                .collect(),
            changes_count: self.changes.iter().map(|change| change.changes_count).sum(),
            dry_run,
        }
    }
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
    let plan = plan_application_area(project_dir, value)?;
    if !dry_run {
        apply_plan_to_disk(&plan)?;
    }
    Ok(plan.result(dry_run))
}

pub fn plan_application_area(project_dir: &Path, value: &str) -> Result<BulkFixPlan, String> {
    validate_property_value(value, "ApplicationArea")?;
    build_plan(project_dir, |source, tree, object_kind| {
        if !is_page_kind(object_kind) {
            return Ok((source.to_string(), 0));
        }
        inject_application_area(source, tree, value)
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
    let plan = plan_tooltips(project_dir, tooltips)?;
    if !dry_run {
        apply_plan_to_disk(&plan)?;
    }
    Ok(plan.result(dry_run))
}

pub fn plan_tooltips(
    project_dir: &Path,
    tooltips: &[(String, String)],
) -> Result<BulkFixPlan, String> {
    let mut normalized = std::collections::BTreeMap::new();
    for (field, tooltip) in tooltips {
        let field = field.trim();
        if field.is_empty() {
            return Err("Tooltip source field name must not be empty".to_string());
        }
        if tooltip.trim().is_empty() {
            return Err(format!("Tooltip for field '{field}' must not be empty"));
        }
        let key = field.to_ascii_lowercase();
        if normalized.insert(key, tooltip.clone()).is_some() {
            return Err(format!(
                "Tooltip source contains duplicate field name '{field}'"
            ));
        }
    }
    build_plan(project_dir, |source, tree, object_kind| {
        if !is_page_kind(object_kind) {
            return Ok((source.to_string(), 0));
        }
        inject_tooltips(source, tree, &normalized)
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
    let plan = plan_data_classification(project_dir, value)?;
    if !dry_run {
        apply_plan_to_disk(&plan)?;
    }
    Ok(plan.result(dry_run))
}

pub fn plan_data_classification(project_dir: &Path, value: &str) -> Result<BulkFixPlan, String> {
    validate_property_value(value, "DataClassification")?;
    build_plan(project_dir, |source, tree, object_kind| {
        if !is_table_kind(object_kind) {
            return Ok((source.to_string(), 0));
        }
        inject_data_classification(source, tree, value)
    })
}

/// Collect project AL files without following symlinks out of the project.
///
/// Project-wide mutations must not quietly skip unreadable directories or
/// disagree with the workspace index about which paths belong to the project.
/// Discovery is therefore delegated to the source index's authoritative
/// walker rather than maintaining a second set of path and exclusion rules.
pub fn collect_al_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    al_source::file_index::collect_al_files(dir).map_err(|error| error.to_string())
}

fn is_page_kind(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "page"
            | "pageextension"
            | "report"
            | "reportextension"
            | "requestpage"
            | "pagecustomization"
    )
}

fn is_table_kind(kind: &str) -> bool {
    matches!(
        kind.to_ascii_lowercase().as_str(),
        "table" | "tableextension"
    )
}

fn validate_property_value(value: &str, property: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{property} value must not be empty"));
    }
    if trimmed
        .chars()
        .any(|character| matches!(character, ';' | '{' | '}' | '=' | '\r' | '\n'))
        || trimmed.contains("//")
        || trimmed.contains("/*")
        || trimmed.contains("*/")
    {
        return Err(format!(
            "{property} value contains characters that cannot appear in an AL property value"
        ));
    }
    Ok(())
}

fn build_plan<F>(project_dir: &Path, transform: F) -> Result<BulkFixPlan, String>
where
    F: Fn(&str, &tree_sitter::Tree, &str) -> Result<(String, usize), String>,
{
    let files = collect_al_files(project_dir)?;
    let mut changes = Vec::new();
    for path in files {
        let source = al_source::file_index::read_source_file(&path)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))?
            .ok_or_else(|| format!("Workspace source disappeared: {}", path.display()))?;
        let parsed = al_syntax::AlParser::parse_quick(&source);
        if parsed.tree.root_node().has_error() {
            let details = parsed
                .errors
                .iter()
                .take(3)
                .map(|error| {
                    format!(
                        "{} at {}:{}",
                        error.message,
                        error.range.start_point.row + 1,
                        error.range.start_point.column + 1
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "Bulk fix refused malformed AL source '{}': {}",
                path.display(),
                if details.is_empty() {
                    "tree-sitter reported an error node"
                } else {
                    &details
                }
            ));
        }
        let object =
            al_syntax::find_object_declaration(&parsed.tree, &source).ok_or_else(|| {
                format!(
                    "Bulk fix refused '{}': no complete AL object declaration was found",
                    path.display()
                )
            })?;
        let (updated, changes_count) = transform(&source, &parsed.tree, &object.kind)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if (changes_count == 0) != (updated == source) {
            return Err(format!(
                "Bulk-fix transformation integrity mismatch for '{}'",
                path.display()
            ));
        }
        if changes_count > 0 {
            let verification = al_syntax::AlParser::parse_quick(&updated);
            if verification.tree.root_node().has_error()
                || al_syntax::find_object_declaration(&verification.tree, &updated).is_none()
            {
                return Err(format!(
                    "Bulk fix generated invalid AL for '{}'; no files were changed",
                    path.display()
                ));
            }
            changes.push(BulkFixChange {
                path,
                original: source,
                updated,
                changes_count,
            });
        }
    }
    Ok(BulkFixPlan { changes })
}

fn apply_plan_to_disk(plan: &BulkFixPlan) -> Result<(), String> {
    for change in &plan.changes {
        let current = al_source::file_index::read_source_file(&change.path)
            .map_err(|error| format!("Failed to re-read {}: {error}", change.path.display()))?
            .ok_or_else(|| {
                format!(
                    "Source disappeared before bulk fix: {}",
                    change.path.display()
                )
            })?;
        if current != change.original {
            return Err(format!(
                "Source changed after bulk-fix planning: {}; no files were changed",
                change.path.display()
            ));
        }
    }

    let mut applied: Vec<&BulkFixChange> = Vec::new();
    for change in &plan.changes {
        if let Err(error) = atomic_replace(&change.path, &change.updated) {
            let mut rollback_errors = Vec::new();
            for previous in applied.into_iter().rev() {
                if let Err(rollback_error) = atomic_replace(&previous.path, &previous.original) {
                    rollback_errors.push(format!("{}: {rollback_error}", previous.path.display()));
                }
            }
            let suffix = if rollback_errors.is_empty() {
                String::new()
            } else {
                format!("; rollback also failed: {}", rollback_errors.join("; "))
            };
            return Err(format!(
                "Failed to update {}: {error}{suffix}",
                change.path.display()
            ));
        }
        applied.push(change);
    }
    Ok(())
}

fn atomic_replace(path: &Path, content: &str) -> Result<(), String> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let permissions = std::fs::metadata(path)
        .map_err(|error| format!("inspect {} failed: {error}", path.display()))?
        .permissions();
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "create temporary file beside {} failed: {error}",
            path.display()
        )
    })?;
    temporary
        .as_file()
        .set_permissions(permissions)
        .map_err(|error| {
            format!(
                "set temporary permissions for {} failed: {error}",
                path.display()
            )
        })?;
    temporary.write_all(content.as_bytes()).map_err(|error| {
        format!(
            "write temporary file for {} failed: {error}",
            path.display()
        )
    })?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("sync temporary file for {} failed: {error}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| format!("replace {} failed: {}", path.display(), error.error))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("sync directory {} failed: {error}", parent.display()))?;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct AstObjectSection {
    pub header_segments: Vec<String>,
    pub properties: std::collections::BTreeMap<String, String>,
    pub line: u32,
    close_byte: usize,
    section_indent: String,
    child_indent_suffix: String,
}

pub(crate) fn collect_ast_sections(
    tree: &tree_sitter::Tree,
    source: &str,
    target_keywords: &[&str],
) -> Result<Vec<AstObjectSection>, String> {
    if tree.root_node().has_error() {
        return Err("cannot inspect sections in malformed AL source".to_string());
    }
    let mut sections = Vec::new();
    collect_ast_sections_from_node(tree.root_node(), source, target_keywords, &mut sections)?;
    Ok(sections)
}

fn collect_ast_sections_from_node(
    node: tree_sitter::Node<'_>,
    source: &str,
    target_keywords: &[&str],
    sections: &mut Vec<AstObjectSection>,
) -> Result<(), String> {
    if node.kind() == "object_section" {
        let keyword_node = node
            .child_by_field_name("keyword")
            .ok_or_else(|| "object section has no keyword node".to_string())?;
        let keyword = keyword_node
            .utf8_text(source.as_bytes())
            .map_err(|error| format!("object-section keyword is not UTF-8: {error}"))?
            .trim()
            .to_ascii_lowercase();
        if target_keywords
            .iter()
            .any(|target| keyword.eq_ignore_ascii_case(target))
        {
            sections.push(parse_ast_section(node, source, keyword)?);
        }
    }

    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_ast_sections_from_node(child, source, target_keywords, sections)?;
    }
    Ok(())
}

fn parse_ast_section(
    node: tree_sitter::Node<'_>,
    source: &str,
    keyword: String,
) -> Result<AstObjectSection, String> {
    let body = node
        .child_by_field_name("body")
        .ok_or_else(|| format!("{keyword} section has no body"))?;
    let close = (0..body.child_count())
        .rev()
        .filter_map(|index| body.child(index))
        .find(|child| child.kind() == "}")
        .ok_or_else(|| format!("{keyword} section body has no closing brace"))?;
    let close_byte = close.start_byte();
    let section_indent = line_indentation(source, node.start_byte())?;
    let closing_indent = line_indentation(source, close_byte)?;
    let mut child_indent_suffix = None;
    let mut properties = std::collections::BTreeMap::new();
    let mut body_cursor = body.walk();
    for child in body.named_children(&mut body_cursor) {
        let child_indent = line_indentation(source, child.start_byte())?;
        if child_indent.starts_with(&closing_indent) && child_indent.len() > closing_indent.len() {
            child_indent_suffix
                .get_or_insert_with(|| child_indent[closing_indent.len()..].to_string());
        }
        if child.kind() != "property_assignment" {
            continue;
        }
        let name_node = child
            .child_by_field_name("name")
            .ok_or_else(|| format!("{keyword} property has no name node"))?;
        let name = name_node
            .utf8_text(source.as_bytes())
            .map_err(|error| format!("{keyword} property name is not UTF-8: {error}"))?
            .trim()
            .to_string();
        let value = child
            .child_by_field_name("value")
            .map(|value| {
                value
                    .utf8_text(source.as_bytes())
                    .map(str::trim)
                    .map(str::to_string)
                    .map_err(|error| format!("{keyword} property value is not UTF-8: {error}"))
            })
            .transpose()?
            .unwrap_or_default();
        let normalized = name.to_ascii_lowercase();
        if properties.insert(normalized, value).is_some() {
            return Err(format!(
                "{keyword} section contains duplicate property '{name}'"
            ));
        }
    }

    let mut node_cursor = node.walk();
    let header_segments = node
        .named_children(&mut node_cursor)
        .find(|child| child.kind() == "parenthesized_block")
        .map(|header| split_header_segments(header, source))
        .transpose()?
        .unwrap_or_default();
    Ok(AstObjectSection {
        header_segments,
        properties,
        line: u32::try_from(node.start_position().row + 1)
            .map_err(|_| "section line exceeds u32".to_string())?,
        close_byte,
        section_indent,
        child_indent_suffix: child_indent_suffix.unwrap_or_else(|| "    ".to_string()),
    })
}

fn split_header_segments(
    header: tree_sitter::Node<'_>,
    source: &str,
) -> Result<Vec<String>, String> {
    let open = header
        .child(0)
        .filter(|node| node.kind() == "(")
        .ok_or_else(|| "section header has no opening parenthesis".to_string())?;
    let close = (0..header.child_count())
        .rev()
        .filter_map(|index| header.child(index))
        .find(|node| node.kind() == ")")
        .ok_or_else(|| "section header has no closing parenthesis".to_string())?;
    let mut start = open.end_byte();
    let mut segments = Vec::new();
    for index in 0..header.child_count() {
        let child = header
            .child(index)
            .ok_or_else(|| "section header child disappeared".to_string())?;
        if child.kind() == "semicolon" {
            segments.push(
                source
                    .get(start..child.start_byte())
                    .ok_or_else(|| "section header byte range is invalid".to_string())?
                    .trim()
                    .to_string(),
            );
            start = child.end_byte();
        }
    }
    segments.push(
        source
            .get(start..close.start_byte())
            .ok_or_else(|| "section header byte range is invalid".to_string())?
            .trim()
            .to_string(),
    );
    Ok(segments)
}

fn line_indentation(source: &str, byte: usize) -> Result<String, String> {
    if byte > source.len() || !source.is_char_boundary(byte) {
        return Err("AST byte offset is outside the UTF-8 source".to_string());
    }
    let line_start = source[..byte].rfind('\n').map_or(0, |index| index + 1);
    let prefix = &source[line_start..byte];
    Ok(prefix
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .collect())
}

#[derive(Debug)]
struct Insertion {
    offset: usize,
    text: String,
}

fn property_insertion(
    source: &str,
    section: &AstObjectSection,
    property: &str,
) -> Result<Insertion, String> {
    let line_start = source[..section.close_byte]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let before_close = source
        .get(line_start..section.close_byte)
        .ok_or_else(|| "section closing-brace byte range is invalid".to_string())?;
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let text = if before_close.trim().is_empty() {
        format!(
            "{}{property}{newline}{}",
            section.child_indent_suffix, before_close
        )
    } else {
        format!(
            "{newline}{}{}{property}{newline}{}",
            section.section_indent, section.child_indent_suffix, section.section_indent
        )
    };
    Ok(Insertion {
        offset: section.close_byte,
        text,
    })
}

fn apply_insertions(source: &str, mut insertions: Vec<Insertion>) -> Result<String, String> {
    insertions.sort_by_key(|insertion| std::cmp::Reverse(insertion.offset));
    for pair in insertions.windows(2) {
        if pair[0].offset == pair[1].offset {
            return Err("multiple properties target the same section insertion point".to_string());
        }
    }
    let mut output = source.to_string();
    for insertion in insertions {
        if insertion.offset > output.len() || !output.is_char_boundary(insertion.offset) {
            return Err("property insertion byte offset is invalid".to_string());
        }
        output.insert_str(insertion.offset, &insertion.text);
    }
    Ok(output)
}

fn normalize_property_atom(value: &str) -> String {
    value
        .trim()
        .trim_matches(['\'', '"'])
        .trim()
        .to_ascii_lowercase()
}

fn normalize_tooltip_text(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else {
        value.to_string()
    }
}

fn parse_record_field_source(expression: &str) -> Option<String> {
    let (receiver, field) = expression.split_once('.')?;
    if !receiver.trim().eq_ignore_ascii_case("rec") {
        return None;
    }
    parse_identifier(field)
}

pub(crate) fn parse_identifier(source: &str) -> Option<String> {
    let source = source.trim();
    if source.len() >= 2 && source.starts_with('"') && source.ends_with('"') {
        let inner = &source[1..source.len() - 1];
        if inner.is_empty() {
            None
        } else {
            Some(inner.replace("\"\"", "\""))
        }
    } else if !source.is_empty()
        && source
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        Some(source.to_string())
    } else {
        None
    }
}

fn inject_application_area(
    source: &str,
    tree: &tree_sitter::Tree,
    value: &str,
) -> Result<(String, usize), String> {
    let sections = collect_ast_sections(tree, source, &["field", "action"])?;
    let mut insertions = Vec::new();
    for section in sections {
        if !section.properties.contains_key("applicationarea") {
            insertions.push(property_insertion(
                source,
                &section,
                &format!("ApplicationArea = {value};"),
            )?);
        }
    }
    let count = insertions.len();
    Ok((apply_insertions(source, insertions)?, count))
}

fn inject_tooltips(
    source: &str,
    tree: &tree_sitter::Tree,
    tooltips: &std::collections::BTreeMap<String, String>,
) -> Result<(String, usize), String> {
    let sections = collect_ast_sections(tree, source, &["field"])?;
    let mut insertions = Vec::new();
    for section in sections {
        if section.properties.contains_key("tooltip") {
            continue;
        }
        let source_expression = section
            .header_segments
            .get(1)
            .ok_or_else(|| format!("field on line {} has no source expression", section.line))?;
        let Some(field_name) = parse_record_field_source(source_expression) else {
            continue;
        };
        let Some(tooltip) = tooltips.get(&field_name.to_ascii_lowercase()) else {
            continue;
        };
        let escaped = normalize_tooltip_text(tooltip).replace('\'', "''");
        insertions.push(property_insertion(
            source,
            &section,
            &format!("ToolTip = 'Specifies {escaped}';"),
        )?);
    }
    let count = insertions.len();
    Ok((apply_insertions(source, insertions)?, count))
}

fn inject_data_classification(
    source: &str,
    tree: &tree_sitter::Tree,
    value: &str,
) -> Result<(String, usize), String> {
    let sections = collect_ast_sections(tree, source, &["field"])?;
    let mut insertions = Vec::new();
    for section in sections {
        if section.properties.contains_key("dataclassification") {
            continue;
        }
        if section
            .properties
            .get("fieldclass")
            .is_some_and(|field_class| {
                matches!(
                    normalize_property_atom(field_class).as_str(),
                    "flowfield" | "flowfilter"
                )
            })
        {
            continue;
        }
        insertions.push(property_insertion(
            source,
            &section,
            &format!("DataClassification = {value};"),
        )?);
    }
    let count = insertions.len();
    Ok((apply_insertions(source, insertions)?, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &str) -> tree_sitter::Tree {
        let parsed = al_syntax::AlParser::parse_quick(source);
        assert!(
            !parsed.tree.root_node().has_error(),
            "fixture must parse: {:?}",
            parsed.errors
        );
        parsed.tree
    }

    fn tooltip_map(field: &str, value: &str) -> std::collections::BTreeMap<String, String> {
        [(field.to_ascii_lowercase(), value.to_string())]
            .into_iter()
            .collect()
    }

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
        let (result, changes) = inject_application_area(source, &parse(source), "All").unwrap();
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
        let (_result, changes) = inject_application_area(source, &parse(source), "All").unwrap();
        assert_eq!(changes, 0);
    }

    #[test]
    fn application_area_uses_only_the_target_sections_direct_properties() {
        let source = r#"page 50100 "Test Page"
{
    layout
    {
        area(Content)
        {
            group(General)
            {
                ApplicationArea = Basic;
                field(myField; Rec."No.")
                {
                    Caption = 'ApplicationArea = All; { not a brace }';
                    // ApplicationArea = Suite;
                    trigger OnValidate()
                    begin
                        Message('ApplicationArea');
                    end;
                }
            }
        }
    }
}
"#;
        let (result, changes) = inject_application_area(source, &parse(source), "All").unwrap();
        assert_eq!(changes, 1);
        let result_tree = parse(&result);
        let fields = collect_ast_sections(&result_tree, &result, &["field"]).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(
            fields[0].properties.get("applicationarea"),
            Some(&"All".to_string())
        );
    }

    #[test]
    fn application_area_handles_inline_sections() {
        let source = r#"page 50100 "Test Page"
{
    layout
    {
        area(Content)
        {
            field(myField; Rec."No.") { Caption = 'No.'; }
        }
    }
}
"#;
        let (result, changes) = inject_application_area(source, &parse(source), "All").unwrap();
        assert_eq!(changes, 1);
        assert!(result.contains("ApplicationArea = All;"));
        assert!(!parse(&result).root_node().has_error());
    }

    #[test]
    fn application_area_preserves_crlf_line_endings() {
        let source = "page 50100 \"Test Page\"\r\n{\r\n    layout\r\n    {\r\n        area(Content)\r\n        {\r\n            field(myField; Rec.\"No.\")\r\n            {\r\n            }\r\n        }\r\n    }\r\n}\r\n";
        let (result, changes) = inject_application_area(source, &parse(source), "All").unwrap();
        assert_eq!(changes, 1);
        assert!(
            !result.replace("\r\n", "").contains('\n'),
            "insertion introduced a bare LF"
        );
        assert!(!parse(&result).root_node().has_error());
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
        let (result, changes) =
            inject_data_classification(source, &parse(source), "CustomerContent").unwrap();
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
        let (_result, changes) =
            inject_data_classification(source, &parse(source), "CustomerContent").unwrap();
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
        let (_result, changes) =
            inject_data_classification(source, &parse(source), "CustomerContent").unwrap();
        assert_eq!(changes, 0);
    }

    #[test]
    fn data_classification_ignores_property_text_in_comments_and_strings() {
        let source = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            Caption = 'DataClassification = SystemMetadata; { literal }';
            // DataClassification = CustomerContent;
        }
    }
}
"#;
        let (result, changes) =
            inject_data_classification(source, &parse(source), "CustomerContent").unwrap();
        assert_eq!(changes, 1);
        assert_eq!(
            result
                .matches("DataClassification = CustomerContent;")
                .count(),
            2,
            "the comment plus one inserted property should remain"
        );
        assert!(!parse(&result).root_node().has_error());
    }

    #[test]
    fn object_kind_filters_are_exact() {
        assert!(is_page_kind("page"));
        assert!(is_page_kind("PageExtension"));
        assert!(!is_page_kind("table"));
        assert!(is_table_kind("table"));
        assert!(is_table_kind("TableExtension"));
        assert!(!is_table_kind("page"));
    }

    #[test]
    fn record_field_source_requires_a_simple_rec_member() {
        assert_eq!(
            parse_record_field_source("Rec.\"No.\""),
            Some("No.".to_string())
        );
        assert_eq!(
            parse_record_field_source("rec.Name"),
            Some("Name".to_string())
        );
        assert_eq!(parse_record_field_source("SomeVariable"), None);
        assert_eq!(parse_record_field_source("Other.Name"), None);
        assert_eq!(parse_record_field_source("Rec.Name.ToString()"), None);
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
        let tooltips = tooltip_map("No.", "the item number");
        let (result, changes) = inject_tooltips(source, &parse(source), &tooltips).unwrap();
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
        let tooltips = tooltip_map("No.", "the item number");
        let (_result, changes) = inject_tooltips(source, &parse(source), &tooltips).unwrap();
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

    #[test]
    fn malformed_file_blocks_the_entire_bulk_fix_before_writes() {
        let dir = tempfile::tempdir().unwrap();
        let good = "page 50100 \"Good\"\n{\n    layout\n    {\n        area(Content)\n        {\n            field(f; Rec.\"No.\") { }\n        }\n    }\n}\n";
        let good_path = dir.path().join("A.Good.al");
        std::fs::write(&good_path, good).unwrap();
        std::fs::write(
            dir.path().join("Z.Broken.al"),
            "page 50101 Broken { layout { area(Content) { field(",
        )
        .unwrap();

        let error = add_application_area(dir.path(), "All", false).unwrap_err();
        assert!(error.contains("malformed AL source"), "{error}");
        assert_eq!(std::fs::read_to_string(good_path).unwrap(), good);
    }

    #[test]
    fn non_dry_run_writes_a_complete_parseable_plan() {
        let dir = tempfile::tempdir().unwrap();
        let first_path = dir.path().join("First.al");
        let second_path = dir.path().join("Second.al");
        std::fs::write(
            &first_path,
            "page 50100 First { layout { area(Content) { field(a; Rec.A) { } } } }\n",
        )
        .unwrap();
        std::fs::write(
            &second_path,
            "page 50101 Second { layout { area(Content) { field(b; Rec.B) { } } } }\n",
        )
        .unwrap();

        let result = add_application_area(dir.path(), "All", false).unwrap();
        assert!(!result.dry_run);
        assert_eq!(result.changes_count, 2);
        assert_eq!(result.modified_files.len(), 2);
        for path in [&first_path, &second_path] {
            let updated = std::fs::read_to_string(path).unwrap();
            assert!(updated.contains("ApplicationArea = All;"));
            assert!(!parse(&updated).root_node().has_error());
        }
    }

    #[test]
    fn duplicate_direct_properties_are_rejected() {
        let source = r#"page 50100 Duplicate
{
    layout
    {
        area(Content)
        {
            field(a; Rec.A)
            {
                ApplicationArea = All;
                ApplicationArea = Basic;
            }
        }
    }
}
"#;
        let error = inject_application_area(source, &parse(source), "All").unwrap_err();
        assert!(
            error.contains("duplicate property 'ApplicationArea'"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_walk_does_not_follow_directory_symlinks() {
        let project = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("Inside.al"),
            "codeunit 50100 Inside { }\n",
        )
        .unwrap();
        std::fs::write(
            external.path().join("Outside.al"),
            "codeunit 50101 Outside { }\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(external.path(), project.path().join("linked")).unwrap();

        let files = collect_al_files(project.path()).unwrap();
        let expected = project.path().join("Inside.al");
        let outside = external.path().join("Outside.al");
        assert_eq!(files, vec![expected]);
        assert!(!files.contains(&outside));
    }
}
