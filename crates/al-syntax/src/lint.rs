//! Native syntax lint rules.
//!
//! Codes use the `AL-NL*` namespace to remain distinct from semantic bridge
//! diagnostics and the `AL-NC*` native workspace checks:
//!
//! - `AL-NL001`: `FindFirst`/`FindLast` called inside a loop (N+1 query risk).
//! - `AL-NL002`: a table field with no `DataClassification` property.
//!
//! Both rules scan text within parsed syntax boundaries because the AL grammar
//! has no dedicated `field_declaration` node.

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

/// Reserved for future rule-specific options.
#[derive(Debug, Clone, Default)]
pub struct LintConfig;

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
    diagnostics
}

/// Run all native lint rules on the parsed tree with custom config.
///
/// This is currently equivalent to [`lint`].
pub fn lint_with_config(tree: &Tree, text: &str, _config: &LintConfig) -> Vec<LintDiagnostic> {
    lint(tree, text)
}

/// Build a `tree_sitter::Range` covering an entire source line, given its
/// 0-based row index. Line-based (not node-based) because both rules below
/// are text scans, not AST queries.
fn line_range(text: &str, line_idx: usize, line: &str) -> Range {
    let mut byte_offset = 0usize;
    for (idx, l) in text.lines().enumerate() {
        if idx == line_idx {
            break;
        }
        // `.lines()` strips the line terminator; account for it when re-deriving
        // byte offsets, so this only needs to be exact for LF-terminated text
        // (which is what the rest of the toolchain assumes elsewhere too).
        byte_offset += l.len() + 1;
    }
    let start_byte = byte_offset;
    let end_byte = byte_offset + line.len();
    Range {
        start_byte,
        end_byte,
        start_point: Point {
            row: line_idx,
            column: 0,
        },
        end_point: Point {
            row: line_idx,
            column: line.len(),
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
    let mut in_double = false;
    for ch in line.chars() {
        match ch {
            '\'' if !in_double => {
                in_single = !in_single;
                out.push(ch);
            }
            '"' if !in_single => {
                in_double = !in_double;
                out.push(ch);
            }
            _ if in_single || in_double => out.push(' '),
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

    struct FieldCtx {
        start_line: usize,
        has_classification: bool,
        brace_depth: i32,
    }

    let mut stack: Vec<FieldCtx> = Vec::new();

    for (line_idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        let open = line.chars().filter(|&c| c == '{').count() as i32;
        let close = line.chars().filter(|&c| c == '}').count() as i32;

        if lower.starts_with("field(") || lower.starts_with("field (") {
            stack.push(FieldCtx {
                start_line: line_idx,
                has_classification: false,
                brace_depth: open - close,
            });
            continue;
        }

        if let Some(ctx) = stack.last_mut() {
            ctx.brace_depth += open - close;
            if lower.contains("dataclassification") {
                ctx.has_classification = true;
            }
            if ctx.brace_depth <= 0 {
                let Some(ctx) = stack.pop() else {
                    continue;
                };
                if !ctx.has_classification {
                    let field_line = text.lines().nth(ctx.start_line).unwrap_or("");
                    out.push(LintDiagnostic {
                        code: "AL-NL002".to_string(),
                        message: "Table field has no DataClassification property.".to_string(),
                        range: line_range(text, ctx.start_line, field_line),
                        severity: LintSeverity::Warning,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::AlParser;

    #[test]
    fn lint_rules_lists_the_starter_set() {
        let rules = lint_rules();
        assert_eq!(rules.len(), 2);
        assert!(rules.iter().any(|r| r.code == "AL-NL001"));
        assert!(rules.iter().any(|r| r.code == "AL-NL002"));
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
    fn lint_config_has_default() {
        let _cfg: LintConfig = Default::default();
    }
}
