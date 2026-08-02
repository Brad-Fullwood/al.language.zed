//! Statement evaluator for the AL interpreter.
//!
//! Walks tree-sitter statement nodes produced by the AL grammar and evaluates
//! them against the active `ScopeStack`. Control-flow (`exit`, `if`, `while`,
//! `for`, `repeat`, `case`) is handled here; expression evaluation is delegated
//! to `eval_expr::eval_expr` and procedure calls to `dispatch::dispatch_call`.
//!
//! ## Statement node types handled
//!
//! | tree-sitter kind              | AL construct                        |
//! |-------------------------------|-------------------------------------|
//! | `if_statement`                | IF cond THEN … [ELSE …]             |
//! | `empty_if_statement`          | IF cond THEN ;  (body-less)         |
//! | `while_statement`             | WHILE cond DO … END                 |
//! | `for_statement`               | FOR i := a TO b DO …                |
//! | `foreach_statement`           | FOREACH x IN list DO …              |
//! | `repeat_statement`            | REPEAT … UNTIL cond                 |
//! | `case_statement`              | CASE expr OF … END                  |
//! | `begin_end_block`             | BEGIN … END                         |
//! | `statement_list`              | sequence of statements              |
//! | `assignment_statement`        | x := expr                           |
//! | `exit_statement`              | EXIT [( expr )]                     |
//! | `expression_statement`        | expr;  (side-effects only)          |
//! | `asserterror_statement`       | ASSERTERROR stmt                    |
//! | anything else                 | treated as an expression node       |
//!
//! ## Error propagation
//!
//! `Eval::Error` short-circuits immediately — the first error in a statement
//! sequence propagates to the caller. `Eval::Exit` also short-circuits,
//! unwinding back to the enclosing procedure.

use tree_sitter::Node;

use crate::interpreter::dispatch::{dispatch_call_scoped, DispatchCtx, MAX_AST_DEPTH};
use crate::interpreter::eval_expr::eval_expr;
use crate::interpreter::records;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{ErrorInfo, Value};

/// Evaluate a single tree-sitter statement node.
///
/// Returns:
/// - `Eval::Normal(Value::Empty)` on a statement that produces no value.
/// - `Eval::Normal(v)` on an expression statement.
/// - `Eval::Exit(v)` when `exit(v)` (or bare `exit`) is reached.
/// - `Eval::Error(info)` on any runtime error (propagates immediately).
pub fn eval_stmt(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Stack-overflow guard. AL test sources with thousands of nested
    // `begin/end` or `if … then if …` blocks would otherwise recurse into
    // `eval_stmt` deeply enough to blow the Rust stack and kill the daemon.
    // Cap at MAX_AST_DEPTH (256) — well clear of typical test nesting (~10)
    // and well below the OS stack limit when accounting for each frame's
    // locals + Node payload.
    if ctx.ast_depth >= MAX_AST_DEPTH {
        return Eval::Error(simple_error(&format!(
            "AST nesting depth exceeded (max {} levels) — likely a pathological or generated test source",
            MAX_AST_DEPTH
        )));
    }
    // Record the source line for dynamic coverage. Untaken branches never reach
    // `eval_stmt`, so their statements are not recorded.
    ctx.cov_record_stmt(node);
    ctx.ast_depth += 1;
    let result = eval_stmt_inner(node, source, stack, ctx);
    ctx.ast_depth -= 1;
    result
}

fn eval_stmt_inner(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    match node.kind() {
        "statement" => {
            if let Some(inner) = named_stmt_child(node, 0) {
                eval_stmt(inner, source, stack, ctx)
            } else {
                Eval::Normal(Value::Empty)
            }
        }
        "begin_end_block" | "statement_list" => eval_block(node, source, stack, ctx),
        "if_statement" | "empty_if_statement" => eval_if(node, source, stack, ctx),
        "while_statement" => eval_while(node, source, stack, ctx),
        "for_statement" => eval_for(node, source, stack, ctx),
        "foreach_statement" => eval_foreach(node, source, stack, ctx),
        "repeat_statement" => eval_repeat(node, source, stack, ctx),
        "case_statement" => eval_case(node, source, stack, ctx),
        "assignment_statement" => eval_assignment(node, source, stack, ctx),
        "exit_statement" => eval_exit(node, source, stack, ctx),
        // Handle the grammar's dedicated `break` and `continue` nodes.
        "break_statement" => Eval::Break,
        "continue_statement" => Eval::Continue,
        "asserterror_statement" => eval_asserterror(node, source, stack, ctx),
        "expression_statement" => {
            if let Some(inner) = node.named_child(0) {
                eval_expression_stmt(inner, source, stack, ctx)
            } else {
                Eval::Normal(Value::Empty)
            }
        }
        // Bare expression or call nodes that appear directly in a statement
        // context (tree-sitter grammar variation).
        _ => eval_expression_stmt(node, source, stack, ctx),
    }
}

fn eval_block(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let mut cursor = node.walk();
    let mut last = Eval::Normal(Value::Empty);
    for child in node.named_children(&mut cursor) {
        if is_punctuation(child.kind()) {
            continue;
        }
        last = eval_stmt(child, source, stack, ctx);
        match &last {
            Eval::Normal(_) => {}
            // Error/Exit unwind the procedure; Break/Continue unwind to the
            // nearest enclosing loop. All propagate up out of the block.
            Eval::Error(_) | Eval::Exit(_) | Eval::Break | Eval::Continue => return last,
        }
    }
    last
}

/// Evaluate one source Boolean decision while capturing its atomic-condition
/// vector for MC/DC. The trace is taken from the same evaluation that drives
/// control flow; coverage never re-runs an expression (and therefore cannot
/// duplicate calls or other side effects).
fn eval_decision_condition(
    condition: Node<'_>,
    decision: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    ctx.cov_begin_condition_trace();
    let result = eval_expr(condition, source, stack, ctx);
    let trace = ctx.cov_finish_condition_trace(condition);
    if let Eval::Normal(Value::Boolean(outcome)) = &result {
        ctx.cov_record_condition_observation(decision, trace, *outcome);
    }
    result
}

fn eval_if(node: Node<'_>, source: &[u8], stack: &mut ScopeStack, ctx: &mut DispatchCtx) -> Eval {
    // Tree-sitter AL grammar fields: `condition`, `consequence`, `alternative`
    // (the ELSE branch). If the grammar doesn't use field names, we fall back to
    // positional named children: [0] = condition, [1] = then-body, [2] = else-body.
    let condition_node = match node.child_by_field_name("condition") {
        Some(n) => n,
        None => match named_stmt_child(node, 0) {
            Some(n) => n,
            None => return Eval::Error(simple_error("if_statement: missing condition node")),
        },
    };

    let cond = match eval_decision_condition(condition_node, node, source, stack, ctx) {
        Eval::Normal(v) => v,
        other => return other,
    };

    if !matches!(cond, Value::Boolean(_)) {
        return Eval::Error(simple_error(&format!(
            "if condition must be Boolean, got {}",
            cond.type_name()
        )));
    }

    // Record the taken side for dynamic coverage. An absent else branch counts
    // as the false side.
    ctx.cov_record_decision(node, cond.is_truthy());

    if cond.is_truthy() {
        let then_node = node
            .child_by_field_name("consequence")
            .or_else(|| named_stmt_child(node, 1));
        match then_node {
            Some(n) => eval_stmt(n, source, stack, ctx),
            None => Eval::Normal(Value::Empty),
        }
    } else {
        let else_node = node
            .child_by_field_name("alternative")
            .or_else(|| named_stmt_child(node, 2));
        match else_node {
            Some(n) => eval_stmt(n, source, stack, ctx),
            None => Eval::Normal(Value::Empty),
        }
    }
}

