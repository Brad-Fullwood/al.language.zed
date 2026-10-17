//! The expression dispatcher: `eval_expr` walks a `tree_sitter::Node` down to
//! the evaluator for its grammar kind, and `eval_expr_inner` holds that
//! per-kind match: literals, identifiers, and the wrappers that route to the
//! other files in this module.

use al_syntax::IdentifierText;
use tree_sitter::Node;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::error_info;
use crate::interpreter::indexing;
use crate::interpreter::records;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{Decimal, Value};

use super::assignment_chain::eval_expression_node;
use super::helpers::{named_child, utf8_text};
use super::literals::{
    eval_date_literal, eval_set_literal, eval_time_literal, int_literal_value,
    niladic_clock_builtin, unescape_al_string,
};
use super::member_access::eval_postfix;
use super::operators::eval_unary;

pub fn eval_expr(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Expression evaluation recurses per AST nesting level. About 400 nested
    // parentheses overflow a 2 MiB worker stack, so mirror eval_stmt's guard.
    if !stack.enter_expr() {
        return Eval::Error(error_info(format!(
            "expression nesting depth exceeded (max {} levels) — likely a pathological or generated test source",
            crate::interpreter::scope::MAX_EXPR_DEPTH
        )));
    }
    let result = eval_expr_inner(node, source, stack, ctx);
    stack.exit_expr();
    record_condition_trace(node, &result, ctx);
    result
}

/// Attach a source-ordered Boolean-condition trace to an evaluated syntax node
/// while dynamic coverage is tracing a decision.
///
/// Logical chains install their precise combined trace in
/// `eval_expression_node`. This fallback propagates that trace through
/// transparent grammar wrappers; genuinely atomic Boolean values (a variable,
/// literal, comparison, field access, or Boolean-returning call) become one
/// condition. Procedure-call internals are intentionally not exposed as
/// conditions of the caller's decision.
fn record_condition_trace(node: Node<'_>, result: &Eval, ctx: &mut DispatchCtx) {
    if !ctx.cov_condition_trace_active() || ctx.cov_expression_trace(node).is_some() {
        return;
    }
    let Eval::Normal(Value::Boolean(outcome)) = result else {
        return;
    };

    let transparent = match node.kind() {
        "expression"
        | "parenthesized_expression"
        | "primary_expression"
        | "case_label_expression" => true,
        "unary_expression" => true,
        "postfix_expression" => !crate::interpreter::eval_stmt::is_call_postfix(node),
        _ => false,
    };
    let inherited = transparent.then(|| {
        let mut cursor = node.walk();
        let trace = node
            .named_children(&mut cursor)
            .find_map(|child| ctx.cov_expression_trace(child));
        trace
    });
    ctx.cov_set_expression_trace(node, inherited.flatten().unwrap_or_else(|| vec![*outcome]));
}

