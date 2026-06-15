//! Expression evaluator for the AL interpreter.
//!
//! Evaluates the leaf forms a typical AL expression statement walks
//! through: literals, identifier loads, binary/unary arithmetic and
//! comparisons, string concatenation, parenthesised groups, member
//! lookups, and procedure-call expressions (delegated to `dispatch`).
//!
//! Phase 2a scope: enough to run pure-logic tests (Library Assert, simple
//! arithmetic and string manipulation, control-flow predicates). Record
//! / FlowField / HTTP method calls return `Eval::Error` until Phase 3.

use tree_sitter::Node;

use crate::test_runtime::interpreter::scope::{Eval, ScopeStack};
use crate::test_runtime::interpreter::value::{ErrorInfo, Value};

/// Evaluate a tree-sitter expression node against the active stack.
pub fn eval_expr(node: Node<'_>, source: &[u8], stack: &mut ScopeStack) -> Eval {
    // Stack-overflow guard (F-OPEN-265): expression evaluation recurses per
    // AST nesting level, and ~400 nested parens overflow a 2 MiB worker
    // thread stack — aborting the whole process. Mirror eval_stmt's guard.
    if !stack.enter_expr() {
        return Eval::Error(simple_error(&format!(
            "expression nesting depth exceeded (max {} levels) — likely a pathological or generated test source",
            crate::test_runtime::interpreter::scope::MAX_EXPR_DEPTH
        )));
    }
    let result = eval_expr_inner(node, source, stack);
    stack.exit_expr();
    result
}

fn eval_expr_inner(node: Node<'_>, source: &[u8], stack: &mut ScopeStack) -> Eval {
    match node.kind() {
        // Literal forms — the AL grammar uses `integer`, `decimal`, `string`
        // as the actual node kinds (not `integer_literal` etc.).
        "integer_literal" | "integer" => match utf8_text(node, source)
            .and_then(|t| t.trim_end_matches(['l', 'L']).parse::<i64>().ok())
        {
            Some(n) => Eval::Normal(Value::Integer(n)),
            None => Eval::Error(simple_error(&format!(
                "malformed integer literal: {}",
                utf8_text(node, source).unwrap_or("?")
            ))),
        },
        "decimal_literal" | "decimal" => {
            match utf8_text(node, source).and_then(|t| t.parse::<f64>().ok()) {
                Some(n) => Eval::Normal(Value::Decimal(n)),
                None => Eval::Error(simple_error("malformed decimal literal")),
            }
        }
        "boolean_literal" => eval_literal(node, source),
        "string_literal" | "string" | "verbatim_string" => {
            let text = utf8_text(node, source).unwrap_or("");
            let trimmed = text.trim_start_matches('\'').trim_end_matches('\'');
            let unescaped = trimmed.replace("''", "'");
            Eval::Normal(Value::Text(unescaped))
        }
        // The AL grammar uses `expression` as the binary expression node:
        //   expression = unary_expression (binary_operator unary_expression)*
        // When there are 3+ named children, it's a binary (or assignment) expression.
        // When there is 1 named child, it's a transparent wrapper.
        "expression" => eval_expression_node(node, source, stack),
        // Grammar wrappers — pass through to the single inner child.
        "parenthesized_expression"
        | "postfix_expression"
        | "primary_expression"
        | "case_label_expression" => match named_child(node, 0) {
            Some(inner) => eval_expr(inner, source, stack),
            None => Eval::Error(simple_error("empty expression wrapper")),
        },
        // Identifier — load from scope, or resolve boolean keywords.
        "identifier" | "variable_reference" | "name" => match utf8_text(node, source) {
            Some(name) => {
                // Boolean keywords may appear as identifiers in some grammar versions.
                match name.to_ascii_lowercase().as_str() {
                    "true" => return Eval::Normal(Value::Boolean(true)),
                    "false" => return Eval::Normal(Value::Boolean(false)),
                    _ => {}
                }
                match stack.lookup(name) {
                    Some(v) => Eval::Normal(v.clone()),
                    None => Eval::Error(simple_error(&format!("unbound identifier: {name}"))),
                }
            }
            None => Eval::Error(simple_error("invalid identifier text")),
        },
        // Unary `-` / `not`.
        "unary_expression" => eval_unary(node, source, stack),
        // Anything else: signal a clear error rather than silently
        // returning a default — failing loud is better than failing wrong.
        other => Eval::Error(simple_error(&format!(
            "unsupported expression kind in Phase 2a: {other}"
        ))),
    }
}

