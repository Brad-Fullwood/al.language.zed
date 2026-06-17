//! Procedure-call dispatch for the AL interpreter — Phase 2b.
//!
//! `dispatch_call` is the single entry point for any procedure call that the
//! statement evaluator encounters. Priority order:
//!
//! 1. **Stub catalogs** — Library Assert and any other native-Rust ports of
//!    BC test libraries (via `crate::test_runtime::stubs`).
//! 2. **Inline builtins** — core AL global procedures implemented here in Rust
//!    (Error, Message, StrSubstNo, Format, StrLen, CopyStr, LowerCase,
//!    UpperCase, IndexOf).
//! 3. **Workspace procedures** — looks up the procedure in the workspace file
//!    index by receiver/object name, finds the `procedure_declaration` node,
//!    and executes its body via `eval_stmt`. Recursion depth is capped at 100.

use std::collections::HashMap;
use std::sync::Arc;

use crate::test_runtime::interpreter::scope::{CallFrame, Eval, ScopeStack};
use crate::test_runtime::interpreter::value::{ErrorInfo, Value};
use crate::test_runtime::mock::record::MockRecord;
use crate::test_runtime::stubs;
use crate::workspace::Workspace;

const MAX_RECURSION_DEPTH: usize = 100;

/// Maximum syntactic nesting depth `eval_stmt` will descend into before
/// aborting with an error. The counter is cumulative across nested
/// procedure calls (a 100-deep call chain stacks ~4 AST levels per frame),
/// so we set the cap above what `MAX_RECURSION_DEPTH` (100) can reach via
/// call recursion alone — that way an infinite-call test trips the call
/// cap first (clearer error message) and only truly pathological single-
/// procedure nesting trips this AST cap. 1024 leaves plenty of headroom
/// for the OS stack (each `eval_stmt` frame is ~256 B of locals plus the
/// Node payload, so 1024 frames is ~1-2 MiB — well under the default
/// 8 MiB stack).
pub const MAX_AST_DEPTH: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchMode {
    PureLogic,
    /// Record store wired in (Phase 3 territory, partial support).
    WithRecords,
}

