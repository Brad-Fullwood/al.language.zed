//! Procedure-call dispatch for the AL interpreter.
//!
//! `dispatch_call` is the single entry point for any procedure call that the
//! statement evaluator encounters. Priority order:
//!
//! 1. **Stub catalogs** — Library Assert and any other native-Rust ports of
//!    BC test libraries (via `crate::stubs`).
//! 2. **Inline builtins** — core AL global procedures implemented here in Rust
//!    (Error, Message, StrSubstNo, Format, StrLen, CopyStr, LowerCase,
//!    UpperCase, IndexOf).
//! 3. **Workspace procedures** — looks up the procedure in the workspace file
//!    index by receiver/object name, finds the `procedure_declaration` node,
//!    and executes its body via `eval_stmt`. Recursion depth is capped at 100.

use std::collections::HashMap;
use std::sync::Arc;

use crate::interpreter::records::{self, RecordStore};
use crate::interpreter::scope::{CallFrame, Eval, ScopeStack};
use crate::interpreter::value::{ErrorInfo, Value};
use crate::stubs;

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
    /// Enable in-memory record operations.
    WithRecords,
}

/// Dialog handlers enabled for the currently executing `[Test]` method.
/// Names are paired with their containing workspace codeunit so dispatch is
/// deterministic even when another object declares the same procedure name.
#[derive(Debug, Clone, Default)]
pub struct TestHandlers {
    pub message: Option<(String, String)>,
    pub confirm: Option<(String, String)>,
}

/// Shared context threaded through `eval_stmt` and `dispatch_call`.
///
/// Owns the workspace handle plus any stateful runtime support (record
/// catalog for `InterpRecord` runs). Cheap to reborrow; pass `&mut DispatchCtx`
/// everywhere.
pub struct DispatchCtx {
    pub source: Arc<dyn al_types::ProcedureSource>,
    /// In-memory record stores keyed by lowercased table name. Built lazily by
    /// the record-op wiring (`interpreter::records`) the first time a record
    /// variable of that table is touched. Keyed by table name (not id) because
    /// the BC-free interpreter has no table-id symbol table; two record
    /// variables of the same table share one backing store (shared physical
    /// table semantics — see `records.rs`).
    pub records: HashMap<String, RecordStore>,
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
    /// an unbounded `while true do …` test can't pin the daemon thread
    /// past the configured per-test budget. `None` means "no deadline" —
    /// used by unit-test paths that need full determinism.
    pub deadline: Option<std::time::Instant>,
    /// Optional external cancellation signal. Loop constructs in
    /// `eval_stmt` check this on every iteration alongside `deadline_exceeded`.
    /// A daemon `$/cancelRequest` can interrupt the interpreter mid-loop without
    /// waiting for the wall-clock deadline. The token is `Arc<AtomicBool>` so it
    /// can be cheaply shared
    /// across the call and signalled from a different task. `None` means
    /// "not cancellable" — unit-test path and CLI default.
    pub cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Optional dynamic-coverage collector. `None` (the default) makes
    /// coverage zero-cost: the `cov_*` helpers below become a single `Option`
    /// check and do nothing. When `Some`, `eval_stmt` records each executed
    /// statement's line and `eval_if`/`eval_case` record the branch decision.
    /// See [`crate::interpreter::coverage`].
    pub coverage: Option<crate::interpreter::coverage::Coverage>,
    /// Write-back channel for `var` (by-reference) parameters. After a
    /// workspace procedure runs, `dispatch_workspace_procedure` records
    /// `(arg_index, final_value)` here for each `var` parameter; the caller in
    /// `eval_stmt` drains it and writes each value back into the argument's
    /// variable so mutations propagate to the caller (BC by-ref semantics).
    /// Cleared at the top of every `dispatch_call`, so builtins and
    /// non-workspace calls leave it empty.
    pub var_writebacks: Vec<(usize, Value)>,
    pub test_handlers: TestHandlers,
}

impl DispatchCtx {
    pub fn new_pure(source: Arc<dyn al_types::ProcedureSource>) -> Self {
        Self {
            source,
            records: HashMap::new(),
            mode: DispatchMode::PureLogic,
            recursion_depth: 0,
            ast_depth: 0,
            deadline: None,
            cancel: None,
            coverage: None,
            var_writebacks: Vec::new(),
            test_handlers: TestHandlers::default(),
        }
    }