fn eval_while(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let cond_node = match node
        .child_by_field_name("condition")
        .or_else(|| named_stmt_child(node, 0))
    {
        Some(n) => n,
        None => return Eval::Error(simple_error("while_statement: missing condition")),
    };
    let body_node = node
        .child_by_field_name("body")
        .or_else(|| named_stmt_child(node, 1));

    loop {
        if ctx.is_cancelled() {
            return Eval::Error(simple_error("interpreter cancelled in while loop"));
        }
        if ctx.deadline_exceeded() {
            return Eval::Error(simple_error("interpreter deadline exceeded in while loop"));
        }
        let cond = match eval_decision_condition(cond_node, node, source, stack, ctx) {
            Eval::Normal(v) => v,
            other => return other,
        };
        ctx.cov_record_decision(node, cond.is_truthy());
        if !cond.is_truthy() {
            break;
        }
        if let Some(body) = body_node {
            match eval_stmt(body, source, stack, ctx) {
                Eval::Normal(_) | Eval::Continue => {}
                Eval::Break => break,
                other => return other,
            }
        }
    }
    Eval::Normal(Value::Empty)
}

fn eval_for(node: Node<'_>, source: &[u8], stack: &mut ScopeStack, ctx: &mut DispatchCtx) -> Eval {
    // Grammar fields: iterator (variable), assign (:= op), from (start), direction
    // (kw_to/kw_downto), to (end), body.  Named non-kw children positionally:
    //   0=iterator  1=assign(:=)  2=from  3=to  4=body
    let var_node = node
        .child_by_field_name("iterator")
        .or_else(|| named_stmt_child(node, 0));
    let start_node = node
        .child_by_field_name("from")
        .or_else(|| named_stmt_child(node, 2));
    let end_node = node
        .child_by_field_name("to")
        .or_else(|| named_stmt_child(node, 3));
    let body_node = node
        .child_by_field_name("body")
        .or_else(|| named_stmt_child(node, 4));

    let var_name = match var_node {
        Some(n) => match n.utf8_text(source) {
            Ok(t) => t.trim_matches('"').to_string(),
            Err(_) => return Eval::Error(simple_error("for_statement: invalid variable name")),
        },
        None => return Eval::Error(simple_error("for_statement: missing variable")),
    };

    let start_val = match start_node {
        Some(n) => match eval_expr(n, source, stack, ctx) {
            Eval::Normal(v) => v,
            other => return other,
        },
        None => return Eval::Error(simple_error("for_statement: missing start value")),
    };

    let end_val = match end_node {
        Some(n) => match eval_expr(n, source, stack, ctx) {
            Eval::Normal(v) => v,
            other => return other,
        },
        None => return Eval::Error(simple_error("for_statement: missing end value")),
    };

    // Determine direction by consulting the grammar's `direction` field,
    // which is `kw_downto` or `kw_to`. The prior implementation lower-cased
    // the whole for_statement text and substring-matched "downto" — that
    // mis-fires when an identifier inside the loop body or range contains
    // "downto" (e.g. `for i := 1 to MyDownToValue do ...`).
    let is_downto = match node.child_by_field_name("direction") {
        Some(dir) => dir.kind() == "kw_downto",
        None => node_text(node, source)
            .to_ascii_lowercase()
            .contains("downto"),
    };

    let start_i = match start_val.as_int() {
        Some(n) => n,
        None => {
            return Eval::Error(simple_error(&format!(
                "for_statement: start must be Integer, got {}",
                start_val.type_name()
            )))
        }
    };
    let end_i = match end_val.as_int() {
        Some(n) => n,
        None => {
            return Eval::Error(simple_error(&format!(
                "for_statement: end must be Integer, got {}",
                end_val.type_name()
            )))
        }
    };

    // The counter is a BigInteger if either bound is one, or if the loop
    // variable is already declared BigInteger — so a wide range counts at i64
    // width instead of overflowing the 32-bit Integer trap.
    let counter_big = matches!(start_val, Value::BigInteger(_))
        || matches!(end_val, Value::BigInteger(_))
        || matches!(stack.lookup(&var_name), Some(Value::BigInteger(_)));
    let make_counter = |i: i64| {
        if counter_big {
            Value::BigInteger(i)
        } else {
            Value::Integer(i)
        }
    };

    let mut i = start_i;
    loop {
        if ctx.is_cancelled() {
            return Eval::Error(simple_error("interpreter cancelled in for loop"));
        }
        if ctx.deadline_exceeded() {
            return Eval::Error(simple_error("interpreter deadline exceeded in for loop"));
        }
        let in_range = if is_downto { i >= end_i } else { i <= end_i };
        ctx.cov_record_decision(node, in_range);
        if !in_range {
            break;
        }

        if let Some(slot) = stack.lookup_mut(&var_name) {
            *slot = make_counter(i);
        } else if let Some(frame) = stack.top_mut() {
            frame.bind(&var_name, make_counter(i));
        }

        if let Some(body) = body_node {
            match eval_stmt(body, source, stack, ctx) {
                // Continue still runs the loop increment below (AL semantics).
                Eval::Normal(_) | Eval::Continue => {}
                Eval::Break => break,
                other => return other,
            }
        }

        if is_downto {
            match i.checked_sub(1) {
                Some(next) => i = next,
                None => break, // i == i64::MIN: next iteration would not run anyway
            }
        } else {
            match i.checked_add(1) {
                Some(next) => i = next,
                None => break, // i == i64::MAX: next iteration would not run anyway
            }
        }
    }
    Eval::Normal(Value::Empty)
}

fn eval_foreach(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let var_node = node
        .child_by_field_name("variable")
        .or_else(|| named_stmt_child(node, 0));
    let list_node = node
        .child_by_field_name("collection")
        .or_else(|| named_stmt_child(node, 1));
    let body_node = node
        .child_by_field_name("body")
        .or_else(|| named_stmt_child(node, 2));

    let var_name = match var_node {
        Some(n) => match n.utf8_text(source) {
            Ok(t) => t.trim_matches('"').to_string(),
            Err(_) => return Eval::Error(simple_error("foreach: invalid variable name")),
        },
        None => return Eval::Error(simple_error("foreach: missing variable")),
    };

    let list_val = match list_node {
        Some(n) => match eval_expr(n, source, stack, ctx) {
            Eval::Normal(v) => v,
            other => return other,
        },
        None => return Eval::Error(simple_error("foreach: missing collection")),
    };

    let items = match list_val {
        Value::List(v) | Value::Array(v) => v,
        other => {
            return Eval::Error(simple_error(&format!(
                "foreach: expected List or Array, got {}",
                other.type_name()
            )))
        }
    };

    let mut broke = false;
    for item in items {
        ctx.cov_record_decision(node, true);
        if ctx.is_cancelled() {
            return Eval::Error(simple_error("interpreter cancelled in foreach loop"));
        }
        if ctx.deadline_exceeded() {
            return Eval::Error(simple_error(
                "interpreter deadline exceeded in foreach loop",
            ));
        }
        if let Some(slot) = stack.lookup_mut(&var_name) {
            *slot = item.clone();
        } else if let Some(frame) = stack.top_mut() {
            frame.bind(&var_name, item.clone());
        }

        if let Some(body) = body_node {
            match eval_stmt(body, source, stack, ctx) {
                Eval::Normal(_) | Eval::Continue => {}
                Eval::Break => {
                    broke = true;
                    break;
                }
                other => return other,
            }
        }
    }
    if !broke {
        ctx.cov_record_decision(node, false);
    }
    Eval::Normal(Value::Empty)
}