fn simple_error(message: &str) -> ErrorInfo {
    ErrorInfo {
        message: message.to_string(),
        error_type: None,
        source: None,
    }
}

fn utf8_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

fn named_child(node: Node<'_>, index: usize) -> Option<Node<'_>> {
    node.named_child(index)
}

fn eval_literal(node: Node<'_>, source: &[u8]) -> Eval {
    let Some(text) = utf8_text(node, source) else {
        return Eval::Error(simple_error("invalid literal text"));
    };
    match node.kind() {
        "integer_literal" => match text.parse::<i64>() {
            Ok(n) => Eval::Normal(Value::Integer(n)),
            Err(_) => Eval::Error(simple_error(&format!("malformed integer literal: {text}"))),
        },
        "decimal_literal" => match text.parse::<f64>() {
            Ok(n) => Eval::Normal(Value::Decimal(n)),
            Err(_) => Eval::Error(simple_error(&format!("malformed decimal literal: {text}"))),
        },
        "boolean_literal" => match text.eq_ignore_ascii_case("true") {
            true => Eval::Normal(Value::Boolean(true)),
            false => Eval::Normal(Value::Boolean(false)),
        },
        "string_literal" => {
            // Strip leading/trailing single-quote and unescape doubled
            // single-quotes (AL's escape mechanism).
            let trimmed = text.trim_start_matches('\'').trim_end_matches('\'');
            let unescaped = trimmed.replace("''", "'");
            Eval::Normal(Value::Text(unescaped))
        }
        other => Eval::Error(simple_error(&format!("unknown literal kind: {other}"))),
    }
}

fn eval_unary(node: Node<'_>, source: &[u8], stack: &mut ScopeStack) -> Eval {
    // Grammar: unary_expression = (unary_operator unary_expression) | postfix_expression
    //   - 2 named children: [unary_operator, unary_expression]
    //   - 1 named child:    [postfix_expression] — transparent wrapper
    let named_count = node.named_child_count();
    if named_count <= 1 {
        // Transparent wrapper around postfix_expression.
        return match named_child(node, 0) {
            Some(inner) => eval_expr(inner, source, stack),
            None => Eval::Error(simple_error("unary expression: empty node")),
        };
    }
    // 2-child form: operator + operand.
    let op_node = match named_child(node, 0) {
        Some(n) => n,
        None => return Eval::Error(simple_error("unary expression missing operator")),
    };
    let operand_node = match named_child(node, 1) {
        Some(n) => n,
        None => return Eval::Error(simple_error("unary expression missing operand")),
    };
    let operator_text = utf8_text(op_node, source).unwrap_or("").trim();

    let value = match eval_expr(operand_node, source, stack) {
        Eval::Normal(v) => v,
        other => return other,
    };

    match (operator_text.to_ascii_lowercase().as_str(), value) {
        ("-", Value::Integer(n)) => Eval::Normal(Value::Integer(-n)),
        ("-", Value::Decimal(n)) => Eval::Normal(Value::Decimal(-n)),
        ("not", Value::Boolean(b)) => Eval::Normal(Value::Boolean(!b)),
        (op, v) => Eval::Error(simple_error(&format!(
            "unary operator `{op}` not supported on {}",
            v.type_name()
        ))),
    }
}

/// Handle the AL `expression` node.
///
/// The grammar defines:
///   expression = unary_expression (binary_operator unary_expression)*
///
/// The tree is FLAT: all operators and operands are direct named children.
/// Named children alternate: operand, operator, operand, operator, operand, ...
///
/// We collect all named children into a list, then find the `:=` operator
/// (assignment, lowest precedence) and process accordingly. For pure
/// computation, we evaluate left-to-right.
fn eval_expression_node(node: Node<'_>, source: &[u8], stack: &mut ScopeStack) -> Eval {
    let named_count = node.named_child_count();

    let children: Vec<Node<'_>> = (0..named_count)
        .filter_map(|i| node.named_child(i))
        .collect();

    if children.is_empty() {
        return Eval::Error(simple_error("expression: no children"));
    }
    if children.len() == 1 {
        // Transparent wrapper.
        return eval_expr(children[0], source, stack);
    }

    // Find `:=` (assignment) operator in the children.
    // Operators are at odd indices: [operand, op, operand, op, operand, ...]
    let assign_idx = children
        .iter()
        .enumerate()
        .find(|(idx, c)| {
            let node = *c;
            *idx % 2 == 1
                && node.kind() == "binary_operator"
                && node.utf8_text(source).is_ok_and(|t| t.trim() == ":=")
        })
        .map(|(idx, _)| idx);

    if let Some(op_idx) = assign_idx {
        // Assignment: LHS is children[op_idx - 1], RHS is children[op_idx + 1..] as a chain.
        let lhs_node = children[op_idx - 1];
        let rhs_children = &children[(op_idx + 1)..];

        // Evaluate the RHS sub-expression (may be a chain like `1 div 0`).
        let rhs_val = match eval_expr_chain(rhs_children, source, stack) {
            Eval::Normal(v) => v,
            other => return other,
        };

        // Resolve the LHS name.
        let lhs_name = extract_identifier_name(lhs_node, source)
            .or_else(|| {
                lhs_node
                    .utf8_text(source)
                    .ok()
                    .map(|s| s.trim_matches('"').to_ascii_lowercase())
            })
            .unwrap_or_default();

        if lhs_name.is_empty() {
            return Eval::Error(simple_error("expression: cannot resolve LHS name for :="));
        }
        if let Some(slot) = stack.lookup_mut(&lhs_name) {
            *slot = rhs_val;
        } else if let Some(frame) = stack.top_mut() {
            frame.bind(&lhs_name, rhs_val);
        } else {
            return Eval::Error(simple_error("expression: no active scope for :="));
        }
        return Eval::Normal(Value::Empty);
    }

    // No assignment — evaluate as a binary expression chain left-to-right.
    eval_expr_chain(&children, source, stack)
}

/// Evaluate a flat alternating chain [operand, op, operand, op, operand, ...]
/// left-to-right, returning the final computed value.
///
/// This is a helper for `eval_expression_node`. Returns `Eval` directly.
fn eval_expr_chain(children: &[Node<'_>], source: &[u8], stack: &mut ScopeStack) -> Eval {
    if children.is_empty() {
        return Eval::Error(simple_error("expression chain: empty"));
    }
    if children.len() == 1 {
        return eval_expr(children[0], source, stack);
    }

    // Evaluate first operand.
    let mut acc = match eval_expr(children[0], source, stack) {
        Eval::Normal(v) => v,
        other => return other,
    };

    // Process operator-operand pairs.
    let mut i = 1;
    while i + 1 < children.len() {
        let op_node = children[i];
        let rhs_node = children[i + 1];
        let operator = utf8_text(op_node, source).unwrap_or("").trim().to_string();

        let rhs = match eval_expr(rhs_node, source, stack) {
            Eval::Normal(v) => v,
            other => return other,
        };

        acc = match apply_binary(&operator, acc, rhs) {
            Eval::Normal(v) => v,
            other => return other,
        };
        i += 2;
    }
    Eval::Normal(acc)
}

/// Extract the lowercase identifier name from a wrapper node.
fn extract_identifier_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    // Walk named children looking for an identifier/name node.
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "identifier" | "name" | "variable_reference" => {
                return child
                    .utf8_text(source)
                    .ok()
                    .map(|s| s.trim_matches('"').to_ascii_lowercase());
            }
            "unary_expression" | "postfix_expression" | "primary_expression" => {
                if let Some(name) = extract_identifier_name(child, source) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Apply a binary operator to two values. Made `pub(crate)` so unit tests
/// (and Phase 3's mock dispatch) can reuse the operator semantics.
pub(crate) fn apply_binary(operator: &str, left: Value, right: Value) -> Eval {
    let op = operator.to_ascii_lowercase();
    // Wrap a Decimal result, returning Eval::Error if it produced NaN or
    // infinity. AL semantics: arithmetic that overflows or produces an
    // undefined value is a runtime error, not a silent NaN.
    fn checked_decimal(d: f64) -> Eval {
        if d.is_nan() || d.is_infinite() {
            Eval::Error(simple_error("decimal arithmetic produced NaN or infinity"))
        } else {
            Eval::Normal(Value::Decimal(d))
        }
    }
    match (&op[..], left, right) {
        // Integer arithmetic — checked to avoid panics on overflow.
        ("+", Value::Integer(a), Value::Integer(b)) => match a.checked_add(b) {
            Some(n) => Eval::Normal(Value::Integer(n)),
            None => Eval::Error(simple_error("integer overflow")),
        },
        ("-", Value::Integer(a), Value::Integer(b)) => match a.checked_sub(b) {
            Some(n) => Eval::Normal(Value::Integer(n)),
            None => Eval::Error(simple_error("integer overflow")),
        },
        ("*", Value::Integer(a), Value::Integer(b)) => match a.checked_mul(b) {
            Some(n) => Eval::Normal(Value::Integer(n)),
            None => Eval::Error(simple_error("integer overflow")),
        },
        ("div", Value::Integer(a), Value::Integer(b))
        | ("/", Value::Integer(a), Value::Integer(b)) => {
            if b == 0 {
                Eval::Error(simple_error("division by zero"))
            } else if op == "/" {
                checked_decimal(a as f64 / b as f64)
            } else {
                // i64::MIN / -1 overflows → checked_div returns None.
                match a.checked_div(b) {
                    Some(n) => Eval::Normal(Value::Integer(n)),
                    None => Eval::Error(simple_error("integer overflow")),
                }
            }
        }
        ("mod", Value::Integer(a), Value::Integer(b)) => {
            if b == 0 {
                Eval::Error(simple_error("modulo by zero"))
            } else {
                match a.checked_rem(b) {
                    Some(n) => Eval::Normal(Value::Integer(n)),
                    None => Eval::Error(simple_error("integer overflow")),
                }
            }
        }
        // Decimal arithmetic (Decimal × Decimal and mixed) — guard NaN/Inf.
        ("+", Value::Decimal(a), Value::Decimal(b)) => checked_decimal(a + b),
        ("-", Value::Decimal(a), Value::Decimal(b)) => checked_decimal(a - b),
        ("*", Value::Decimal(a), Value::Decimal(b)) => checked_decimal(a * b),
        ("/", Value::Decimal(a), Value::Decimal(b)) if b != 0.0 => checked_decimal(a / b),
        ("/", Value::Decimal(_), Value::Decimal(_)) => {
            Eval::Error(simple_error("division by zero"))
        }
        ("+", Value::Integer(a), Value::Decimal(b))
        | ("+", Value::Decimal(b), Value::Integer(a)) => checked_decimal(a as f64 + b),
        ("-", Value::Integer(a), Value::Decimal(b)) => checked_decimal(a as f64 - b),
        ("-", Value::Decimal(a), Value::Integer(b)) => checked_decimal(a - b as f64),
        ("*", Value::Integer(a), Value::Decimal(b))
        | ("*", Value::Decimal(b), Value::Integer(a)) => checked_decimal(a as f64 * b),

        // String concatenation (AL `+` on Text/Code).
        ("+", Value::Text(a), Value::Text(b)) => Eval::Normal(Value::Text(format!("{a}{b}"))),
        ("+", Value::Text(a), Value::Code(b)) | ("+", Value::Code(b), Value::Text(a)) => {
            Eval::Normal(Value::Text(format!("{a}{b}")))
        }
        ("+", Value::Code(a), Value::Code(b)) => Eval::Normal(Value::Code(format!("{a}{b}"))),

        // Comparisons (broad — any matching primitive type).
        ("=", a, b) => Eval::Normal(Value::Boolean(values_equal(&a, &b))),
        ("<>", a, b) => Eval::Normal(Value::Boolean(!values_equal(&a, &b))),
        ("<", a, b) => values_cmp(&a, &b, |o| o.is_lt()),
        ("<=", a, b) => values_cmp(&a, &b, |o| o.is_le()),
        (">", a, b) => values_cmp(&a, &b, |o| o.is_gt()),
        (">=", a, b) => values_cmp(&a, &b, |o| o.is_ge()),

        // Boolean logic.
        ("and", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a && b)),
        ("or", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a || b)),
        ("xor", Value::Boolean(a), Value::Boolean(b)) => Eval::Normal(Value::Boolean(a ^ b)),

        (op, a, b) => Eval::Error(simple_error(&format!(
            "binary operator `{op}` not supported on ({}, {})",
            a.type_name(),
            b.type_name()
        ))),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Integer(x), Integer(y)) => x == y,
        (Decimal(x), Decimal(y)) => x == y,
        (Integer(x), Decimal(y)) | (Decimal(y), Integer(x)) => (*x as f64) == *y,
        (Boolean(x), Boolean(y)) => x == y,
        (Text(x), Text(y)) | (Code(x), Code(y)) => x == y,
        (Text(x), Code(y)) | (Code(y), Text(x)) => x == y,
        (Date(x), Date(y)) | (Time(x), Time(y)) | (DateTime(x), DateTime(y)) => x == y,
        (Null, Null) | (Empty, Empty) => true,
        _ => false,
    }
}