    pub fn new_with_records(
        source: Arc<dyn al_types::ProcedureSource>,
        records: HashMap<String, RecordStore>,
    ) -> Self {
        Self {
            source,
            records,
            mode: DispatchMode::WithRecords,
            recursion_depth: 0,
            ast_depth: 0,
            deadline: None,
            cancel: None,
            coverage: None,
            var_writebacks: Vec::new(),
            test_handlers: TestHandlers::default(),
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

    /// Set the file that subsequent coverage records
    /// attribute to, returning the previous file so a caller crossing a
    /// procedure boundary can restore it via [`Self::cov_restore_file`]. A no-op
    /// returning `None` when coverage is disabled.
    pub fn cov_enter_file(&mut self, file: &str) -> Option<String> {
        self.coverage.as_mut().map(|c| c.set_current_file(file))
    }

    /// Restore the coverage file previously returned by
    /// [`Self::cov_enter_file`]. No-op when coverage is disabled or `prev` is
    /// `None`.
    pub fn cov_restore_file(&mut self, prev: Option<String>) {
        if let (Some(c), Some(p)) = (self.coverage.as_mut(), prev) {
            c.set_current_file(&p);
        }
    }

    /// Record that the statement at `node` executed. Reads
    /// the node's 1-based start line. Zero-cost when coverage is disabled.
    pub fn cov_record_stmt(&mut self, node: tree_sitter::Node<'_>) {
        if let Some(c) = self.coverage.as_mut() {
            c.record_statement(node.start_position().row as u32 + 1);
        }
    }

    /// Record a two-way branch decision taken at the `if` or
    /// `case` head `node`. `taken == true` is THEN / arm-matched; `false` is
    /// ELSE / no-arm. Zero-cost when coverage is disabled.
    pub fn cov_record_decision(&mut self, node: tree_sitter::Node<'_>, taken: bool) {
        if let Some(c) = self.coverage.as_mut() {
            c.record_decision(node.start_position().row as u32 + 1, taken);
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
    // Clear any var-parameter write-backs left over from a previous call so
    // builtins and non-workspace calls (which never populate it) leave the
    // channel empty for the caller to observe.
    ctx.var_writebacks.clear();
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
        "message" => {
            if let Some((object, handler)) = ctx.test_handlers.message.clone() {
                let message = formatted_dialog_text(&args);
                let result = dispatch_workspace_procedure(
                    Some(&object),
                    &handler,
                    vec![Value::Text(message)],
                    ctx,
                );
                ctx.var_writebacks.clear();
                return result;
            }
            return builtin_message(&args);
        }
        "confirm" => {
            if let Some((object, handler)) = ctx.test_handlers.confirm.clone() {
                let question = formatted_dialog_text(&args);
                let result = dispatch_workspace_procedure(
                    Some(&object),
                    &handler,
                    vec![Value::Text(question), Value::Boolean(false)],
                    ctx,
                );
                if result.is_error() {
                    return result;
                }
                let reply = ctx
                    .var_writebacks
                    .iter()
                    .find(|(index, _)| *index == 1)
                    .and_then(|(_, value)| match value {
                        Value::Boolean(reply) => Some(*reply),
                        _ => None,
                    })
                    .unwrap_or(false);
                ctx.var_writebacks.clear();
                return Eval::Normal(Value::Boolean(reply));
            }
            return Eval::Normal(Value::Boolean(
                args.get(1)
                    .and_then(|value| match value {
                        Value::Boolean(default) => Some(*default),
                        _ => None,
                    })
                    .unwrap_or(false),
            ));
        }
        "strsubstno" => return builtin_strsubstno(&args),
        "format" => return builtin_format(&args),
        "strlen" => return builtin_strlen(&args),
        "copystr" => return builtin_copystr(&args),
        "lowercase" => return builtin_lowercase(&args),
        "uppercase" => return builtin_uppercase(&args),
        "indexof" => return builtin_indexof(&args),
        "maxstrlen" => return builtin_maxstrlen(&args),
        "createdatetime" => return builtin_createdatetime(&args),
        "currentdatetime" => return Eval::Normal(Value::DateTime(clock_current_datetime())),
        "today" => return Eval::Normal(Value::Date(clock_today())),
        "time" => return Eval::Normal(Value::Time(clock_time())),
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
///    visible procedures in the current object).
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
        match ctx.source.find_by_object_name(recv) {
            Some(path) => vec![path],
            None => {
                return simple_error(format!("object '{}' not found in workspace", recv));
            }
        }
    } else {
        ctx.source.iter_paths()
    };

    for path in &candidate_paths {
        let Some((text, tree)) = ctx.source.get_cached_parse(path) else {
            continue;
        };

        let source = text.as_bytes();
        let root = tree.root_node();

        let object_name = ctx.source.object_name(path).unwrap_or_default();

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

        if args.len() != params.len() {
            return simple_error(format!(
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
            return simple_error(format!("procedure '{}' has no body", procedure));
        };

        let mut frame = CallFrame::new(object_name.as_str(), procedure);
        for (i, param) in params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::Empty);
            // Coerce an integer argument to the parameter's declared width so a
            // `BigInteger` parameter keeps i64 semantics even when passed a small
            // Integer literal and vice versa, matching BC's fixed parameter types.
            let val = coerce_int_width(val, &param.type_name);
            frame.bind(&param.name, val);
            if let Some(length) = declared_text_length(&param.type_name) {
                frame.bind_declared_text_length(&param.name, length);
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

        ctx.recursion_depth += 1;
        let mut scope = ScopeStack::new();
        scope.push(frame);
        // Attribute this procedure's statements to the file
        // it is defined in (which may differ from the caller's file), then
        // restore the caller's file when the call returns.
        let cov_prev_file = ctx.cov_enter_file(&path.to_string_lossy());
        let result = crate::interpreter::eval_stmt::eval_stmt(body, source, &mut scope, ctx);
        ctx.cov_restore_file(cov_prev_file);
        ctx.recursion_depth -= 1;

        // Record final values of `var` (by-reference) parameters so the caller
        // can write them back into its own argument variables. Read from
        // the still-live callee frame before it is dropped. Nested calls during
        // the body already cleared/consumed the channel via their own
        // `dispatch_call`, so populating it here (after the body) is safe.
        ctx.var_writebacks.clear();
        for (i, param) in params.iter().enumerate() {
            if param.is_var {
                if let Some(val) = scope.top().and_then(|f| f.get(&param.name)).cloned() {
                    ctx.var_writebacks.push((i, val));
                }
            }
        }

        // Unwrap Exit into Normal (exit only unwinds the current procedure).
        // A break/continue that reached here escaped all loops — a runtime
        // error in AL, not silent success.
        return match result {
            Eval::Exit(v) => Eval::Normal(v),
            Eval::Break => simple_error("break statement not inside a loop"),
            Eval::Continue => simple_error("continue statement not inside a loop"),
            other => other,
        };
    }

    simple_error(format!(
        "procedure not found: {}{}",
        receiver.map(|r| format!("{r}.")).unwrap_or_default(),
        procedure
    ))
}

/// Bind a procedure's local variables into `frame` exactly as workspace
/// dispatch does: scalar `var`-section locals to their type defaults, then
/// structured locals (`Record`/`Codeunit`/`List of [T]`) to their handle
/// defaults. Exposed so the test-runner path can share the same frame setup
/// and not execute `[Test]` bodies with unbound locals — BC zero-initializes
/// every local, so reading one before assignment must not error.
pub fn bind_procedure_locals(
    proc_node: tree_sitter::Node<'_>,
    source: &[u8],
    frame: &mut CallFrame,
) {
    bind_local_vars(proc_node, source, frame);
    bind_structured_locals(proc_node, source, frame);
}

/// Bind object-level `var` declarations into a long-lived frame. Test
/// lifecycle execution keeps this frame beneath initialize/test/cleanup
/// procedure frames so scalar and structured codeunit globals retain state.
pub fn bind_object_globals(root: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "object_var_section" {
            let mut cursor = node.walk();
            for declaration in node.named_children(&mut cursor) {
                if declaration.kind() != "object_variable_declaration" {
                    continue;
                }
                if declaration.kind() == "regular_variable_declaration" {
                    bind_regular_var_decl(declaration, source, frame);
                    bind_structured_var_decl(declaration, source, frame);
                    continue;
                }
                let mut declaration_cursor = declaration.walk();
                for regular in declaration.named_children(&mut declaration_cursor) {
                    if regular.kind() == "regular_variable_declaration" {
                        bind_regular_var_decl(regular, source, frame);
                        bind_structured_var_decl(regular, source, frame);
                    }
                }
            }
            continue;
        }
        if matches!(node.kind(), "procedure_declaration" | "trigger_declaration") {
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}

#[derive(Debug, Clone)]
struct ParamDecl {
    name: String,
    type_name: String,
    /// True when the parameter is declared `var` (passed by reference). The
    /// caller's argument variable is updated with the parameter's final value
    /// after the call returns.
    is_var: bool,
}

/// Pre-bind a procedure's structured local variables. Scans the `var_section`
/// for `regular_variable_declaration`s and, for each whose type is a
/// `Record`/`Codeunit`/`List of [T]` (the kinds [`records::default_for_structured`]
/// recognises), binds every declared name to the structured handle default.
/// Scalar locals are intentionally left unbound (they auto-bind on first
/// assignment), preserving the pre-existing behaviour.
fn bind_structured_locals(proc_node: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let mut cursor = proc_node.walk();
    let var_sections: Vec<tree_sitter::Node<'_>> = proc_node
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "var_section")
        .collect();

    for section in var_sections {
        let mut sc = section.walk();
        for decl in section.named_children(&mut sc) {
            if decl.kind() != "variable_declaration" {
                continue;
            }
            let mut dc = decl.walk();
            for reg in decl.named_children(&mut dc) {
                if reg.kind() != "regular_variable_declaration" {
                    continue;
                }
                bind_structured_var_decl(reg, source, frame);
            }
        }
    }
}

/// Bind every name on one `regular_variable_declaration` whose type is a
/// structured handle (`Record`/`Codeunit`/`List`).
fn bind_structured_var_decl(reg: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let mut names: Vec<String> = Vec::new();
    let mut type_text: Option<String> = None;

    let mut rc = reg.walk();
    if rc.goto_first_child() {
        loop {
            match rc.field_name() {
                Some("name") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        names.push(t.trim_matches('"').to_string());
                    }
                }
                Some("type") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        type_text = Some(t.to_string());
                    }
                }
                _ => {}
            }
            if !rc.goto_next_sibling() {
                break;
            }
        }
    }

    let Some(type_text) = type_text else {
        return;
    };
    let Some(default) = records::default_for_structured(&type_text) else {
        return; // scalar / unknown type — leave for lazy auto-bind on assignment.
    };

    for name in names {
        if frame.get(&name).is_none() {
            frame.bind(&name, default.clone());
        }
    }
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

        // `var` (by-reference) modifier: the grammar puts an optional `kw_var`
        // token before the name. Check both the node kind and the raw text so
        // this works whether or not the token is exposed as a named child.
        let mut is_var = false;
        for i in 0..child.child_count() {
            if let Some(n) = child.child(i) {
                if n.kind() == "kw_var"
                    || n.utf8_text(source)
                        .map(|t| t.eq_ignore_ascii_case("var"))
                        .unwrap_or(false)
                {
                    is_var = true;
                    break;
                }
            }
        }

        let type_node = child_by_field_or_kind(
            child,
            "type",
            &["type_reference", "type", "builtin_type", "primitive_type"],
        );
        let type_name = type_node
            .and_then(|n| n.utf8_text(source).ok())
            .map(|t| t.trim().to_string())
            .unwrap_or_default();

        params.push(ParamDecl {
            name,
            type_name,
            is_var,
        });
    }

