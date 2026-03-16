//! Native lint rules (tree-sitter only, no semantic knowledge).
//!
//! 18 rules: AL-L001 through AL-L018. Each operates on the tree-sitter AST
//! and raw source text. No type information or cross-file knowledge is used.

use tree_sitter::{Node, Tree};

/// A lint diagnostic from a native rule.
#[derive(Debug, Clone)]
pub struct LintDiagnostic {
    pub code: String,
    pub message: String,
    pub range: tree_sitter::Range,
    pub severity: LintSeverity,
}

/// Severity level for lint diagnostics.
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

/// Lint configuration.
#[derive(Debug, Clone)]
pub struct LintConfig {
    /// Maximum lines for a procedure body (AL-L002). Default: 100.
    pub max_procedure_lines: usize,
    /// Maximum nesting depth for if statements (AL-L004). Default: 5.
    pub max_if_depth: usize,
    /// Maximum parameters for a procedure (AL-L009). Default: 7.
    pub max_parameters: usize,
}

impl Default for LintConfig {
    fn default() -> Self {
        Self {
            max_procedure_lines: 100,
            max_if_depth: 5,
            max_parameters: 7,
        }
    }
}

/// Metadata for a lint rule (for listing/documentation).
#[derive(Debug, Clone)]
pub struct LintRuleInfo {
    pub code: &'static str,
    pub name: &'static str,
    pub severity: LintSeverity,
    pub description: &'static str,
}

/// Return metadata for all available lint rules.
pub fn lint_rules() -> &'static [LintRuleInfo] {
    static RULES: &[LintRuleInfo] = &[
        LintRuleInfo { code: "AL-L001", name: "EmptyBeginEnd", severity: LintSeverity::Warning, description: "Empty begin..end block" },
        LintRuleInfo { code: "AL-L002", name: "LongProcedure", severity: LintSeverity::Warning, description: "Procedure exceeds maximum line count" },
        LintRuleInfo { code: "AL-L003", name: "MissingSemicolon", severity: LintSeverity::Error, description: "Missing semicolon (detected via parser errors)" },
        LintRuleInfo { code: "AL-L004", name: "DeepNesting", severity: LintSeverity::Warning, description: "Nested if depth exceeds maximum" },
        LintRuleInfo { code: "AL-L005", name: "UnusedVariable", severity: LintSeverity::Warning, description: "Variable declared but not used in procedure body" },
        LintRuleInfo { code: "AL-L006", name: "EmptyTrigger", severity: LintSeverity::Hint, description: "Trigger has an empty body" },
        LintRuleInfo { code: "AL-L007", name: "TodoComment", severity: LintSeverity::Info, description: "TODO/FIXME/HACK comment found" },
        LintRuleInfo { code: "AL-L008", name: "MagicNumber", severity: LintSeverity::Info, description: "Magic number — consider using a named constant" },
        LintRuleInfo { code: "AL-L009", name: "ExcessiveParams", severity: LintSeverity::Warning, description: "Procedure has too many parameters" },
        LintRuleInfo { code: "AL-L010", name: "MissingCaseElse", severity: LintSeverity::Warning, description: "Case statement is missing an else branch" },
        LintRuleInfo { code: "AL-L011", name: "RedundantBeginEnd", severity: LintSeverity::Hint, description: "Redundant begin..end around single statement" },
        // AL-L012 (AssignmentInCondition) is intentionally omitted: check_suspicious_equals is
        // a no-op stub. TODO: implement detection of `:=` vs `=` confusion in if conditions.
        LintRuleInfo { code: "AL-L013", name: "EmptyRepeat", severity: LintSeverity::Warning, description: "Empty repeat..until loop" },
        LintRuleInfo { code: "AL-L014", name: "UnreachableCode", severity: LintSeverity::Warning, description: "Unreachable code after exit/error" },
        LintRuleInfo { code: "AL-L015", name: "GlobalVarNaming", severity: LintSeverity::Info, description: "Global variable has a non-descriptive name" },
        LintRuleInfo { code: "AL-L016", name: "ProcedureNaming", severity: LintSeverity::Warning, description: "Procedure name does not follow PascalCase convention" },
        LintRuleInfo { code: "AL-L017", name: "HardcodedString", severity: LintSeverity::Info, description: "Hard-coded text string — consider using a Label variable" },
        LintRuleInfo { code: "AL-L018", name: "RecordVarNaming", severity: LintSeverity::Info, description: "Record variable should use a descriptive name matching the table" },
    ];
    RULES
}

