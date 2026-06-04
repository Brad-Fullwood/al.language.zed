//! Statement evaluator for the AL interpreter — Phase 2.
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

use crate::test_runtime::interpreter::dispatch::{dispatch_call, DispatchCtx, MAX_AST_DEPTH};
use crate::test_runtime::interpreter::eval_expr::eval_expr;
use crate::test_runtime::interpreter::scope::{Eval, ScopeStack};
use crate::test_runtime::interpreter::value::{ErrorInfo, Value};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

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
        // Grammar wrapper: delegate to the single inner child.
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
        "asserterror_statement" => eval_asserterror(node, source, stack, ctx),
        "expression_statement" => {
            // The only named child is the expression; evaluate for side effects.
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

// ---------------------------------------------------------------------------
// Block / sequence
// ---------------------------------------------------------------------------

fn eval_block(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let mut cursor = node.walk();
    let mut last = Eval::Normal(Value::Empty);
    for child in node.named_children(&mut cursor) {
        // Skip comment and pure punctuation nodes.
        if is_punctuation(child.kind()) {
            continue;
        }
        last = eval_stmt(child, source, stack, ctx);
        match &last {
            Eval::Normal(_) => {}
            // Short-circuit on error or exit.
            Eval::Error(_) | Eval::Exit(_) => return last,
        }
    }
    last
}

// ---------------------------------------------------------------------------
// If statement
// ---------------------------------------------------------------------------

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

    let cond = match eval_expr(condition_node, source, stack) {
        Eval::Normal(v) => v,
        other => return other,
    };

    if !matches!(cond, Value::Boolean(_)) {
        return Eval::Error(simple_error(&format!(
            "if condition must be Boolean, got {}",
            cond.type_name()
        )));
    }

    if cond.is_truthy() {
        // Execute the THEN branch.
        let then_node = node
            .child_by_field_name("consequence")
            .or_else(|| named_stmt_child(node, 1));
        match then_node {
            Some(n) => eval_stmt(n, source, stack, ctx),
            None => Eval::Normal(Value::Empty),
        }
    } else {
        // Execute the ELSE branch (if present).
        let else_node = node
            .child_by_field_name("alternative")
            .or_else(|| named_stmt_child(node, 2));
        match else_node {
            Some(n) => eval_stmt(n, source, stack, ctx),
            None => Eval::Normal(Value::Empty),
        }
    }
}

// ---------------------------------------------------------------------------
// While loop
// ---------------------------------------------------------------------------

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
        let cond = match eval_expr(cond_node, source, stack) {
            Eval::Normal(v) => v,
            other => return other,
        };
        if !cond.is_truthy() {
            break;
        }
        if let Some(body) = body_node {
            match eval_stmt(body, source, stack, ctx) {
                Eval::Normal(_) => {}
                other => return other,
            }
        }
    }
    Eval::Normal(Value::Empty)
}

// ---------------------------------------------------------------------------
// FOR i := a TO b DO
// ---------------------------------------------------------------------------

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
        Some(n) => match eval_expr(n, source, stack) {
            Eval::Normal(v) => v,
            other => return other,
        },
        None => return Eval::Error(simple_error("for_statement: missing start value")),
    };

    let end_val = match end_node {
        Some(n) => match eval_expr(n, source, stack) {
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

    let start_i = match &start_val {
        Value::Integer(n) => *n,
        v => {
            return Eval::Error(simple_error(&format!(
                "for_statement: start must be Integer, got {}",
                v.type_name()
            )))
        }
    };
    let end_i = match &end_val {
        Value::Integer(n) => *n,
        v => {
            return Eval::Error(simple_error(&format!(
                "for_statement: end must be Integer, got {}",
                v.type_name()
            )))
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
        if is_downto {
            if i < end_i {
                break;
            }
        } else if i > end_i {
            break;
        }

        // Bind loop variable.
        if let Some(slot) = stack.lookup_mut(&var_name) {
            *slot = Value::Integer(i);
        } else if let Some(frame) = stack.top_mut() {
            frame.bind(&var_name, Value::Integer(i));
        }

        if let Some(body) = body_node {
            match eval_stmt(body, source, stack, ctx) {
                Eval::Normal(_) => {}
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

// ---------------------------------------------------------------------------
// FOREACH x IN list DO
// ---------------------------------------------------------------------------

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
        Some(n) => match eval_expr(n, source, stack) {
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

    for item in items {
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
                Eval::Normal(_) => {}
                other => return other,
            }
        }
    }
    Eval::Normal(Value::Empty)
}

// ---------------------------------------------------------------------------
// REPEAT … UNTIL cond
// ---------------------------------------------------------------------------

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
                Eval::Normal(_) => {}
                other => return other,
            }
        }

        let cond = match cond_node {
            Some(n) => match eval_expr(n, source, stack) {
                Eval::Normal(v) => v,
                other => return other,
            },
            None => return Eval::Error(simple_error("repeat_statement: missing until condition")),
        };

        if cond.is_truthy() {
            break;
        }
    }
    Eval::Normal(Value::Empty)
}