fn eval_repeat(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let body_node = node
        .child_by_field_name("body")
        .or_else(|| named_stmt_child(node, 0));
    let cond_node = node
        .child_by_field_name("condition")
        .or_else(|| named_stmt_child(node, 1));

    loop {
        if ctx.is_cancelled() {
            return Eval::Error(simple_error("interpreter cancelled in repeat loop"));
        }
        if ctx.deadline_exceeded() {
            return Eval::Error(simple_error("interpreter deadline exceeded in repeat loop"));
        }
        if let Some(body) = body_node {
            match eval_stmt(body, source, stack, ctx) {
                // Continue falls through to the `until` check (AL semantics).
                Eval::Normal(_) | Eval::Continue => {}
                Eval::Break => break,
                other => return other,
            }
        }

        let cond = match cond_node {
            Some(n) => match eval_decision_condition(n, node, source, stack, ctx) {
                Eval::Normal(v) => v,
                other => return other,
            },
            None => return Eval::Error(simple_error("repeat_statement: missing until condition")),
        };

        ctx.cov_record_decision(node, cond.is_truthy());
        if cond.is_truthy() {
            break;
        }
    }
    Eval::Normal(Value::Empty)
}

fn eval_case(node: Node<'_>, source: &[u8], stack: &mut ScopeStack, ctx: &mut DispatchCtx) -> Eval {
    let selector_node = match node
        .child_by_field_name("subject")
        .or_else(|| named_stmt_child(node, 0))
    {
        Some(n) => n,
        None => return Eval::Error(simple_error("case_statement: missing selector")),
    };
    let selector = match eval_expr(selector_node, source, stack, ctx) {
        Eval::Normal(v) => v,
        other => return other,
    };

    // Read the optional `else_body` directly off the case_statement node.
    // The AL grammar carries it as a field on `case_statement`, not as a
    // separate `case_else` / `else_clause` child node — the prior arm
    // matching those kinds never fired (those node kinds don't exist).
    let else_body = node.child_by_field_name("else_body");

    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    let arms: Vec<Node> = children
        .iter()
        .copied()
        .filter(|child| matches!(child.kind(), "case_arm" | "case_branch"))
        .collect();
    for (index, arm) in arms.iter().enumerate() {
        ctx.cov_ensure_path(
            node,
            format!("arm:{}:{}", index + 1, arm.start_position().row + 1),
        );
    }
    let fallback_path = if else_body.is_some() {
        "else"
    } else {
        "no-match"
    };
    ctx.cov_ensure_path(node, fallback_path);

    for (index, child) in arms.iter().enumerate() {
        // Grammar: case_branch has fields 'labels' (case_label_list), 'sep' (:),
        // 'body', and an optional trailing semicolon (named). Use field lookups
        // so a trailing semicolon node can't shift the positional fallback.
        let arm_body = match child
            .child_by_field_name("body")
            .or_else(|| child.named_child(1))
        {
            Some(n) => n,
            None => continue,
        };

        let label_list = child
            .child_by_field_name("labels")
            .or_else(|| child.named_child(0));

        let mut matched = false;
        if let Some(ll) = label_list {
            let mut lc = ll.walk();
            for lbl in ll.named_children(&mut lc) {
                if is_punctuation(lbl.kind()) {
                    continue;
                }
                // A runtime error inside a label expression (div-by-zero,
                // unbound identifier, failing call) propagates — it must not
                // be silently treated as "label did not match".
                let v = match eval_expr(lbl, source, stack, ctx) {
                    Eval::Normal(v) => v,
                    other => return other,
                };
                let label_matches = match &v {
                    Value::Range { start, end } => {
                        match crate::interpreter::eval_expr::value_in_range(&selector, start, end) {
                            Ok(matches) => matches,
                            Err(error) => return Eval::Error(error),
                        }
                    }
                    _ => crate::interpreter::eval_expr::values_equal(&selector, &v),
                };
                if label_matches {
                    matched = true;
                    break;
                }
            }
        }

        if matched {
            // Retain the historical matched/no-match counters and additionally
            // identify the exact arm for multi-way path coverage.
            ctx.cov_record_decision(node, true);
            ctx.cov_record_path(
                node,
                format!("arm:{}:{}", index + 1, child.start_position().row + 1),
            );
            return eval_stmt(arm_body, source, stack, ctx);
        }
    }

    // No arm matched: record the ELSE side of the case decision (whether or
    // not an explicit `else` clause exists).
    ctx.cov_record_decision(node, false);
    ctx.cov_record_path(node, fallback_path);
    match else_body {
        Some(body) => eval_stmt(body, source, stack, ctx),
        None => Eval::Normal(Value::Empty),
    }
}

fn eval_assignment(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let lhs_node = match node
        .child_by_field_name("target")
        .or_else(|| named_stmt_child(node, 0))
    {
        Some(n) => n,
        None => return Eval::Error(simple_error("assignment: missing LHS")),
    };
    let rhs_node = match node
        .child_by_field_name("value")
        .or_else(|| named_stmt_child(node, 1))
    {
        Some(n) => n,
        None => return Eval::Error(simple_error("assignment: missing RHS")),
    };

    let rhs_val = match eval_expr(rhs_node, source, stack, ctx) {
        Eval::Normal(v) => v,
        other => return other,
    };

    // Record field assignment (`Rec."Field" := value`) — handled before the
    // plain-identifier path so the receiver record isn't overwritten wholesale.
    if let Some(result) = records::try_field_assign(lhs_node, source, &rhs_val, stack, ctx) {
        return result;
    }

    let lhs_name = match lhs_node.utf8_text(source) {
        Ok(t) => t.trim_matches('"').to_ascii_lowercase(),
        Err(_) => return Eval::Error(simple_error("assignment: invalid LHS identifier")),
    };

    if let Some(slot) = stack.lookup_mut(&lhs_name) {
        // Preserve the slot's declared type (Code caselessness / integer width)
        // rather than adopting the RHS's — see `coerce_into_slot`.
        match Value::coerce_into_slot(slot, rhs_val) {
            Ok(value) => *slot = value,
            Err(message) => return Eval::Error(simple_error(&message)),
        }
    } else {
        // AL has no implicit declaration: assigning to an unknown name is a
        // compile error in BC, so a typo'd LHS must fail loudly instead of
        // silently creating a fresh variable.
        return Eval::Error(simple_error(&format!(
            "assignment to unbound identifier '{lhs_name}' — variables must be declared"
        )));
    }

    Eval::Normal(Value::Empty)
}

fn eval_exit(node: Node<'_>, source: &[u8], stack: &mut ScopeStack, ctx: &mut DispatchCtx) -> Eval {
    if let Some(expr) = named_stmt_child(node, 0) {
        // The AL grammar represents `exit(value)` as:
        //   exit_statement → argument_list → [expression_list →] expression
        // Unwrap containers to reach the actual expression.
        let inner = unwrap_exit_expr(expr);
        match eval_expr(inner, source, stack, ctx) {
            Eval::Normal(v) => Eval::Exit(v),
            other => other,
        }
    } else {
        Eval::Exit(Value::Empty)
    }
}

