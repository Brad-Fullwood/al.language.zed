//! Native lint rules (tree-sitter only, no semantic knowledge).
//!
//! 18 rules: AL-L001 through AL-L018. Each operates on the tree-sitter AST
//! and raw source text. No type information or cross-file knowledge is used.

use tree_sitter::{Node, Tree};
use crate::traversal::walk_tree;

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
        // AL-L012 (AssignmentInCondition) is intentionally omitted: in AL, `=` is the
        // comparison operator and `:=` is assignment; `:=` cannot appear in an if-condition,
        // so there is no confusion to detect.
        LintRuleInfo { code: "AL-L013", name: "EmptyRepeat", severity: LintSeverity::Warning, description: "Empty repeat..until loop" },
        LintRuleInfo { code: "AL-L014", name: "UnreachableCode", severity: LintSeverity::Warning, description: "Unreachable code after exit/error" },
        LintRuleInfo { code: "AL-L015", name: "GlobalVarNaming", severity: LintSeverity::Info, description: "Global variable has a non-descriptive name" },
        LintRuleInfo { code: "AL-L016", name: "ProcedureNaming", severity: LintSeverity::Warning, description: "Procedure name does not follow PascalCase convention" },
        LintRuleInfo { code: "AL-L017", name: "HardcodedString", severity: LintSeverity::Info, description: "Hard-coded text string — consider using a Label variable" },
        LintRuleInfo { code: "AL-L018", name: "RecordVarNaming", severity: LintSeverity::Info, description: "Record variable should use a descriptive name matching the table" },
        LintRuleInfo { code: "AL-L019", name: "FlowFieldEditable", severity: LintSeverity::Warning, description: "FlowField/FlowFilter field marked Editable = true" },
        LintRuleInfo { code: "AL-L020", name: "SecretTextEnforcement", severity: LintSeverity::Warning, description: "Variable with sensitive name (Password/Secret/ApiKey/Token) should use SecretText type" },
        LintRuleInfo { code: "AL-L021", name: "LockTableDeprecated", severity: LintSeverity::Info, description: "LockTable() is deprecated — use ReadIsolation instead (BC 21+)" },
        LintRuleInfo { code: "AL-L022", name: "ApiPageMandatoryFields", severity: LintSeverity::Warning, description: "API page is missing mandatory properties (ODataKeyFields, EntityName, EntitySetName, APIVersion)" },
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

    walk_and_lint(root, source, text, config, &mut diagnostics);

    // AL-L019: text-based FlowField editability check (AL grammar doesn't parse field
    // properties as structured nodes — braced_block contains raw tokens).
    check_flowfield_editable_text(text, &mut diagnostics);

    // AL-L022: text-based API page mandatory property check
    check_api_page_mandatory_fields_text(text, &mut diagnostics);

    diagnostics
}

