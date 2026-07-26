//! Native syntax lint rules.
//!
//! Codes use the `AL-NL*` namespace to remain distinct from semantic bridge
//! diagnostics and the `AL-NC*` native workspace checks:
//!
//! - `AL-NL001`: `FindFirst`/`FindLast` called inside a loop (N+1 query risk).
//! - `AL-NL002`: a table field with no `DataClassification` property.
//! - `AL-NL005`: a record read without a preceding `SetLoadFields`.
//! - `AL-NL006`: a page field/action with no effective `ApplicationArea`.
//! - `AL-NL007`: a page field/action with no `ToolTip`.
//! - `AL-NL010`: a local variable or label that is never referenced.
//!
//! Rules use syntax nodes wherever the grammar exposes them. Text scanning is
//! retained only for statement-order checks that need lightweight control-flow
//! state.

use std::collections::BTreeMap;
use tree_sitter::{Point, Range, Tree};

#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub code: String,
    pub message: String,
    pub range: tree_sitter::Range,
    pub severity: LintSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LintSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

impl std::fmt::Display for LintSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LintSeverity::Error => write!(f, "error"),
            LintSeverity::Warning => write!(f, "warning"),
            LintSeverity::Info => write!(f, "info"),
            LintSeverity::Hint => write!(f, "hint"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LintRuleInfo {
    pub code: &'static str,
    pub name: &'static str,
    pub severity: LintSeverity,
    pub description: &'static str,
}

const RULES: &[LintRuleInfo] = &[
    LintRuleInfo {
        code: "AL-NL001",
        name: "find-first-in-loop",
        severity: LintSeverity::Warning,
        description: "FindFirst()/FindLast() called inside a loop causes N+1 database queries; \
                      use FindSet()/repeat..until Next() = 0 to iterate instead.",
    },
    LintRuleInfo {
        code: "AL-NL002",
        name: "field-missing-data-classification",
        severity: LintSeverity::Warning,
        description: "A table field has no DataClassification property, which is required for \
                      GDPR compliance and AppSource validation.",
    },
    LintRuleInfo {
        code: "AL-NL005",
        name: "record-read-missing-set-load-fields",
        severity: LintSeverity::Warning,
        description: "A record is read with FindSet/FindFirst/FindLast without a preceding \
                      SetLoadFields call in the same procedure.",
    },
    LintRuleInfo {
        code: "AL-NL006",
        name: "control-missing-application-area",
        severity: LintSeverity::Warning,
        description: "A page field or action has no ApplicationArea and the page does not \
                      provide an object-level ApplicationArea.",
    },
    LintRuleInfo {
        code: "AL-NL007",
        name: "control-missing-tooltip",
        severity: LintSeverity::Warning,
        description: "A page field or action has no ToolTip.",
    },
    LintRuleInfo {
        code: "AL-NL010",
        name: "unused-local-variable",
        severity: LintSeverity::Warning,
        description: "A local variable or label is declared but never referenced in its callable.",
    },
];

/// Return metadata for all available lint rules.
pub fn lint_rules() -> &'static [LintRuleInfo] {
    RULES
}

/// Run all native lint rules on the parsed tree with default config.
pub fn lint(tree: &Tree, text: &str) -> Vec<LintDiagnostic> {
    let mut diagnostics = Vec::new();
    lint_find_in_loop(tree, text, &mut diagnostics);
    lint_missing_data_classification(tree, text, &mut diagnostics);
    lint_missing_set_load_fields(tree, text, &mut diagnostics);
    lint_page_control_properties(tree, text, &mut diagnostics);
    lint_unused_local_variables(tree, text, &mut diagnostics);
    diagnostics
}