/// Unwrap `argument_list` / `expression_list` wrappers that the grammar inserts
/// around the `exit(value)` argument, returning the innermost expression node.
fn unwrap_exit_expr(node: Node<'_>) -> Node<'_> {
    match node.kind() {
        "argument_list" | "expression_list" => {
            // Get first named child; if absent, return the node itself so
            // eval_expr produces a meaningful error rather than panicking.
            node.named_child(0).map(unwrap_exit_expr).unwrap_or(node)
        }
        _ => node,
    }
}

fn eval_asserterror(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let body_node = match named_stmt_child(node, 0) {
        Some(n) => n,
        None => return Eval::Error(simple_error("asserterror: missing body statement")),
    };

    match eval_stmt(body_node, source, stack, ctx) {
        Eval::Error(caught) => {
            // Retain the caught error so the standard pattern
            // `asserterror X; Assert.ExpectedError(GetLastErrorText())` works.
            ctx.last_error = Some(caught);
            Eval::Normal(Value::Empty)
        }
        // Exit unwinds the procedure; asserterror does NOT swallow it. AL
        // semantics treat Exit as control flow that bypasses the assertion.
        // Break/Continue are loop control flow — likewise pass them through.
        cf @ (Eval::Exit(_) | Eval::Break | Eval::Continue) => cf,
        Eval::Normal(_) => Eval::Error(ErrorInfo {
            message: "asserterror: expected an error to be raised, but none was".to_string(),
            error_type: Some("AssertError".to_string()),
            source: None,
        }),
    }
}

fn eval_expression_stmt(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let effective = resolve_to_call_node(node);
    match effective.kind() {
        "member_access_expression" | "method_call_expression" | "call_expression" => {
            // Mark statement position: a `Rec.Get(...)`/`Rec.FindFirst()` miss
            // must raise here (BC) instead of silently yielding false. The
            // record dispatcher consumes and resets the marker.
            ctx.stmt_position = true;
            eval_call(effective, source, stack, ctx)
        }
        // The AL grammar expresses bare calls as `postfix_expression`:
        //   primary_expression + call_suffix   → ForwardCall(args)
        //   primary_expression + member_call_suffix → Recv.Call(args)
        //   primary_expression + scope_call_suffix  → Codeunit::Call(args)
        // We detect calls by checking for a call_suffix / member_call_suffix child.
        "postfix_expression" => {
            if is_call_postfix(effective) {
                ctx.stmt_position = true;
                eval_call(effective, source, stack, ctx)
            } else {
                eval_expr(node, source, stack, ctx)
            }
        }
        _ => eval_expr(node, source, stack, ctx),
    }
}

/// Descend through transparent expression/unary_expression wrappers to find
/// the innermost meaningful node. Stops at the first non-wrapper kind.
///
/// The AL grammar wraps calls as:
///   expression(unary_expression(postfix_expression(primary_expression, call_suffix)))
/// We need to reach `postfix_expression` for call detection.
fn resolve_to_call_node(node: Node<'_>) -> Node<'_> {
    match node.kind() {
        "expression" | "unary_expression" => {
            if node.named_child_count() == 1 {
                if let Some(inner) = node.named_child(0) {
                    return resolve_to_call_node(inner);
                }
            }
            node
        }
        _ => node,
    }
}

/// Return true if this postfix_expression ends with a call_suffix,
/// member_call_suffix, or scope_call_suffix (i.e., it is a call, not just
/// a field access).
pub(crate) fn is_call_postfix(node: Node<'_>) -> bool {
    let count = node.child_count();
    if let Some(last) = (0..count).rev().find_map(|i| node.child(i)) {
        matches!(
            last.kind(),
            "call_suffix" | "member_call_suffix" | "scope_call_suffix"
        )
    } else {
        false
    }
}

pub(crate) fn eval_call(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Determine receiver and procedure name from the call node.
    // tree-sitter AL grammar has several call shapes:
    //   - `identifier ( args )` → no receiver
    //   - `member_access . identifier ( args )` → receiver.proc
    //
    // We handle both by inspecting child kinds.
    let (receiver, proc_name, args_node) = extract_call_parts(node, source);

    // MaxStrLen is defined by the argument's declared Text[N]/Code[N] type,
    // not its current contents. Preserve that lvalue metadata before normal
    // argument evaluation erases the distinction.
    if receiver.is_none() && proc_name.eq_ignore_ascii_case("MaxStrLen") {
        if let Some(arguments) = args_node {
            let argument_nodes = arg_expr_nodes(arguments);
            if argument_nodes.len() == 1 {
                if let Some(name) = simple_lvalue_name(argument_nodes[0], source) {
                    if let Some(length) = stack.declared_text_length(&name) {
                        return Eval::Normal(Value::Integer(length as i64));
                    }
                }
            }
        }
    }

    // A method call on a bound variable routes by the variable's value kind:
    //   * `Value::Record`   → in-memory MockRecord operations.
    //   * `Value::List`     → List of [T] member calls.
    //   * `Value::Codeunit` → dispatch to the declared subtype object.
    // Field-reference args (e.g. `SetRange("Field", …)`) need the AST, so record
    // dispatch is handed the raw `args_node` rather than pre-evaluated values.
    if let Some(recv) = receiver.as_deref() {
        match stack.lookup(recv) {
            Some(Value::Record(_)) if records::supports_record_method(&proc_name) => {
                let Some((table_name, handle)) = records::record_binding(recv, stack, ctx) else {
                    return Eval::Error(simple_error(&format!(
                        "record variable '{recv}' is not bound"
                    )));
                };
                return records::dispatch_record_method(
                    &table_name,
                    handle,
                    &proc_name,
                    args_node,
                    source,
                    stack,
                    ctx,
                );
            }
            Some(Value::List(_)) if records::supports_list_method(&proc_name) => {
                let args = match eval_args_opt(args_node, source, stack, ctx) {
                    Ok(v) => v,
                    Err(ArgsShort::Error(e)) => return Eval::Error(e),
                    Err(ArgsShort::Exit(v)) => return Eval::Exit(v),
                };
                return records::dispatch_list_method(recv, &proc_name, args, stack);
            }
            Some(Value::Text(_) | Value::Code(_)) if records::supports_text_method(&proc_name) => {
                let recv = recv.to_string();
                let args = match eval_args_opt(args_node, source, stack, ctx) {
                    Ok(v) => v,
                    Err(ArgsShort::Error(e)) => return Eval::Error(e),
                    Err(ArgsShort::Exit(v)) => return Eval::Exit(v),
                };
                return records::dispatch_text_method(&recv, &proc_name, args, stack);
            }
            Some(Value::Dict(_))
                if records::supports_dict_method(&proc_name)
                    || proc_name.eq_ignore_ascii_case("get") =>
            {
                let recv = recv.to_string();
                let args = match eval_args_opt(args_node, source, stack, ctx) {
                    Ok(v) => v,
                    Err(ArgsShort::Error(e)) => return Eval::Error(e),
                    Err(ArgsShort::Exit(v)) => return Eval::Exit(v),
                };
                return records::dispatch_dict_method(&recv, &proc_name, args, stack);
            }
            Some(Value::Codeunit { object_name }) => {
                let object_name = object_name.clone();
                let args = match eval_args_opt(args_node, source, stack, ctx) {
                    Ok(v) => v,
                    Err(ArgsShort::Error(e)) => return Eval::Error(e),
                    Err(ArgsShort::Exit(v)) => return Eval::Exit(v),
                };
                let result = dispatch_call_scoped(Some(&object_name), &proc_name, args, stack, ctx);
                apply_var_writebacks(args_node, source, stack, ctx);
                return result;
            }
            _ => {}
        }
    }

    let args = match eval_args_opt(args_node, source, stack, ctx) {
        Ok(v) => v,
        Err(ArgsShort::Error(e)) => return Eval::Error(e),
        Err(ArgsShort::Exit(v)) => return Eval::Exit(v),
    };

    let result = dispatch_call_scoped(receiver.as_deref(), &proc_name, args, stack, ctx);
    apply_var_writebacks(args_node, source, stack, ctx);
    result
}

/// After a workspace procedure returns, propagate the final values of its
/// `var` (by-reference) parameters back into the caller's argument variables.
/// `dispatch_workspace_procedure` populates `ctx.var_writebacks` with
/// `(arg_index, final_value)`; here we map each index to its argument
/// expression and, when that argument is a plain variable reference (a valid
/// lvalue), overwrite the caller's binding. Arguments that are not simple
/// variables (literals, computed expressions, field access) are skipped —
/// they have no single slot to write back to, matching AL, which only permits
/// lvalues in `var` argument positions.
fn apply_var_writebacks(
    args_node: Option<Node<'_>>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) {
    if ctx.var_writebacks.is_empty() {
        return;
    }
    let writebacks = std::mem::take(&mut ctx.var_writebacks);
    let Some(an) = args_node else {
        return;
    };
    let arg_nodes = arg_expr_nodes(an);
    for (idx, val) in writebacks {
        if let Some(node) = arg_nodes.get(idx) {
            if let Some(name) = simple_lvalue_name(*node, source) {
                if let Some(slot) = stack.lookup_mut(&name) {
                    *slot = val;
                }
            }
        }
    }
}

/// Return the lowercased variable name if `node` is a plain variable reference
/// (a single AL identifier, optionally quoted), or `None` for anything that is
/// not a simple lvalue. Text-based and deliberately conservative: it never
/// treats a literal, operator expression, or member access as an lvalue, so it
/// cannot corrupt a caller variable by matching the wrong slot.
fn simple_lvalue_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?.trim();
    let inner = text.trim_matches('"');
    if inner.is_empty() {
        return None;
    }
    let mut chars = inner.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(inner.to_ascii_lowercase())
}

/// Evaluate an optional argument-list node into a `Vec<Value>`.
fn eval_args_opt(
    args_node: Option<Node<'_>>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<Vec<Value>, ArgsShort> {
    // Argument expressions are never in statement position, whatever the
    // enclosing call was — `Foo(Rec.Get(1));` evaluates the Get as an
    // expression.
    ctx.stmt_position = false;
    match args_node {
        Some(an) => eval_args(an, source, stack, ctx),
        None => Ok(vec![]),
    }
}

/// Extract (receiver, procedure_name, args_node) from a call expression node.
///
/// Returns `(None, name, args)` for a bare call or `(Some(recv), name, args)`
/// for a member call.
///
/// Handles two grammar shapes:
/// 1. Legacy: flat children `identifier [arg_list]` or `recv . proc arg_list`.
/// 2. Postfix: `primary_expression { call_suffix | member_call_suffix }`.
fn extract_call_parts<'a>(
    node: Node<'a>,
    source: &[u8],
) -> (Option<String>, String, Option<Node<'a>>) {
    // Children: primary_expression, [member_call_suffix | scope_call_suffix | call_suffix]
    // member_call_suffix has: "." identifier argument_list
    // scope_call_suffix  has: "::" identifier argument_list
    // call_suffix        has: argument_list  (bare call, name is in primary_expression)
    if node.kind() == "postfix_expression" {
        let child_count = node.child_count();
        // Find the suffix (last meaningful child).
        let suffix = (0..child_count)
            .rev()
            .find_map(|i| node.child(i))
            .filter(|n| {
                matches!(
                    n.kind(),
                    "call_suffix" | "member_call_suffix" | "scope_call_suffix"
                )
            });

        if let Some(sfx) = suffix {
            let args = find_argument_list(sfx);

            match sfx.kind() {
                "member_call_suffix" | "scope_call_suffix" => {
                    // Receiver is the primary_expression; proc name is the identifier in the suffix.
                    let receiver_text = node
                        .child(0)
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|t| t.trim_matches('"').to_string());
                    let proc_name = sfx
                        .named_children(&mut sfx.walk())
                        .find(|n| n.kind() == "identifier" || n.kind() == "name")
                        .and_then(|n| n.utf8_text(source).ok())
                        .map(|t| t.trim_matches('"').to_string())
                        .unwrap_or_default();
                    return (receiver_text, proc_name, args);
                }
                "call_suffix" => {
                    // Bare call: name is the text of the primary_expression.
                    let name = node
                        .child(0)
                        .and_then(|n| {
                            // Unwrap primary_expression → identifier if needed.
                            if n.kind() == "primary_expression" {
                                n.named_child(0)
                                    .and_then(|id| id.utf8_text(source).ok())
                                    .map(|t| t.trim_matches('"').to_string())
                                    .or_else(|| {
                                        n.utf8_text(source)
                                            .ok()
                                            .map(|t| t.trim_matches('"').to_string())
                                    })
                            } else {
                                n.utf8_text(source)
                                    .ok()
                                    .map(|t| t.trim_matches('"').to_string())
                            }
                        })
                        .unwrap_or_default();
                    return (None, name, args);
                }
                _ => {}
            }
        }
    }

    let child_count = node.child_count();
    let parts: Vec<(bool, Node)> = (0..child_count)
        .filter_map(|i| node.child(i))
        .map(|c| (c.is_named(), c))
        .collect();

    let args_node = parts
        .iter()
        .find(|(named, c)| {
            *named
                && matches!(
                    c.kind(),
                    "argument_list" | "call_arguments" | "procedure_call_arguments"
                )
        })
        .map(|(_, c)| *c);

    let name_parts: Vec<String> = parts
        .iter()
        .filter(|(named, c)| {
            *named
                && !matches!(
                    c.kind(),
                    "argument_list" | "call_arguments" | "procedure_call_arguments"
                )
        })
        .filter_map(|(_, c)| c.utf8_text(source).ok())
        .map(|t| t.trim_matches('"').to_string())
        .collect();

    match name_parts.as_slice() {
        [] => (None, node_text(node, source), args_node),
        [single] => (None, single.clone(), args_node),
        [recv, proc, ..] => (Some(recv.clone()), proc.clone(), args_node),
    }
}