/// Shared context threaded through `eval_stmt` and `dispatch_call`.
///
/// Owns the workspace handle plus any stateful runtime support (record
/// catalog for `InterpRecord` runs). Cheap to reborrow; pass `&mut DispatchCtx`
/// everywhere.
pub struct DispatchCtx {
    pub workspace: Arc<Workspace>,
    /// In-memory record store keyed by table ID. Only populated when
    /// `mode == WithRecords`. Phase 2 does not populate this.
    pub records: HashMap<i32, MockRecord>,
    pub mode: DispatchMode,
    /// Current call depth — incremented on each workspace-procedure call and
    /// decremented on return. Capped at `MAX_RECURSION_DEPTH`.
    pub recursion_depth: usize,
    /// Current AST-evaluation depth — incremented when `eval_stmt` recurses
    /// into a nested block/branch/loop body, decremented on return. Capped at
    /// `MAX_AST_DEPTH` so a pathological test source with thousands of
    /// nested `begin/end` or `if … then if …` cannot blow the Rust stack and
    /// kill the daemon. Distinct from `recursion_depth`, which counts only
    /// call frames.
    pub ast_depth: usize,
    /// Optional wall-clock deadline for this dispatch. `eval_stmt` loop
    /// constructs (while / repeat / for) check this on every iteration so
    /// an adversarial `while true do …` test can't pin the daemon thread
    /// past the configured per-test budget. `None` means "no deadline" —
    /// used by unit-test paths that need full determinism. F-OPEN-015b.
    pub deadline: Option<std::time::Instant>,
    /// Optional external cancellation signal. Loop constructs in
    /// `eval_stmt` check this on every iteration alongside `deadline_exceeded`.
    /// Closes F-OPEN-093/F-OPEN-096: a daemon `$/cancelRequest` can now
    /// interrupt the interpreter mid-loop without waiting for the wall-clock
    /// deadline. The token is `Arc<AtomicBool>` so it can be cheaply shared
    /// across the call and signalled from a different task. `None` means
    /// "not cancellable" — unit-test path and CLI default.
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl DispatchCtx {
    pub fn new_pure(workspace: Arc<Workspace>) -> Self {
        Self {
            workspace,
            records: HashMap::new(),
            mode: DispatchMode::PureLogic,
            recursion_depth: 0,
            ast_depth: 0,
            deadline: None,
            cancel: None,
        }
    }

    pub fn new_with_records(workspace: Arc<Workspace>, records: HashMap<i32, MockRecord>) -> Self {
        Self {
            workspace,
            records,
            mode: DispatchMode::WithRecords,
            recursion_depth: 0,
            ast_depth: 0,
            deadline: None,
            cancel: None,
        }
    }

    /// True when a deadline has been set and has now passed. Cheap to call
    /// in a hot loop (Instant comparison is monotonic-clock arithmetic).
    pub fn deadline_exceeded(&self) -> bool {
        match self.deadline {
            Some(d) => std::time::Instant::now() >= d,
            None => false,
        }
    }

    /// True when an external cancel signal has been raised. Cheap atomic
    /// load — safe to call on every loop iteration. Returns false when no
    /// token is attached.
    pub fn is_cancelled(&self) -> bool {
        match &self.cancel {
            Some(flag) => flag.load(std::sync::atomic::Ordering::Relaxed),
            None => false,
        }
    }
}

/// Resolve and execute a procedure call.
///
/// `receiver` is the optional object qualifier (e.g. `"Assert"` for
/// `Assert.AreEqual(...)`). `procedure` is the bare procedure name.
/// `args` are already-evaluated positional arguments.
///
/// Returns `Eval::Normal` on success, `Eval::Error` on failure. Never panics.
pub fn dispatch_call(
    receiver: Option<&str>,
    procedure: &str,
    args: Vec<Value>,
    ctx: &mut DispatchCtx,
) -> Eval {
    if let Some(recv) = receiver {
        if let Some(stub_fn) = stubs::resolve(recv, procedure) {
            return stub_fn(&args);
        }
    }
    if receiver.is_none() {
        for cat in stubs::CATALOGS {
            if let Some(f) = (cat.resolve)(procedure) {
                return f(&args);
            }
        }
    }

    match procedure.to_ascii_lowercase().as_str() {
        "error" => return builtin_error(&args),
        "message" => return builtin_message(&args),
        "strsubstno" => return builtin_strsubstno(&args),
        "format" => return builtin_format(&args),
        "strlen" => return builtin_strlen(&args),
        "copystr" => return builtin_copystr(&args),
        "lowercase" => return builtin_lowercase(&args),
        "uppercase" => return builtin_uppercase(&args),
        "indexof" => return builtin_indexof(&args),
        _ => {}
    }

    dispatch_workspace_procedure(receiver, procedure, args, ctx)
}

/// Look up a procedure in the workspace and execute it.
///
/// Search order:
/// 1. If `receiver` is `Some(name)`, search the `file_index` for a codeunit
///    object whose name matches `name` (case-insensitive).
/// 2. If `receiver` is `None`, search every file in the index (same as all
///    visible procedures in the current object — Phase 2b allows any file).
///
/// When the procedure node is found:
/// - Parse parameter declarations; type-check each arg.
/// - Push a new `CallFrame`, execute the body, pop and return.
/// - Increment/decrement `ctx.recursion_depth`; error if > MAX.
fn dispatch_workspace_procedure(
    receiver: Option<&str>,
    procedure: &str,
    args: Vec<Value>,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Recursion guard. Use `>=` (not `>`) so MAX_RECURSION_DEPTH is the
    // inclusive upper bound on simultaneous frames — without this, one
    // extra frame slipped through (101 instead of the documented 100).
    if ctx.recursion_depth >= MAX_RECURSION_DEPTH {
        return simple_error("recursion depth exceeded");
    }

    let candidate_paths: Vec<std::path::PathBuf> = if let Some(recv) = receiver {
        match ctx.workspace.file_index.find_by_object_name(recv) {
            Some(path) => vec![path],
            None => {
                return simple_error(format!("object '{}' not found in workspace", recv));
            }
        }
    } else {
        ctx.workspace
            .file_index
            .files
            .iter()
            .map(|e| e.key().clone())
            .collect()
    };

    for path in &candidate_paths {
        let Some((text, tree)) = ctx.workspace.file_index.get_cached_parse(path) else {
            continue;
        };

        let source = text.as_bytes();
        let root = tree.root_node();

        let object_name = ctx
            .workspace
            .file_index
            .object_info
            .get(path)
            .map(|info| info.name.clone())
            .unwrap_or_default();

        // Walk the tree to find a procedure_declaration with the matching name.
        // Iterative traversal (rule: no recursion).
        let mut stack_nodes = vec![root];
        let mut found_proc: Option<(tree_sitter::Node<'_>, Vec<ParamDecl>)> = None;

        'outer: while let Some(node) = stack_nodes.pop() {
            if node.kind() == "procedure_declaration" {
                if let Some(name_node) = node.child_by_field_name("name") {
                    if let Ok(name_text) = name_node.utf8_text(source) {
                        let clean = name_text.trim_matches('"');
                        if clean.eq_ignore_ascii_case(procedure) {
                            let params = collect_params(node, source);
                            found_proc = Some((node, params));
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

        let Some((proc_node, params)) = found_proc else {
            continue;
        };

        for (i, param) in params.iter().enumerate() {
            let arg = match args.get(i) {
                Some(v) => v,
                None => {
                    // Missing argument — use default value for the type.
                    // (AL allows calling with fewer args if trailing params have defaults;
                    // Phase 2b: treat as type-check pass since we can't check unknown.)
                    continue;
                }
            };
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
            return simple_error(format!("procedure '{}' has no body", procedure));
        };

        let mut frame = CallFrame::new(object_name.as_str(), procedure);
        for (i, param) in params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::Empty);
            frame.bind(&param.name, val);
        }

        ctx.recursion_depth += 1;
        let mut scope = ScopeStack::new();
        scope.push(frame);
        let result =
            crate::test_runtime::interpreter::eval_stmt::eval_stmt(body, source, &mut scope, ctx);
        ctx.recursion_depth -= 1;

        // Unwrap Exit into Normal (exit only unwinds the current procedure).
        return match result {
            Eval::Exit(v) => Eval::Normal(v),
            other => other,
        };
    }

    simple_error(format!(
        "procedure not found: {}{}",
        receiver.map(|r| format!("{r}.")).unwrap_or_default(),
        procedure
    ))
}

/// A parsed parameter declaration from a `procedure_declaration` node.
#[derive(Debug, Clone)]
struct ParamDecl {
    name: String,
    type_name: String,
}

/// Extract parameter declarations from a `procedure_declaration` node.
///
/// Handles the AL grammar shape:
///   `parameter_list`  →  `(` `parameter`* `)`
///   `parameter`       →  [`kw_var`] `name_or_keyword` `:` `type_reference`
fn collect_params(proc_node: tree_sitter::Node<'_>, source: &[u8]) -> Vec<ParamDecl> {
    /// Find a child node by grammar field name, falling back to the first named
    /// child whose kind is in `kinds` (the grammar doesn't always attach the
    /// field, so accept the kind as well).
    fn child_by_field_or_kind<'a>(
        node: tree_sitter::Node<'a>,
        field: &str,
        kinds: &[&str],
    ) -> Option<tree_sitter::Node<'a>> {
        node.child_by_field_name(field).or_else(|| {
            let mut cursor = node.walk();
            let mut children = node.named_children(&mut cursor);
            children.find(|n| kinds.contains(&n.kind()))
        })
    }

    let mut params = Vec::new();

    let Some(param_list) = child_by_field_or_kind(proc_node, "parameters", &["parameter_list"])
    else {
        return params;
    };

    let mut cursor2 = param_list.walk();
    for child in param_list.named_children(&mut cursor2) {
        // Accept both "parameter" and "parameter_declaration" node kinds.
        if child.kind() != "parameter" && child.kind() != "parameter_declaration" {
            continue;
        }
        let name_node =
            child_by_field_or_kind(child, "name", &["identifier", "name", "name_or_keyword"]);
        let Some(name_node) = name_node else {
            continue;
        };
        let Ok(name_text) = name_node.utf8_text(source) else {
            continue;
        };
        let name = name_text.trim_matches('"').to_string();

        let type_node = child_by_field_or_kind(
            child,
            "type",
            &["type_reference", "type", "builtin_type", "primitive_type"],
        );
        let type_name = type_node
            .and_then(|n| n.utf8_text(source).ok())
            .map(|t| t.trim().to_string())
            .unwrap_or_default();

        params.push(ParamDecl { name, type_name });
    }

    params
}

/// Check whether a `Value` matches the declared AL type name.
///
/// Returns `Some(error_message)` on mismatch, `None` on pass.
/// Unknown type names are accepted (Phase 2b: allow through).
fn check_param_type(arg: &Value, type_name: &str) -> Option<String> {
    if type_name.is_empty() {
        return None;
    }
    let lower = type_name.to_lowercase();
    match lower.as_str() {
        "integer" | "biginteger" if !matches!(arg, Value::Integer(_)) => {
            return Some(format!("expected Integer, got {}", arg.type_name()));
        }
        "decimal" if !matches!(arg, Value::Decimal(_) | Value::Integer(_)) => {
            return Some(format!("expected Decimal, got {}", arg.type_name()));
        }
        "boolean" if !matches!(arg, Value::Boolean(_)) => {
            return Some(format!("expected Boolean, got {}", arg.type_name()));
        }
        t if t.starts_with("text") && !matches!(arg, Value::Text(_) | Value::Code(_)) => {
            return Some(format!("expected Text, got {}", arg.type_name()));
        }
        t if t.starts_with("code") && !matches!(arg, Value::Text(_) | Value::Code(_)) => {
            return Some(format!("expected Code, got {}", arg.type_name()));
        }
        // All other type names: pass through (Phase 2b can't check complex types).
        _ => {}
    }
    None
}

fn simple_error(msg: impl Into<String>) -> Eval {
    Eval::Error(ErrorInfo {
        message: msg.into(),
        error_type: None,
        source: None,
    })
}

/// `Error(msg[, arg1, …])` — raise a runtime error.
///
/// The first argument is the format string; subsequent arguments are
/// substituted as %1, %2, … (same semantics as StrSubstNo).
fn builtin_error(args: &[Value]) -> Eval {
    let msg = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return simple_error("Error() called with no arguments"),
    };
    let formatted = if args.len() > 1 {
        substitute_placeholders(&msg, &args[1..])
    } else {
        msg
    };
    Eval::Error(ErrorInfo {
        message: formatted,
        error_type: None,
        source: None,
    })
}

/// `Message(msg[, arg1, …])` — display a message (no-op in the interpreter;
/// returns Normal so execution continues).
fn builtin_message(args: &[Value]) -> Eval {
    let msg = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return Eval::Normal(Value::Empty),
    };
    let _formatted = if args.len() > 1 {
        substitute_placeholders(&msg, &args[1..])
    } else {
        msg
    };
    Eval::Normal(Value::Empty)
}

