//! Cyclomatic and cognitive complexity metrics for AL procedures.
//!
//! Cyclomatic complexity: counts decision points (if, for, while, repeat, case, and, or).
//! Cognitive complexity: weights nested structures more heavily (Sonar-style).

use tree_sitter::{Node, Tree};

/// Complexity measurement for a single procedure.
#[derive(Debug, Clone)]
pub struct ProcedureComplexity {
    /// Procedure name.
    pub name: String,
    /// Cyclomatic complexity (decision-point count + 1).
    pub cyclomatic: u32,
    /// Cognitive complexity (weighted nesting score).
    pub cognitive: u32,
    /// Start line (1-based).
    pub line: u32,
}

/// Compute complexity metrics for all procedures in a parsed tree.
pub fn compute_complexity(tree: &Tree, text: &str) -> Vec<ProcedureComplexity> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut results = Vec::new();

    collect_procedure_complexity(root, source, &mut results);
    results
}

fn collect_procedure_complexity(node: Node, source: &[u8], results: &mut Vec<ProcedureComplexity>) {
    if matches!(
        node.kind(),
        "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
    ) {
        let name = node
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .unwrap_or("(unknown)")
            .trim_matches('"')
            .to_string();

        let line = node.start_position().row as u32 + 1;
        let cyclomatic = compute_cyclomatic(node, source);
        let cognitive = compute_cognitive(node, source, 0);

        results.push(ProcedureComplexity {
            name,
            cyclomatic,
            cognitive,
            line,
        });
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_procedure_complexity(child, source, results);
    }
}

/// Cyclomatic complexity: 1 (base) + number of branching decision points.
fn compute_cyclomatic(proc_node: Node, source: &[u8]) -> u32 {
    let mut count = 1u32; // base path
    count_cyclomatic_decisions(proc_node, source, &mut count);
    count
}

fn count_cyclomatic_decisions(node: Node, source: &[u8], count: &mut u32) {
    match node.kind() {
        "if_statement" | "empty_if_statement" => *count += 1,
        "for_statement" | "foreach_statement" | "while_statement" | "repeat_statement" => {
            *count += 1
        }
        "case_statement" => {
            // Each case arm adds a branch
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "case_arm" || child.kind() == "case_element" {
                    *count += 1;
                }
            }
        }
        "binary_expression" => {
            // AND/OR operators add paths
            if let Some(op_node) = node.child_by_field_name("op") {
                if let Ok(op) = op_node.utf8_text(source) {
                    let lower = op.to_lowercase();
                    if lower == "and" || lower == "or" {
                        *count += 1;
                    }
                }
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count_cyclomatic_decisions(child, source, count);
    }
}

/// Cognitive complexity: increments for structural nesting, with nesting multiplier.
fn compute_cognitive(node: Node, source: &[u8], nesting: u32) -> u32 {
    let mut total = 0u32;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let kind = child.kind();
        match kind {
            "if_statement" | "empty_if_statement" => {
                total += 1 + nesting; // +1 for structure, +nesting for depth
                total += compute_cognitive(child, source, nesting + 1);
            }
            "for_statement" | "foreach_statement" | "while_statement" | "repeat_statement" => {
                total += 1 + nesting;
                total += compute_cognitive(child, source, nesting + 1);
            }
            "case_statement" => {
                total += 1 + nesting;
                // Each arm counts as 1 at the same level
                let arm_count = count_case_arms(child);
                total += arm_count;
                total += compute_cognitive(child, source, nesting + 1);
            }
            "binary_expression" => {
                // Boolean operators: count sequences
                if let Some(op_node) = child.child_by_field_name("op") {
                    if let Ok(op) = op_node.utf8_text(source) {
                        let lower = op.to_lowercase();
                        if lower == "and" || lower == "or" {
                            total += 1;
                        }
                    }
                }
                total += compute_cognitive(child, source, nesting);
            }
            _ => {
                total += compute_cognitive(child, source, nesting);
            }
        }
    }
    total
}

fn count_case_arms(case_node: Node) -> u32 {
    let mut cursor = case_node.walk();
    case_node
        .children(&mut cursor)
        .filter(|c| c.kind() == "case_arm" || c.kind() == "case_element")
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AlParser;

    fn complexity_for(src: &str) -> Vec<ProcedureComplexity> {
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        compute_complexity(&result.tree, src)
    }

    #[test]
    fn simple_procedure_complexity_1() {
        let src = r#"codeunit 50100 Test
{
    procedure Simple()
    begin
        Message('Hello');
    end;
}"#;
        let metrics = complexity_for(src);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].name, "Simple");
        assert_eq!(metrics[0].cyclomatic, 1, "No branches = cyclomatic 1");
        assert_eq!(metrics[0].cognitive, 0, "No nesting = cognitive 0");
    }

    #[test]
    fn if_adds_cyclomatic() {
        let src = r#"codeunit 50100 Test
{
    procedure WithIf()
    var
        x: Integer;
    begin
        if x > 0 then
            Message('positive');
    end;
}"#;
        let metrics = complexity_for(src);
        assert_eq!(metrics.len(), 1);
        assert!(
            metrics[0].cyclomatic >= 2,
            "if adds 1: got {}",
            metrics[0].cyclomatic
        );
    }

    #[test]
    fn nested_if_increases_cognitive() {
        let src = r#"codeunit 50100 Test
{
    procedure NestedIfs()
    var
        x: Integer;
        y: Integer;
    begin
        if x > 0 then
            if y > 0 then
                Message('both positive');
    end;
}"#;
        let metrics = complexity_for(src);
        assert_eq!(metrics.len(), 1);
        // Outer if: +1 (nesting=0), inner if: +1+1 (nesting=1) = total 3
        assert!(
            metrics[0].cognitive >= 3,
            "Nested if should have higher cognitive: got {}",
            metrics[0].cognitive
        );
    }

    #[test]
    fn complexity_empty() {
        let metrics = complexity_for("");
        assert!(metrics.is_empty());
    }

    #[test]
    fn complexity_name_correct() {
        let src = r#"codeunit 50100 Test
{
    procedure ProcessOrder()
    begin
    end;
}"#;
        let metrics = complexity_for(src);
        assert_eq!(metrics[0].name, "ProcessOrder");
    }
}