fn find_argument_list(node: Node<'_>) -> Option<Node<'_>> {
    // First try field "call" (as defined in the grammar for call_suffix).
    if let Some(n) = node.child_by_field_name("call") {
        return Some(n);
    }
    let mut cursor = node.walk();
    let mut found = None;
    for child in node.named_children(&mut cursor) {
        if matches!(
            child.kind(),
            "argument_list" | "call_arguments" | "procedure_call_arguments"
        ) {
            found = Some(child);
            break;
        }
    }
    found
}

/// Short-circuit signal from argument evaluation: either an error stops
/// the call entirely, or an `exit` inside an argument expression should
/// unwind the enclosing procedure rather than being absorbed as a value.
enum ArgsShort {
    Error(ErrorInfo),
    /// AL semantics: `exit(v)` inside `Foo(exit(42), 1)` unwinds the
    /// caller's procedure with value `v`, not the inner expression.
    Exit(Value),
}

/// Evaluate an argument list node.
///
/// Handles both flat shapes (direct `expression` children) and the grammar shape
/// where `argument_list` wraps a single `expression_list` containing the
/// comma-separated `expression` items.
fn eval_args(
    args_node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<Vec<Value>, ArgsShort> {
    let mut out = Vec::new();
    eval_args_into(args_node, source, stack, ctx, &mut out)?;
    Ok(out)
}

fn eval_args_into(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
    out: &mut Vec<Value>,
) -> Result<(), ArgsShort> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if is_punctuation(child.kind()) {
            continue;
        }
        // Unwrap expression_list — it is a named container for comma-separated args.
        if child.kind() == "expression_list" {
            eval_args_into(child, source, stack, ctx, out)?;
            continue;
        }
        match eval_expr(child, source, stack, ctx) {
            Eval::Normal(v) => out.push(v),
            Eval::Error(e) => return Err(ArgsShort::Error(e)),
            Eval::Exit(v) => return Err(ArgsShort::Exit(v)),
            // An expression cannot legally produce break/continue (they are
            // statements); treat as an error rather than silently dropping.
            Eval::Break | Eval::Continue => {
                return Err(ArgsShort::Error(simple_error(
                    "break/continue is not valid in an expression",
                )))
            }
        }
    }
    Ok(())
}

