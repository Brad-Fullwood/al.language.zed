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
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        if matches!(
            current.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
        ) {
            let name = current
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
                .unwrap_or("(unknown)")
                .trim_matches('"')
                .to_string();

            let line = current.start_position().row as u32 + 1;
            let cyclomatic = compute_cyclomatic(current, source);
            let cognitive = compute_cognitive(current, source);

            results.push(ProcedureComplexity {
                name,
                cyclomatic,
                cognitive,
                line,
            });
            // Don't recurse into procedure bodies for nested procedures
            continue;
        }

        for i in (0..current.child_count()).rev() {
            if let Some(child) = current.child(i) {
                stack.push(child);
            }
        }
    }
}

/// Cyclomatic complexity: 1 (base) + number of branching decision points.
fn compute_cyclomatic(proc_node: Node, source: &[u8]) -> u32 {
    let mut count = 1u32; // base path
    count_cyclomatic_decisions(proc_node, source, &mut count);
    count
}

fn count_cyclomatic_decisions(node: Node, _source: &[u8], count: &mut u32) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        match current.kind() {
            "if_statement" | "empty_if_statement" => *count += 1,
            "for_statement" | "foreach_statement" | "while_statement" | "repeat_statement" => {
                *count += 1
            }
            "case_statement" => {
                let mut cursor = current.walk();
                for child in current.children(&mut cursor) {
                    if child.kind() == "case_branch" {
                        *count += 1;
                    }
                }
            }
            // Binary operators: the grammar does NOT wrap binary expressions in a
            // `binary_expression` node — operators (op_and, op_or, etc.) appear as
            // direct children inside expression nodes. Each AND/OR adds a path.
            "expression" => {
                let mut c = current.walk();
                for child in current.children(&mut c) {
                    if matches!(child.kind(), "op_and" | "op_or") {
                        *count += 1;
                    }
                }
            }
            _ => {}
        }

        for i in (0..current.child_count()).rev() {
            if let Some(child) = current.child(i) {
                stack.push(child);
            }
        }
    }
}

/// Cognitive complexity: increments for structural nesting, with nesting multiplier.
fn compute_cognitive(node: Node, _source: &[u8]) -> u32 {
    let mut total = 0u32;
    // Stack holds (node, nesting_depth)
    let mut stack: Vec<(Node, u32)> = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        stack.push((child, 0));
    }

    while let Some((current, nesting)) = stack.pop() {
        let kind = current.kind();
        match kind {
            "if_statement" | "empty_if_statement" => {
                total += 1 + nesting;
                let mut cur = current.walk();
                for child in current.children(&mut cur) {
                    stack.push((child, nesting + 1));
                }
            }
            "for_statement" | "foreach_statement" | "while_statement" | "repeat_statement" => {
                total += 1 + nesting;
                let mut cur = current.walk();
                for child in current.children(&mut cur) {
                    stack.push((child, nesting + 1));
                }
            }
            "case_statement" => {
                total += 1 + nesting;
                // Each arm counts as 1 at the same level
                total += count_case_arms(current);
                let mut cur = current.walk();
                for child in current.children(&mut cur) {
                    stack.push((child, nesting + 1));
                }
            }
            "expression" => {
                // Boolean operators: count sequences. The grammar does NOT
                // wrap binary expressions in a binary_expression node — the
                // operators (op_and, op_or) are direct children of expression.
                let mut cur = current.walk();
                for child in current.children(&mut cur) {
                    if matches!(child.kind(), "op_and" | "op_or") {
                        total += 1;
                    }
                }
                let mut cur = current.walk();
                for child in current.children(&mut cur) {
                    stack.push((child, nesting));
                }
            }
            _ => {
                let mut cur = current.walk();
                for child in current.children(&mut cur) {
                    stack.push((child, nesting));
                }
            }
        }
    }
    total
}

fn count_case_arms(case_node: Node) -> u32 {
    let mut cursor = case_node.walk();
    case_node
        .children(&mut cursor)
        .filter(|c| c.kind() == "case_branch")
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::AlParser;

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

    /// Regression: cyclomatic complexity must count `case_branch` arms.
    /// Before this fix the matcher looked for `case_arm`/`case_element`,
    /// which the grammar never emits — so case statements added nothing
    /// to cyclomatic complexity.
    #[test]
    fn case_branch_arms_increment_cyclomatic() {
        // Use begin/end blocks per arm so the grammar produces three distinct
        // `case_branch` nodes (statement_list bodies otherwise greedily swallow
        // following labels as `:` binary expressions).
        let src = r#"codeunit 50100 T
{
    procedure P()
    var
        x: Integer;
    begin
        case x of
            1:
                begin
                    Message('a');
                end;
            2:
                begin
                    Message('b');
                end;
            3:
                begin
                    Message('c');
                end;
        end;
    end;
}"#;
        let metrics = complexity_for(src);
        assert_eq!(metrics.len(), 1);
        assert!(
            metrics[0].cyclomatic >= 4,
            "expected at least base 1 + 3 case arms = 4, got {} — case_branch is not being counted",
            metrics[0].cyclomatic
        );
    }
}