/// `StrSubstNo(fmt, arg1, …)` — substitute %1, %2, … in `fmt`.
fn builtin_strsubstno(args: &[Value]) -> Eval {
    let fmt = match args.first() {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => render_value(v),
        None => return simple_error("StrSubstNo requires at least 1 argument"),
    };
    let result = substitute_placeholders(&fmt, &args[1..]);
    Eval::Normal(Value::Text(result))
}

/// `Format(value[, length[, format_str]])` — convert a value to Text.
///
/// Phase 2 implements only the single-argument form.
fn builtin_format(args: &[Value]) -> Eval {
    match args.first() {
        Some(v) => Eval::Normal(Value::Text(render_value(v))),
        None => simple_error("Format() requires at least 1 argument"),
    }
}

fn builtin_strlen(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] | [Value::Code(s)] => {
            Eval::Normal(Value::Integer(s.chars().count() as i64))
        }
        [v] => simple_error(format!(
            "StrLen expects Text or Code, got {}",
            v.type_name()
        )),
        _ => simple_error("StrLen expects exactly 1 argument"),
    }
}

/// `CopyStr(s, pos[, len])` — extract a substring. 1-based position.
fn builtin_copystr(args: &[Value]) -> Eval {
    // Keep `pos` signed so a negative value is rejected explicitly rather than
    // wrapping to a huge usize via an `as usize` cast.
    let (s, pos) = match args {
        [Value::Text(s), Value::Integer(pos)]
        | [Value::Code(s), Value::Integer(pos)]
        | [Value::Text(s), Value::Integer(pos), _]
        | [Value::Code(s), Value::Integer(pos), _] => (s.clone(), *pos),
        _ => return simple_error("CopyStr expects (Text, Integer[, Integer])"),
    };
    let len = match args.get(2) {
        Some(Value::Integer(n)) => {
            if *n < 0 {
                return simple_error(format!("CopyStr: len must be >= 0, got {n}"));
            }
            *n as usize
        }
        None => s.chars().count(),
        Some(v) => {
            return simple_error(format!(
                "CopyStr: len must be Integer, got {}",
                v.type_name()
            ))
        }
    };
    if pos <= 0 {
        return simple_error("CopyStr: position must be >= 1");
    }
    let pos = pos as usize;
    let chars: Vec<char> = s.chars().collect();
    // AL runtime raises an error when position exceeds the string length.
    if pos > chars.len() {
        return simple_error(format!(
            "CopyStr: position {} is beyond the string length {}",
            pos,
            chars.len()
        ));
    }
    let start = pos - 1;
    let end = (start + len).min(chars.len());
    Eval::Normal(Value::Text(chars[start..end].iter().collect()))
}