/// Collect the individual argument *expression nodes* of an `argument_list`
/// (unwrapping the `expression_list` container and skipping punctuation),
/// without evaluating them. Used by record-method dispatch, where the first
/// argument of `SetRange`/`SetFilter`/`SetCurrentKey` is a field *reference*
/// (read as a name) rather than a value to evaluate.
pub(crate) fn arg_expr_nodes(args_node: Node<'_>) -> Vec<Node<'_>> {
    let mut out = Vec::new();
    collect_arg_nodes(args_node, &mut out);
    out
}

fn collect_arg_nodes<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if is_punctuation(child.kind()) {
            continue;
        }
        if child.kind() == "expression_list" {
            collect_arg_nodes(child, out);
            continue;
        }
        out.push(child);
    }
}

fn simple_error(msg: &str) -> ErrorInfo {
    ErrorInfo {
        message: msg.to_string(),
        error_type: None,
        source: None,
    }
}

fn node_text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source)
        .unwrap_or("")
        .trim_matches('"')
        .to_string()
}

fn is_punctuation(kind: &str) -> bool {
    // Skip pure syntax tokens: punctuation, comments, and AL keyword nodes
    // (the grammar emits named `kw_*` nodes for control-flow keywords like
    // `kw_begin`, `kw_end`, `kw_then`, `kw_do`, etc.).
    matches!(
        kind,
        "comment" | ";" | "," | "(" | ")" | "{" | "}" | "semicolon" | "comma"
    ) || kind.starts_with("kw_")
}

/// Get the Nth named child that is not a keyword or punctuation node.
///
/// Used to handle grammars where keyword nodes (kw_begin, kw_then, etc.)
/// appear as named siblings and would otherwise shift positional indices.
fn named_stmt_child(node: Node<'_>, n: usize) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node
        .named_children(&mut cursor)
        .filter(|c| !is_punctuation(c.kind()))
        .collect();
    children.into_iter().nth(n)
}

