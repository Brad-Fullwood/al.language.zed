//! Cyclomatic and cognitive complexity metrics for AL procedures.
//!
//! Cyclomatic complexity: counts decision points (if, for, while, repeat, case, and, or).
//! Cognitive complexity: weights nested structures more heavily (Sonar-style).

use tree_sitter::{Node, Tree};

#[derive(Debug, Clone)]
pub struct ProcedureComplexity {
    pub name: String,
    /// Cyclomatic complexity (decision-point count + 1).
    pub cyclomatic: u32,
    /// Cognitive complexity (weighted nesting score).
    pub cognitive: u32,
    /// Start line (1-based).
    pub line: u32,
    /// Procedure-declaration nesting depth (zero for an object-level member).
    pub nesting_depth: u32,
}

pub fn compute_complexity(tree: &Tree, text: &str) -> Vec<ProcedureComplexity> {
    let root = tree.root_node();
    let source = text.as_bytes();
    let mut results = Vec::new();

    collect_procedure_complexity(root, source, &mut results);
    results
}

fn collect_procedure_complexity(node: Node, source: &[u8], results: &mut Vec<ProcedureComplexity>) {
    let mut stack = vec![(node, 0u32)];
    while let Some((current, nesting_depth)) = stack.pop() {
        if is_procedure_declaration(current) {
            let name = crate::node_name_or(current, source, "(unknown)");

            let line = current.start_position().row as u32 + 1;
            let cyclomatic = compute_cyclomatic(current, source);
            let cognitive = compute_cognitive(current, source);

            results.push(ProcedureComplexity {
                name,
                cyclomatic,
                cognitive,
                line,
                nesting_depth,
            });
        }

        for i in (0..current.child_count()).rev() {
            if let Some(child) = current.child(i) {
                stack.push((
                    child,
                    nesting_depth + u32::from(is_procedure_declaration(current)),
                ));
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

fn count_cyclomatic_decisions(node: Node, source: &[u8], count: &mut u32) {
    let mut stack = vec![node];
    while let Some(current) = stack.pop() {
        // Nested declarations have their own metric entry. Their decisions
        // must not inflate the enclosing procedure's score.
        if current != node && is_procedure_declaration(current) {
            continue;
        }
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
            // `binary_expression` node — each operator appears as a
            // `(binary_operator (operator_word))` child inside expression nodes
            // (the standalone `op_and`/`op_or` tokens are never emitted).
            // Each AND/OR adds a path.
            "expression" => {
                let mut c = current.walk();
                for child in current.children(&mut c) {
                    if is_and_or_operator(child, source) {
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

/// True for a `binary_operator` node whose word operator is `and`/`or`
/// (case-insensitive — AL keywords are case-insensitive).
fn is_and_or_operator(node: Node, source: &[u8]) -> bool {
    if node.kind() != "binary_operator" {
        return false;
    }
    let Some(word) = (0..node.child_count())
        .filter_map(|i| node.child(i))
        .find(|child| child.kind() == "operator_word")
    else {
        return false;
    };
    matches!(
        word.utf8_text(source),
        Ok(text) if text.eq_ignore_ascii_case("and") || text.eq_ignore_ascii_case("or")
    )
}

/// Cognitive complexity: increments for structural nesting, with nesting multiplier.
fn compute_cognitive(node: Node, source: &[u8]) -> u32 {
    let mut total = 0u32;
    // Stack holds (node, nesting_depth)
    let mut stack: Vec<(Node, u32)> = Vec::new();

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        stack.push((child, 0));
    }

    while let Some((current, nesting)) = stack.pop() {
        // As with cyclomatic complexity, a nested declaration is a separate
        // unit of analysis rather than a cognitive branch of its parent.
        if is_procedure_declaration(current) {
            continue;
        }
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
                // wrap binary expressions in a binary_expression node — each
                // operator is a `(binary_operator (operator_word))` child of
                // the expression node.
                let mut cur = current.walk();
                for child in current.children(&mut cur) {
                    if is_and_or_operator(child, source) {
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

fn is_procedure_declaration(node: Node) -> bool {
    matches!(node.kind(), "procedure_declaration" | "trigger_declaration")
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
    fn and_or_word_operators_add_decision_points() {
        // Word operators lex as `(binary_operator (operator_word))`, not as
        // the grammar's never-emitted `op_and`/`op_or` tokens. `a and b or c`
        // must contribute two decision points.
        let src = r#"codeunit 50100 Test
{
    procedure WithBoolOps()
    var
        a: Boolean;
        b: Boolean;
        c: Boolean;
    begin
        if a and b or c then
            Message('yes');
    end;
}"#;
        let metrics = complexity_for(src);
        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].cyclomatic, 4,
            "base 1 + if + and + or = 4, got {}",
            metrics[0].cyclomatic
        );
        assert_eq!(
            metrics[0].cognitive, 3,
            "if (+1) + and (+1) + or (+1) = 3, got {}",
            metrics[0].cognitive
        );
    }

    #[test]
    fn xor_and_div_word_operators_are_not_decision_points() {
        let src = r#"codeunit 50100 Test
{
    procedure NoBranching()
    var
        a: Boolean;
        b: Boolean;
        x: Integer;
    begin
        a := a xor b;
        x := x div 2;
    end;
}"#;
        let metrics = complexity_for(src);
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].cyclomatic, 1, "xor/div add no paths");
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

    #[test]
    fn separate_procedures_have_independent_complexity_scores() {
        // Keep the two decision sets deliberately different: this guards the
        // traversal contract used for nested declarations too, where a
        // declaration's descendants must never inflate its parent's score.
        let src = r#"codeunit 50100 T
{
    procedure Outer()
    begin
        if OuterCondition then
            Message('outer');
    end;

    procedure Inner()
    begin
        if FirstInnerCondition then
            if SecondInnerCondition then
                Message('inner');
    end;
}"#;

        let metrics = complexity_for(src);
        assert_eq!(metrics.len(), 2);

        let outer = metrics
            .iter()
            .find(|metric| metric.name == "Outer")
            .unwrap();
        assert_eq!(outer.nesting_depth, 0);
        assert_eq!(outer.cyclomatic, 2, "only Outer’s if belongs to Outer");
        assert_eq!(outer.cognitive, 1);

        let inner = metrics
            .iter()
            .find(|metric| metric.name == "Inner")
            .unwrap();
        assert_eq!(inner.nesting_depth, 0);
        assert_eq!(inner.cyclomatic, 3, "base path plus Inner’s two ifs");
        assert_eq!(inner.cognitive, 3, "nested Inner if has cognitive weight");
    }
}