fn builtin_lowercase(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] => Eval::Normal(Value::Text(s.to_lowercase())),
        [Value::Code(s)] => Eval::Normal(Value::Text(s.to_lowercase())),
        [v] => simple_error(format!(
            "LowerCase expects Text or Code, got {}",
            v.type_name()
        )),
        _ => simple_error("LowerCase expects exactly 1 argument"),
    }
}

fn builtin_uppercase(args: &[Value]) -> Eval {
    match args {
        [Value::Text(s)] => Eval::Normal(Value::Text(s.to_uppercase())),
        [Value::Code(s)] => Eval::Normal(Value::Code(s.to_uppercase())),
        [v] => simple_error(format!(
            "UpperCase expects Text or Code, got {}",
            v.type_name()
        )),
        _ => simple_error("UpperCase expects exactly 1 argument"),
    }
}

/// `IndexOf(s, needle)` — return the 1-based index of `needle` in `s`,
/// or 0 if not found. Empty needle always returns 0 (AL convention).
fn builtin_indexof(args: &[Value]) -> Eval {
    let (s, needle) = match args {
        [Value::Text(s), Value::Text(n)]
        | [Value::Code(s), Value::Text(n)]
        | [Value::Text(s), Value::Code(n)]
        | [Value::Code(s), Value::Code(n)] => (s.as_str(), n.as_str()),
        _ => return simple_error("IndexOf expects (Text, Text)"),
    };
    if needle.is_empty() {
        return Eval::Normal(Value::Integer(0));
    }
    let result = s
        .find(needle)
        .map(|i| {
            s[..i].chars().count() as i64 + 1
        })
        .unwrap_or(0);
    Eval::Normal(Value::Integer(result))
}