/// Walk the tree and apply all lint rules.
fn walk_and_lint(
    root: Node,
    source: &[u8],
    text: &str,
    config: &LintConfig,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    // depth_stack tracks the if_depth that applies when we *enter* a node.
    // When descending into a child, we push the depth the child should use.
    // When ascending back to a parent, we pop back to restore parent's depth.
    let mut depth_stack: Vec<usize> = Vec::new();
    let mut cursor = root.walk();
    let mut current_depth = 0usize;
    let mut did_visit = false;

    loop {
        if !did_visit {
            let node = cursor.node();
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
                    let new_depth = current_depth + 1;
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
                    // Children of this if_statement use new_depth.
                    // We store current_depth on the stack so we can restore it
                    // when we ascend back past this node.
                    // The depth update happens below when goto_first_child succeeds.
                    // We stash new_depth as a sentinel: set current_depth to new_depth
                    // before descending, and push old depth for restoration.
                    if cursor.goto_first_child() {
                        depth_stack.push(current_depth);
                        current_depth = new_depth;
                        did_visit = false;
                        continue;
                    }
                    // No children — fall through to sibling/parent traversal
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

                    // AL-L021: LockTable() deprecated
                    check_locktable_deprecated(node, source, diagnostics);
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

                "regular_variable_declaration" => {
                    // AL-L020: SecretText enforcement for sensitive variable names
                    check_secret_text_enforcement(node, source, diagnostics);
                }

                _ => {}
            }
        }

        if !did_visit && cursor.goto_first_child() {
            depth_stack.push(current_depth);
            did_visit = false;
            continue;
        }
        if cursor.goto_next_sibling() {
            did_visit = false;
            continue;
        }
        if cursor.goto_parent() {
            // Restore the depth that was active when we descended into this subtree
            if let Some(d) = depth_stack.pop() {
                current_depth = d;
            }
            did_visit = true;
            continue;
        }
        break;
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
        // Word-boundary check: the name must not be surrounded by alphanumeric / underscore
        // characters to avoid false positives (e.g. variable "n" matching inside "end").
        if !contains_word(&body, &lower_name) {
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

/// Return `true` if `text` contains `word` as a whole token (word-boundary match).
///
/// A match is accepted only when the character immediately before and after the match
/// are not ASCII alphanumeric or `_`.  This prevents a short variable name (e.g. `n`)
/// from being considered "used" because it appears as a substring of keywords like
/// `end`, `then`, `integer`, etc.
fn contains_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    for (i, _) in text.match_indices(word) {
        let before_ok = i == 0 || {
            let b = text.as_bytes()[i - 1];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        let after_ok = i + word.len() >= text.len() || {
            let b = text.as_bytes()[i + word.len()];
            !b.is_ascii_alphanumeric() && b != b'_'
        };
        if before_ok && after_ok {
            return true;
        }
    }
    false
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
            // Skip empty names
            if clean.is_empty() {
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

fn find_hardcoded_strings(root: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    walk_tree(root, &mut |node| {
        if node.kind() == "string" || node.kind() == "verbatim_string" {
            if let Ok(text) = node.utf8_text(source) {
                // Strip quotes
                let inner = text.trim_start_matches("@'").trim_start_matches('\'').trim_end_matches('\'');
                // Skip empty strings, format strings (%1), single characters
                if !inner.is_empty() && inner.len() > 1 {
                    // Skip if it looks like a format placeholder
                    let skip = inner.starts_with('%') && inner.len() <= 3;
                    if !skip {
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
                            }
                        }
                    }
                }
            }
        }
    });
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

// ── AL-L019: FlowField Editable = true (text-based) ─────────────────
//
// The AL tree-sitter grammar parses field properties as raw tokens in a
// braced_block — not as structured property_assignment nodes.
// Use text-based scanning (same approach as audit.rs).

fn check_flowfield_editable_text(text: &str, diagnostics: &mut Vec<LintDiagnostic>) {
    struct FieldCtx {
        is_flowfield: bool,
        editable_true_line: Option<u32>,
        brace_depth: i32,
    }

    let mut stack: Vec<FieldCtx> = Vec::new();

    for (idx, line) in text.lines().enumerate() {
        let line_num = idx as u32;
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Detect `field(` opening
        if lower.starts_with("field(") || lower.starts_with("field (") {
            stack.push(FieldCtx {
                is_flowfield: false,
                editable_true_line: None,
                brace_depth: crate::count_net_delimiters(line, '{', '}'),
            });
            continue;
        }

        if let Some(ctx) = stack.last_mut() {
            ctx.brace_depth += crate::count_net_delimiters(line, '{', '}');

            // Detect FieldClass = FlowField / FlowFilter
            if lower.contains("fieldclass") {
                let no_ws: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
                if no_ws.contains("fieldclass=flowfield") || no_ws.contains("fieldclass=flowfilter") {
                    ctx.is_flowfield = true;
                }
            }

            // Detect Editable = true
            if lower.contains("editable") {
                let no_ws: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
                if no_ws.contains("editable=true") {
                    ctx.editable_true_line = Some(line_num);
                }
            }

            // End of field block
            if ctx.brace_depth <= 0 {
                let ctx = stack.pop().unwrap();
                if ctx.is_flowfield {
                    if let Some(editable_line) = ctx.editable_true_line {
                        let range = tree_sitter::Range {
                            start_byte: 0,
                            end_byte: 0,
                            start_point: tree_sitter::Point { row: editable_line as usize, column: 0 },
                            end_point: tree_sitter::Point { row: editable_line as usize, column: 80 },
                        };
                        diagnostics.push(LintDiagnostic {
                            code: "AL-L019".to_string(),
                            message: "FlowField/FlowFilter field is marked Editable = true — FlowFields are computed and cannot be edited directly".to_string(),
                            range,
                            severity: LintSeverity::Warning,
                        });
                    }
                }
            }
        }
    }
}


// ── AL-L020: SecretText enforcement ─────────────────────────────────

/// Sensitive name patterns that should use SecretText instead of Text.
const SECRET_PATTERNS: &[&str] = &["password", "secret", "apikey", "token", "privatekey"];

fn check_secret_text_enforcement(
    node: Node,
    source: &[u8],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let name_node = match node.child_by_field_name("name") {
        Some(n) => n,
        None => return,
    };
    let name = match name_node.utf8_text(source) {
        Ok(n) => n.trim_matches('"').to_lowercase(),
        Err(_) => return,
    };

    let is_sensitive = SECRET_PATTERNS.iter().any(|pat| name.contains(pat));
    if !is_sensitive {
        return;
    }

    // Check if the type is Text (not SecretText)
    let type_node = match node.child_by_field_name("type") {
        Some(n) => n,
        None => return,
    };
    let type_text = match type_node.utf8_text(source) {
        Ok(t) => t.trim().to_lowercase(),
        Err(_) => return,
    };

    // Text[n] or plain Text — but not SecretText
    if (type_text.starts_with("text") && !type_text.starts_with("secrettext"))
        || type_text == "code"
    {
        diagnostics.push(LintDiagnostic {
            code: "AL-L020".to_string(),
            message: format!(
                "Variable '{}' has a sensitive name — use SecretText instead of {}",
                name_node.utf8_text(source).unwrap_or("").trim_matches('"'),
                type_node.utf8_text(source).unwrap_or("Text").trim(),
            ),
            range: name_node.range(),
            severity: LintSeverity::Warning,
        });
    }
}

// ── AL-L021: LockTable deprecated ───────────────────────────────────

fn check_locktable_deprecated(node: Node, source: &[u8], diagnostics: &mut Vec<LintDiagnostic>) {
    if let Ok(text) = node.utf8_text(source) {
        let lower = text.trim().to_lowercase();
        if lower.contains(".locktable()") || lower.contains(".locktable ()") {
            diagnostics.push(LintDiagnostic {
                code: "AL-L021".to_string(),
                message: "LockTable() is deprecated — use ReadIsolation property or SetLoadFields() with ReadIsolation instead (BC 21+)".to_string(),
                range: node.range(),
                severity: LintSeverity::Info,
            });
        }
    }
}

// ── AL-L022: API page mandatory field validation (text-based) ────────
//
// AL page objects have top-level property assignments as raw identifier
// tokens, not structured AST nodes.  Scan the file text directly.

fn check_api_page_mandatory_fields_text(text: &str, diagnostics: &mut Vec<LintDiagnostic>) {
    // Only applies to page objects.  Scan lines for:
    //   page <id> "<name>"  { ... }
    // and within the object body check for APIVersion, EntityName, EntitySetName, ODataKeyFields.
    //
    // We scan until we find a line starting with "page " and then track brace depth
    // to stay within the object, collecting property lines.

    struct PageCtx {
        start_line: u32,
        brace_depth: i32,
        has_api_version: bool,
        api_version_line: Option<u32>,
        has_entity_name: bool,
        has_entity_set_name: bool,
        has_odata_key_fields: bool,
    }

    let mut page_ctx: Option<PageCtx> = None;

    for (idx, line) in text.lines().enumerate() {
        let line_num = idx as u32;
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();

        // Detect page object start
        if page_ctx.is_none() && lower.starts_with("page ") {
            page_ctx = Some(PageCtx {
                start_line: line_num,
                brace_depth: crate::count_net_delimiters(line, '{', '}'),
                has_api_version: false,
                api_version_line: None,
                has_entity_name: false,
                has_entity_set_name: false,
                has_odata_key_fields: false,
            });
            continue;
        }

        if let Some(ctx) = page_ctx.as_mut() {
            ctx.brace_depth += crate::count_net_delimiters(line, '{', '}');

            // Scan property assignments (only at the top level of the page object, depth ~1)
            if ctx.brace_depth == 1 {
                let no_ws: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
                if no_ws.starts_with("apiversion=") || no_ws.starts_with("apiversion =") {
                    ctx.has_api_version = true;
                    ctx.api_version_line = Some(line_num);
                }
                if no_ws.starts_with("entityname=") {
                    ctx.has_entity_name = true;
                }
                if no_ws.starts_with("entitysetname=") {
                    ctx.has_entity_set_name = true;
                }
                if no_ws.starts_with("odatakeyfields=") {
                    ctx.has_odata_key_fields = true;
                }
            }

            // End of page object
            if ctx.brace_depth <= 0 {
                let ctx = page_ctx.take().unwrap();

                // Only emit if this is an API page
                if ctx.has_api_version {
                    let diag_line = ctx.api_version_line.unwrap_or(ctx.start_line);
                    let range = tree_sitter::Range {
                        start_byte: 0,
                        end_byte: 0,
                        start_point: tree_sitter::Point { row: diag_line as usize, column: 0 },
                        end_point: tree_sitter::Point { row: diag_line as usize, column: 80 },
                    };
                    if !ctx.has_entity_name {
                        diagnostics.push(LintDiagnostic {
                            code: "AL-L022".to_string(),
                            message: "API page is missing mandatory property 'EntityName'".to_string(),
                            range,
                            severity: LintSeverity::Warning,
                        });
                    }
                    if !ctx.has_entity_set_name {
                        diagnostics.push(LintDiagnostic {
                            code: "AL-L022".to_string(),
                            message: "API page is missing mandatory property 'EntitySetName'".to_string(),
                            range,
                            severity: LintSeverity::Warning,
                        });
                    }
                    if !ctx.has_odata_key_fields {
                        diagnostics.push(LintDiagnostic {
                            code: "AL-L022".to_string(),
                            message: "API page is missing mandatory property 'ODataKeyFields'".to_string(),
                            range,
                            severity: LintSeverity::Warning,
                        });
                    }
                }
            }
        }
    }
}


// ── Helpers ─────────────────────────────────────────────────────────

/// Get the name of a declaration node.
fn get_name(node: Node, source: &[u8]) -> String {
    node.child_by_field_name("name")
        .map(|n| crate::node_text_or(n, source, "(unknown)"))
        .unwrap_or_else(|| "(unknown)".to_string())
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
    fn test_lint_rules_returns_21_rules() {
        let rules = lint_rules();
        // AL-L012 is excluded (stub — no-op implementation).
        assert_eq!(rules.len(), 21, "Should have exactly 21 lint rules (AL-L012 excluded as stub)");
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

    // ── T1801: FlowField editability ──────────────────────────────────

    #[test]
    fn test_l019_flowfield_editable_true() {
        let src = r#"table 50100 "Test Table"
{
    fields
    {
        field(1; "Amount Sold"; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = sum("Sales Line".Amount);
            Editable = true;
        }
    }
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L019"), "Should detect FlowField with Editable=true: {:?}", diags);
    }

    #[test]
    fn test_l019_flowfield_no_editable_no_warn() {
        let src = r#"table 50100 "Test Table"
{
    fields
    {
        field(1; "Amount Sold"; Decimal)
        {
            FieldClass = FlowField;
            CalcFormula = sum("Sales Line".Amount);
        }
    }
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L019"), "Should not warn when Editable not set: {:?}", diags);
    }

    #[test]
    fn test_l019_normal_field_editable_no_warn() {
        let src = r#"table 50100 "Test Table"
{
    fields
    {
        field(1; Name; Text[50])
        {
            Editable = true;
        }
    }
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L019"), "Normal field with Editable=true is fine: {:?}", diags);
    }

    // ── T1803: SecretText enforcement ─────────────────────────────────

    #[test]
    fn test_l020_password_as_text_warns() {
        let src = r#"codeunit 50100 Test
{
    procedure Login()
    var
        Password: Text[100];
    begin
        Password := 'secret';
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L020"), "Should warn on Text variable named Password: {:?}", diags);
    }

    #[test]
    fn test_l020_apikey_as_text_warns() {
        let src = r#"codeunit 50100 Test
{
    procedure Connect()
    var
        ApiKey: Text[50];
    begin
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L020"), "Should warn on Text variable named ApiKey: {:?}", diags);
    }

    #[test]
    fn test_l020_secrettext_no_warn() {
        let src = r#"codeunit 50100 Test
{
    procedure Login()
    var
        Password: SecretText;
    begin
    end;
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L020"), "SecretText variable should not warn: {:?}", diags);
    }

    #[test]
    fn test_l020_non_sensitive_name_no_warn() {
        let src = r#"codeunit 50100 Test
{
    procedure Process()
    var
        CustomerName: Text[100];
    begin
    end;
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L020"), "Non-sensitive name should not warn: {:?}", diags);
    }

    // ── T1804: ReadIsolation over LockTable ───────────────────────────

    #[test]
    fn test_l021_locktable_warns() {
        let src = r#"codeunit 50100 Test
{
    procedure ProcessItem()
    var
        Item: Record Item;
    begin
        Item.LockTable();
        if Item.Get('ITEM001') then
            Item.Modify();
    end;
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L021"), "Should warn on LockTable(): {:?}", diags);
    }

    #[test]
    fn test_l021_no_locktable_no_warn() {
        let src = r#"codeunit 50100 Test
{
    procedure ProcessItem()
    var
        Item: Record Item;
    begin
        if Item.Get('ITEM001') then
            Item.Modify();
    end;
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L021"), "No LockTable should not warn: {:?}", diags);
    }

    // ── T1805: API page mandatory fields ─────────────────────────────

    #[test]
    fn test_l022_api_page_missing_odatakeyfields() {
        let src = r#"page 50100 "Customer API"
{
    APIVersion = 'v2.0';
    EntityName = 'customer';
    EntitySetName = 'customers';

    layout
    {
        area(content)
        {
            group(General)
            {
                field(number; Rec."No.") { }
            }
        }
    }
}"#;
        let diags = lint_src(src);
        assert!(has_code(&diags, "AL-L022"), "Should warn when ODataKeyFields missing: {:?}", diags);
    }

    #[test]
    fn test_l022_api_page_complete_no_warn() {
        let src = r#"page 50100 "Customer API"
{
    APIVersion = 'v2.0';
    EntityName = 'customer';
    EntitySetName = 'customers';
    ODataKeyFields = "No.";

    layout
    {
        area(content)
        {
            group(General)
            {
                field(number; Rec."No.") { }
            }
        }
    }
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L022"), "Complete API page should not warn: {:?}", diags);
    }

    #[test]
    fn test_l022_non_api_page_no_warn() {
        let src = r#"page 50100 "Customer List"
{
    layout
    {
        area(content)
        {
            group(General)
            {
                field(number; Rec."No.") { }
            }
        }
    }
}"#;
        let diags = lint_src(src);
        assert!(!has_code(&diags, "AL-L022"), "Non-API page should not warn: {:?}", diags);
    }
}