// CASE selector-vs-arm matching uses the same `values_equal` as `=`/`<>`
// BC evaluates a CASE arm exactly like an equality test, so Text is
// case-sensitive and Code is case-insensitive.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::scope::{CallFrame, ScopeStack};
    use crate::interpreter::value::Value;
    use crate::test_support::MockSource as Workspace;
    use std::sync::Arc;

    fn ctx() -> DispatchCtx {
        DispatchCtx::new_pure(Arc::new(Workspace::new()))
    }

    /// Parse a snippet and run eval_stmt on the first statement in the body.
    ///
    /// Wraps `source_snippet` in a full codeunit so the AL parser accepts it.
    fn run_stmt(source_snippet: &str) -> (Eval, ScopeStack) {
        let wrapper = format!(
            "codeunit 50100 \"X\"\n{{\n    procedure Test()\n    var\n        x: Integer;\n        y: BigInteger;\n        s: Text;\n    begin\n        {source_snippet}\n    end;\n}}"
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();

        let body = find_proc_body(root, bytes).expect("could not find procedure body");

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("x", Value::Integer(0));
        frame.bind("y", Value::BigInteger(0));
        frame.bind("s", Value::Text(String::new()));
        stack.push(frame);

        let mut ctx = ctx();
        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        (eval, stack)
    }

    fn find_proc_body<'a>(node: Node<'a>, _source: &[u8]) -> Option<Node<'a>> {
        let mut stack = vec![node];
        while let Some(current) = stack.pop() {
            if current.kind() == "begin_end_block" {
                return Some(current);
            }
            let mut cursor = current.walk();
            stack.extend(current.named_children(&mut cursor));
        }
        None
    }

    /// Run statements against a frame that declares `bi: BigInteger(0)`, so the
    /// declared-type stickiness path can be exercised end to end.
    fn run_stmt_with_bigint(source_snippet: &str) -> (Eval, ScopeStack) {
        let wrapper = format!(
            "codeunit 50100 \"X\"\n{{\n    procedure Test()\n    var\n        bi: BigInteger;\n    begin\n        {source_snippet}\n    end;\n}}"
        );
        let result = al_syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).expect("procedure body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("bi", Value::BigInteger(0));
        stack.push(frame);
        let mut ctx = ctx();
        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        (eval, stack)
    }

    #[test]
    fn biginteger_literal_arithmetic_end_to_end() {
        let (eval, stack) = run_stmt("y := 5000000000 + 1;");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("y"), Some(&Value::BigInteger(5_000_000_001)));
    }

    #[test]
    fn unary_negation_of_biginteger() {
        let (eval, stack) = run_stmt("y := -5000000000;");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("y"), Some(&Value::BigInteger(-5_000_000_000)));
    }

    #[test]
    fn integer_literal_overflow_errors_end_to_end() {
        let (eval, _) = run_stmt("x := 2147483647 * 2;");
        assert!(eval.is_error(), "expected Integer overflow, got {eval:?}");
    }

    #[test]
    fn declared_biginteger_variable_keeps_width() {
        let (_e1, stack1) = run_stmt_with_bigint("bi := 5;");
        assert_eq!(
            stack1.lookup("bi"),
            Some(&Value::BigInteger(5)),
            "small literal assigned to a BigInteger slot must stay BigInteger"
        );
        let (eval, stack) = run_stmt_with_bigint("bi := 5; bi := bi * 1000000000;");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("bi"), Some(&Value::BigInteger(5_000_000_000)));
    }

    #[test]
    fn assignment_binds_value() {
        let (eval, stack) = run_stmt("x := 42;");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "expected Normal, got {:?}",
            eval
        );
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(42)));
    }

    #[test]
    fn assignment_rhs_error_propagates() {
        let (eval, _) = run_stmt("x := 1 div 0;");
        assert!(
            eval.is_error(),
            "expected Error from div-by-zero assignment"
        );
    }

    #[test]
    fn if_true_branch_executes() {
        let (eval, stack) = run_stmt("if true then x := 99;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(99)));
    }

    #[test]
    fn if_false_branch_skipped() {
        let (eval, stack) = run_stmt("if false then x := 99;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(0)));
    }

    #[test]
    fn exit_without_value_returns_empty() {
        let (eval, _) = run_stmt("exit;");
        assert!(
            matches!(eval, Eval::Exit(Value::Empty)),
            "expected Exit(Empty), got {:?}",
            eval
        );
    }

    #[test]
    fn asserterror_catches_error() {
        let (eval, _) = run_stmt("asserterror error('boom');");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "asserterror should succeed when body errors; got {:?}",
            eval
        );
    }

    #[test]
    fn asserterror_no_error_is_itself_error() {
        let (eval, _) = run_stmt("asserterror x := 1;");
        assert!(
            eval.is_error(),
            "asserterror should fail when body doesn't error; got {:?}",
            eval
        );
        if let Eval::Error(e) = eval {
            assert!(
                e.message.contains("expected") || e.message.contains("none was"),
                "error message should mention 'expected': {}",
                e.message
            );
        }
    }

    #[test]
    fn string_literal_escaped_quotes_survive_unescaping() {
        // '''' is the one-character string ' — stripping ALL outer quotes
        // before unescaping used to collapse it to "".
        let (eval, stack) = run_stmt("s := '''';");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("s"), Some(&Value::Text("'".into())));

        let (eval, stack) = run_stmt("s := 'abc''';");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("s"), Some(&Value::Text("abc'".into())));
    }

    #[test]
    fn assignment_to_undeclared_variable_errors() {
        // A typo'd LHS must fail loudly, not silently create a variable.
        let (eval, _) = run_stmt("Totl := 5;");
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains("unbound identifier 'totl'"),
                "got: {}",
                e.message
            ),
            other => panic!("expected an error for an undeclared LHS, got {other:?}"),
        }
    }

    #[test]
    fn integer_slot_rejects_out_of_range_assignment() {
        // BC raises an overflow error when a BigInteger value outside the
        // i32 range is narrowed into an Integer variable.
        let (eval, _) = run_stmt("x := 5000000000;");
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains("outside the Integer range"),
                "got: {}",
                e.message
            ),
            other => panic!("expected an Integer overflow error, got {other:?}"),
        }
    }

    #[test]
    fn decimal_slot_keeps_decimal_type_for_integer_rhs() {
        // `d := 5` must store Decimal(5) so a later `d div 2` fails like BC
        // (div is integer-only).
        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        d: Decimal;\n        x: Integer;\n    begin\n        d := 5;\n        x := d div 2;\n    end;\n}";
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(result.tree.root_node(), bytes).expect("body");
        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        if let Some(proc_node) = body.parent() {
            crate::interpreter::dispatch::bind_procedure_locals(proc_node, bytes, &mut frame);
        }
        stack.push(frame);
        let mut ctx = ctx();
        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains("integer-only"),
                "div on a Decimal slot must fail like BC; got: {}",
                e.message
            ),
            other => panic!("expected a div-on-Decimal error, got {other:?}"),
        }
    }

    #[test]
    fn case_label_runtime_error_propagates() {
        // A failing label expression (here: an unbound identifier) must
        // propagate, not be treated as "no match".
        let (eval, _) = run_stmt("case x of NoSuchConst: x := 1; else x := 2; end;");
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains("unbound identifier"),
                "got: {}",
                e.message
            ),
            other => panic!("expected the label error to propagate, got {other:?}"),
        }
    }

    #[test]
    fn asserterror_captures_error_for_get_last_error_text() {
        let (eval, stack) = run_stmt("asserterror error('boom'); s := GetLastErrorText();");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(
            stack.lookup("s"),
            Some(&Value::Text("boom".into())),
            "GetLastErrorText must return the asserterror-caught message"
        );

        let (eval, stack) =
            run_stmt("asserterror error('boom'); ClearLastError(); s := GetLastErrorText();");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("s"), Some(&Value::Text(String::new())));
    }

    #[test]
    fn text_instance_methods_execute_locally() {
        let (eval, stack) = run_stmt("s := 'Hello World'; s := s.Replace('World', 'AL');");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("s"), Some(&Value::Text("Hello AL".into())));

        let (eval, stack) = run_stmt("s := '  pad  '; s := s.Trim();");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("s"), Some(&Value::Text("pad".into())));

        let (eval, stack) = run_stmt("s := 'abc'; if s.Contains('b') then x := 1;");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(1)));

        let (eval, stack) = run_stmt("s := 'abcdef'; s := s.Substring(2, 3);");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("s"), Some(&Value::Text("bcd".into())));
    }

    #[test]
    fn set_literal_in_loop_uses_cached_fragment_parses() {
        // `in [...]` members are re-parsed as expression fragments; inside a
        // loop each member must parse once (memoized) and keep evaluating
        // correctly on every iteration from the cached tree.
        let (eval, stack) =
            run_stmt("while x < 3 do begin if (x + 1) in [1, 2, 3] then x := x + 1; end;");
        assert!(matches!(eval, Eval::Normal(_)), "got {eval:?}");
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(3)));
    }

    #[test]
    fn while_loop_accumulates() {
        let (eval, stack) = run_stmt("while x < 5 do x := x + 1;");
        assert!(matches!(eval, Eval::Normal(_)), "got {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(5)));
    }

    #[test]
    fn runaway_while_loop_trips_deadline() {
        use crate::interpreter::scope::{CallFrame, ScopeStack};
        use crate::interpreter::value::Value;
        use crate::test_support::MockSource as Workspace;
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; while x >= 0 do x := x + 1;\n    end;\n}";
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).unwrap();

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("x", Value::Integer(0));
        stack.push(frame);

        let mut ctx = DispatchCtx::new_pure(Arc::new(Workspace::new()));
        ctx.deadline = Some(std::time::Instant::now() + std::time::Duration::from_millis(5));

        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains("deadline exceeded"),
                "expected deadline message, got: {}",
                e.message
            ),
            other => panic!("expected deadline error, got {other:?}"),
        }
    }

    #[test]
    fn cancel_token_interrupts_while_loop() {
        use crate::interpreter::scope::{CallFrame, ScopeStack};
        use crate::interpreter::value::Value;
        use crate::test_support::MockSource as Workspace;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; while x >= 0 do x := x + 1;\n    end;\n}";
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).unwrap();

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("x", Value::Integer(0));
        stack.push(frame);

        let cancel = Arc::new(AtomicBool::new(false));
        let mut ctx = DispatchCtx::new_pure(Arc::new(Workspace::new()));
        ctx.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        ctx.cancel = Some(cancel.clone());

        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            cancel.store(true, Ordering::Relaxed);
        });

        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        canceller.join().unwrap();

        match eval {
            Eval::Error(e) => assert!(
                e.message.contains("cancelled"),
                "expected cancel message, got: {}",
                e.message
            ),
            other => panic!("expected cancel error, got {other:?}"),
        }
    }

    #[test]
    fn cancel_token_can_be_attached_and_pre_signalled() {
        use crate::interpreter::scope::{CallFrame, ScopeStack};
        use crate::interpreter::value::Value;
        use crate::test_support::MockSource as Workspace;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; while x >= 0 do x := x + 1;\n    end;\n}";
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).unwrap();

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("x", Value::Integer(0));
        stack.push(frame);

        let mut ctx = DispatchCtx::new_pure(Arc::new(Workspace::new()));
        ctx.cancel = Some(Arc::new(AtomicBool::new(true)));

        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        assert!(eval.is_error());
        if let Eval::Error(e) = eval {
            assert!(e.message.contains("cancelled"), "got: {}", e.message);
        }
    }

    #[test]
    fn block_executes_sequence() {
        let (eval, stack) = run_stmt("begin x := 1; x := x + 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(2)));
    }

    #[test]
    fn block_short_circuits_on_error() {
        let (eval, stack) = run_stmt("begin error('stop'); x := 99; end;");
        assert!(eval.is_error());
        assert_ne!(
            stack.lookup("x"),
            Some(&Value::Integer(99)),
            "x should NOT be 99 after short-circuit error"
        );
    }

    #[test]
    fn asserterror_propagates_exit() {
        let (eval, _) = run_stmt("asserterror exit;");
        assert!(
            matches!(eval, Eval::Exit(_)),
            "asserterror wrapping exit must propagate Eval::Exit, got: {:?}",
            eval
        );
    }

    #[test]
    fn for_downto_decrements_when_direction_field_is_downto() {
        let (eval, stack) = run_stmt("for x := 3 downto 1 do begin end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(1)));
    }

    #[test]
    fn for_to_with_substring_downto_in_body_still_counts_up() {
        let (eval, stack) = run_stmt(r#"for x := 1 to 3 do begin s := 'mydowntoval'; end;"#);
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(3)),
            "loop with `to` direction must count UP regardless of body content; \
             prior substring detection would have flipped this to a downto loop"
        );
    }

    #[test]
    fn case_else_branch_runs_when_no_arm_matches() {
        let (eval, stack) = run_stmt("case 42 of 1: x := 1; 2: x := 2; else x := 99; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(99)),
            "else branch should fire when no case arm matches the selector"
        );
    }

    #[test]
    fn case_integer_decimal_compare_exact_for_whole_numbers() {
        let (eval, stack) = run_stmt("case 5 of 5.0: x := 7; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(7)));
    }

    #[test]
    fn case_integer_decimal_compare_rejects_fractional() {
        let (eval, stack) = run_stmt("case 5 of 5.1: x := 7; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(1)));
    }

    #[test]
    fn exit_inside_argument_propagates_out_of_call() {
        let (eval, _) = run_stmt("case 1 of 1: exit; else x := 99; end;");
        assert!(
            matches!(eval, Eval::Exit(_)),
            "exit inside a case arm must unwind the enclosing procedure; got {:?}",
            eval
        );
    }

    #[test]
    fn deep_nesting_errors_instead_of_stack_overflow() {
        let depth = 1500;
        let mut body = String::new();
        for _ in 0..depth {
            body.push_str("begin ");
        }
        body.push_str("x := 1; ");
        for _ in 0..depth {
            body.push_str("end; ");
        }
        let (eval, _) = run_stmt(&body);
        assert!(
            eval.is_error(),
            "deep nesting must produce a clean Eval::Error, got {:?}",
            eval
        );
        let msg = match eval {
            Eval::Error(info) => info.message,
            _ => String::new(),
        };
        assert!(
            msg.contains("AST nesting depth exceeded"),
            "error message must name the depth cap, got: {msg}"
        );
    }

    #[test]
    fn for_to_counts_up_and_leaves_last_value() {
        let (eval, stack) = run_stmt("for x := 1 to 3 do begin end;");
        assert!(matches!(eval, Eval::Normal(_)), "got {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(3)));
    }

    #[test]
    fn for_to_accumulates_in_body() {
        let (eval, stack) = run_stmt("for x := 1 to 4 do begin end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(4)));
    }

    #[test]
    fn for_empty_range_does_not_run_body() {
        let (eval, stack) = run_stmt("for x := 5 to 1 do begin end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(0)),
            "empty upward range must not bind the loop variable"
        );
    }

    #[test]
    fn for_start_must_be_integer() {
        let (eval, _) = run_stmt("for x := 'a' to 3 do begin end;");
        assert!(eval.is_error(), "non-integer FOR start must error");
        if let Eval::Error(e) = eval {
            assert!(
                e.message.contains("start must be Integer"),
                "got: {}",
                e.message
            );
        }
    }

    #[test]
    fn for_end_must_be_integer() {
        let (eval, _) = run_stmt("for x := 1 to 'z' do begin end;");
        assert!(eval.is_error(), "non-integer FOR end must error");
        if let Eval::Error(e) = eval {
            assert!(
                e.message.contains("end must be Integer"),
                "got: {}",
                e.message
            );
        }
    }

    #[test]
    fn for_body_error_propagates() {
        let (eval, _) = run_stmt("for x := 1 to 3 do begin error('boom'); end;");
        assert!(eval.is_error(), "FOR body error must propagate");
    }

    #[test]
    fn foreach_over_non_collection_errors() {
        let (eval, _) = run_stmt("foreach x in 42 do begin end;");
        assert!(eval.is_error(), "foreach over a scalar must error");
        if let Eval::Error(e) = eval {
            assert!(
                e.message.contains("expected List or Array"),
                "got: {}",
                e.message
            );
        }
    }

    #[test]
    fn repeat_runs_body_at_least_once() {
        let (eval, stack) = run_stmt("repeat x := x + 1; until x >= 0;");
        assert!(matches!(eval, Eval::Normal(_)), "got {:?}", eval);
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(1)),
            "REPEAT must run the body at least once"
        );
    }

    #[test]
    fn repeat_loops_until_condition_true() {
        let (eval, stack) = run_stmt("repeat x := x + 1; until x >= 3;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(3)));
    }

    #[test]
    fn repeat_deadline_trips_on_runaway() {
        use crate::interpreter::scope::{CallFrame, ScopeStack};
        use crate::interpreter::value::Value;
        use crate::test_support::MockSource as Workspace;
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; repeat x := x + 1; until x < 0;\n    end;\n}";
        let result = al_syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).unwrap();

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("x", Value::Integer(0));
        stack.push(frame);

        let mut ctx = DispatchCtx::new_pure(Arc::new(Workspace::new()));
        ctx.deadline = Some(std::time::Instant::now() + std::time::Duration::from_millis(5));

        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        match eval {
            Eval::Error(e) => assert!(
                e.message.contains("deadline exceeded"),
                "expected deadline message, got: {}",
                e.message
            ),
            other => panic!("expected deadline error, got {other:?}"),
        }
    }

    #[test]
    fn case_matches_arm() {
        let (eval, stack) = run_stmt("case 1 of 1: x := 2; else x := 9; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(2)),
            "the matching arm body must run, not the else branch"
        );
    }

    #[test]
    fn case_no_match_no_else_is_noop() {
        let (eval, stack) = run_stmt("case 99 of 1: x := 1; 2: x := 2; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(0)),
            "no matching arm and no else must leave state untouched"
        );
    }

    #[test]
    fn case_multi_label_arm_matches_any_label() {
        let (eval, stack) = run_stmt("case 2 of 1, 2, 3: x := 7; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(7)),
            "a multi-label arm must match if ANY label equals the selector"
        );
    }

    #[test]
    fn case_text_selector_is_case_sensitive() {
        let (eval, stack) = run_stmt("case 'ABC' of 'abc': x := 5; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(1)),
            "Text CASE labels compare case-sensitively; 'ABC' must not match 'abc'"
        );
    }

    #[test]
    fn case_text_selector_exact_match() {
        let (eval, stack) = run_stmt("case 'abc' of 'abc': x := 5; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(5)));
    }

    #[test]
    fn if_else_branch_executes_when_false() {
        let (eval, stack) = run_stmt("if false then x := 1 else x := 2;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(2)));
    }

    #[test]
    fn if_non_boolean_condition_errors() {
        let (eval, _) = run_stmt("if 5 then x := 1;");
        assert!(eval.is_error(), "non-boolean IF condition must error");
        if let Eval::Error(e) = eval {
            assert!(e.message.contains("must be Boolean"), "got: {}", e.message);
        }
    }

    #[test]
    fn while_body_error_propagates() {
        let (eval, _) = run_stmt("while x < 5 do begin error('boom'); end;");
        assert!(eval.is_error(), "WHILE body error must propagate");
    }

    #[test]
    fn exit_with_value_returns_it() {
        let (eval, _) = run_stmt("exit(7);");
        assert!(
            matches!(eval, Eval::Exit(Value::Integer(7))),
            "expected Exit(Integer(7)), got {:?}",
            eval
        );
    }
}