/// Render a `Value` as AL would show it in StrSubstNo / Format.
fn render_value(v: &Value) -> String {
    match v {
        Value::Integer(n) => n.to_string(),
        Value::Decimal(n) => n.to_string(),
        Value::Boolean(true) => "Yes".to_string(),
        Value::Boolean(false) => "No".to_string(),
        Value::Text(s) | Value::Code(s) => s.clone(),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::DateTime(dt) => dt.to_string(),
        Value::Duration(d) => d.to_string(),
        Value::Guid(g) => g.clone(),
        Value::Char(c) => c.to_string(),
        Value::Null => String::new(),
        Value::Empty => String::new(),
        other => format!("<{}>", other.type_name()),
    }
}

/// Substitute %1, %2, … placeholders in `fmt` with rendered arg values.
///
/// Replaces in decreasing placeholder-number order so that `%10` is handled
/// before `%1`, preventing `%1` from consuming the `%1` prefix of `%10`.
fn substitute_placeholders(fmt: &str, args: &[Value]) -> String {
    let mut result = fmt.to_string();
    for i in (0..args.len()).rev() {
        let placeholder = format!("%{}", i + 1);
        result = result.replace(&placeholder, &render_value(&args[i]));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use std::sync::Arc;

    fn ctx() -> DispatchCtx {
        DispatchCtx::new_pure(Arc::new(Workspace::new()))
    }

    fn ok(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
            Eval::Exit(v) => v,
        }
    }

    fn err(eval: Eval) -> ErrorInfo {
        match eval {
            Eval::Error(e) => e,
            Eval::Normal(v) => panic!("expected error, got Normal({})", v.type_name()),
            Eval::Exit(v) => panic!("expected error, got Exit({})", v.type_name()),
        }
    }

    #[test]
    fn stub_routed_library_assert_are_equal() {
        let mut ctx = ctx();
        let result = dispatch_call(
            Some("Library Assert"),
            "AreEqual",
            vec![Value::Integer(1), Value::Integer(1)],
            &mut ctx,
        );
        assert!(
            matches!(result, Eval::Normal(_)),
            "Expected Normal, got {:?}",
            result
        );
    }

    #[test]
    fn stub_routed_library_assert_are_equal_fail() {
        let mut ctx = ctx();
        let result = dispatch_call(
            Some("Library Assert"),
            "AreEqual",
            vec![Value::Integer(1), Value::Integer(2)],
            &mut ctx,
        );
        let e = err(result);
        assert!(
            e.message.contains("expected"),
            "expected message containing 'expected', got: {}",
            e.message
        );
    }

    #[test]
    fn error_builtin_produces_eval_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "Error", vec![Value::Text("boom".into())], &mut ctx);
        let e = err(result);
        assert_eq!(e.message, "boom");
    }

    #[test]
    fn error_builtin_with_no_args_is_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "Error", vec![], &mut ctx);
        assert!(result.is_error());
    }

    #[test]
    fn strsubstno_formats_correctly() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("Hello %1, you are %2 years old".into()),
                Value::Text("Alice".into()),
                Value::Integer(30),
            ],
            &mut ctx,
        );
        match ok(result) {
            Value::Text(s) => assert_eq!(s, "Hello Alice, you are 30 years old"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn strsubstno_no_args_is_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrSubstNo", vec![], &mut ctx);
        assert!(result.is_error());
    }

    #[test]
    fn format_integer_to_text() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "Format", vec![Value::Integer(42)], &mut ctx);
        match ok(result) {
            Value::Text(s) => assert_eq!(s, "42"),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    #[test]
    fn unknown_procedure_returns_descriptive_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "CompletelyUnknownProc", vec![], &mut ctx);
        let e = err(result);
        assert!(
            e.message.contains("CompletelyUnknownProc"),
            "expected procedure name in error, got: {}",
            e.message
        );
        assert!(
            e.message.contains("not found"),
            "expected 'not found' in error, got: {}",
            e.message
        );
    }

    #[test]
    fn strlen_returns_char_count() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrLen", vec![Value::Text("hello".into())], &mut ctx);
        assert_eq!(ok(result), Value::Integer(5));
    }

    #[test]
    fn strlen_non_text_is_error() {
        let mut ctx = ctx();
        let result = dispatch_call(None, "StrLen", vec![Value::Integer(123)], &mut ctx);
        assert!(result.is_error());
    }

    #[test]
    fn copystr_extracts_substring() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("Hello World".into()),
                Value::Integer(1),
                Value::Integer(5),
            ],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("Hello".into()));
    }

    #[test]
    fn indexof_finds_needle() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![
                Value::Text("Hello World".into()),
                Value::Text("World".into()),
            ],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Integer(7));
    }

    #[test]
    fn indexof_missing_returns_zero() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![Value::Text("Hello".into()), Value::Text("xyz".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Integer(0));
    }

    #[test]
    fn lowercase_works() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "LowerCase",
            vec![Value::Text("HELLO".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("hello".into()));
    }

    #[test]
    fn uppercase_works() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "UpperCase",
            vec![Value::Text("hello".into())],
            &mut ctx,
        );
        assert_eq!(ok(result), Value::Text("HELLO".into()));
    }

    #[test]
    fn strsubstno_percent10_placeholder_corrupted_adversarial_h_8() {
        // FINDING P1 wrong-result: substitute_placeholders iterates i=0..9
        // and replaces %1 first, consuming the %1 prefix inside %10. Format
        // "%1 and %10" with 10 args yields "FIRST and FIRST0" not "FIRST and TENTH".
        // Root cause: str::replace scans the original string left-to-right; the
        // %1 at position 0 AND the %1 inside %10 both get replaced on iteration 0.
        // Expected: "FIRST and TENTH"
        // Observed: "FIRST and FIRST0"
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("%1 and %10".into()),
                Value::Text("FIRST".into()),
                Value::Text("TWO".into()),
                Value::Text("THREE".into()),
                Value::Text("FOUR".into()),
                Value::Text("FIVE".into()),
                Value::Text("SIX".into()),
                Value::Text("SEVEN".into()),
                Value::Text("EIGHT".into()),
                Value::Text("NINE".into()),
                Value::Text("TENTH".into()),
            ],
            &mut ctx,
        );
        match ok(result) {
            Value::Text(s) => assert_eq!(
                s, "FIRST and TENTH",
                "%%10 must map to 10th arg; got: {s:?}"
            ),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn copystr_pos_beyond_string_length_adversarial_h_9() {
        // FINDING P2 spec-deviation: CopyStr("abc", 4, 1) silently returns ""
        // instead of raising an error. Position 4 is beyond the 3-char string.
        // AL/BC runtime raises "The value is too large" for out-of-bounds pos.
        // Expected: Eval::Error
        // Observed: Normal(Text(""))
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("abc".into()),
                Value::Integer(4),
                Value::Integer(1),
            ],
            &mut ctx,
        );
        assert!(
            result.is_error(),
            "CopyStr pos > string length must error, got: {:?}",
            result
        );
    }

    #[test]
    fn copystr_negative_len_errors_not_silent_truncate() {
        // Regression: CopyStr("hello", 1, -3) must raise an error. A naive
        // `*n as usize` cast wraps -3 to 2^64-3, which then clamps to the
        // string end and silently returns "hello" instead of erroring.
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("hello".into()),
                Value::Integer(1),
                Value::Integer(-3),
            ],
            &mut ctx,
        );
        assert!(
            result.is_error(),
            "CopyStr with negative len must error, got: {:?}",
            result
        );
    }

    #[test]
    fn copystr_negative_pos_errors() {
        // Regression: CopyStr("hello", -1, 2) must raise an error. A naive
        // `*pos as usize` cast wraps -1 to 2^64-1; the primary `pos <= 0`
        // guard must reject it directly.
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "CopyStr",
            vec![
                Value::Text("hello".into()),
                Value::Integer(-1),
                Value::Integer(2),
            ],
            &mut ctx,
        );
        assert!(
            result.is_error(),
            "CopyStr with negative pos must error, got: {:?}",
            result
        );
    }

    #[test]
    fn indexof_empty_needle_returns_one_not_zero_adversarial_h_10() {
        // FINDING P2 edge-case: IndexOf("ab", "") returns Integer(1) because
        // Rust str::find("") returns Some(0). AL convention: empty needle → 0.
        // Expected: Integer(0)
        // Observed: Integer(1)
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "IndexOf",
            vec![Value::Text("ab".into()), Value::Text(String::new())],
            &mut ctx,
        );
        assert_eq!(
            ok(result),
            Value::Integer(0),
            "IndexOf with empty needle should return 0"
        );
    }

    fn workspace_with_helper() -> Arc<Workspace> {
        let ws = Arc::new(Workspace::new());
        let source = r#"codeunit 50999 "Helper"
{
    procedure Add(a: Integer; b: Integer): Integer
    begin
        exit(a + b);
    end;

    procedure Forever()
    begin
        Forever();
    end;
}
"#;
        ws.file_index.add_file(
            std::path::PathBuf::from("/test/Helper.al"),
            source.to_string(),
        );
        ws
    }

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
    fn workspace_dispatch_unknown_procedure_not_found_negative() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);
        let result = dispatch_call(Some("Helper"), "NoSuchProc", vec![], &mut ctx);
        let e = err(result);
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
        let e = err(result);
        assert!(
            e.message.to_lowercase().contains("type"),
            "expected 'type' in error message, got: {}",
            e.message
        );
    }

    #[test]
    fn workspace_dispatch_deep_recursion_negative() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);
        let result = dispatch_call(Some("Helper"), "Forever", vec![], &mut ctx);
        let e = err(result);
        assert!(
            e.message.contains("recursion depth exceeded"),
            "expected 'recursion depth exceeded' in error, got: {}",
            e.message
        );
    }

    #[test]
    fn deadline_exceeded_returns_false_when_unset() {
        let ws = workspace_with_helper();
        let ctx = DispatchCtx::new_pure(ws);
        assert!(!ctx.deadline_exceeded());
    }

    #[test]
    fn deadline_exceeded_returns_true_when_past() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);
        ctx.deadline = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(ctx.deadline_exceeded());
    }

    #[test]
    fn deadline_exceeded_returns_false_when_future() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);
        ctx.deadline = Some(std::time::Instant::now() + std::time::Duration::from_secs(60));
        assert!(!ctx.deadline_exceeded());
    }
}