/// Run all native lint rules on the parsed tree with default config.
pub fn lint(tree: &Tree, text: &str) -> Vec<LintDiagnostic> {
    lint_with_config(tree, text, &LintConfig::default())
}

/// Run all native lint rules on the parsed tree with custom config.
pub fn lint_with_config(tree: &Tree, text: &str, config: &LintConfig) -> Vec<LintDiagnostic> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut diagnostics = Vec::new();

    walk_and_lint(root, source, text, config, &mut diagnostics, 0);

    diagnostics
}

/// Walk the tree and apply all lint rules.
fn walk_and_lint(
    node: Node,
    source: &[u8],
    text: &str,
    config: &LintConfig,
    diagnostics: &mut Vec<LintDiagnostic>,
    if_depth: usize,
) {
    let kind = node.kind();

    match kind {
        "begin_end_block" => {
            // AL-L001: Empty BEGIN..END block
            check_empty_begin_end(node, source, diagnostics);

            // AL-L011: Redundant BEGIN..END (single statement inside)
            check_redundant_begin_end(node, source, diagnostics);
        }

        "procedure_declaration" | "event_procedure_declaration" => {
            // AL-L002: Procedure exceeds max lines
            check_procedure_length(node, text, config, diagnostics);

            // AL-L005: Variable declared but unused (within same procedure)
            check_unused_variables(node, source, text, diagnostics);

            // AL-L009: Excessive parameters
            check_excessive_parameters(node, source, config, diagnostics);

            // AL-L016: Procedure naming (PascalCase required)
            check_procedure_naming(node, source, diagnostics);
        }

        "trigger_declaration" => {
            // AL-L006: Empty trigger body
            check_empty_trigger(node, source, diagnostics);

            // AL-L002: Trigger can also be too long
            check_procedure_length(node, text, config, diagnostics);
        }

        "if_statement" | "empty_if_statement" => {
            // AL-L004: Nested IF depth exceeds max
            let new_depth = if_depth + 1;
            if new_depth > config.max_if_depth {
                diagnostics.push(LintDiagnostic {
                    code: "AL-L004".to_string(),
                    message: format!(
                        "Nested if depth ({}) exceeds maximum ({})",
                        new_depth, config.max_if_depth
                    ),
                    range: node.range(),
                    severity: LintSeverity::Warning,
                });
            }

            // AL-L012: Assignment in IF condition (= instead of :=)
            check_assignment_in_condition(node, source, diagnostics);

            // Recurse with incremented depth
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                walk_and_lint(child, source, text, config, diagnostics, new_depth);
            }
            return; // Don't recurse again below
        }

        "comment" => {
            // AL-L007: TODO/FIXME in comments
            check_todo_comments(node, source, diagnostics);
        }

        "case_statement" => {
            // AL-L010: Missing CASE else branch
            check_case_else(node, source, diagnostics);
        }

        "repeat_statement" => {
            // AL-L013: Empty REPEAT..UNTIL loop
            check_empty_repeat(node, source, diagnostics);
        }

        "exit_statement" => {
            // AL-L014: Unreachable code after EXIT/ERROR
            check_unreachable_after_exit(node, diagnostics);
        }

        "expression_statement" => {
            // AL-L014: Also check for Error() calls
            if let Some(expr) = node.child(0) {
                check_error_call_unreachable(expr, node, source, diagnostics);
            }

            // AL-L017: Hard-coded text string (should use Label)
            check_hardcoded_string(node, source, diagnostics);
        }

        "integer" => {
            // AL-L008: Magic numbers
            check_magic_number(node, source, diagnostics);
        }

        "object_var_section" => {
            // AL-L015: Global variable naming (should use g prefix or similar)
            check_global_variable_naming(node, source, diagnostics);

            // AL-L018: Record variable naming convention
            check_record_variable_naming(node, source, diagnostics);
        }

        _ => {}
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_and_lint(child, source, text, config, diagnostics, if_depth);
    }
}

// ── AL-L001: Empty BEGIN..END block ─────────────────────────────────

