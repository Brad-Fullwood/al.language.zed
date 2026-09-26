//! Bulk fix planning for AL projects.
//!
//! Each `plan_*` function reads the project and returns a [`BulkFixPlan`]: the
//! original and updated text of every file it would change. Applying the plan
//! belongs to the caller, because the only caller that writes (the daemon's
//! `fix.*` methods) must go through the workspace refresh protocol so the
//! document store, the file index and the generation counter stay in step.
//!
//! - `plan_application_area` — add ApplicationArea to page fields/actions missing it
//! - `plan_tooltips` — add ToolTip to page fields from symbol data
//! - `plan_data_classification` — add DataClassification to table fields

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Why a bulk fix could not be planned.
#[derive(Debug, thiserror::Error)]
pub enum BulkFixError {
    /// The caller's value or tooltip list cannot be written into AL.
    #[error("{0}")]
    InvalidInput(String),
    /// The project could not be read: a scan limit or a disk fault, which
    /// the caller reports differently (the user can narrow one of them).
    #[error(transparent)]
    Scan(#[from] al_source::file_index::ScanError),
    /// The project's source cannot be changed safely: malformed AL, a file
    /// with no object, or a transformation that would produce invalid AL.
    #[error("{0}")]
    Refused(String),
}

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

pub fn plan_application_area(project_dir: &Path, value: &str) -> Result<BulkFixPlan, BulkFixError> {
    validate_property_value(value, "ApplicationArea")?;
    build_plan(project_dir, |source, tree, object_kind| {
        if !is_page_kind(object_kind) {
            return Ok((source.to_string(), 0));
        }
        inject_application_area(source, tree, value)
    })
}

pub fn plan_tooltips(
    project_dir: &Path,
    tooltips: &[(String, String)],
) -> Result<BulkFixPlan, BulkFixError> {
    let mut normalized = std::collections::BTreeMap::new();
    for (field, tooltip) in tooltips {
        let field = field.trim();
        if field.is_empty() {
            return Err(BulkFixError::InvalidInput(
                "Tooltip source field name must not be empty".to_string(),
            ));
        }
        if tooltip.trim().is_empty() {
            return Err(BulkFixError::InvalidInput(format!(
                "Tooltip for field '{field}' must not be empty"
            )));
        }
        let key = field.to_ascii_lowercase();
        if normalized.insert(key, tooltip.clone()).is_some() {
            return Err(BulkFixError::InvalidInput(format!(
                "Tooltip source contains duplicate field name '{field}'"
            )));
        }
    }
    build_plan(project_dir, |source, tree, object_kind| {
        if !is_page_kind(object_kind) {
            return Ok((source.to_string(), 0));
        }
        inject_tooltips(source, tree, &normalized)
    })
}

pub fn plan_data_classification(
    project_dir: &Path,
    value: &str,
) -> Result<BulkFixPlan, BulkFixError> {
    validate_property_value(value, "DataClassification")?;
    build_plan(project_dir, |source, tree, object_kind| {
        if !is_table_kind(object_kind) {
            return Ok((source.to_string(), 0));
        }
        inject_data_classification(source, tree, value)
    })
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

fn validate_property_value(value: &str, property: &str) -> Result<(), BulkFixError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(BulkFixError::InvalidInput(format!(
            "{property} value must not be empty"
        )));
    }
    if trimmed
        .chars()
        .any(|character| matches!(character, ';' | '{' | '}' | '=' | '\r' | '\n'))
        || trimmed.contains("//")
        || trimmed.contains("/*")
        || trimmed.contains("*/")
    {
        return Err(BulkFixError::InvalidInput(format!(
            "{property} value contains characters that cannot appear in an AL property value"
        )));
    }
    Ok(())
}