    params
}

/// Bind a procedure's local `var` section into `frame`, default-initialising
/// each declared variable. Supports multi-name declarations
/// (`A, B, C : Integer;`): every `name:` field on a `regular_variable_declaration`
/// gets its own slot with the type's default value.
///
/// Variables already bound (parameters) are left untouched. Types the
/// interpreter cannot default (records, lists, etc.) are skipped — those still
/// auto-bind lazily on first assignment, preserving prior behaviour.
fn bind_local_vars(proc_node: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    // Find the `var_section` directly under the procedure declaration.
    let mut cursor = proc_node.walk();
    let var_sections: Vec<tree_sitter::Node<'_>> = proc_node
        .named_children(&mut cursor)
        .filter(|n| n.kind() == "var_section")
        .collect();

    for section in var_sections {
        let mut sc = section.walk();
        for decl in section.named_children(&mut sc) {
            if decl.kind() != "variable_declaration" {
                continue;
            }
            // The concrete declaration shape is `regular_variable_declaration`
            // (the only kind that carries `name:`/`type:` fields we default).
            let mut dc = decl.walk();
            for reg in decl.named_children(&mut dc) {
                if reg.kind() != "regular_variable_declaration" {
                    continue;
                }
                bind_regular_var_decl(reg, source, frame);
            }
        }
    }
}