fn check_empty_begin_end(node: Node, _source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    // A begin..end block is empty if it has no statement_list child
    let mut cursor = node.walk();
    let has_statements = node.children(&mut cursor).any(|c| {
        c.kind() == "statement_list"
            || (c.is_named()
                && !matches!(c.kind(), "kw_begin" | "kw_end" | "comment"))
    });

    if !has_statements {
        // Don't flag if this is inside a trigger (AL-L006 handles that)
        if let Some(parent) = node.parent() {
            if parent.kind() == "trigger_declaration" {
                return;
            }
        }

        diagnostics.push(LintDiagnostic {
            code: "AL-L001".to_string(),
            message: "Empty begin..end block".to_string(),
            range: node.range(),
            severity: LintSeverity::Warning,
        });
    }
}

// ── AL-L002: Procedure exceeds max lines ────────────────────────────

fn check_procedure_length(
    node: Node,
    text: &str,
    config: &LintConfig,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let start_line = node.start_position().row;
    let end_line = node.end_position().row;
    let line_count = end_line - start_line + 1;

    if line_count > config.max_procedure_lines {
        let name = get_name(node, text.as_bytes());
        diagnostics.push(LintDiagnostic {
            code: "AL-L002".to_string(),
            message: format!(
                "Procedure '{}' is {} lines (max {})",
                name, line_count, config.max_procedure_lines
            ),
            range: node.range(),
            severity: LintSeverity::Warning,
        });
    }
}

// ── AL-L003: Missing semicolon (detected from tree-sitter errors) ───

// Note: AL-L003 is implicitly handled by the parser's error collection.
// We detect tree-sitter ERROR/MISSING nodes in parser.rs.
// This rule exists for completeness but is not separately implemented
// since tree-sitter errors already surface these issues.

// ── AL-L004: Nested IF depth ────────────────────────────────────────
// Handled inline in walk_and_lint

// ── AL-L005: Variable declared but unused ───────────────────────────

fn check_unused_variables(
    node: Node,
    source: &[u8],
    text: &str,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    // Find var section within the procedure
    let mut cursor = node.walk();
    let mut var_declarations: Vec<(String, tree_sitter::Range)> = Vec::new();

    for child in node.children(&mut cursor) {
        if child.kind() == "var_section" || child.kind() == "empty_var_section" {
            collect_var_names(child, source, &mut var_declarations);
        }
    }

    if var_declarations.is_empty() {
        return;
    }

    // Find the begin..end block
    let mut cursor2 = node.walk();
    let body_text: Option<String> = node.children(&mut cursor2).find_map(|c| {
        if c.kind() == "begin_end_block" {
            c.utf8_text(source).ok().map(|s| s.to_string())
        } else {
            None
        }
    });

    let body = match body_text {
        Some(b) => b.to_lowercase(),
        None => return,
    };

    for (var_name, var_range) in &var_declarations {
        let lower_name = var_name.to_lowercase();
        // Simple heuristic: check if the variable name appears in the body text
        // This doesn't account for scoping but works for most cases
        if !body.contains(&lower_name) {
            diagnostics.push(LintDiagnostic {
                code: "AL-L005".to_string(),
                message: format!("Variable '{}' is declared but not used", var_name),
                range: *var_range,
                severity: LintSeverity::Warning,
            });
        }
    }

    let _ = text; // text is used via source
}

fn collect_var_names(
    node: Node,
    source: &[u8],
    vars: &mut Vec<(String, tree_sitter::Range)>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declaration"
            || child.kind() == "regular_variable_declaration"
            || child.kind() == "label_declaration"
        {
            if let Some(name_node) = child.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(source) {
                    let clean_name = name.trim_matches('"').to_string();
                    if !clean_name.is_empty() {
                        vars.push((clean_name, name_node.range()));
                    }
                }
            }
        }
        // Recurse for nested structures
        if child.kind() == "variable_declaration" {
            collect_var_names(child, source, vars);
        }
    }
}

// ── AL-L006: Empty trigger body ─────────────────────────────────────