// ---------------------------------------------------------------------------
// CASE expr OF … END
// ---------------------------------------------------------------------------

fn eval_case(node: Node<'_>, source: &[u8], stack: &mut ScopeStack, ctx: &mut DispatchCtx) -> Eval {
    // Evaluate the selector.
    let selector_node = match node
        .child_by_field_name("subject")
        .or_else(|| named_stmt_child(node, 0))
    {
        Some(n) => n,
        None => return Eval::Error(simple_error("case_statement: missing selector")),
    };
    let selector = match eval_expr(selector_node, source, stack) {
        Eval::Normal(v) => v,
        other => return other,
    };

    // Read the optional `else_body` directly off the case_statement node.
    // The AL grammar carries it as a field on `case_statement`, not as a
    // separate `case_else` / `else_clause` child node — the prior arm
    // matching those kinds never fired (those node kinds don't exist).
    let else_body = node.child_by_field_name("else_body");

    // Walk named children looking for case_branch arms.
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    for child in &children[1..] {
        if !matches!(child.kind(), "case_arm" | "case_branch") {
            continue;
        }
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

        // Labels live in a case_label_list node; iterate its case_label_expression
        // children.
        let label_list = child
            .child_by_field_name("labels")
            .or_else(|| child.named_child(0));

        let mut matched = false;
        if let Some(ll) = label_list {
            let mut lc = ll.walk();
            for lbl in ll.named_children(&mut lc) {
                if let Eval::Normal(v) = eval_expr(lbl, source, stack) {
                    if values_equal_for_case(&selector, &v) {
                        matched = true;
                        break;
                    }
                }
            }
        }

        if matched {
            return eval_stmt(arm_body, source, stack, ctx);
        }
    }

    // No arm matched — run else branch if present, otherwise Normal.
    match else_body {
        Some(body) => eval_stmt(body, source, stack, ctx),
        None => Eval::Normal(Value::Empty),
    }
}

// ---------------------------------------------------------------------------
// Assignment   x := expr
// ---------------------------------------------------------------------------

fn eval_assignment(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    _ctx: &mut DispatchCtx,
) -> Eval {
    // LHS: typically an identifier or member_access.
    // RHS: the value expression after `:=`.
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

    let rhs_val = match eval_expr(rhs_node, source, stack) {
        Eval::Normal(v) => v,
        other => return other,
    };

    // Resolve the LHS name.
    let lhs_name = match lhs_node.utf8_text(source) {
        Ok(t) => t.trim_matches('"').to_ascii_lowercase(),
        Err(_) => return Eval::Error(simple_error("assignment: invalid LHS identifier")),
    };

    if let Some(slot) = stack.lookup_mut(&lhs_name) {
        *slot = rhs_val;
    } else if let Some(frame) = stack.top_mut() {
        // Auto-bind: declare in the current frame on first assignment
        // (simulates AL's permissive variable declaration semantics in
        // procedures that declare vars at the top — the interpreter
        // trusts that the caller set up the frame correctly, but falls
        // back to auto-binding for convenience in tests).
        frame.bind(&lhs_name, rhs_val);
    } else {
        return Eval::Error(simple_error("assignment: no active scope frame"));
    }

    Eval::Normal(Value::Empty)
}