/// Bind every name on a single `regular_variable_declaration` to the default
/// value of its declared type.
fn bind_regular_var_decl(reg: tree_sitter::Node<'_>, source: &[u8], frame: &mut CallFrame) {
    let mut names: Vec<String> = Vec::new();
    let mut type_text: Option<String> = None;

    let mut rc = reg.walk();
    if rc.goto_first_child() {
        loop {
            match rc.field_name() {
                Some("name") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        names.push(t.trim_matches('"').to_string());
                    }
                }
                Some("type") => {
                    if let Ok(t) = rc.node().utf8_text(source) {
                        type_text = Some(t.to_string());
                    }
                }
                _ => {}
            }
            if !rc.goto_next_sibling() {
                break;
            }
        }
    }

    let Some(type_text) = type_text else {
        return;
    };
    // Strip any length/subtype suffix (`Text[20]`, `Code[10]`) to the base name.
    let base = type_text
        .split(['[', ' '])
        .next()
        .unwrap_or(&type_text)
        .trim();
    let Some(default) = Value::default_for(base) else {
        return; // complex/unknown type — leave for lazy auto-bind on assignment.
    };

    for name in names {
        if let Some(length) = declared_text_length(&type_text) {
            frame.bind_declared_text_length(&name, length);
        }
        if frame.get(&name).is_none() {
            frame.bind(&name, default.clone());
        }
    }
}