fn check_empty_trigger(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    let mut cursor = node.walk();
    let mut has_begin_end = false;
    let mut body_is_empty = true;

    for child in node.children(&mut cursor) {
        if child.kind() == "begin_end_block" {
            has_begin_end = true;
            let mut bc = child.walk();
            for bchild in child.children(&mut bc) {
                if bchild.kind() == "statement_list" {
                    body_is_empty = false;
                    break;
                }
                if bchild.is_named() && !matches!(bchild.kind(), "kw_begin" | "kw_end" | "comment")
                {
                    body_is_empty = false;
                    break;
                }
            }
        }
    }

    if has_begin_end && body_is_empty {
        let name = get_name(node, source);
        diagnostics.push(LintDiagnostic {
            code: "AL-L006".to_string(),
            message: format!(
                "Trigger '{}' has an empty body — consider removing it",
                name
            ),
            range: node.range(),
            severity: LintSeverity::Hint,
        });
    }
}

// ── AL-L007: TODO/FIXME in comments ────────────────────────────────

fn check_todo_comments(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    if let Ok(comment_text) = node.utf8_text(source) {
        let upper = comment_text.to_uppercase();
        if upper.contains("TODO") || upper.contains("FIXME") || upper.contains("HACK") {
            diagnostics.push(LintDiagnostic {
                code: "AL-L007".to_string(),
                message: "TODO/FIXME comment found".to_string(),
                range: node.range(),
                severity: LintSeverity::Info,
            });
        }
    }
}

// ── AL-L008: Magic numbers ──────────────────────────────────────────

fn check_magic_number(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    if let Ok(text) = node.utf8_text(source) {
        // Allow 0 and 1 as they are commonly used
        if text == "0" || text == "1" || text == "0L" || text == "1L" {
            return;
        }

        // Check parent context — skip if in object/enum ID, property value, or case label
        if let Some(parent) = node.parent() {
            let pk = parent.kind();
            // Skip object IDs, enum value IDs, array sizes, property values
            if matches!(
                pk,
                "object_declaration"
                    | "enum_value_declaration"
                    | "property_assignment"
                    | "case_label_expression"
                    | "case_label_list"
                    | "attribute_argument"
                    | "attribute_argument_list"
                    | "bracketed_block" // array index
                    | "type_reference" // e.g., Text[100]
            ) {
                return;
            }

            // Skip for...to ranges
            if matches!(pk, "for_statement") {
                return;
            }

            // Check if inside a const or label declaration
            if pk == "regular_variable_declaration" || pk == "label_declaration" {
                return;
            }

            // Skip primary_expression that's part of a simple expression
            if pk == "primary_expression" {
                if let Some(grandparent) = parent.parent() {
                    let gk = grandparent.kind();
                    if matches!(
                        gk,
                        "property_assignment"
                            | "case_label_expression"
                            | "for_statement"
                            | "attribute_argument"
                            | "regular_variable_declaration"
                            | "label_declaration"
                    ) {
                        return;
                    }
                }
            }
        }

        diagnostics.push(LintDiagnostic {
            code: "AL-L008".to_string(),
            message: format!("Magic number '{}' — consider using a named constant", text),
            range: node.range(),
            severity: LintSeverity::Info,
        });
    }
}

// ── AL-L009: Excessive parameters ───────────────────────────────────

fn check_excessive_parameters(
    node: Node,
    source: &[u8],
    config: &LintConfig,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    if let Some(param_list) = node.child_by_field_name("parameters") {
        let mut cursor = param_list.walk();
        let param_count = param_list
            .children(&mut cursor)
            .filter(|c| c.kind() == "parameter")
            .count();

        if param_count > config.max_parameters {
            let name = get_name(node, source);
            diagnostics.push(LintDiagnostic {
                code: "AL-L009".to_string(),
                message: format!(
                    "Procedure '{}' has {} parameters (max {})",
                    name, param_count, config.max_parameters
                ),
                range: node.range(),
                severity: LintSeverity::Warning,
            });
        }
    }
}

// ── AL-L010: Missing CASE else branch ───────────────────────────────

fn check_case_else(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    let mut cursor = node.walk();
    let has_else = node.children(&mut cursor).any(|c| c.kind() == "kw_else");

    if !has_else {
        diagnostics.push(LintDiagnostic {
            code: "AL-L010".to_string(),
            message: "Case statement is missing an else branch".to_string(),
            range: node.range(),
            severity: LintSeverity::Warning,
        });
    }

    let _ = source;
}

// ── AL-L011: Redundant BEGIN..END ───────────────────────────────────

