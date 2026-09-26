//! Running a procedure declared in workspace AL source.
//!
//! The receiver or object name selects the file, the procedure declaration
//! node comes from the parse tree, and the body runs through `eval_stmt` on a
//! frame bound by [`super::frames`]. Recursion is capped so a cyclic call
//! graph reports an error instead of overflowing the stack.

use crate::interpreter::eval_error;
use crate::interpreter::scope::{CallFrame, Eval, ScopeStack};
use crate::interpreter::value::{ErrorInfo, Value};
use al_syntax::IdentifierText;

use super::frames::{
    bind_local_vars, bind_object_globals, bind_structured_locals, check_param_type,
    coerce_int_width, collect_params, collect_return, declared_text_length,
    default_for_declared_type, ParamDecl, ReturnDecl,
};
use super::{DispatchCtx, MAX_RECURSION_DEPTH};

/// Look up a procedure in the workspace and execute it.
///
/// The explicit receiver wins. An unqualified call resolves only against the
/// active frame's object; searching every workspace object would make duplicate
/// procedure names execute whichever file happened to be indexed first.
///
/// When the procedure node is found:
/// - Parse parameter declarations; type-check each arg.
/// - Push a new `CallFrame`, execute the body, pop and return.
/// - Increment/decrement `ctx.recursion_depth`; error if > MAX.
pub(super) fn dispatch_workspace_procedure(
    receiver: Option<&str>,
    procedure: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Recursion guard. Use `>=` (not `>`) so MAX_RECURSION_DEPTH is the
    // inclusive upper bound on simultaneous frames — without this, one
    // extra frame slipped through (101 instead of the documented 100).
    if ctx.recursion_depth >= MAX_RECURSION_DEPTH {
        return eval_error(format!(
            "call depth of {MAX_RECURSION_DEPTH} exceeded at '{procedure}'. This is a limit of \
             the local test runner, not of Business Central: re-run this test on live BC if the \
             recursion is genuine."
        ));
    }

    let target_object = receiver
        .map(str::to_string)
        .or_else(|| stack.top().map(|frame| frame.object.clone()));
    let candidate_paths: Vec<std::path::PathBuf> = if let Some(target_object) = target_object {
        match ctx.source.find_by_object_name(&target_object) {
            Some(path) => vec![path],
            None => {
                return eval_error(format!("object '{}' not found in workspace", target_object));
            }
        }
    } else {
        return eval_error(format!(
            "procedure not found: workspace call '{procedure}' has no current object context"
        ));
    };

    for path in &candidate_paths {
        let Some((text, tree)) = ctx.source.get_cached_parse(path) else {
            return eval_error(format!(
                "object source '{}' has no coherent cached parse",
                path.display()
            ));
        };

        let source = text.as_bytes();
        let root = tree.root_node();

        let Some(object_name) = ctx.source.object_name(path) else {
            return eval_error(format!(
                "object source '{}' has no indexed object identity",
                path.display()
            ));
        };
        let needs_object_globals = object_has_global_declarations(root);
        let install_root_globals = needs_object_globals && !stack.has_object_globals(&object_name);
        if install_root_globals && stack.depth() != 0 {
            return eval_error(format!(
                "stateful codeunit '{}' requires live BC execution",
                object_name
            ));
        }

        // Walk the tree to find a procedure_declaration with the matching name.
        // Iterative traversal (rule: no recursion).
        let mut stack_nodes = vec![root];
        let mut found_proc: Option<(tree_sitter::Node<'_>, Vec<ParamDecl>, Option<ReturnDecl>)> =
            None;

        'outer: while let Some(node) = stack_nodes.pop() {
            if node.kind() == "procedure_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name_node.utf8_text(source) {
                        let clean = name_text.unquote_identifier();
                        if clean.eq_ignore_ascii_case(procedure) {
                            let params = collect_params(node, source);
                            let ret = collect_return(node, source);
                            found_proc = Some((node, params, ret));
                            break 'outer;
                        }
                    }
                }
                // Don't descend into procedure bodies when just searching by name.
                continue;
            }
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                stack_nodes.push(child);
            }
        }

        let Some((proc_node, params, return_decl)) = found_proc else {
            continue;
        };
        return run_declaration(
            Declaration {
                node: proc_node,
                name: procedure,
                params,
                return_decl,
            },
            DeclarationSite {
                path,
                source,
                object_name: &object_name,
                globals_root: install_root_globals.then_some(root),
                implicit_record: None,
            },
            args,
            stack,
            ctx,
        );
    }

    eval_error(format!(
        "procedure not found: {}{}",
        receiver.map(|r| format!("{r}.")).unwrap_or_default(),
        procedure
    ))
}