fn eval_expr_inner(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    match node.kind() {
        // Literals: the grammar's kinds are `integer`, `decimal` and `string`.
        "integer" => match utf8_text(node, source) {
            Some(t) => match int_literal_value(t) {
                Some(v) => Eval::Normal(v),
                None => Eval::Error(error_info(format!("malformed integer literal: {t}"))),
            },
            None => Eval::Error(error_info("invalid integer literal text")),
        },
        "decimal" => match utf8_text(node, source).and_then(|t| t.parse::<Decimal>().ok()) {
            Some(n) => Eval::Normal(Value::Decimal(n)),
            None => Eval::Error(error_info("malformed decimal literal")),
        },
        // Date / Time / DateTime literals — `20240701D`, `063030T`. The
        // lexer tokenises these; evaluation maps them onto the day/ms carriers.
        "date_literal" => eval_date_literal(node, source),
        "time_literal" => eval_time_literal(node, source),
        "string" | "verbatim_string" => {
            let text = utf8_text(node, source).unwrap_or("");
            Eval::Normal(Value::Text(unescape_al_string(text)))
        }
        // The AL grammar uses `expression` as the binary expression node:
        //   expression = unary_expression (binary_operator unary_expression)*
        // When there are 3+ named children, it's a binary (or assignment) expression.
        // When there is 1 named child, it's a transparent wrapper.
        "expression" => eval_expression_node(node, source, stack, ctx),
        // A postfix_expression is a call (`Foo(args)`, `Recv.Proc(args)`), a
        // scope-qualified enum access (`"Enum"::Member`), a record field read
        // (`Rec."Field"`), or a transparent wrapper around a primary_expression.
        // Calls and record reads need the dispatch context; scope access yields
        // an Option value; everything else unwraps.
        "postfix_expression" => eval_postfix(node, source, stack, ctx),
        // A case label holding an operator (`Points >= 1000:` under
        // `case true of`) is a binary expression; only its first operand was
        // evaluated, so every such label compared `Points` with `true`.
        "case_label_expression" if node.named_child_count() > 1 => {
            eval_expression_node(node, source, stack, ctx)
        }
        "parenthesized_expression" | "primary_expression" | "case_label_expression" => {
            match named_child(node, 0) {
                Some(inner) => eval_expr(inner, source, stack, ctx),
                None => Eval::Error(error_info("empty expression wrapper")),
            }
        }
        // In expression position the grammar's lossless generic bracket block
        // is an AL set literal (`[A, B, Low .. High]`). Evaluate each top-level
        // member as an ordinary expression so calls, enum scopes, variables,
        // and ranges use exactly the same semantics as expressions elsewhere.
        "bracketed_block" => eval_set_literal(node, source, stack, ctx),
        "identifier" | "name" => match utf8_text(node, source) {
            Some(name) => {
                // `true` and `false` parse as names.
                match name.to_ascii_lowercase().as_str() {
                    "true" => return Eval::Normal(Value::Boolean(true)),
                    "false" => return Eval::Normal(Value::Boolean(false)),
                    _ => {}
                }
                // Declarations bind `"My Limit"` as `My Limit`.
                let name = name.unquote_identifier();
                let name = name.as_ref();
                match stack.lookup(name) {
                    Some(v) => Eval::Normal(v.clone()),
                    // Niladic clock builtins may appear without parentheses
                    // (`dt := CurrentDateTime`). Only treated as builtins when
                    // not shadowed by a bound variable of the same name.
                    None => match niladic_clock_builtin(name, ctx) {
                        Some(v) => Eval::Normal(v),
                        None => {
                            records::implicit_field_get(name, stack, ctx).unwrap_or_else(|| {
                                Eval::Error(error_info(format!("unbound identifier: {name}")))
                            })
                        }
                    },
                }
            }
            None => Eval::Error(error_info("invalid identifier text")),
        },
        // A variable, parameter or field named `Value`, `Code`, `Page` or
        // another object or type word parses as a keyword node. It reads as
        // a name when something binds it: `Page.RunModal` and `Database::X`
        // receivers are handled before they reach here.
        "object_keyword" | "type_keyword" => {
            let name = utf8_text(node, source).unwrap_or_default();
            match stack.lookup(name) {
                Some(value) => Eval::Normal(value.clone()),
                None => niladic_clock_builtin(name, ctx)
                    .map(Eval::Normal)
                    .or_else(|| records::implicit_field_get(name, stack, ctx))
                    .unwrap_or_else(|| {
                        Eval::Error(error_info(format!(
                            "unsupported expression kind: {}",
                            node.kind()
                        )))
                    }),
            }
        }
        "unary_expression" => eval_unary(node, source, stack, ctx),
        // `-1:` or `-2.5:` in a case: the grammar makes a label with a leading
        // minus one token, so its text is evaluated as an expression.
        "signed_case_label" => {
            let text = utf8_text(node, source).unwrap_or_default().trim();
            match indexing::eval_standalone_expression(text, stack, ctx) {
                Ok(value) => Eval::Normal(value),
                Err(error) => error,
            }
        }
        // Anything else: signal a clear error rather than silently
        // returning a default — failing loud is better than failing wrong.
        other => Eval::Error(error_info(format!("unsupported expression kind: {other}"))),
    }
}