fn check_redundant_begin_end(node: Node, _source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    // Count actual statements inside the begin..end
    let mut cursor = node.walk();
    let mut stmt_count = 0;

    for child in node.children(&mut cursor) {
        if child.kind() == "statement_list" {
            let mut sc = child.walk();
            for stmt in child.children(&mut sc) {
                if stmt.is_named()
                    && !matches!(stmt.kind(), "semicolon" | "comment")
                {
                    stmt_count += 1;
                }
            }
        }
    }

    // Only flag if exactly 1 statement and the parent expects a single statement
    if stmt_count == 1 {
        if let Some(parent) = node.parent() {
            // Only flag for control flow (if, for, while, etc.), not procedure bodies
            if matches!(
                parent.kind(),
                "if_statement"
                    | "for_statement"
                    | "foreach_statement"
                    | "while_statement"
                    | "with_statement"
            ) {
                diagnostics.push(LintDiagnostic {
                    code: "AL-L011".to_string(),
                    message: "Redundant begin..end around single statement".to_string(),
                    range: node.range(),
                    severity: LintSeverity::Hint,
                });
            }
        }
    }
}

// ── AL-L012: Assignment in IF condition ─────────────────────────────

fn check_assignment_in_condition(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    if let Some(condition) = node.child_by_field_name("condition") {
        // Look for lone `=` operators in the condition that might be assignment
        check_suspicious_equals(condition, source, diagnostics);
    }
}

fn check_suspicious_equals(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    // We look for `=` used where `:=` might be intended, but `=` is the correct
    // comparison operator in AL. This is more of a style hint — only flag
    // patterns like `x = y` at the top level of a condition where the user
    // likely meant `:=`.
    // For now, we skip this rule as `=` is the comparison operator in AL.
    // In Pascal-derived languages, assignment is `:=` and comparison is `=`.
    let _ = (node, source, diagnostics);
}

// ── AL-L013: Empty REPEAT..UNTIL loop ───────────────────────────────

fn check_empty_repeat(node: Node, _source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    // A repeat..until is empty if it has no statement_list
    let mut cursor = node.walk();
    let has_statements = node
        .children(&mut cursor)
        .any(|c| c.kind() == "statement_list");

    if !has_statements {
        diagnostics.push(LintDiagnostic {
            code: "AL-L013".to_string(),
            message: "Empty repeat..until loop".to_string(),
            range: node.range(),
            severity: LintSeverity::Warning,
        });
    }
}

// ── AL-L014: Unreachable code after EXIT/ERROR ──────────────────────

fn check_unreachable_after_exit(node: Node, diagnostics: &mut Vec<LintDiagnostic>) {
    // Check if there is a sibling statement after this exit
    if let Some(next) = node.next_named_sibling() {
        if next.kind() != "semicolon" && next.kind() != "comment" {
            // Check if the next sibling is in the same statement_list
            if let Some(parent) = node.parent() {
                if parent.kind() == "statement_list" || parent.kind() == "statement" {
                    diagnostics.push(LintDiagnostic {
                        code: "AL-L014".to_string(),
                        message: "Unreachable code after exit statement".to_string(),
                        range: next.range(),
                        severity: LintSeverity::Warning,
                    });
                }
            }
        }
    }
}

fn check_error_call_unreachable(
    expr: Node,
    stmt_node: Node,
    source: &[u8],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    // Check if this is an Error() call
    let is_error_call = if expr.kind() == "postfix_expression" || expr.kind() == "expression" {
        if let Ok(text) = expr.utf8_text(source) {
            let lower = text.to_lowercase();
            lower.starts_with("error(") || lower.starts_with("error (")
        } else {
            false
        }
    } else {
        false
    };

    if is_error_call {
        // Check for unreachable siblings after this
        if let Some(next) = stmt_node.next_named_sibling() {
            if next.kind() != "semicolon" && next.kind() != "comment" {
                diagnostics.push(LintDiagnostic {
                    code: "AL-L014".to_string(),
                    message: "Unreachable code after Error() call".to_string(),
                    range: next.range(),
                    severity: LintSeverity::Warning,
                });
            }
        }
    }
}

// ── AL-L015: Global variable naming ─────────────────────────────────