/// A procedure or trigger declaration ready to run.
pub(super) struct Declaration<'t> {
    pub(super) node: tree_sitter::Node<'t>,
    pub(super) name: &'t str,
    pub(super) params: Vec<ParamDecl>,
    pub(super) return_decl: Option<ReturnDecl>,
}

/// Where a declaration runs: its file, its object, and the frame state the
/// object brings.
pub(super) struct DeclarationSite<'t> {
    pub(super) path: &'t std::path::Path,
    pub(super) source: &'t [u8],
    pub(super) object_name: &'t str,
    /// The object node whose globals a fresh globals frame binds, when one
    /// must be installed for this call.
    pub(super) globals_root: Option<tree_sitter::Node<'t>>,
    /// Table code runs with its record as the implicit `Rec` (and `xRec`):
    /// bare field names read and write that record.
    pub(super) implicit_record: Option<(Value, Value)>,
}

/// Bind `args` to the declaration's parameters, run its body and return what
/// it yields. `ctx.var_writebacks` carries the final values of `var`
/// parameters.
pub(super) fn run_declaration(
    declaration: Declaration<'_>,
    site: DeclarationSite<'_>,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    let Declaration {
        node: proc_node,
        name: procedure,
        params,
        return_decl,
    } = declaration;
    let DeclarationSite {
        path,
        source,
        object_name,
        globals_root,
        implicit_record,
    } = site;

    if args.len() != params.len() {
        return eval_error(format!(
            "procedure '{}' expects {} argument(s), got {}",
            procedure,
            params.len(),
            args.len()
        ));
    }

    for (i, param) in params.iter().enumerate() {
        let arg = &args[i];
        if let Some(err) = check_param_type(arg, &param.type_name) {
            return Eval::Error(ErrorInfo {
                message: format!("type mismatch for parameter '{}': {}", param.name, err),
                error_type: None,
                source: None,
            });
        }
    }

    // NB: cursor must outlive the iterator, so we use an explicit loop.
    let body_node = {
        let mut cursor = proc_node.walk();
        let mut found_body = None;
        for child in proc_node.named_children(&mut cursor) {
            if child.kind() == "begin_end_block" || child.kind() == "statement_list" {
                found_body = Some(child);
                break;
            }
        }
        found_body
    };

    let Some(body) = body_node else {
        return eval_error(format!("procedure '{}' has no body", procedure));
    };

    let mut frame = CallFrame::new(object_name, procedure);
    for (i, param) in params.iter().enumerate() {
        let mut val = args.get(i).cloned().unwrap_or(Value::Empty);
        // A by-value record parameter is the callee's own copy: BC gives
        // it the caller's buffer but its own filters/cursor. `RecordValue`
        // is `Clone` and carries the caller's view `handle`, so keeping it
        // would alias the caller's view and let the callee's SetRange/Next
        // corrupt the caller's filters. Fork a fresh view seeded with the
        // caller's buffer instead; `var` parameters keep the shared handle
        // (by-reference semantics).
        if !param.is_var {
            if let Value::Record(rv) = &mut val {
                crate::interpreter::records::fork_record_for_by_value(ctx, rv);
            }
        }
        // Coerce an integer argument to the parameter's declared width so a
        // `BigInteger` parameter keeps i64 semantics even when passed a small
        // Integer literal and vice versa, matching BC's fixed parameter types.
        let val = coerce_int_width(val, &param.type_name);
        frame.bind(&param.name, val);
        if let Some(length) = declared_text_length(&param.type_name) {
            frame.bind_declared_text_length(&param.name, length);
        }
    }
    // A named return value (`procedure F() Result: Integer`) is an ordinary
    // local initialised to the return type's default. It is what the call
    // yields when the body falls off the end or runs a bare `exit`.
    let return_default = return_decl
        .as_ref()
        .and_then(|r| default_for_declared_type(&r.type_name))
        .unwrap_or(Value::Empty);
    if let Some(r) = &return_decl {
        if let Some(name) = &r.name {
            frame.bind(name, return_default.clone());
            if let Some(length) = declared_text_length(&r.type_name) {
                frame.bind_declared_text_length(name, length);
            }
        }
    }
    // Bind the procedure's local `var` section to default values so a
    // variable can be read before its first assignment. Handles
    // multi-name declarations (`A, B, C : Integer;`) — every name on the
    // line gets its own default-initialised slot. Scalar and simple types;
    // complex types are skipped here.
    bind_local_vars(proc_node, source, &mut frame);
    // Then pre-bind structured local variables (`Record`/`Codeunit`/
    // `List of [T]`) to their handle defaults so member calls / field
    // access resolution, complementing bind_local_vars.
    bind_structured_locals(proc_node, source, &mut frame);
    if let Some((rec, x_rec)) = implicit_record {
        frame.bind("Rec", rec);
        frame.bind("xRec", x_rec);
        frame.implicit_record = true;
    }

    ctx.recursion_depth += 1;
    if let Some(root) = globals_root {
        let mut globals = CallFrame::new(object_name, "<globals>");
        bind_object_globals(root, source, &mut globals);
        stack.push(globals);
    }
    stack.push(frame);
    // Attribute this procedure's statements to the file
    // it is defined in (which may differ from the caller's file), then
    // restore the caller's file when the call returns.
    let cov_prev_file = ctx.cov_enter_file(&path.to_string_lossy());
    let mut result = crate::interpreter::eval_stmt::eval_stmt(body, source, stack, ctx);
    ctx.cov_restore_file(cov_prev_file);
    // Calling an event publisher raises its event: every subscriber runs
    // with the publisher's arguments, and `var` parameters take the values
    // they leave.
    if !matches!(result, Eval::Error(_)) && super::events::is_publisher(proc_node, source) {
        if let Err(error) = raise_published_event(
            proc_node,
            source,
            object_name,
            procedure,
            &params,
            stack,
            ctx,
        ) {
            result = error;
        }
    }
    ctx.recursion_depth -= 1;

    // Record final values of `var` (by-reference) parameters so the caller
    // can write them back into its own argument variables. Read from
    // the still-live callee frame before it is dropped. Nested calls during
    // the body already cleared/consumed the channel via their own
    // `dispatch_call`, so populating it here (after the body) is safe.
    ctx.var_writebacks.clear();
    for (i, param) in params.iter().enumerate() {
        if param.is_var {
            if let Some(val) = stack.top().and_then(|f| f.get(&param.name)).cloned() {
                ctx.var_writebacks.push((i, val));
            }
        }
    }
    // The value a fall-through or a bare `exit` yields: the named return
    // variable if the declaration has one, otherwise the return type's
    // default. Read before the frame is dropped.
    let fallthrough_value = match return_decl.as_ref().and_then(|r| r.name.as_deref()) {
        Some(name) => stack
            .top()
            .and_then(|f| f.get(name))
            .cloned()
            .unwrap_or(return_default),
        None => return_default,
    };
    stack.pop();
    if globals_root.is_some() {
        stack.pop();
    }

    // Unwrap Exit into Normal (exit only unwinds the current procedure).
    // A break/continue that reached here escaped all loops — a runtime
    // error in AL, not silent success.
    //
    // A body that ends without `exit` does not return its last statement's
    // value: BC gives the caller the return type's default, so a Boolean
    // function whose last statement is `Rec.Insert()` returns false.
    match result {
        Eval::Exit(Value::Empty) => Eval::Normal(fallthrough_value),
        Eval::Exit(v) => Eval::Normal(v),
        Eval::Normal(_) => Eval::Normal(fallthrough_value),
        Eval::Break => eval_error("break statement not inside a loop"),
        Eval::Continue => eval_error("continue statement not inside a loop"),
        other => other,
    }
}