/// Build a `tree_sitter::Range` covering an entire source line, given its
/// 0-based row index. Byte offsets remain exact for LF, CRLF, and an unterminated
/// final line.
fn line_range(text: &str, line_idx: usize, _line: &str) -> Range {
    let start_byte = text
        .split_inclusive('\n')
        .take(line_idx)
        .map(str::len)
        .sum::<usize>()
        .min(text.len());
    let remainder = &text[start_byte..];
    let physical_len = remainder.find('\n').unwrap_or(remainder.len());
    let content_len = remainder[..physical_len]
        .strip_suffix('\r')
        .map_or(physical_len, str::len);
    let end_byte = start_byte + content_len;
    Range {
        start_byte,
        end_byte,
        start_point: Point {
            row: line_idx,
            column: 0,
        },
        end_point: Point {
            row: line_idx,
            column: content_len,
        },
    }
}

/// Replace AL string literal contents (`'...'` and `"..."`) with spaces so a
/// text scan doesn't see method-call-like substrings buried inside literals
/// or identifiers (e.g. a field named "FindFirst Date"). Quotes themselves
/// are preserved so token shape is unchanged for the caller's own scanning.
fn strip_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_single = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' if in_single && chars.peek() == Some(&'\'') => {
                out.push(' ');
                out.push(' ');
                chars.next();
            }
            '\'' => {
                in_single = !in_single;
                out.push(ch);
            }
            _ if in_single => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

fn is_loop_start(lower: &str) -> bool {
    lower.starts_with("for ")
        || lower.starts_with("foreach ")
        || lower.starts_with("while ")
        || lower == "repeat"
        || lower.starts_with("repeat ")
}