fn check_global_variable_naming(
    node: Node,
    source: &[u8],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    // Object-level var sections contain global variables
    // Check that they follow naming conventions (e.g., g prefix, or descriptive names)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "object_variable_declaration"
            || child.kind() == "regular_variable_declaration"
            || child.kind() == "label_declaration"
        {
            check_single_global_var(child, source, diagnostics);
        }
    }
}

fn check_single_global_var(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    if let Some(name_node) = node.child_by_field_name("name") {
        if let Ok(name) = name_node.utf8_text(source) {
            let clean = name.trim_matches('"');
            // Skip if already has a prefix convention
            if clean.len() <= 1 {
                return;
            }
            // Single lowercase letter variables are suspicious for globals
            if clean.len() == 1 && clean.chars().next().is_some_and(|c| c.is_lowercase()) {
                diagnostics.push(LintDiagnostic {
                    code: "AL-L015".to_string(),
                    message: format!(
                        "Global variable '{}' — consider a more descriptive name",
                        clean
                    ),
                    range: name_node.range(),
                    severity: LintSeverity::Info,
                });
            }
        }
    }
}

// ── AL-L016: Procedure naming (PascalCase required) ─────────────────

fn check_procedure_naming(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    if let Some(name_node) = node.child_by_field_name("name") {
        if let Ok(name) = name_node.utf8_text(source) {
            let clean = name.trim_matches('"');
            if clean.is_empty() {
                return;
            }
            // PascalCase: first letter should be uppercase
            let first_char = clean.chars().next().unwrap();
            if first_char.is_lowercase() {
                diagnostics.push(LintDiagnostic {
                    code: "AL-L016".to_string(),
                    message: format!(
                        "Procedure '{}' should use PascalCase (start with uppercase)",
                        clean
                    ),
                    range: name_node.range(),
                    severity: LintSeverity::Warning,
                });
            }
        }
    }
}

// ── AL-L017: Hard-coded text string ─────────────────────────────────

fn check_hardcoded_string(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    // Look for string literals in expression statements that could be user-facing
    find_hardcoded_strings(node, source, diagnostics);
}