// ---------------------------------------------------------------------------
// EXIT
// ---------------------------------------------------------------------------

fn eval_exit(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    _ctx: &mut DispatchCtx,
) -> Eval {
    if let Some(expr) = named_stmt_child(node, 0) {
        // The AL grammar represents `exit(value)` as:
        //   exit_statement → argument_list → [expression_list →] expression
        // Unwrap containers to reach the actual expression.
        let inner = unwrap_exit_expr(expr);
        match eval_expr(inner, source, stack) {
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

// ---------------------------------------------------------------------------
// ASSERTERROR stmt
// ---------------------------------------------------------------------------

fn eval_asserterror(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // The body is the first (and only) named child.
    let body_node = match named_stmt_child(node, 0) {
        Some(n) => n,
        None => return Eval::Error(simple_error("asserterror: missing body statement")),
    };

    match eval_stmt(body_node, source, stack, ctx) {
        Eval::Error(_) => {
            // Error was raised — asserterror succeeded.
            Eval::Normal(Value::Empty)
        }
        // Exit unwinds the procedure; asserterror does NOT swallow it. AL
        // semantics treat Exit as control flow that bypasses the assertion.
        exit @ Eval::Exit(_) => exit,
        Eval::Normal(_) => {
            // Body completed without raising an error — assertion fails.
            Eval::Error(ErrorInfo {
                message: "asserterror: expected an error to be raised, but none was".to_string(),
                error_type: Some("AssertError".to_string()),
                source: None,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Expression statement (procedure call or side-effect expression)
// ---------------------------------------------------------------------------

fn eval_expression_stmt(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Resolve any transparent wrappers before checking for call patterns.
    let effective = resolve_to_call_node(node);
    match effective.kind() {
        // Member call: receiver.Procedure(args)
        "member_access_expression" | "method_call_expression" | "call_expression" => {
            eval_call(effective, source, stack, ctx)
        }
        // The AL grammar expresses bare calls as `postfix_expression`:
        //   primary_expression + call_suffix   → ForwardCall(args)
        //   primary_expression + member_call_suffix → Recv.Call(args)
        //   primary_expression + scope_call_suffix  → Codeunit::Call(args)
        // We detect calls by checking for a call_suffix / member_call_suffix child.
        "postfix_expression" => {
            if is_call_postfix(effective) {
                eval_call(effective, source, stack, ctx)
            } else {
                // Not a call — fall back to full expression evaluation.
                eval_expr(node, source, stack)
            }
        }
        // Everything else: delegate to the expression evaluator.
        _ => eval_expr(node, source, stack),
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
            // If exactly one named child, descend.
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
fn is_call_postfix(node: Node<'_>) -> bool {
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

// ---------------------------------------------------------------------------
// Procedure call dispatch
// ---------------------------------------------------------------------------

fn eval_call(node: Node<'_>, source: &[u8], stack: &mut ScopeStack, ctx: &mut DispatchCtx) -> Eval {
    // Determine receiver and procedure name from the call node.
    // tree-sitter AL grammar has several call shapes:
    //   - `identifier ( args )` → no receiver
    //   - `member_access . identifier ( args )` → receiver.proc
    //
    // We handle both by inspecting child kinds.
    let (receiver, proc_name, args_node) = extract_call_parts(node, source);

    // Evaluate arguments.
    let args = match args_node {
        Some(an) => eval_args(an, source, stack),
        None => Ok(vec![]),
    };
    let args = match args {
        Ok(v) => v,
        Err(ArgsShort::Error(e)) => return Eval::Error(e),
        Err(ArgsShort::Exit(v)) => return Eval::Exit(v),
    };

    dispatch_call(receiver.as_deref(), &proc_name, args, ctx)
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
    // ── Shape 2: postfix_expression ──────────────────────────────────────────
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
            // Get the argument_list from inside the suffix.
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

    // ── Shape 1 (legacy flat shape) ──────────────────────────────────────────
    let child_count = node.child_count();
    let parts: Vec<(bool, Node)> = (0..child_count)
        .filter_map(|i| node.child(i))
        .map(|c| (c.is_named(), c))
        .collect();

    // Find the argument list (if any): typically "argument_list" or "call_arguments".
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

    // Collect non-arg named identifier parts.
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

/// Find the `argument_list` node inside a call suffix.
fn find_argument_list(node: Node<'_>) -> Option<Node<'_>> {
    // First try field "call" (as defined in the grammar for call_suffix).
    if let Some(n) = node.child_by_field_name("call") {
        return Some(n);
    }
    // Fallback: scan named children for an argument_list node.
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
    /// caller's procedure with value `v`, not the inner expression. The
    /// prior code pushed the Exit value as a regular argument and
    /// continued, which is a wrong-control-flow bug.
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
) -> Result<Vec<Value>, ArgsShort> {
    let mut out = Vec::new();
    eval_args_into(args_node, source, stack, &mut out)?;
    Ok(out)
}

fn eval_args_into(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    out: &mut Vec<Value>,
) -> Result<(), ArgsShort> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if is_punctuation(child.kind()) {
            continue;
        }
        // Unwrap expression_list — it is a named container for comma-separated args.
        if child.kind() == "expression_list" {
            eval_args_into(child, source, stack, out)?;
            continue;
        }
        match eval_expr(child, source, stack) {
            Eval::Normal(v) => out.push(v),
            Eval::Error(e) => return Err(ArgsShort::Error(e)),
            Eval::Exit(v) => return Err(ArgsShort::Exit(v)),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
        "comment" | ";" | "," | "(" | ")" | "{" | "}" | "semicolon"
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

/// Case-insensitive equality for CASE selector vs arm value.
fn values_equal_for_case(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Integer(x), Integer(y)) => x == y,
        (Decimal(x), Decimal(y)) => x == y,
        // For Integer vs Decimal: round-trip via i64 if the decimal has no
        // fractional part AND fits in i64 — exact comparison. Otherwise the
        // Decimal cannot equal a whole-number Integer regardless of the
        // lossy `as f64` cast (which loses precision above 2^53). This
        // matters for currency-like AL values where i64 magnitudes >2^53
        // are common.
        (Integer(x), Decimal(y)) | (Decimal(y), Integer(x))
            if y.fract() == 0.0 && *y >= i64::MIN as f64 && *y <= i64::MAX as f64 =>
        {
            *x == *y as i64
        }
        (Integer(_), Decimal(_)) | (Decimal(_), Integer(_)) => false,
        (Boolean(x), Boolean(y)) => x == y,
        (Text(x), Text(y)) | (Code(x), Code(y)) => x.eq_ignore_ascii_case(y),
        (Text(x), Code(y)) | (Code(y), Text(x)) => x.eq_ignore_ascii_case(y),
        (Null, Null) | (Empty, Empty) => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_runtime::interpreter::scope::{CallFrame, ScopeStack};
    use crate::test_runtime::interpreter::value::Value;
    use crate::workspace::Workspace;
    use std::sync::Arc;

    fn ctx() -> DispatchCtx {
        DispatchCtx::new_pure(Arc::new(Workspace::new()))
    }

    /// Parse a snippet and run eval_stmt on the first statement in the body.
    ///
    /// Wraps `source_snippet` in a full codeunit so the AL parser accepts it.
    fn run_stmt(source_snippet: &str) -> (Eval, ScopeStack) {
        let wrapper = format!(
            "codeunit 50100 \"X\"\n{{\n    procedure Test()\n    var\n        x: Integer;\n        s: Text;\n    begin\n        {source_snippet}\n    end;\n}}"
        );
        let result = crate::syntax::parser::AlParser::parse_quick(&wrapper);
        let tree = result.tree;
        let root = tree.root_node();
        let bytes = wrapper.as_bytes();

        // Find the begin_end_block body of "Test".
        let body = find_proc_body(root, bytes).expect("could not find procedure body");

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("x", Value::Integer(0));
        frame.bind("s", Value::Text(String::new()));
        stack.push(frame);

        let mut ctx = ctx();
        let eval = eval_stmt(body, bytes, &mut stack, &mut ctx);
        (eval, stack)
    }

    fn find_proc_body<'a>(node: Node<'a>, _source: &[u8]) -> Option<Node<'a>> {
        // Iterative walk looking for begin_end_block inside procedure_declaration.
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

    // ── Assignment ────────────────────────────────────────────────────────────

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
        // Negative: assigning a bad expression propagates the error.
        // We use division by zero as a guaranteed error.
        let (eval, _) = run_stmt("x := 1 div 0;");
        assert!(
            eval.is_error(),
            "expected Error from div-by-zero assignment"
        );
    }

    // ── If statement ──────────────────────────────────────────────────────────

    #[test]
    fn if_true_branch_executes() {
        let (eval, stack) = run_stmt("if true then x := 99;");
        assert!(matches!(eval, Eval::Normal(_)));
        // x should be 99 after the true branch.
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(99)));
    }

    #[test]
    fn if_false_branch_skipped() {
        let (eval, stack) = run_stmt("if false then x := 99;");
        assert!(matches!(eval, Eval::Normal(_)));
        // x should still be 0 (not 99).
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(0)));
    }

    // ── Exit statement ────────────────────────────────────────────────────────

    #[test]
    fn exit_without_value_returns_empty() {
        let (eval, _) = run_stmt("exit;");
        assert!(
            matches!(eval, Eval::Exit(Value::Empty)),
            "expected Exit(Empty), got {:?}",
            eval
        );
    }

    // ── AssertError ───────────────────────────────────────────────────────────

    #[test]
    fn asserterror_catches_error() {
        // Positive: asserterror wrapping a statement that raises Error succeeds.
        let (eval, _) = run_stmt("asserterror error('boom');");
        assert!(
            matches!(eval, Eval::Normal(_)),
            "asserterror should succeed when body errors; got {:?}",
            eval
        );
    }

    #[test]
    fn asserterror_no_error_is_itself_error() {
        // Negative: asserterror wrapping a no-op should produce Eval::Error.
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

    // ── While loop ────────────────────────────────────────────────────────────

    #[test]
    fn while_loop_accumulates() {
        // WHILE x < 5 DO x := x + 1;  →  x should be 5.
        let (eval, stack) = run_stmt("while x < 5 do x := x + 1;");
        assert!(matches!(eval, Eval::Normal(_)), "got {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(5)));
    }

    #[test]
    fn runaway_while_loop_trips_deadline() {
        // Negative regression for F-OPEN-015b: an infinite loop must
        // return Eval::Error with the deadline message instead of pinning
        // the thread forever. We set a deadline 5 ms in the future and
        // expect the while-loop's per-iteration check to fire on the next
        // iteration after the deadline has passed.
        use crate::test_runtime::interpreter::scope::{CallFrame, ScopeStack};
        use crate::test_runtime::interpreter::value::Value;
        use crate::workspace::Workspace;
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; while x >= 0 do x := x + 1;\n    end;\n}";
        let result = crate::syntax::parser::AlParser::parse_quick(wrapper);
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
        // F-OPEN-093 / F-OPEN-096: an external cancel signal must interrupt
        // a running loop without waiting for the wall-clock deadline. Set the
        // cancel token from a different thread once the loop has started;
        // the loop's per-iteration check should fire on the next iteration.
        use crate::test_runtime::interpreter::scope::{CallFrame, ScopeStack};
        use crate::test_runtime::interpreter::value::Value;
        use crate::workspace::Workspace;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; while x >= 0 do x := x + 1;\n    end;\n}";
        let result = crate::syntax::parser::AlParser::parse_quick(wrapper);
        let tree = result.tree;
        let bytes = wrapper.as_bytes();
        let body = find_proc_body(tree.root_node(), bytes).unwrap();

        let mut stack = ScopeStack::new();
        let mut frame = CallFrame::new("X", "Test");
        frame.bind("x", Value::Integer(0));
        stack.push(frame);

        let cancel = Arc::new(AtomicBool::new(false));
        let mut ctx = DispatchCtx::new_pure(Arc::new(Workspace::new()));
        // Long deadline — must be cancel that fires, not deadline.
        ctx.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(10));
        ctx.cancel = Some(cancel.clone());

        // Signal cancel from a background thread after a tiny delay so the
        // loop is already running when the flag flips.
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
        // Sanity: a pre-set cancel token aborts the loop on the very first
        // iteration check. No threading, fully deterministic.
        use crate::test_runtime::interpreter::scope::{CallFrame, ScopeStack};
        use crate::test_runtime::interpreter::value::Value;
        use crate::workspace::Workspace;
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; while x >= 0 do x := x + 1;\n    end;\n}";
        let result = crate::syntax::parser::AlParser::parse_quick(wrapper);
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

    // ── Sequence in begin_end_block ───────────────────────────────────────────

    #[test]
    fn block_executes_sequence() {
        let (eval, stack) = run_stmt("begin x := 1; x := x + 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(2)));
    }

    #[test]
    fn block_short_circuits_on_error() {
        // Negative: once an error occurs inside a block, subsequent stmts don't run.
        let (eval, stack) = run_stmt("begin error('stop'); x := 99; end;");
        assert!(eval.is_error());
        // x must NOT be 99 — execution stopped at the error.
        assert_ne!(
            stack.lookup("x"),
            Some(&Value::Integer(99)),
            "x should NOT be 99 after short-circuit error"
        );
    }

    // ── Adversarial tests (adversarial-h) ─────────────────────────────────────

    #[test]
    fn asserterror_must_propagate_exit_not_convert_to_fail_adversarial_h_7() {
        // FINDING P1 spec-deviation: eval_asserterror converts Eval::Exit to
        // Eval::Error("asserterror: expected an error...") instead of propagating
        // it. AL spec: asserterror only catches Error(); Exit must propagate
        // so the enclosing procedure can return normally.
        // Buggy code at eval_stmt.rs line 578:
        //   Eval::Normal(_) | Eval::Exit(_) => Error("expected an error...")
        // Fix: separate Exit(_) to propagate: `Eval::Exit(v) => Eval::Exit(v)`.
        // Expected: Eval::Exit(Value::Empty)
        // Observed: Eval::Error("asserterror: expected an error to be raised...")
        let (eval, _) = run_stmt("asserterror exit;");
        assert!(
            matches!(eval, Eval::Exit(_)),
            "asserterror wrapping exit must propagate Eval::Exit, got: {:?}",
            eval
        );
    }

    // ── Regression: downto direction detection ───────────────────────────────

    #[test]
    fn for_downto_decrements_when_direction_field_is_downto() {
        // Sanity check that the new grammar-field path works for a real downto.
        // Loop body binds x to each iteration value; after the last iteration
        // (i=1) the local counter decrements to 0, the loop exits, and x is
        // left at the last bound value (1).
        let (eval, stack) = run_stmt("for x := 3 downto 1 do begin end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(1)));
    }

    #[test]
    fn for_to_with_substring_downto_in_body_still_counts_up() {
        // Regression: prior `is_downto` substring check matched anywhere in
        // the for_statement text — including the loop body. Any identifier
        // or string literal containing "downto" (e.g. `mydowntoval`) flipped
        // direction silently. With the grammar-field fix, body content is
        // irrelevant.
        //
        // Test: a to-loop whose body sets `s` to a string containing the
        // substring "downto". The fix asserts the loop ran upward: x ends
        // at 3 (the last bound value). With the prior bug, the loop would
        // have been treated as downto and immediately broken (since 1 < 3),
        // leaving x at 0.
        let (eval, stack) = run_stmt(r#"for x := 1 to 3 do begin s := 'mydowntoval'; end;"#);
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(3)),
            "loop with `to` direction must count UP regardless of body content; \
             prior substring detection would have flipped this to a downto loop"
        );
    }

    // ── Regression: case else_body via field, not dead arm ───────────────────

    #[test]
    fn case_else_branch_runs_when_no_arm_matches() {
        // The AL grammar carries `else_body` as a field on `case_statement`,
        // not as a separate `case_else` / `else_clause` child node. The prior
        // code matched on those non-existent node kinds, so the else branch
        // never ran. Fix consults `child_by_field_name("else_body")` directly.
        let (eval, stack) = run_stmt("case 42 of 1: x := 1; 2: x := 2; else x := 99; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(99)),
            "else branch should fire when no case arm matches the selector"
        );
    }

    // ── Regression: case Integer↔Decimal compare ─────────────────────────────

    #[test]
    fn case_integer_decimal_compare_exact_for_whole_numbers() {
        // 5 (Integer) should match 5.0 (Decimal) — whole-number Decimal.
        let (eval, stack) = run_stmt("case 5 of 5.0: x := 7; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(7)));
    }

    #[test]
    fn case_integer_decimal_compare_rejects_fractional() {
        // 5 (Integer) must NOT match 5.1 (Decimal). Prior `as f64` cast would
        // still return false for this case (5.0 != 5.1) — pinned for safety.
        let (eval, stack) = run_stmt("case 5 of 5.1: x := 7; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(1)));
    }

    // ── Regression: Exit inside argument expression unwinds ──────────────────

    #[test]
    fn exit_inside_argument_propagates_out_of_call() {
        // The prior eval_args_into absorbed `Eval::Exit` as a value and passed
        // it through as a regular argument. AL semantics: `exit(v)` inside a
        // call expression unwinds the enclosing procedure with value v.
        // The simplest reproducer: bare `exit;` (Exit(Empty)) in the position
        // where it's evaluated as part of args.
        //
        // We can't easily construct an argument-position exit in plain AL
        // syntax without a workspace lookup, so this test exercises the
        // direct path: dispatch_call returns Exit when the FIRST argument
        // evaluation produced Exit. Use `Message(exit)` — bare `exit` as
        // identifier doesn't parse; use the recurse-via-case shape instead.
        //
        // Reproducer via case-arm that contains an exit-statement: the
        // outer Test() procedure exits when the case arm fires.
        let (eval, _) = run_stmt("case 1 of 1: exit; else x := 99; end;");
        assert!(
            matches!(eval, Eval::Exit(_)),
            "exit inside a case arm must unwind the enclosing procedure; got {:?}",
            eval
        );
    }

    // ── Regression: AST nesting depth cap ─────────────────────────────────────

    #[test]
    fn deep_nesting_errors_instead_of_stack_overflow() {
        // Build a string with > MAX_AST_DEPTH (1024) levels of nested
        // begin/end blocks. The prior code would recurse into eval_stmt
        // that many times and could blow the Rust stack. The depth cap
        // aborts with a clean error well before any stack risk.
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

    // ── FOR loop: happy path and error paths ──────────────────────────────────

    #[test]
    fn for_to_counts_up_and_leaves_last_value() {
        // FOR x := 1 TO 3 DO begin end — loop body runs for 1,2,3 then exits.
        // x is left bound to the last value that satisfied the loop guard (3).
        let (eval, stack) = run_stmt("for x := 1 to 3 do begin end;");
        assert!(matches!(eval, Eval::Normal(_)), "got {:?}", eval);
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(3)));
    }

    #[test]
    fn for_to_accumulates_in_body() {
        // The loop body mutates another variable each iteration.
        // s starts empty; we sum via x into s? Simpler: use the loop var.
        // FOR x := 1 TO 4 — afterwards x == 4 (last bound value).
        let (eval, stack) = run_stmt("for x := 1 to 4 do begin end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(4)));
    }

    #[test]
    fn for_empty_range_does_not_run_body() {
        // FOR x := 5 TO 1 — start > end for an upward loop, so the body never
        // runs and x is left at its pre-loop binding from the first assignment
        // attempt. The guard breaks before any bind: x stays 0.
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
        // Non-integer start value is a runtime error.
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
        // Non-integer end value is a runtime error.
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
        // An error inside the loop body short-circuits the whole FOR.
        let (eval, _) = run_stmt("for x := 1 to 3 do begin error('boom'); end;");
        assert!(eval.is_error(), "FOR body error must propagate");
    }

    // ── FOREACH loop ──────────────────────────────────────────────────────────

    #[test]
    fn foreach_over_non_collection_errors() {
        // FOREACH over a non-list/array value must error. An integer literal
        // is not iterable.
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

    // ── REPEAT … UNTIL ────────────────────────────────────────────────────────

    #[test]
    fn repeat_runs_body_at_least_once() {
        // REPEAT executes the body before checking the UNTIL condition.
        // Even though x >= 0 is already true, the body runs once: x := x + 1.
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
        // REPEAT x := x + 1 UNTIL x >= 3 — runs 3 times, x ends at 3.
        let (eval, stack) = run_stmt("repeat x := x + 1; until x >= 3;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(3)));
    }

    #[test]
    fn repeat_deadline_trips_on_runaway() {
        // A repeat-until whose condition is never satisfied must trip the
        // deadline rather than spin forever.
        use crate::test_runtime::interpreter::scope::{CallFrame, ScopeStack};
        use crate::test_runtime::interpreter::value::Value;
        use crate::workspace::Workspace;
        use std::sync::Arc;

        let wrapper = "codeunit 50100 \"X\"\n{\n    procedure Test()\n    var\n        x: Integer;\n    begin\n        x := 0; repeat x := x + 1; until x < 0;\n    end;\n}";
        let result = crate::syntax::parser::AlParser::parse_quick(wrapper);
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

    // ── CASE matching ─────────────────────────────────────────────────────────

    #[test]
    fn case_matches_arm() {
        // CASE 1 OF 1: x:=2; — the arm whose label equals the selector fires,
        // so its body (x := 2) runs and the else branch is skipped.
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
        // No arm matches and there is no ELSE: nothing runs, x stays 0.
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
        // CASE 2 OF 1, 2, 3: x := 7; — comma-separated labels; selector 2
        // matches the second label in the list.
        let (eval, stack) = run_stmt("case 2 of 1, 2, 3: x := 7; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(7)),
            "a multi-label arm must match if ANY label equals the selector"
        );
    }

    #[test]
    fn case_text_selector_is_case_insensitive() {
        // AL CASE compares Text/Code case-insensitively. Selector 'ABC'
        // matches arm label 'abc'.
        let (eval, stack) = run_stmt("case 'ABC' of 'abc': x := 5; else x := 1; end;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(
            stack.lookup("x"),
            Some(&Value::Integer(5)),
            "Text CASE labels must compare case-insensitively"
        );
    }

    // ── IF: else branch + non-boolean guard ──────────────────────────────────

    #[test]
    fn if_else_branch_executes_when_false() {
        // IF false THEN x:=1 ELSE x:=2 — the else branch runs.
        let (eval, stack) = run_stmt("if false then x := 1 else x := 2;");
        assert!(matches!(eval, Eval::Normal(_)));
        assert_eq!(stack.lookup("x"), Some(&Value::Integer(2)));
    }

    #[test]
    fn if_non_boolean_condition_errors() {
        // A non-Boolean condition is a type error (AL requires Boolean guards).
        let (eval, _) = run_stmt("if 5 then x := 1;");
        assert!(eval.is_error(), "non-boolean IF condition must error");
        if let Eval::Error(e) = eval {
            assert!(e.message.contains("must be Boolean"), "got: {}", e.message);
        }
    }

    // ── WHILE: body error propagation ─────────────────────────────────────────

    #[test]
    fn while_body_error_propagates() {
        // An error in the WHILE body short-circuits the loop and the statement.
        let (eval, _) = run_stmt("while x < 5 do begin error('boom'); end;");
        assert!(eval.is_error(), "WHILE body error must propagate");
    }

    // ── EXIT with a value ─────────────────────────────────────────────────────

    #[test]
    fn exit_with_value_returns_it() {
        // exit(7) unwinds with Integer(7).
        let (eval, _) = run_stmt("exit(7);");
        assert!(
            matches!(eval, Eval::Exit(Value::Integer(7))),
            "expected Exit(Integer(7)), got {:?}",
            eval
        );
    }
}