/// AL-NL001: FindFirst()/FindLast() inside a loop.
///
/// Scans individual procedure and trigger nodes so loop state cannot leak
/// across declarations.
fn lint_find_in_loop(tree: &Tree, text: &str, out: &mut Vec<LintDiagnostic>) {
    let source = text.as_bytes();
    let root = tree.root_node();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            if let Ok(proc_text) = node.utf8_text(source) {
                let start_row = node.start_position().row;
                scan_procedure_for_find_in_loop(text, proc_text, start_row, out);
            }
            // Do not recurse into the procedure body: nested constructs are
            // already covered by scanning proc_text as a whole.
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn scan_procedure_for_find_in_loop(
    file_text: &str,
    proc_text: &str,
    proc_start_row: usize,
    out: &mut Vec<LintDiagnostic>,
) {
    let mut loop_begin_depth: Vec<u32> = Vec::new();

    for (offset, line) in proc_text.lines().enumerate() {
        let line_idx = proc_start_row + offset;
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        let cleaned = strip_string_literals(line);
        let lower = cleaned.trim().to_lowercase();

        if is_loop_start(&lower) {
            let opens_body =
                lower.ends_with(" begin") || lower.ends_with("\tbegin") || lower == "begin";
            loop_begin_depth.push(if opens_body { 1 } else { 0 });
        } else if lower == "begin" {
            if let Some(top) = loop_begin_depth.last_mut() {
                *top += 1;
            }
        } else if lower == "end;" || lower == "end" {
            if let Some(top) = loop_begin_depth.last_mut() {
                if *top > 0 {
                    *top -= 1;
                } else {
                    loop_begin_depth.pop();
                }
            }
        } else if lower.starts_with("until ") {
            loop_begin_depth.pop();
        }

        let in_loop = !loop_begin_depth.is_empty();
        if in_loop && (lower.contains(".findfirst()") || lower.contains(".findlast()")) {
            out.push(LintDiagnostic {
                code: "AL-NL001".to_string(),
                message: "FindFirst()/FindLast() inside a loop causes N+1 queries; use \
                          FindSet()/repeat..until Next() = 0 instead."
                    .to_string(),
                range: line_range(file_text, line_idx, line),
                severity: LintSeverity::Warning,
            });
        }
    }
}

/// AL-NL002: table field with no `DataClassification` property.
///
/// Only runs when the file's object declaration is a
/// table or tableextension; a table-level default `DataClassification`
/// (outside any `field(...) { }` block) does not suppress this per-field
/// check.
fn lint_missing_data_classification(tree: &Tree, text: &str, out: &mut Vec<LintDiagnostic>) {
    let Some(obj_info) = crate::find_object_declaration(tree, text) else {
        return;
    };
    if !matches!(
        obj_info.kind.to_lowercase().as_str(),
        "table" | "tableextension"
    ) {
        return;
    }

    let Some(object) = first_object_declaration(tree.root_node()) else {
        return;
    };
    for section in collect_object_sections(object, text, &["field"]) {
        let is_calculated = section.properties.get("fieldclass").is_some_and(|value| {
            matches!(
                normalize_property_atom(value).as_str(),
                "flowfield" | "flowfilter"
            )
        });
        if !section.properties.contains_key("dataclassification") && !is_calculated {
            let row = section.node.start_position().row;
            out.push(LintDiagnostic {
                code: "AL-NL002".to_string(),
                message: "Table field has no DataClassification property.".to_string(),
                range: line_range(text, row, ""),
                severity: LintSeverity::Warning,
            });
        }
    }
}

fn method_calls(line: &str, method: &str) -> Vec<(usize, String)> {
    let needle = format!(".{}(", method.to_ascii_lowercase());
    let lower = line.to_ascii_lowercase();
    let mut calls = Vec::new();
    let mut from = 0;
    while let Some(relative) = lower[from..].find(&needle) {
        let dot = from + relative;
        let before = &line[..dot];
        let trimmed = before.trim_end();
        let receiver = if let Some(without_closing_quote) = trimmed.strip_suffix('"') {
            without_closing_quote
                .rfind('"')
                .map(|start| trimmed[start..].to_string())
        } else {
            let start = trimmed
                .char_indices()
                .rev()
                .find(|(_, ch)| !ch.is_ascii_alphanumeric() && *ch != '_')
                .map_or(0, |(idx, ch)| idx + ch.len_utf8());
            (start < trimmed.len()).then(|| trimmed[start..].to_string())
        };
        if let Some(receiver) = receiver.filter(|value| !value.is_empty()) {
            calls.push((dot, receiver.to_ascii_lowercase()));
        }
        from = dot + needle.len();
    }
    calls
}

/// AL-NL005: a record read without a preceding `SetLoadFields`.
///
/// The rule is deliberately procedure-local and receiver-specific. It does not
/// infer a call on one record variable from a call on another, and a
/// `SetLoadFields` that appears after the read does not suppress the finding.
fn lint_missing_set_load_fields(tree: &Tree, text: &str, out: &mut Vec<LintDiagnostic>) {
    let source = text.as_bytes();
    let root = tree.root_node();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            if let Ok(proc_text) = node.utf8_text(source) {
                let mut prepared = std::collections::HashSet::new();
                let start_row = node.start_position().row;
                for (offset, line) in proc_text.lines().enumerate() {
                    let cleaned =
                        strip_string_literals(line.split_once("//").map_or(line, |(code, _)| code));
                    let mut events: Vec<(usize, bool, String)> =
                        method_calls(&cleaned, "setloadfields")
                            .into_iter()
                            .map(|(position, receiver)| (position, true, receiver))
                            .collect();
                    for method in ["findset", "findfirst", "findlast"] {
                        events.extend(
                            method_calls(&cleaned, method)
                                .into_iter()
                                .map(|(position, receiver)| (position, false, receiver)),
                        );
                    }
                    events.sort_by_key(|(position, is_prepare, _)| (*position, !*is_prepare));
                    for (_, is_prepare, receiver) in events {
                        if is_prepare {
                            prepared.insert(receiver);
                        } else if !prepared.contains(&receiver) {
                            out.push(LintDiagnostic {
                                code: "AL-NL005".to_string(),
                                message: format!(
                                    "Record {receiver} is read without a preceding \
                                     SetLoadFields call in this procedure."
                                ),
                                range: line_range(text, start_row + offset, line),
                                severity: LintSeverity::Warning,
                            });
                        }
                    }
                }
            }
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

struct ObjectSection<'tree> {
    node: tree_sitter::Node<'tree>,
    keyword: String,
    properties: BTreeMap<String, String>,
}

fn first_object_declaration(root: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_declaration" {
            return Some(node);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

fn direct_properties(container: tree_sitter::Node<'_>, text: &str) -> BTreeMap<String, String> {
    let Some(body) = container.child_by_field_name("body") else {
        return BTreeMap::new();
    };
    let mut properties = BTreeMap::new();
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        if child.kind() != "property_assignment" {
            continue;
        }
        let Some(name) = child
            .child_by_field_name("name")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .map(str::trim)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };
        let value = child
            .child_by_field_name("value")
            .and_then(|node| node.utf8_text(text.as_bytes()).ok())
            .map(str::trim)
            .unwrap_or_default();
        properties.insert(name.to_ascii_lowercase(), value.to_string());
    }
    properties
}