fn find_hardcoded_strings(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    if node.kind() == "string" || node.kind() == "verbatim_string" {
        if let Ok(text) = node.utf8_text(source) {
            // Strip quotes
            let inner = text.trim_start_matches("@'").trim_start_matches('\'').trim_end_matches('\'');
            // Skip empty strings, format strings (%1), single characters
            if inner.is_empty() || inner.len() <= 1 {
                return;
            }
            // Skip if it looks like a format placeholder
            if inner.starts_with('%') && inner.len() <= 3 {
                return;
            }
            // Check if parent is a procedure call (Message, Error, Confirm, StrSubstNo, etc.)
            if let Some(parent) = node.parent() {
                let is_in_call = is_user_facing_call_context(parent, source);
                if is_in_call {
                    diagnostics.push(LintDiagnostic {
                        code: "AL-L017".to_string(),
                        message: "Hard-coded text string — consider using a Label variable"
                            .to_string(),
                        range: node.range(),
                        severity: LintSeverity::Info,
                    });
                    return;
                }
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_hardcoded_strings(child, source, diagnostics);
    }
}

fn is_user_facing_call_context(node: Node, source: &[u8]) -> bool {
    // Walk up to find if we are in a call to a user-facing function
    let mut current = node;
    loop {
        if current.kind() == "member_call_suffix" || current.kind() == "call_suffix" {
            return false; // Method calls like Rec.SetFilter('...') are OK
        }
        if current.kind() == "postfix_expression" || current.kind() == "expression" {
            if let Ok(text) = current.utf8_text(source) {
                let lower = text.to_lowercase();
                if lower.starts_with("message(")
                    || lower.starts_with("error(")
                    || lower.starts_with("confirm(")
                    || lower.starts_with("strsubstno(")
                    || lower.starts_with("fieldcaption(")
                {
                    return true;
                }
            }
        }
        current = match current.parent() {
            Some(p) => p,
            None => break,
        };
        // Don't go too far up
        if matches!(
            current.kind(),
            "procedure_declaration"
                | "trigger_declaration"
                | "object_declaration"
                | "begin_end_block"
        ) {
            break;
        }
    }
    false
}

// ── AL-L018: Record variable naming convention ──────────────────────

fn check_record_variable_naming(
    node: Node,
    source: &[u8],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "object_variable_declaration"
            || child.kind() == "regular_variable_declaration"
        {
            if let Some(type_node) = child.child_by_field_name("type") {
                if let Ok(type_text) = type_node.utf8_text(source) {
                    let type_lower = type_text.to_lowercase();
                    // Check if this is a Record variable
                    if type_lower.starts_with("record ") || type_lower == "record" {
                        if let Some(name_node) = child.child_by_field_name("name") {
                            if let Ok(name) = name_node.utf8_text(source) {
                                let clean = name.trim_matches('"');
                                // Record variables should typically match the table name
                                // or use "Rec" / table abbreviation patterns
                                // Flag single-letter names
                                if clean.len() == 1
                                    && clean.chars().next().is_some_and(|c| c.is_lowercase())
                                {
                                    diagnostics.push(LintDiagnostic {
                                        code: "AL-L018".to_string(),
                                        message: format!(
                                            "Record variable '{}' — use a descriptive name matching the table",
                                            clean
                                        ),
                                        range: name_node.range(),
                                        severity: LintSeverity::Info,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Get the name of a declaration node.
fn get_name<'a>(node: Node<'a>, source: &'a [u8]) -> String {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .unwrap_or("(unknown)")
        .trim_matches('"')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlParser;

    fn lint_src(src: &str) -> Vec<LintDiagnostic> {
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        lint(&result.tree, src)
    }

    fn lint_src_with_config(src: &str, config: &LintConfig) -> Vec<LintDiagnostic> {
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        lint_with_config(&result.tree, src, config)
    }

    fn has_code(diagnostics: &[LintDiagnostic], code: &str) -> bool {
        diagnostics.iter().any(|d| d.code == code)
    }

    #[test]
    fn test_l001_empty_begin_end() {
        let src = r#"codeunit 50100 Test
{
    procedure DoNothing()
    begin
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L001"), "Should detect empty begin..end: {:?}", diags);
    }

    #[test]
    fn test_l001_non_empty_begin_end() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        Message('Hello');
    end;
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L001"), "Should not flag non-empty begin..end");
    }

    #[test]
    fn test_l002_long_procedure() {
        // Create a procedure with many lines
        let mut lines = vec![
            "codeunit 50100 Test".to_string(),
            "{".to_string(),
            "    procedure LongProc()".to_string(),
            "    begin".to_string(),
        ];
        for i in 0..110 {
            lines.push(format!("        x := {};", i));
        }
        lines.push("    end;".to_string());
        lines.push("}".to_string());
        let src = lines.join("\n");

        let diags = lint_src(&src);
        assert!(has_code(&diags, "AL-L002"), "Should detect long procedure");
    }

    #[test]
    fn test_l006_empty_trigger() {
        let src = r#"table 50100 Test
{
    trigger OnInsert()
    begin
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L006"), "Should detect empty trigger body: {:?}", diags);
    }

    #[test]
    fn test_l007_todo_comment() {
        let src = r#"codeunit 50100 Test
{
    // TODO: implement this
    procedure DoSomething()
    begin
        Message('Hello');
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L007"), "Should detect TODO comment");
    }

    #[test]
    fn test_l009_excessive_params() {
        let src = r#"codeunit 50100 Test
{
    procedure TooManyParams(a: Integer; b: Integer; c: Integer; d: Integer; e: Integer; f: Integer; g: Integer; h: Integer)
    begin
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L009"), "Should detect excessive parameters: {:?}", diags);
    }

    #[test]
    fn test_l010_missing_case_else() {
        let src = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        case x of
            1:
                Message('one');
        end;
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L010"), "Should detect missing case else: {:?}", diags);
    }

    #[test]
    fn test_l016_procedure_naming() {
        let src = r#"codeunit 50100 Test
{
    procedure badName()
    begin
        Message('Hello');
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L016"), "Should detect non-PascalCase procedure name: {:?}", diags);
    }

    #[test]
    fn test_l016_good_naming() {
        let src = r#"codeunit 50100 Test
{
    procedure GoodName()
    begin
        Message('Hello');
    end;
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L016"), "Should not flag PascalCase procedure name");
    }

    #[test]
    fn test_l004_nested_if() {
        let src = r#"codeunit 50100 Test
{
    procedure DeepNesting()
    begin
        if a then
            if b then
                if c then
                    if d then
                        if e then
                            if f then
                                Message('deep');
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L004"), "Should detect deep if nesting: {:?}", diags);
    }

    #[test]
    fn test_no_false_positives_on_good_code() {
        let src = r#"codeunit 50100 "Good Code"
{
    procedure ProcessData()
    var
        Counter: Integer;
    begin
        Counter := 0;
        if Counter > 0 then begin
            Message('Processing...');
            Counter += 1;
        end;
    end;
}"#;
        let diags = lint_src(src);
        // Should not have L001 (not empty), L016 (PascalCase OK)
        assert!(!has_code(&diags, "AL-L001"));
        assert!(!has_code(&diags, "AL-L016"));
    }

    #[test]
    fn test_lint_empty_source() {
        let diags = lint_src("");
        // Empty file should have no lint issues (or minimal)
        let _ = diags;
    }

    #[test]
    fn test_lint_clean_code_no_errors() {
        let src = r#"codeunit 50100 "Clean"
{
    procedure ValidName()
    var
        x: Integer;
    begin
        x := 42;
    end;
}"#;
        let diags = lint_src(src);
        // Should have no errors for clean code
        let errors: Vec<_> = diags.iter().filter(|d| d.severity == LintSeverity::Error).collect();
        assert!(errors.is_empty(), "Clean code should have no errors, got: {:?}", errors);
    }

    #[test]
    fn test_lint_config_custom_max_proc_lines() {
        // Create a procedure with 12 lines, set max to 10
        let mut lines = vec![
            "codeunit 50100 Test".to_string(),
            "{".to_string(),
            "    procedure ShortButOverLimit()".to_string(),
            "    begin".to_string(),
        ];
        for i in 0..8 {
            lines.push(format!("        x := {};", i));
        }
        lines.push("    end;".to_string());
        lines.push("}".to_string());
        let src = lines.join("\n");

        let config = LintConfig {
            max_procedure_lines: 10,
            ..LintConfig::default()
        };
        let diags = lint_src_with_config(&src, &config);
        assert!(has_code(&diags, "AL-L002"), "Should detect procedure over custom limit");
    }

    #[test]
    fn test_lint_config_custom_max_params() {
        let src = r#"codeunit 50100 Test
{
    procedure FewParams(a: Integer; b: Integer; c: Integer)
    begin
    end;
}"#;
        let config = LintConfig {
            max_parameters: 2,
            ..LintConfig::default()
        };
        let diags = lint_src_with_config(src, &config);
        assert!(has_code(&diags, "AL-L009"), "Should detect excessive params with custom limit of 2");
    }

    #[test]
    fn test_lint_severity_display() {
        assert_eq!(format!("{}", LintSeverity::Error), "error");
        assert_eq!(format!("{}", LintSeverity::Warning), "warning");
        assert_eq!(format!("{}", LintSeverity::Info), "info");
        assert_eq!(format!("{}", LintSeverity::Hint), "hint");
    }

    #[test]
    fn test_lint_rules_returns_17_rules() {
        let rules = lint_rules();
        // AL-L012 is excluded (stub — no-op implementation).
        assert_eq!(rules.len(), 17, "Should have exactly 17 lint rules (AL-L012 excluded as stub)");
        // AL-L012 must not appear in the registry
        assert!(
            !rules.iter().any(|r| r.code == "AL-L012"),
            "AL-L012 (stub) should not be in the registry"
        );
        // All codes must be non-empty and unique
        let mut seen = std::collections::HashSet::new();
        for rule in rules {
            assert!(!rule.code.is_empty(), "Rule code must be non-empty");
            assert!(seen.insert(rule.code), "Duplicate rule code: {}", rule.code);
        }
    }

    #[test]
    fn test_l007_fixme_and_hack_comments() {
        let src = r#"codeunit 50100 Test
{
    // FIXME: broken logic
    procedure A()
    begin
        Message('Hello');
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L007"), "Should detect FIXME comment");

        let src2 = r#"codeunit 50100 Test
{
    // HACK: workaround
    procedure B()
    begin
        Message('Hello');
    end;
}"#;
        let diags2 = lint_src(src2);
        assert!(has_code(&diags2, "AL-L007"), "Should detect HACK comment");
    }
}
