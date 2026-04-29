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

use crate::test_runtime::interpreter::dispatch::{dispatch_call, DispatchCtx};
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
    // Expected children (by field name or positional):
    // variable, start_value, end_value, body
    let var_node = node
        .child_by_field_name("variable")
        .or_else(|| named_stmt_child(node, 0));
    let start_node = node
        .child_by_field_name("start")
        .or_else(|| named_stmt_child(node, 1));
    let end_node = node
        .child_by_field_name("end")
        .or_else(|| named_stmt_child(node, 2));
    let body_node = node
        .child_by_field_name("body")
        .or_else(|| named_stmt_child(node, 3));

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

    // Determine direction: check for "DOWNTO" keyword.
    let is_downto = node_text(node, source)
        .to_ascii_lowercase()
        .contains("downto");

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
            i -= 1;
        } else {
            i += 1;
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

    // Walk named children looking for case_arm / case_else.
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();

    let mut else_body: Option<Node> = None;

    for child in &children[1..] {
        match child.kind() {
            "case_arm" | "case_branch" => {
                // Arm children: [values..., body]. The last named child is the body;
                // the preceding ones are the match values (possibly multiple).
                let arm_child_count = child.named_child_count();
                if arm_child_count < 2 {
                    continue;
                }
                let arm_body = match child.named_child(arm_child_count - 1) {
                    Some(n) => n,
                    None => continue,
                };

                // Try each value before the body.
                let mut matched = false;
                for vi in 0..(arm_child_count - 1) {
                    if let Some(val_node) = child.named_child(vi) {
                        // Case arms can list multiple comma-separated values or ranges.
                        if let Eval::Normal(v) = eval_expr(val_node, source, stack) {
                            if values_equal_for_case(&selector, &v) {
                                matched = true;
                                break;
                            }
                        }
                        // If we can't evaluate a value, skip this arm value.
                    }
                }

                if matched {
                    return eval_stmt(arm_body, source, stack, ctx);
                }
            }
            "case_else" | "else_clause" => {
                // else body is the last named child of the case_else node.
                if let Some(b) = child.named_child(0) {
                    else_body = Some(b);
                }
            }
            _ => {}
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
        Err(e) => return Eval::Error(e),
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

/// Evaluate an argument list node, returning a Vec of Values or an ErrorInfo.
///
/// Handles both flat shapes (direct `expression` children) and the grammar shape
/// where `argument_list` wraps a single `expression_list` containing the
/// comma-separated `expression` items.
fn eval_args(
    args_node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
) -> Result<Vec<Value>, ErrorInfo> {
    let mut out = Vec::new();
    eval_args_into(args_node, source, stack, &mut out)?;
    Ok(out)
}

fn eval_args_into(
    node: Node<'_>,
    source: &[u8],
    stack: &mut ScopeStack,
    out: &mut Vec<Value>,
) -> Result<(), ErrorInfo> {
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
            Eval::Error(e) => return Err(e),
            Eval::Exit(v) => out.push(v),
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
        (Integer(x), Decimal(y)) | (Decimal(y), Integer(x)) => (*x as f64) == *y,
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

    fn find_proc_body<'a>(node: Node<'a>, source: &[u8]) -> Option<Node<'a>> {
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
}