fn collect_object_sections<'tree>(
    root: tree_sitter::Node<'tree>,
    text: &str,
    target_keywords: &[&str],
) -> Vec<ObjectSection<'tree>> {
    let mut sections = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_section" {
            let keyword = node
                .child_by_field_name("keyword")
                .and_then(|keyword| keyword.utf8_text(text.as_bytes()).ok())
                .map(str::trim)
                .unwrap_or_default()
                .to_ascii_lowercase();
            if target_keywords
                .iter()
                .any(|target| keyword.eq_ignore_ascii_case(target))
            {
                sections.push(ObjectSection {
                    node,
                    keyword,
                    properties: direct_properties(node, text),
                });
            }
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    sections
}

fn normalize_property_atom(value: &str) -> String {
    value
        .trim()
        .trim_matches(['\'', '"'])
        .trim()
        .to_ascii_lowercase()
}

/// AL-NL006/7: page fields and actions must expose their application area and
/// user-facing purpose. An object-level ApplicationArea satisfies AL-NL006 for
/// all child controls, matching modern AL inheritance behavior.
fn lint_page_control_properties(tree: &Tree, text: &str, out: &mut Vec<LintDiagnostic>) {
    let Some(obj_info) = crate::find_object_declaration(tree, text) else {
        return;
    };
    if !matches!(
        obj_info.kind.to_ascii_lowercase().as_str(),
        "page" | "pageextension" | "pagecustomization"
    ) {
        return;
    }

    let Some(object) = first_object_declaration(tree.root_node()) else {
        return;
    };
    let object_application_area = direct_properties(object, text).contains_key("applicationarea");
    for section in collect_object_sections(object, text, &["field", "action"]) {
        let row = section.node.start_position().row;
        if !object_application_area && !section.properties.contains_key("applicationarea") {
            out.push(LintDiagnostic {
                code: "AL-NL006".to_string(),
                message: "Page field/action has no effective ApplicationArea.".to_string(),
                range: line_range(text, row, ""),
                severity: LintSeverity::Warning,
            });
        }
        if !section.properties.contains_key("tooltip") {
            out.push(LintDiagnostic {
                code: "AL-NL007".to_string(),
                message: format!("Page {} has no ToolTip.", section.keyword),
                range: line_range(text, row, ""),
                severity: LintSeverity::Warning,
            });
        }
    }
}

#[derive(Debug)]
struct LocalDeclaration<'tree> {
    name: String,
    name_node: tree_sitter::Node<'tree>,
}