fn build_plan<F>(project_dir: &Path, transform: F) -> Result<BulkFixPlan, BulkFixError>
where
    F: Fn(&str, &tree_sitter::Tree, &str) -> Result<(String, usize), String>,
{
    // Discovery goes through the source index's walker so a bulk fix cannot
    // disagree with the workspace about which paths belong to the project.
    let files = al_source::file_index::collect_al_files(project_dir)?;
    let mut changes = Vec::new();
    for path in files {
        let source = al_source::file_index::read_source_file(&path)?.ok_or_else(|| {
            BulkFixError::Refused(format!("Workspace source disappeared: {}", path.display()))
        })?;
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
            return Err(BulkFixError::Refused(format!(
                "Bulk fix refused malformed AL source '{}': {}",
                path.display(),
                if details.is_empty() {
                    "tree-sitter reported an error node"
                } else {
                    &details
                }
            )));
        }
        let object =
            al_syntax::find_object_declaration(&parsed.tree, &source).ok_or_else(|| {
                BulkFixError::Refused(format!(
                    "Bulk fix refused '{}': no complete AL object declaration was found",
                    path.display()
                ))
            })?;
        let (updated, changes_count) = transform(&source, &parsed.tree, &object.kind)
            .map_err(|error| BulkFixError::Refused(format!("{}: {error}", path.display())))?;
        if (changes_count == 0) != (updated == source) {
            return Err(BulkFixError::Refused(format!(
                "Bulk-fix transformation integrity mismatch for '{}'",
                path.display()
            )));
        }
        if changes_count > 0 {
            let verification = al_syntax::AlParser::parse_quick(&updated);
            if verification.tree.root_node().has_error()
                || al_syntax::find_object_declaration(&verification.tree, &updated).is_none()
            {
                return Err(BulkFixError::Refused(format!(
                    "Bulk fix generated invalid AL for '{}'; no files were changed",
                    path.display()
                )));
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

/// [`collect_ast_sections`] restricted to one subtree.
///
/// A file declaring two tables would otherwise hand both tables' fields to a
/// caller asking about one of them.
pub(crate) fn collect_ast_sections_in(
    node: tree_sitter::Node<'_>,
    source: &str,
    target_keywords: &[&str],
) -> Result<Vec<AstObjectSection>, String> {
    if node.has_error() {
        return Err("cannot inspect sections in malformed AL source".to_string());
    }
    let mut sections = Vec::new();
    collect_ast_sections_from_node(node, source, target_keywords, &mut sections)?;
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

/// Give `value` the "Specifies " opening that UICop AA0218 asks for, unless it
/// already has one.
///
/// The sole caller reads the base-app table field's own `ToolTip` property,
/// which follows that convention already, so prefixing unconditionally wrote
/// `Specifies Specifies the number of the customer.` across every matching
/// page field in the project. A value that merely starts with the same letters
/// ("Specification number") still gets the prefix.
///
/// The prefix is counted in characters. A tooltip read from a package is in
/// the package's language, and slicing the first nine bytes of
/// `Spécifié le numéro` lands inside an `é`.
fn with_specifies_prefix(value: &str) -> String {
    const PREFIX: &str = "Specifies";
    let already_prefixed = value
        .char_indices()
        .nth(PREFIX.chars().count())
        .is_some_and(|(byte, following)| {
            following.is_whitespace() && value[..byte].eq_ignore_ascii_case(PREFIX)
        });
    if already_prefixed {
        value.to_string()
    } else {
        format!("{PREFIX} {value}")
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
        let escaped = with_specifies_prefix(&normalize_tooltip_text(tooltip)).replace('\'', "''");
        insertions.push(property_insertion(
            source,
            &section,
            &format!("ToolTip = '{escaped}';"),
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
        assert!(
            result.contains("                ToolTip = 'Specifies the item number';"),
            "{result}"
        );
        assert!(!parse(&result).root_node().has_error());
    }

    /// The only caller reads the base-app field's own `ToolTip`, which by the
    /// UICop AA0218 convention already starts with "Specifies".
    #[test]
    fn inject_tooltips_does_not_repeat_an_existing_specifies_prefix() {
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
        for value in [
            "Specifies the number of the customer.",
            "specifies the number of the customer.",
        ] {
            let tooltips = tooltip_map("No.", value);
            let (result, changes) = inject_tooltips(source, &parse(source), &tooltips).unwrap();
            assert_eq!(changes, 1);
            assert!(
                result.contains(&format!("                ToolTip = '{value}';")),
                "{value}: {result}"
            );
            assert!(!parse(&result).root_node().has_error());
        }
    }

    /// A tooltip read from a symbol package is in the package's language. The
    /// prefix check sliced the first nine *bytes*, and byte 9 of
    /// `Spécifié le numéro` is the second byte of an `é`, so the whole
    /// add-tooltips run panicked on a French base application.
    #[test]
    fn inject_tooltips_handles_a_non_ascii_tooltip() {
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
        let tooltips = tooltip_map("No.", "Spécifié le numéro");
        let (result, changes) = inject_tooltips(source, &parse(source), &tooltips).unwrap();
        assert_eq!(changes, 1);
        assert!(
            result.contains("ToolTip = 'Specifies Spécifié le numéro';"),
            "{result}"
        );
        assert!(!parse(&result).root_node().has_error());
    }

    /// "Specification" starts with the same letters but is not the prefix.
    #[test]
    fn inject_tooltips_prefixes_a_word_that_merely_starts_with_specifies() {
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
        let tooltips = tooltip_map("No.", "Specification number");
        let (result, _) = inject_tooltips(source, &parse(source), &tooltips).unwrap();
        assert!(
            result.contains("                ToolTip = 'Specifies Specification number';"),
            "{result}"
        );
        assert!(!parse(&result).root_node().has_error());
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

    /// Planning reads the project and touches nothing on disk; writing is the
    /// daemon's job, through the workspace refresh protocol.
    #[test]
    fn planning_reports_changes_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let page_content = "page 50100 \"Test\"\n{\n    layout\n    {\n        area(Content)\n        {\n            field(f; Rec.\"No.\")\n            {\n            }\n        }\n    }\n}\n";
        let page_path = dir.path().join("Test.Page.al");
        std::fs::write(&page_path, page_content).unwrap();

        let plan = plan_application_area(dir.path(), "All").unwrap();
        let result = plan.result(true);
        assert!(result.dry_run);
        assert!(result.changes_count > 0);
        assert_eq!(std::fs::read_to_string(&page_path).unwrap(), page_content);
    }

    #[test]
    fn malformed_file_blocks_the_entire_bulk_fix_plan() {
        let dir = tempfile::tempdir().unwrap();
        let good = "page 50100 \"Good\"\n{\n    layout\n    {\n        area(Content)\n        {\n            field(f; Rec.\"No.\") { }\n        }\n    }\n}\n";
        let good_path = dir.path().join("A.Good.al");
        std::fs::write(&good_path, good).unwrap();
        std::fs::write(
            dir.path().join("Z.Broken.al"),
            "page 50101 Broken { layout { area(Content) { field(",
        )
        .unwrap();

        let error = plan_application_area(dir.path(), "All").unwrap_err();
        assert!(error.to_string().contains("malformed AL source"), "{error}");
        assert_eq!(std::fs::read_to_string(good_path).unwrap(), good);
    }

    /// Every file the plan covers must parse after the edit, or the daemon
    /// would write a broken project.
    #[test]
    fn a_complete_plan_is_parseable_for_every_file_it_covers() {
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

        let plan = plan_application_area(dir.path(), "All").unwrap();
        let result = plan.result(false);
        assert!(!result.dry_run);
        assert_eq!(result.changes_count, 2);
        assert_eq!(result.modified_files.len(), 2);
        assert_eq!(plan.changes.len(), 2);
        for change in &plan.changes {
            assert!(change.updated.contains("ApplicationArea = All;"));
            assert!(!parse(&change.updated).root_node().has_error());
        }
        for path in [&first_path, &second_path] {
            assert!(!std::fs::read_to_string(path)
                .unwrap()
                .contains("ApplicationArea"));
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

        let files = al_source::file_index::collect_al_files(project.path()).unwrap();
        let expected = project.path().join("Inside.al");
        let outside = external.path().join("Outside.al");
        assert_eq!(files, vec![expected]);
        assert!(!files.contains(&outside));
    }
}