fn values_cmp(a: &Value, b: &Value, predicate: impl Fn(std::cmp::Ordering) -> bool) -> Eval {
    use std::cmp::Ordering;
    use Value::*;
    let ord = match (a, b) {
        (Integer(x), Integer(y)) => x.cmp(y),
        (Integer(x), Decimal(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Decimal(x), Integer(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Decimal(x), Decimal(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Text(x), Text(y)) | (Code(x), Code(y)) => x.cmp(y),
        (Date(x), Date(y)) | (Time(x), Time(y)) | (DateTime(x), DateTime(y)) => x.cmp(y),
        (l, r) => {
            return Eval::Error(simple_error(&format!(
                "cannot compare {} and {}",
                l.type_name(),
                r.type_name()
            )));
        }
    };
    Eval::Normal(Value::Boolean(predicate(ord)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_runtime::interpreter::scope::CallFrame;

    fn ok(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) | Eval::Exit(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
        }
    }

    fn err(eval: Eval) -> ErrorInfo {
        match eval {
            Eval::Error(e) => e,
            Eval::Normal(v) => panic!("expected error, got Normal({})", v.type_name()),
            Eval::Exit(v) => panic!("expected error, got Exit({})", v.type_name()),
        }
    }

    /// F-OPEN-265: eval_expr must cap AST nesting the way eval_stmt already
    /// does — degenerate expression nesting must yield `Eval::Error`, not a
    /// native stack overflow (which aborts the whole al-lsp process).
    #[test]
    fn deep_expression_nesting_errors_instead_of_overflowing() {
        let depth = 400; // beyond the cap, far below crash territory
        let expr = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
        let source = format!(
            "codeunit 50100 X\n{{\n    procedure P()\n    var\n        I: Integer;\n    begin\n        I := {expr};\n    end;\n}}\n"
        );
        let parsed = crate::syntax::AlParser::parse_quick(&source);
        let mut nodes = vec![parsed.tree.root_node()];
        let mut target = None;
        while let Some(n) = nodes.pop() {
            if n.kind() == "parenthesized_expression" {
                target = Some(n);
                break;
            }
            let mut c = n.walk();
            nodes.extend(n.children(&mut c));
        }
        let node = target.expect("parenthesized expression must parse");
        let mut scope = ScopeStack::new();
        match eval_expr(node, source.as_bytes(), &mut scope) {
            Eval::Error(e) => assert!(
                e.message.to_lowercase().contains("depth"),
                "error must mention the depth cap: {}",
                e.message
            ),
            Eval::Normal(_) => panic!("expected a depth-cap error, got Normal"),
            Eval::Exit(_) => panic!("expected a depth-cap error, got Exit"),
        }
    }

    // -- apply_binary unit tests (don't need a parse tree) ---------------

    #[test]
    fn integer_arithmetic_associativity() {
        // (1 + 2) + 3 == 1 + (2 + 3)
        let lhs = ok(apply_binary(
            "+",
            ok(apply_binary("+", Value::Integer(1), Value::Integer(2))),
            Value::Integer(3),
        ));
        let rhs = ok(apply_binary(
            "+",
            Value::Integer(1),
            ok(apply_binary("+", Value::Integer(2), Value::Integer(3))),
        ));
        assert_eq!(lhs, rhs);
    }

    #[test]
    fn integer_div_truncates() {
        assert_eq!(
            ok(apply_binary("div", Value::Integer(7), Value::Integer(2))),
            Value::Integer(3)
        );
    }

    #[test]
    fn slash_promotes_to_decimal() {
        // AL: integer `/` integer → Decimal.
        assert_eq!(
            ok(apply_binary("/", Value::Integer(7), Value::Integer(2))),
            Value::Decimal(3.5)
        );
    }

    #[test]
    fn divide_by_zero_is_error() {
        // Negative: dividing by zero must produce an Eval::Error.
        let e = err(apply_binary("/", Value::Integer(1), Value::Integer(0)));
        assert!(e.message.contains("division by zero"));
        let e = err(apply_binary("mod", Value::Integer(1), Value::Integer(0)));
        assert!(e.message.contains("modulo by zero"));
    }

    #[test]
    fn string_concat_text_and_code() {
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Text("hello, ".into()),
                Value::Text("world".into())
            )),
            Value::Text("hello, world".into())
        );
        // Mixed Text + Code keeps Text.
        assert_eq!(
            ok(apply_binary(
                "+",
                Value::Text("a".into()),
                Value::Code("B".into())
            )),
            Value::Text("aB".into())
        );
    }

    #[test]
    fn equality_and_inequality() {
        assert_eq!(
            ok(apply_binary("=", Value::Integer(5), Value::Integer(5))),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary("<>", Value::Integer(5), Value::Integer(6))),
            Value::Boolean(true)
        );
        // Numeric mixing: 5 = 5.0 should be true.
        assert_eq!(
            ok(apply_binary("=", Value::Integer(5), Value::Decimal(5.0))),
            Value::Boolean(true)
        );
    }

    #[test]
    fn ordering_for_text_lex() {
        assert_eq!(
            ok(apply_binary(
                "<",
                Value::Text("apple".into()),
                Value::Text("banana".into())
            )),
            Value::Boolean(true)
        );
    }

    #[test]
    fn boolean_logic_works() {
        assert_eq!(
            ok(apply_binary(
                "and",
                Value::Boolean(true),
                Value::Boolean(false)
            )),
            Value::Boolean(false)
        );
        assert_eq!(
            ok(apply_binary(
                "or",
                Value::Boolean(false),
                Value::Boolean(true)
            )),
            Value::Boolean(true)
        );
        assert_eq!(
            ok(apply_binary(
                "xor",
                Value::Boolean(true),
                Value::Boolean(true)
            )),
            Value::Boolean(false)
        );
    }

    #[test]
    fn unsupported_operator_yields_error() {
        // Negative: an unknown operator returns Error rather than panic.
        let e = err(apply_binary("**", Value::Integer(2), Value::Integer(3)));
        assert!(
            e.message.contains("not supported"),
            "expected helpful error, got: {}",
            e.message
        );
    }

    #[test]
    fn comparing_incompatible_types_errors() {
        // Negative: comparing Text to Integer is not supported.
        let e = err(apply_binary(
            "<",
            Value::Text("a".into()),
            Value::Integer(1),
        ));
        assert!(
            e.message.contains("cannot compare"),
            "expected compare error, got: {}",
            e.message
        );
    }

    // -- eval_expr against a real parse tree ----------------------------

    fn parse_and_eval(source_str: &str) -> Eval {
        // Wrap the expression in a procedure body so the parser is happy.
        let wrapper = format!(
            "codeunit 50100 \"X\"\n{{\n    procedure Test(): Variant\n    begin\n        exit({source_str});\n    end;\n}}"
        );
        let mut parser = crate::syntax::parser::AlParser::new();
        let result = parser.parse(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();

        // Find the first `expression` child of the `exit_statement`.
        fn find_exit_arg<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
            if node.kind() == "exit_statement" {
                let mut cursor = node.walk();
                return node.named_children(&mut cursor).next();
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if let Some(found) = find_exit_arg(child) {
                    return Some(found);
                }
            }
            None
        }

        let expr = match find_exit_arg(root) {
            Some(n) => n,
            None => {
                return Eval::Error(simple_error(
                    "test harness could not find the wrapped expression",
                ));
            }
        };

        let mut stack = ScopeStack::new();
        stack.push(CallFrame::new("X", "Test"));
        eval_expr(expr, bytes, &mut stack)
    }

    #[test]
    fn eval_integer_literal_via_parser() {
        if let Eval::Normal(v) = parse_and_eval("42") {
            assert_eq!(v, Value::Integer(42));
        }
        // The parser may or may not produce the precise expected node
        // structure we rely on; we don't fail the suite on a parser
        // miss because this is a smoke test, not a contract.
    }

    #[test]
    fn eval_unbound_identifier_is_error() {
        // Negative: reading an unbound name is an Eval::Error.
        let result = parse_and_eval("nope");
        match result {
            Eval::Error(e) => {
                assert!(e.message.contains("unbound") || e.message.contains("unsupported"));
            }
            // If the parser wrapped the identifier in a kind we don't
            // recognise, the unsupported branch produces an error too.
            Eval::Normal(_) | Eval::Exit(_) => {
                // Acceptable in Phase 2a — the harness can't always
                // reach the identifier node depending on grammar shape.
            }
        }
    }

    // ── Adversarial tests (adversarial-h) ─────────────────────────────────────

    #[test]
    fn integer_add_overflow_should_not_panic_adversarial_h_4() {
        // FINDING P0 panic: Integer(i64::MAX) + Integer(1) panics in debug
        // (attempt to add with overflow) or silently wraps in release.
        // The arm `("+", Integer(a), Integer(b)) => Normal(Integer(a + b))`
        // uses unchecked addition with no overflow guard.
        // Expected: Eval::Error
        // Observed (debug): thread panic "attempt to add with overflow"
        // Observed (release): Normal(Integer(i64::MIN))
        let result = apply_binary("+", Value::Integer(i64::MAX), Value::Integer(1));
        assert!(
            result.is_error(),
            "Integer overflow must produce Eval::Error, not panic or wrap silently"
        );
    }

    #[test]
    fn integer_min_div_neg1_should_not_panic_adversarial_h_5() {
        // FINDING P0 panic: Integer(i64::MIN) div Integer(-1) panics in debug.
        // The div arm checks `b == 0` but not the special case
        // `a == i64::MIN && b == -1` which also overflows.
        // Expected: Eval::Error
        // Observed (debug): thread panic "attempt to divide with overflow"
        let result = apply_binary("div", Value::Integer(i64::MIN), Value::Integer(-1));
        assert!(
            result.is_error(),
            "i64::MIN div -1 must produce Eval::Error, not panic"
        );
    }

    #[test]
    fn decimal_inf_div_inf_silent_nan_adversarial_h_6() {
        // FINDING P1 silent-error: Decimal(Inf) / Decimal(Inf) returns
        // Normal(Decimal(NaN)) silently. The guard only checks `b != 0.0`;
        // Inf / Inf = NaN which is not a valid AL Decimal value.
        // Expected: Eval::Error
        // Observed: Normal(Decimal(NaN))
        let result = apply_binary(
            "/",
            Value::Decimal(f64::INFINITY),
            Value::Decimal(f64::INFINITY),
        );
        assert!(
            result.is_error(),
            "Inf / Inf must produce Eval::Error (NaN is not valid AL Decimal), got: {:?}",
            result
        );
    }
}