/// AL-NL010: a local variable or label that has no reference in its callable.
///
/// This deliberately reports only declarations with *no* references. A local
/// that is written but never read requires data-flow analysis and is left to
/// CodeCop AA0206; treating every assignment as removable would produce an
/// unsafe quick fix. AST primary expressions are used instead of text search,
/// so strings, comments, declaration types, and member names such as
/// `Customer.Name` cannot make a same-named local look used.
fn lint_unused_local_variables(tree: &Tree, text: &str, out: &mut Vec<LintDiagnostic>) {
    let source = text.as_bytes();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_declaration"
        ) {
            lint_unused_in_callable(node, source, out);
            // A valid callable cannot contain another callable. Avoid walking
            // its body again; malformed legacy recovery nodes remain reachable
            // through the ordinary traversal branch below.
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn lint_unused_in_callable(
    callable: tree_sitter::Node<'_>,
    source: &[u8],
    out: &mut Vec<LintDiagnostic>,
) {
    // In an incomplete callable, a missing body fragment can make a real
    // reference disappear from the syntax tree. Avoid a false positive until
    // the parse error itself is fixed.
    if callable.has_error() {
        return;
    }

    let mut var_sections = Vec::new();
    let mut bodies = Vec::new();
    let mut cursor = callable.walk();
    for child in callable.children(&mut cursor) {
        match child.kind() {
            "var_section" => var_sections.push(child),
            "begin_end_block" => bodies.push(child),
            _ => {}
        }
    }
    if var_sections.is_empty() {
        return;
    }

    let mut declarations = Vec::new();
    let mut initializer_values = Vec::new();
    for section in var_sections {
        collect_local_declarations(section, source, &mut declarations, &mut initializer_values);
    }
    declarations.sort_by_key(|declaration| declaration.name_node.start_byte());
    if declarations.is_empty() {
        return;
    }

    let mut references = std::collections::HashSet::new();
    for body in bodies {
        collect_primary_expression_names(body, source, &mut references);
    }
    for value in initializer_values {
        collect_primary_expression_names(value, source, &mut references);
    }

    for declaration in declarations {
        if references.contains(&declaration.name.to_ascii_lowercase()) {
            continue;
        }
        out.push(LintDiagnostic {
            code: "AL-NL010".to_string(),
            message: format!(
                "Local variable '{}' is declared but never referenced.",
                declaration.name
            ),
            range: declaration.name_node.range(),
            severity: LintSeverity::Warning,
        });
    }
}

fn collect_local_declarations<'tree>(
    section: tree_sitter::Node<'tree>,
    source: &[u8],
    declarations: &mut Vec<LocalDeclaration<'tree>>,
    initializer_values: &mut Vec<tree_sitter::Node<'tree>>,
) {
    let mut stack = vec![section];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "regular_variable_declaration" => {
                let mut names = node.walk();
                declarations.extend(node.children_by_field_name("name", &mut names).filter_map(
                    |name_node| {
                        crate::node_text_clean(name_node, source)
                            .map(|name| LocalDeclaration { name, name_node })
                    },
                ));
                if let Some(value) = node.child_by_field_name("value") {
                    initializer_values.push(value);
                }
                continue;
            }
            "label_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Some(name) = crate::node_text_clean(name_node, source) {
                        declarations.push(LocalDeclaration { name, name_node });
                    }
                }
                continue;
            }
            _ => {}
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