/// Raise the event `procedure` publishes, with the parameter values in the
/// running frame, and store what subscribers leave in its `var` parameters.
fn raise_published_event(
    proc_node: tree_sitter::Node<'_>,
    source: &[u8],
    object_name: &str,
    procedure: &str,
    params: &[ParamDecl],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<(), Eval> {
    let mut object = proc_node;
    while object.kind() != "object_declaration" {
        match object.parent() {
            Some(parent) => object = parent,
            None => break,
        }
    }
    let kind = object
        .child_by_field_name("kind")
        .and_then(|kind| kind.utf8_text(source).ok())
        .unwrap_or("codeunit")
        .to_string();
    let names: Vec<&str> = params.iter().map(|param| param.name.as_str()).collect();
    let mut values: Vec<Value> = params
        .iter()
        .map(|param| {
            stack
                .top()
                .and_then(|frame| frame.get(&param.name))
                .cloned()
                .unwrap_or(Value::Empty)
        })
        .collect();
    super::events::raise(
        &kind,
        object_name,
        procedure,
        "",
        &names,
        &mut values,
        stack,
        ctx,
    )?;
    if let Some(frame) = stack.top_mut() {
        for (param, value) in params.iter().zip(values) {
            if param.is_var {
                frame.bind(&param.name, value);
            }
        }
    }
    Ok(())
}

pub(super) fn object_has_global_declarations(root: tree_sitter::Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_var_section" {
            let mut cursor = node.walk();
            return node
                .named_children(&mut cursor)
                .any(|child| child.kind() == "object_variable_declaration");
        }
        if matches!(
            node.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_declaration"
        ) {
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::interpreter::dispatch::INTERP_STACK_BYTES;

    use crate::interpreter::dispatch::routing::dispatch_call;
    use crate::test_support::MockSource as Workspace;

    use crate::interpreter::dispatch::test_support::{error_of, ok, workspace_with_helper};

    #[test]
    fn workspace_dispatch_add_helper_positive() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);
        let result = dispatch_call(
            Some("Helper"),
            "Add",
            vec![Value::Integer(2), Value::Integer(3)],
            &mut ctx,
        );
        assert_eq!(
            ok(result),
            Value::Integer(5),
            "Helper.Add(2, 3) should return 5"
        );
    }

    #[test]
    fn falling_off_the_end_returns_the_declared_types_default() {
        let ws = Arc::new(Workspace::new());
        ws.file_index.add_file(
            std::path::PathBuf::from("/test/FallThrough.al"),
            r#"codeunit 50997 "Fall Through"
{
    procedure LastStatementIsTrue(): Boolean
    var
        b: Boolean;
    begin
        b := true;
        b := b;
    end;

    procedure LastStatementIsAnAssignment(): Integer
    var
        n: Integer;
    begin
        n := 7;
    end;

    procedure NamedResult() Result: Integer
    begin
        Result := 42;
    end;

    procedure NamedResultNeverAssigned() Result: Text
    var
        n: Integer;
    begin
        n := 1;
    end;

    procedure BareExitKeepsTheNamedResult() Result: Integer
    begin
        Result := 9;
        exit;
    end;

    procedure BareExitWithNoReturnType()
    begin
        exit;
    end;

    procedure ExitWinsOverTheNamedResult() Result: Integer
    begin
        Result := 9;
        exit(3);
    end;
}"#
            .to_string(),
        );
        let mut ctx = DispatchCtx::new_pure(ws);
        let call = |ctx: &mut DispatchCtx, name: &str| {
            ok(dispatch_call(Some("Fall Through"), name, vec![], ctx))
        };

        assert_eq!(
            call(&mut ctx, "LastStatementIsTrue"),
            Value::Boolean(false),
            "a Boolean function that falls off the end returns false, not its last statement"
        );
        assert_eq!(
            call(&mut ctx, "LastStatementIsAnAssignment"),
            Value::Integer(0)
        );
        assert_eq!(call(&mut ctx, "NamedResult"), Value::Integer(42));
        assert_eq!(
            call(&mut ctx, "NamedResultNeverAssigned"),
            Value::Text(String::new())
        );
        assert_eq!(
            call(&mut ctx, "BareExitKeepsTheNamedResult"),
            Value::Integer(9)
        );
        assert_eq!(call(&mut ctx, "BareExitWithNoReturnType"), Value::Empty);
        assert_eq!(
            call(&mut ctx, "ExitWinsOverTheNamedResult"),
            Value::Integer(3)
        );
    }

    #[test]
    fn workspace_dispatch_unknown_procedure_not_found_negative() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);
        let result = dispatch_call(Some("Helper"), "NoSuchProc", vec![], &mut ctx);
        let e = error_of(result);
        assert!(
            e.message.contains("not found"),
            "expected 'not found' in error message, got: {}",
            e.message
        );
    }

    #[test]
    fn workspace_dispatch_type_mismatch_negative() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);
        let result = dispatch_call(
            Some("Helper"),
            "Add",
            vec![Value::Text("hello".into()), Value::Integer(3)],
            &mut ctx,
        );
        let e = error_of(result);
        assert!(
            e.message.to_lowercase().contains("type"),
            "expected 'type' in error message, got: {}",
            e.message
        );
    }

    #[test]
    fn workspace_dispatch_rejects_wrong_argument_count() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);

        let missing = dispatch_call(Some("Helper"), "Add", vec![Value::Integer(2)], &mut ctx);
        assert!(error_of(missing)
            .message
            .contains("expects 2 argument(s), got 1"));

        let extra = dispatch_call(
            Some("Helper"),
            "Add",
            vec![Value::Integer(2), Value::Integer(3), Value::Integer(4)],
            &mut ctx,
        );
        assert!(error_of(extra)
            .message
            .contains("expects 2 argument(s), got 3"));
    }

    #[test]
    fn workspace_dispatch_deep_recursion_negative() {
        // Run on a thread with an explicit large stack so this verifies the
        // interpreter's own depth guard, not the runner's thread-stack size
        // (CI test threads default to 2 MiB and debug frames vary by
        // toolchain).
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn(|| {
                let ws = workspace_with_helper();
                let mut ctx = DispatchCtx::new_pure(ws);
                let result = dispatch_call(Some("Helper"), "Forever", vec![], &mut ctx);
                let e = error_of(result);
                assert!(
                    e.message.contains("local test runner"),
                    "expected the message to name the runner limit, got: {}",
                    e.message
                );
            })
            .expect("spawn recursion test thread")
            .join()
            .expect("recursion test thread panicked");
    }

    #[test]
    fn recursion_over_a_data_hierarchy_completes() {
        // A BOM explosion or a chart-of-accounts total recurses once per row.
        // 200 frames is ordinary for that shape and used to fail locally with
        // "recursion depth exceeded" while passing on BC.
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn(|| {
                let ws = Arc::new(Workspace::new());
                ws.file_index.add_file(
                    std::path::PathBuf::from("/test/Depth.al"),
                    r#"codeunit 50996 "Depth"
{
    procedure Walk(n: Integer): Integer
    begin
        if n <= 0 then
            exit(0);
        exit(1 + Walk(n - 1));
    end;
}"#
                    .to_string(),
                );
                let mut ctx = DispatchCtx::new_pure(ws);
                let result =
                    dispatch_call(Some("Depth"), "Walk", vec![Value::Integer(200)], &mut ctx);
                assert_eq!(ok(result), Value::Integer(200));
            })
            .expect("spawn deep recursion test thread")
            .join()
            .expect("deep recursion test thread panicked");
    }
}