fn declared_text_length(type_text: &str) -> Option<usize> {
    let trimmed = type_text.trim();
    let base = trimmed.split('[').next()?.trim();
    if !matches!(base.to_ascii_lowercase().as_str(), "text" | "code") {
        return None;
    }
    let start = trimmed.find('[')? + 1;
    let end = trimmed[start..].find(']')? + start;
    trimmed[start..end].trim().parse().ok()
}

/// Check whether a `Value` matches the declared AL type name.
///
/// Returns `Some(error_message)` on mismatch, `None` on pass.
/// Unknown complex type names are accepted because this layer has no complete
/// runtime type catalog.
fn check_param_type(arg: &Value, type_name: &str) -> Option<String> {
    if type_name.is_empty() {
        return None;
    }
    let lower = type_name.to_lowercase();
    match lower.as_str() {
        "integer" | "biginteger" if !matches!(arg, Value::Integer(_) | Value::BigInteger(_)) => {
            return Some(format!("expected Integer, got {}", arg.type_name()));
        }
        "decimal"
            if !matches!(
                arg,
                Value::Decimal(_) | Value::Integer(_) | Value::BigInteger(_)
            ) =>
        {
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
        // Complex types require symbol metadata not available in this layer.
        _ => {}
    }
    None
}

/// Coerce an integer value to the width named by `type_name` (`Integer` vs
/// `BigInteger`), leaving non-integer values and non-integer types untouched.
/// Used at parameter binding so a `BigInteger` parameter keeps i64 arithmetic
/// semantics even when the caller passes a small `Integer` literal.
fn coerce_int_width(val: Value, type_name: &str) -> Value {
    // Match on the value first so the (allocation-free) type-name check is only
    // reached for integer arguments — the common Text/Record/Boolean args skip
    // it entirely.
    match val {
        Value::Integer(n) if type_name.eq_ignore_ascii_case("biginteger") => Value::BigInteger(n),
        Value::BigInteger(n) if type_name.eq_ignore_ascii_case("integer") => Value::Integer(n),
        other => other,
    }
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

fn formatted_dialog_text(args: &[Value]) -> String {
    let message = match args.first() {
        Some(Value::Text(text)) | Some(Value::Code(text)) => text.clone(),
        Some(value) => render_value(value),
        None => String::new(),
    };
    if args.len() > 1 {
        substitute_placeholders(&message, &args[1..])
    } else {
        message
    }
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
/// Supports the single-argument form.
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
        .map(|i| s[..i].chars().count() as i64 + 1)
        .unwrap_or(0);
    Eval::Normal(Value::Integer(result))
}

/// `MaxStrLen` requires declared type metadata that `Value` does not carry.
fn builtin_maxstrlen(args: &[Value]) -> Eval {
    match args {
        [Value::Text(_)] | [Value::Code(_)] => simple_error(
            "MaxStrLen is unavailable because the interpreter does not retain declared text lengths",
        ),
        [v] => simple_error(format!(
            "MaxStrLen expects Text or Code, got {}",
            v.type_name()
        )),
        _ => simple_error("MaxStrLen expects exactly 1 argument"),
    }
}

/// `CreateDateTime(date, time)` — combine a Date and Time into a DateTime.
///
/// Carriers: `Date` is days since the AL epoch, `Time` is milliseconds since
/// midnight, `DateTime` is milliseconds since the AL epoch — so the result is
/// `days * MS_PER_DAY + time_ms`.
fn builtin_createdatetime(args: &[Value]) -> Eval {
    match args {
        [Value::Date(d), Value::Time(t)] => {
            match d
                .checked_mul(crate::interpreter::value::MS_PER_DAY)
                .and_then(|ms| ms.checked_add(*t))
            {
                Some(dt) => Eval::Normal(Value::DateTime(dt)),
                None => simple_error("CreateDateTime: datetime overflow"),
            }
        }
        [a, b] => simple_error(format!(
            "CreateDateTime expects (Date, Time), got ({}, {})",
            a.type_name(),
            b.type_name()
        )),
        _ => simple_error("CreateDateTime expects exactly 2 arguments"),
    }
}

/// Signed milliseconds since the Unix epoch.
fn unix_now_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).expect("system time exceeds i64"),
        Err(error) => {
            -i64::try_from(error.duration().as_millis()).expect("system time exceeds i64")
        }
    }
}