fn collect_primary_expression_names(
    root: tree_sitter::Node<'_>,
    source: &[u8],
    names: &mut std::collections::HashSet<String>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "primary_expression" {
            if let Some(child) = node.named_child(0) {
                if matches!(
                    child.kind(),
                    "name"
                        | "name_or_keyword"
                        | "identifier"
                        | "quoted_identifier"
                        | "object_keyword"
                        | "type_keyword"
                        | "metadata_keyword"
                        | "property_keyword"
                        | "keyword"
                ) {
                    if let Some(name) = crate::node_text_clean(child, source) {
                        names.insert(name.to_ascii_lowercase());
                    }
                    continue;
                }
            }
        }

        // FOR/FOREACH iterator fields are identifier nodes rather than primary
        // expressions, but the loop machinery itself is a meaningful use.
        if matches!(node.kind(), "for_statement" | "foreach_statement") {
            if let Some(iterator) = node.child_by_field_name("iterator") {
                if let Some(name) = crate::node_text_clean(iterator, source) {
                    names.insert(name.to_ascii_lowercase());
                }
            }
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::AlParser;

    #[test]
    fn lint_rules_lists_the_starter_set() {
        let rules = lint_rules();
        assert_eq!(rules.len(), 6);
        assert!(rules.iter().any(|r| r.code == "AL-NL001"));
        assert!(rules.iter().any(|r| r.code == "AL-NL002"));
        assert!(rules.iter().any(|r| r.code == "AL-NL005"));
        assert!(rules.iter().any(|r| r.code == "AL-NL006"));
        assert!(rules.iter().any(|r| r.code == "AL-NL007"));
        assert!(rules.iter().any(|r| r.code == "AL-NL010"));
    }

    #[test]
    fn lint_flags_findfirst_in_loop() {
        let src = r#"codeunit 50100 Test
{
    procedure ProcessItems()
    var
        Item: Record Item;
    begin
        repeat
            if Item.FindFirst() then
                Message(Item."No.");
        until Item.Next() = 0;
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            diags.iter().any(|d| d.code == "AL-NL001"),
            "expected AL-NL001, got {diags:?}"
        );
    }

    #[test]
    fn lint_does_not_flag_findfirst_outside_loop() {
        let src = r#"codeunit 50100 Test
{
    procedure GetItem()
    var
        Item: Record Item;
    begin
        if Item.FindFirst() then
            Message(Item."No.");
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|d| d.code == "AL-NL001"),
            "did not expect AL-NL001, got {diags:?}"
        );
    }

    #[test]
    fn lint_does_not_flag_findfirst_in_string_literal_or_comment() {
        let src = r#"codeunit 50100 Test
{
    procedure DoIt()
    var
        i: Integer;
        msg: Text;
    begin
        for i := 1 to 10 do begin
            msg := 'FindFirst() should not match';
            // .FindFirst() in a comment should not match either
            Message(msg);
        end;
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|d| d.code == "AL-NL001"),
            "did not expect AL-NL001, got {diags:?}"
        );
    }

    #[test]
    fn lint_flags_table_field_missing_data_classification() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            DataClassification = CustomerContent;
        }
        field(2; Description; Text[100])
        {
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        let al_l002: Vec<_> = diags.iter().filter(|d| d.code == "AL-NL002").collect();
        assert_eq!(
            al_l002.len(),
            1,
            "expected exactly one AL-NL002 (the field without DataClassification): {diags:?}"
        );
    }

    #[test]
    fn lint_does_not_flag_fully_classified_table() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
        field(1; "No."; Code[20])
        {
            DataClassification = CustomerContent;
        }
        field(2; Description; Text[100])
        {
            DataClassification = CustomerContent;
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|d| d.code == "AL-NL002"),
            "did not expect AL-NL002, got {diags:?}"
        );
    }

    #[test]
    fn lint_does_not_require_data_classification_on_flow_fields_or_filters() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Total; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = sum("My Table".Amount);
        }
        field(2; Filter; Text[20])
        {
            FieldClass = FlowFilter;
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|d| d.code == "AL-NL002"),
            "calculated fields must not receive DataClassification fixes: {diags:?}"
        );
    }

    #[test]
    fn data_classification_lint_uses_direct_ast_properties() {
        let src = r#"table 50100 "My Table"
{
    fields
    {
        field(1; Stored; Text[100]) { Caption = 'DataClassification = CustomerContent; { }'; }
        field(2; Classified; Text[100])
        {
            DataClassification = CustomerContent;
            trigger OnValidate()
            begin
                Message('FieldClass = FlowField;');
            end;
        }
        field(3; Calculated; Decimal)
        {
            // DataClassification = CustomerContent;
            FieldClass = FlowField;
            CalcFormula = Sum("My Table".Calculated);
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        assert!(!result.tree.root_node().has_error(), "{:?}", result.errors);
        let diags = lint(&result.tree, src);
        let findings: Vec<_> = diags
            .iter()
            .filter(|diagnostic| diagnostic.code == "AL-NL002")
            .collect();
        assert_eq!(findings.len(), 1, "{diags:?}");
        assert_eq!(findings[0].range.start_point.row, 4);
    }

    #[test]
    fn lint_does_not_flag_non_table_objects() {
        let src = r#"codeunit 50100 Test
{
    procedure Foo()
    begin
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(diags.is_empty(), "expected no diagnostics, got {diags:?}");
    }

    #[test]
    fn lint_flags_record_read_without_set_load_fields() {
        let src = r#"codeunit 50100 Test
{
    procedure ReadItems()
    var
        Item: Record Item;
    begin
        if Item.FindSet() then
            repeat
                Message(Item.Description);
            until Item.Next() = 0;
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            diags.iter().any(|d| d.code == "AL-NL005"),
            "expected AL-NL005, got {diags:?}"
        );
    }

    #[test]
    fn lint_accepts_receiver_specific_set_load_fields_before_read() {
        let src = r#"codeunit 50100 Test
{
    procedure ReadItems()
    var
        Item: Record Item;
    begin
        Item.SetLoadFields(Description);
        if Item.FindSet() then
            repeat
                Message(Item.Description);
            until Item.Next() = 0;
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|d| d.code == "AL-NL005"),
            "did not expect AL-NL005, got {diags:?}"
        );
    }

    #[test]
    fn lint_does_not_apply_one_records_set_load_fields_to_another() {
        let src = r#"codeunit 50100 Test
{
    procedure ReadItems()
    var
        Item: Record Item;
        Customer: Record Customer;
    begin
        Item.SetLoadFields(Description);
        if Customer.FindFirst() then;
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            diags.iter().any(|d| d.code == "AL-NL005"),
            "expected receiver-specific AL-NL005, got {diags:?}"
        );
    }

    #[test]
    fn lint_flags_page_controls_missing_application_area_and_tooltip() {
        let src = r#"page 50100 "Item List"
{
    PageType = List;
    layout
    {
        area(Content)
        {
            field(Description; Rec.Description)
            {
            }
        }
    }
    actions
    {
        area(Processing)
        {
            action(RefreshItems)
            {
                Caption = 'Refresh';
            }
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert_eq!(
            diags.iter().filter(|d| d.code == "AL-NL006").count(),
            2,
            "expected both controls to require ApplicationArea: {diags:?}"
        );
        assert_eq!(
            diags.iter().filter(|d| d.code == "AL-NL007").count(),
            2,
            "expected both controls to require ToolTip: {diags:?}"
        );
    }

    #[test]
    fn lint_accepts_control_properties() {
        let src = r#"page 50100 "Item List"
{
    PageType = List;
    layout
    {
        area(Content)
        {
            field(Description; Rec.Description)
            {
                ApplicationArea = All;
                ToolTip = 'Specifies the description.';
            }
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags
                .iter()
                .any(|d| matches!(d.code.as_str(), "AL-NL006" | "AL-NL007")),
            "did not expect page-control diagnostics, got {diags:?}"
        );
    }

    #[test]
    fn object_application_area_is_inherited_but_tooltip_is_not() {
        let src = r#"page 50100 "Item List"
{
    PageType = List;
    ApplicationArea = All;
    layout
    {
        area(Content)
        {
            field(Description; Rec.Description)
            {
            }
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|d| d.code == "AL-NL006"),
            "object-level ApplicationArea should suppress AL-NL006: {diags:?}"
        );
        assert!(
            diags.iter().any(|d| d.code == "AL-NL007"),
            "missing ToolTip should still surface: {diags:?}"
        );
    }

    #[test]
    fn page_control_lints_use_direct_ast_properties() {
        let src = r#"page 50100 "Item List"
{
    Caption = 'ApplicationArea = All; ToolTip = ''not a property''; { }';
    // ApplicationArea = All;
    layout
    {
        area(Content)
        {
            group(General)
            {
                ApplicationArea = All;
                ToolTip = 'Group tooltip';
                field(Description; Rec.Description) { Caption = 'ToolTip = fake'; }
            }
        }
    }
    actions
    {
        area(Processing)
        {
            action(Refresh) { ApplicationArea = All; ToolTip = 'Refresh'; }
        }
    }
}"#;
        let result = AlParser::parse_quick(src);
        assert!(!result.tree.root_node().has_error(), "{:?}", result.errors);
        let diags = lint(&result.tree, src);
        assert_eq!(
            diags
                .iter()
                .filter(|diagnostic| diagnostic.code == "AL-NL006")
                .count(),
            1,
            "{diags:?}"
        );
        assert_eq!(
            diags
                .iter()
                .filter(|diagnostic| diagnostic.code == "AL-NL007")
                .count(),
            1,
            "{diags:?}"
        );
    }

    #[test]
    fn line_ranges_are_exact_for_crlf_sources() {
        let source = "first\r\nsecond\r\nthird";
        let range = line_range(source, 1, "ignored");
        assert_eq!(&source[range.start_byte..range.end_byte], "second");
        assert_eq!(range.start_point, Point { row: 1, column: 0 });
        assert_eq!(range.end_point, Point { row: 1, column: 6 });
    }

    #[test]
    fn unused_local_lint_uses_ast_references_not_text_or_member_names() {
        let src = r#"codeunit 50100 Test
{
    procedure Exercise()
    var
        Unused, Used: Integer;
        Customer: Record Customer;
        InitializerSource: Integer;
        InitializedFromSource: Integer := InitializerSource;
        "Unused Label": Label 'Unused and InitializerSource are text here';
    begin
        Used := 1;
        Customer.Unused := Used;
        Message('%1', InitializedFromSource);
        // Unused and "Unused Label" in a comment are not references.
    end;
}"#;
        let result = AlParser::parse_quick(src);
        assert!(!result.tree.root_node().has_error());
        let diags = lint(&result.tree, src);
        let unused: Vec<_> = diags
            .iter()
            .filter(|diagnostic| diagnostic.code == "AL-NL010")
            .collect();
        let names: Vec<_> = unused
            .iter()
            .map(|diagnostic| &src[diagnostic.range.start_byte..diagnostic.range.end_byte])
            .collect();
        assert_eq!(names, ["Unused", "\"Unused Label\""]);
    }

    #[test]
    fn loop_iterators_are_meaningful_local_references() {
        let src = r#"codeunit 50100 Test
{
    procedure Exercise(Values: List of [Integer])
    var
        Index, Item: Integer;
    begin
        for Index := 1 to 2 do
            Message('%1', Index);
        foreach Item in Values do
            Message('%1', Item);
    end;
}"#;
        let result = AlParser::parse_quick(src);
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|diagnostic| diagnostic.code == "AL-NL010"),
            "loop iterators must count as references: {diags:?}"
        );
    }

    #[test]
    fn unused_local_lint_skips_incomplete_callable() {
        let src = r#"codeunit 50100 Test
{
    procedure Incomplete()
    var
        MaybeUsedInMissingSource: Integer;
    begin
        if true then
}"#;
        let result = AlParser::parse_quick(src);
        assert!(result.tree.root_node().has_error());
        let diags = lint(&result.tree, src);
        assert!(
            !diags.iter().any(|diagnostic| diagnostic.code == "AL-NL010"),
            "an incomplete body must not create unused-variable false positives: {diags:?}"
        );
    }
}