/// Current date as days since the AL epoch (0001-01-01).
pub(crate) fn clock_today() -> i64 {
    unix_now_ms() / crate::interpreter::value::MS_PER_DAY
        + crate::interpreter::value::AL_EPOCH_TO_UNIX_DAYS
}

/// Current wall-clock time as milliseconds since midnight UTC.
pub(crate) fn clock_time() -> i64 {
    unix_now_ms().rem_euclid(crate::interpreter::value::MS_PER_DAY)
}

/// Current date-time as milliseconds since the AL epoch (0001-01-01).
pub(crate) fn clock_current_datetime() -> i64 {
    unix_now_ms()
        + crate::interpreter::value::AL_EPOCH_TO_UNIX_DAYS * crate::interpreter::value::MS_PER_DAY
}

/// Render a `Value` as AL would show it in StrSubstNo / Format.
fn render_value(v: &Value) -> String {
    match v {
        Value::Integer(n) | Value::BigInteger(n) => n.to_string(),
        Value::Decimal(n) => n.normalize().to_string(),
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
    use crate::test_support::MockSource as Workspace;
    use std::sync::Arc;

    fn ctx() -> DispatchCtx {
        DispatchCtx::new_pure(Arc::new(Workspace::new()))
    }

    fn ok(eval: Eval) -> Value {
        match eval {
            Eval::Normal(v) => v,
            Eval::Error(e) => panic!("unexpected error: {}", e.message),
            Eval::Exit(v) => v,
            Eval::Break | Eval::Continue => panic!("unexpected break/continue"),
        }
    }

    fn err(eval: Eval) -> ErrorInfo {
        match eval {
            Eval::Error(e) => e,
            Eval::Normal(v) => panic!("expected error, got Normal({})", v.type_name()),
            Eval::Exit(v) => panic!("expected error, got Exit({})", v.type_name()),
            Eval::Break | Eval::Continue => panic!("expected error, got break/continue"),
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
    fn strsubstno_handles_percent10_without_corrupting_percent1() {
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
    fn copystr_rejects_position_beyond_string_length() {
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
    fn indexof_empty_needle_returns_zero() {
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
    fn workspace_dispatch_rejects_wrong_argument_count() {
        let ws = workspace_with_helper();
        let mut ctx = DispatchCtx::new_pure(ws);

        let missing = dispatch_call(Some("Helper"), "Add", vec![Value::Integer(2)], &mut ctx);
        assert!(err(missing)
            .message
            .contains("expects 2 argument(s), got 1"));

        let extra = dispatch_call(
            Some("Helper"),
            "Add",
            vec![Value::Integer(2), Value::Integer(3), Value::Integer(4)],
            &mut ctx,
        );
        assert!(err(extra).message.contains("expects 2 argument(s), got 3"));
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
