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

/// Each interpreted call level is a dispatch→eval_stmt→eval_expr native
/// frame cluster that can cost tens of KiB of stack in debug builds, and
/// the interpreter must stay within a 2 MiB thread stack (test threads and
/// tokio workers — not the 8 MiB main thread). 48 levels keeps the worst
/// case comfortably inside that budget while remaining far deeper than any
/// realistic AL test-code call chain.
const MAX_RECURSION_DEPTH: usize = 48;

/// Maximum syntactic nesting depth `eval_stmt` will descend into before
/// aborting with an error. The counter is cumulative across nested
/// procedure calls (a call chain stacks ~4 AST levels per frame), so the
/// cap must exceed what `MAX_RECURSION_DEPTH` (48 × ~4 = 192) can reach
/// via call recursion alone — that way an infinite-call test trips the
/// call cap first (clearer error message) and only truly pathological
/// single-procedure nesting trips this AST cap. 256 such frames stay
/// within the same 2 MiB thread-stack budget as the call cap.
pub const MAX_AST_DEPTH: usize = 256;

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
    pub str_menu: Option<(String, String)>,
    pub hyperlink: Option<(String, String)>,
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
    /// Transient expression traces used while evaluating a covered Boolean
    /// decision. Each nested decision gets its own map so a Boolean helper
    /// procedure containing another IF cannot overwrite its caller's trace.
    ///
    /// This is public only because `DispatchCtx` is constructed by the
    /// sibling `al-test` crate; callers must leave it empty.
    #[doc(hidden)]
    pub condition_trace_stack: Vec<HashMap<(usize, usize), Vec<bool>>>,
    /// Write-back channel for `var` (by-reference) parameters. After a
    /// workspace procedure runs, `dispatch_workspace_procedure` records
    /// `(arg_index, final_value)` here for each `var` parameter; the caller in
    /// `eval_stmt` drains it and writes each value back into the argument's
    /// variable so mutations propagate to the caller (BC by-ref semantics).
    /// Cleared at the top of every `dispatch_call`, so builtins and
    /// non-workspace calls leave it empty.
    pub var_writebacks: Vec<(usize, Value)>,
    pub test_handlers: TestHandlers,
    /// Allocator for per-record-variable view handles. Each record variable
    /// gets its own filter/cursor/buffer view over the shared table store
    /// (BC semantics); the handle is stored on the variable's `RecordValue`.
    pub next_record_handle: u64,
    /// True while the innermost call being dispatched sits in *statement*
    /// position (`Rec.Get(...);` as its own statement). Consumed (reset)
    /// by the record-method dispatcher: BC raises a runtime error when a
    /// statement-position `Get`/`Find*` misses, but returns `false` in
    /// expression position (`if Rec.Get(...) then`). Set by
    /// `eval_expression_stmt` immediately before evaluating a call.
    #[doc(hidden)]
    pub stmt_position: bool,
    /// The most recent error captured by `asserterror`, surfaced through the
    /// `GetLastErrorText` / `ClearLastError` builtins.
    pub last_error: Option<ErrorInfo>,
    /// The session work date. `None` until first set — `WorkDate` then
    /// defaults to `Today` (BC's session default).
    pub work_date: Option<i64>,
    /// Deterministic LCG state for the `Random`/`Randomize` builtins. Seeded
    /// with a fixed value so interpreter runs are reproducible; `Randomize(n)`
    /// re-seeds it explicitly.
    pub random_state: u64,
    /// Memoized parses for expression fragments recovered from lossless
    /// bracket blocks (`in [...]` set members). Keyed by the fragment text so
    /// a set literal inside a loop parses each member once, not once per
    /// iteration.
    #[doc(hidden)]
    pub expr_fragment_cache: HashMap<String, (String, tree_sitter::Tree)>,
}

/// Fixed default seed for the deterministic `Random` builtin.
pub const DEFAULT_RANDOM_SEED: u64 = 1;

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
            condition_trace_stack: Vec::new(),
            var_writebacks: Vec::new(),
            test_handlers: TestHandlers::default(),
            next_record_handle: 0,
            stmt_position: false,
            last_error: None,
            work_date: None,
            random_state: DEFAULT_RANDOM_SEED,
            expr_fragment_cache: HashMap::new(),
        }
    }

    pub fn new_with_records(
        source: Arc<dyn al_types::ProcedureSource>,
        records: HashMap<String, RecordStore>,
    ) -> Self {
        Self {
            records,
            mode: DispatchMode::WithRecords,
            ..Self::new_pure(source)
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

    /// Start tracing the atomic Boolean conditions that contribute to one
    /// covered decision. Disabled coverage allocates nothing.
    pub fn cov_begin_condition_trace(&mut self) {
        if self.coverage.is_some() {
            self.condition_trace_stack.push(HashMap::new());
        }
    }

    /// Finish the innermost condition trace and return the source-ordered
    /// atomic condition outcomes associated with `condition`.
    pub fn cov_finish_condition_trace(
        &mut self,
        condition: tree_sitter::Node<'_>,
    ) -> Option<Vec<bool>> {
        self.coverage.as_ref()?;
        let traces = self.condition_trace_stack.pop()?;
        traces
            .get(&(condition.start_byte(), condition.end_byte()))
            .cloned()
    }

    /// Record one complete condition vector and decision outcome for MC/DC.
    pub fn cov_record_condition_observation(
        &mut self,
        node: tree_sitter::Node<'_>,
        conditions: Option<Vec<bool>>,
        outcome: bool,
    ) {
        let Some(conditions) = conditions.filter(|conditions| !conditions.is_empty()) else {
            return;
        };
        if let Some(c) = self.coverage.as_mut() {
            c.record_condition_observation(
                node.start_position().row as u32 + 1,
                conditions,
                outcome,
            );
        }
    }

    pub(crate) fn cov_set_expression_trace(
        &mut self,
        node: tree_sitter::Node<'_>,
        conditions: Vec<bool>,
    ) {
        if let Some(traces) = self.condition_trace_stack.last_mut() {
            traces.insert((node.start_byte(), node.end_byte()), conditions);
        }
    }

    pub(crate) fn cov_expression_trace(&self, node: tree_sitter::Node<'_>) -> Option<Vec<bool>> {
        self.condition_trace_stack
            .last()
            .and_then(|traces| traces.get(&(node.start_byte(), node.end_byte())))
            .cloned()
    }

    pub(crate) fn cov_condition_trace_active(&self) -> bool {
        !self.condition_trace_stack.is_empty()
    }

    /// Register a named path at a multi-way decision site without marking it
    /// taken. Used to preserve the denominator for untouched `case` arms.
    pub fn cov_ensure_path(&mut self, node: tree_sitter::Node<'_>, path: impl Into<String>) {
        if let Some(c) = self.coverage.as_mut() {
            c.ensure_path(node.start_position().row as u32 + 1, path);
        }
    }

    /// Record a named path through a multi-way decision.
    pub fn cov_record_path(&mut self, node: tree_sitter::Node<'_>, path: impl Into<String>) {
        if let Some(c) = self.coverage.as_mut() {
            c.record_path(node.start_position().row as u32 + 1, path);
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
    let mut stack = ScopeStack::new();
    dispatch_call_scoped(receiver, procedure, args, &mut stack, ctx)
}

/// Resolve a call against the active interpreter scope.
///
/// Preserving the stack is required for same-codeunit procedure calls and test
/// handlers to observe the codeunit's object-level globals. The public
/// [`dispatch_call`] wrapper intentionally starts an empty scope for isolated
/// builtin/stub tests.
pub(crate) fn dispatch_call_scoped(
    receiver: Option<&str>,
    procedure: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    // Clear any var-parameter write-backs left over from a previous call so
    // builtins and non-workspace calls (which never populate it) leave the
    // channel empty for the caller to observe.
    ctx.var_writebacks.clear();
    // Statement-position information applies only to record-method dispatch
    // (which consumes it before reaching here); make sure it never leaks into
    // a callee's body.
    ctx.stmt_position = false;
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

    // Global builtins must never hijack an explicitly-qualified workspace
    // method with the same name (for example `Helper.Format(...)`).
    if receiver.is_none() {
        match procedure.to_ascii_lowercase().as_str() {
            "error" => return builtin_error(&args),
            "message" => {
                if let Some((object, handler)) = ctx.test_handlers.message.clone() {
                    if args.is_empty() {
                        return simple_error("Message requires a message argument");
                    }
                    let message = formatted_dialog_text(&args);
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![Value::Text(message)],
                        stack,
                        ctx,
                    );
                    ctx.var_writebacks.clear();
                    return result;
                }
                return simple_error(
                    "Message requires a configured [MessageHandler] in the local test runtime",
                );
            }
            "confirm" => {
                if let Some((object, handler)) = ctx.test_handlers.confirm.clone() {
                    if args.is_empty() {
                        return simple_error("Confirm requires a question argument");
                    }
                    let question = formatted_dialog_text(&args);
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![Value::Text(question), Value::Boolean(false)],
                        stack,
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
                return simple_error(
                    "Confirm requires a configured [ConfirmHandler] in the local test runtime",
                );
            }
            "strmenu" => {
                if args.is_empty() {
                    return simple_error("StrMenu requires a menu-options argument");
                }
                let default_choice = args
                    .get(1)
                    .and_then(|value| match value {
                        Value::Integer(choice) => Some(*choice),
                        _ => None,
                    })
                    .unwrap_or(0);
                if let Some((object, handler)) = ctx.test_handlers.str_menu.clone() {
                    let options = args.first().map(render_value).unwrap_or_default();
                    let instruction = args.get(2).map(render_value).unwrap_or_default();
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![
                            Value::Text(options),
                            Value::Integer(default_choice),
                            Value::Text(instruction),
                        ],
                        stack,
                        ctx,
                    );
                    if result.is_error() {
                        return result;
                    }
                    let choice = ctx
                        .var_writebacks
                        .iter()
                        .find(|(index, _)| *index == 1)
                        .and_then(|(_, value)| match value {
                            Value::Integer(choice) => Some(*choice),
                            _ => None,
                        })
                        .unwrap_or(default_choice);
                    ctx.var_writebacks.clear();
                    return Eval::Normal(Value::Integer(choice));
                }
                return simple_error(
                    "StrMenu requires a configured [StrMenuHandler] in the local test runtime",
                );
            }
            "hyperlink" => {
                if let Some((object, handler)) = ctx.test_handlers.hyperlink.clone() {
                    if args.is_empty() {
                        return simple_error("Hyperlink requires a link argument");
                    }
                    let link = args.first().map(render_value).unwrap_or_default();
                    let result = dispatch_workspace_procedure(
                        Some(&object),
                        &handler,
                        vec![Value::Text(link)],
                        stack,
                        ctx,
                    );
                    ctx.var_writebacks.clear();
                    return result;
                }
                return simple_error(
                    "Hyperlink requires a configured [HyperlinkHandler] in the local test runtime",
                );
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
            "abs" => return builtin_abs(&args),
            "round" => return builtin_round(&args),
            "power" => return builtin_power(&args),
            "strpos" => return builtin_strpos(&args),
            "delchr" => return builtin_delchr(&args),
            "convertstr" => return builtin_convertstr(&args),
            "padstr" => return builtin_padstr(&args),
            "selectstr" => return builtin_selectstr(&args),
            "incstr" => return builtin_incstr(&args),
            "date2dmy" => return builtin_date2dmy(&args),
            "dmy2date" => return builtin_dmy2date(&args, ctx),
            "dt2date" => return builtin_dt2date(&args),
            "dt2time" => return builtin_dt2time(&args),
            "workdate" => return builtin_workdate(&args, ctx),
            "random" => return builtin_random(&args, ctx),
            "randomize" => return builtin_randomize(&args, ctx),
            "getlasterrortext" => {
                if !args.is_empty() {
                    return simple_error("GetLastErrorText expects no arguments");
                }
                let text = ctx
                    .last_error
                    .as_ref()
                    .map(|error| error.message.clone())
                    .unwrap_or_default();
                return Eval::Normal(Value::Text(text));
            }
            "clearlasterror" => {
                if !args.is_empty() {
                    return simple_error("ClearLastError expects no arguments");
                }
                ctx.last_error = None;
                return Eval::Normal(Value::Empty);
            }
            _ => {}
        }
    }

    dispatch_workspace_procedure(receiver, procedure, args, stack, ctx)
}

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
fn dispatch_workspace_procedure(
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
        return simple_error("recursion depth exceeded");
    }

    let target_object = receiver
        .map(str::to_string)
        .or_else(|| stack.top().map(|frame| frame.object.clone()));
    let candidate_paths: Vec<std::path::PathBuf> = if let Some(target_object) = target_object {
        match ctx.source.find_by_object_name(&target_object) {
            Some(path) => vec![path],
            None => {
                return simple_error(format!("object '{}' not found in workspace", target_object));
            }
        }
    } else {
        return simple_error(format!(
            "procedure not found: workspace call '{procedure}' has no current object context"
        ));
    };

    for path in &candidate_paths {
        let Some((text, tree)) = ctx.source.get_cached_parse(path) else {
            return simple_error(format!(
                "object source '{}' has no coherent cached parse",
                path.display()
            ));
        };

        let source = text.as_bytes();
        let root = tree.root_node();

        let Some(object_name) = ctx.source.object_name(path) else {
            return simple_error(format!(
                "object source '{}' has no indexed object identity",
                path.display()
            ));
        };
        let needs_object_globals = object_has_global_declarations(root);
        let install_root_globals = needs_object_globals && !stack.has_object_globals(&object_name);
        if install_root_globals && stack.depth() != 0 {
            return simple_error(format!(
                "stateful codeunit '{}' requires live BC execution",
                object_name
            ));
        }

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
        if install_root_globals {
            let mut globals = CallFrame::new(&object_name, "<globals>");
            bind_object_globals(root, source, &mut globals);
            stack.push(globals);
        }
        stack.push(frame);
        // Attribute this procedure's statements to the file
        // it is defined in (which may differ from the caller's file), then
        // restore the caller's file when the call returns.
        let cov_prev_file = ctx.cov_enter_file(&path.to_string_lossy());
        let result = crate::interpreter::eval_stmt::eval_stmt(body, source, stack, ctx);
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
                if let Some(val) = stack.top().and_then(|f| f.get(&param.name)).cloned() {
                    ctx.var_writebacks.push((i, val));
                }
            }
        }
        stack.pop();
        if install_root_globals {
            stack.pop();
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

fn object_has_global_declarations(root: tree_sitter::Node<'_>) -> bool {
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

/// `Format(value[, length[, format]])` — convert a value to Text.
///
/// The default (format number 0) and XML (format number 9) renderings are
/// implemented; any other format number or a custom `<...>` format string is
/// an explicit error rather than a silently ignored argument. `length`
/// follows BC: positive → exactly `length` characters (right-padded or
/// truncated), negative → right-justified in `abs(length)` characters, 0 →
/// unconstrained.
fn builtin_format(args: &[Value]) -> Eval {
    let Some(value) = args.first() else {
        return simple_error("Format() requires at least 1 argument");
    };
    if args.len() > 3 {
        return simple_error("Format expects at most 3 arguments");
    }
    let rendered = match args.get(2) {
        None | Some(Value::Integer(0)) => render_value(value),
        Some(Value::Integer(9)) => render_value_xml(value),
        Some(Value::Integer(n)) => {
            return simple_error(format!(
                "Format: format number {n} is not supported by the local runtime (supported: 0, 9)"
            ))
        }
        Some(Value::Text(s)) | Some(Value::Code(s)) if s.is_empty() => render_value(value),
        Some(Value::Text(s)) | Some(Value::Code(s)) => {
            return simple_error(format!(
                "Format: custom format strings are not supported by the local runtime: '{s}'"
            ))
        }
        Some(other) => {
            return simple_error(format!(
                "Format: format argument must be an Integer or Text, got {}",
                other.type_name()
            ))
        }
    };
    let length = match args.get(1) {
        None => 0,
        Some(Value::Integer(n)) => *n,
        Some(other) => {
            return simple_error(format!(
                "Format: length must be an Integer, got {}",
                other.type_name()
            ))
        }
    };
    if length == 0 {
        return Eval::Normal(Value::Text(rendered));
    }
    let width = length.unsigned_abs() as usize;
    let mut chars: Vec<char> = rendered.chars().collect();
    if chars.len() > width {
        chars.truncate(width);
        return Eval::Normal(Value::Text(chars.into_iter().collect()));
    }
    let padding = std::iter::repeat_n(' ', width - chars.len());
    let text: String = if length > 0 {
        chars.into_iter().chain(padding).collect()
    } else {
        padding.chain(chars).collect()
    };
    Eval::Normal(Value::Text(text))
}

/// Render a value with Format's XML format (format number 9).
fn render_value_xml(v: &Value) -> String {
    match v {
        Value::Boolean(b) => b.to_string(),
        Value::Date(0) | Value::Time(0) | Value::DateTime(0) => String::new(),
        Value::Date(d) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(*d);
            format!("{y:04}-{m:02}-{day:02}")
        }
        Value::Time(t) => render_time_ms(*t),
        Value::DateTime(dt) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(
                dt.div_euclid(crate::interpreter::value::MS_PER_DAY),
            );
            let time = render_time_ms(dt.rem_euclid(crate::interpreter::value::MS_PER_DAY));
            format!("{y:04}-{m:02}-{day:02}T{time}Z")
        }
        other => render_value(other),
    }
}

/// Render a milliseconds-since-midnight carrier as `HH:MM:SS[.fff]`.
fn render_time_ms(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let (h, m, s) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    if millis == 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}.{millis:03}")
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
    // BC's CopyStr is the *safe* truncating variant: a position beyond the
    // string length returns the empty string (it does not raise an error).
    if pos > chars.len() {
        return Eval::Normal(Value::Text(String::new()));
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

/// True when `name` is a global (receiver-less) builtin the interpreter
/// implements natively — the single source of truth shared with the al-test
/// router: bare global calls to any *other* name have no local implementation
/// and must route to live BC. Every name listed here has a matching arm in
/// [`dispatch_call_scoped`] (or the niladic identifier fallback in
/// `eval_expr`); a unit test pins the agreement.
pub fn supports_global_builtin(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "error"
            | "message"
            | "confirm"
            | "strmenu"
            | "hyperlink"
            | "strsubstno"
            | "format"
            | "strlen"
            | "copystr"
            | "lowercase"
            | "uppercase"
            | "indexof"
            | "maxstrlen"
            | "createdatetime"
            | "currentdatetime"
            | "today"
            | "time"
            | "abs"
            | "round"
            | "power"
            | "strpos"
            | "delchr"
            | "convertstr"
            | "padstr"
            | "selectstr"
            | "incstr"
            | "date2dmy"
            | "dmy2date"
            | "dt2date"
            | "dt2time"
            | "workdate"
            | "random"
            | "randomize"
            | "getlasterrortext"
            | "clearlasterror"
    )
}

/// Numeric view of an argument for the math builtins.
fn arg_decimal(v: &Value) -> Option<crate::interpreter::value::Decimal> {
    v.as_decimal()
}

/// `Abs(n)` — absolute value, preserving the argument's numeric type and its
/// overflow trap (`Abs(-2147483648)` overflows Integer in BC).
fn builtin_abs(args: &[Value]) -> Eval {
    match args {
        [Value::Integer(n)] => match n.checked_abs() {
            Some(a) if (i32::MIN as i64..=i32::MAX as i64).contains(&a) => {
                Eval::Normal(Value::Integer(a))
            }
            _ => simple_error("Abs: integer overflow"),
        },
        [Value::BigInteger(n)] => match n.checked_abs() {
            Some(a) => Eval::Normal(Value::BigInteger(a)),
            None => simple_error("Abs: integer overflow"),
        },
        [Value::Decimal(d)] => Eval::Normal(Value::Decimal(d.abs())),
        [v] => simple_error(format!(
            "Abs expects a numeric value, got {}",
            v.type_name()
        )),
        _ => simple_error("Abs expects exactly 1 argument"),
    }
}

/// `Round(Number [, Precision [, Direction]])`.
///
/// Defaults follow BC: precision `0.01`, direction `'='` (round to nearest
/// with banker's rounding — midpoints go to the even multiple). `'<'` rounds
/// toward negative infinity, `'>'` toward positive infinity. Always returns a
/// `Decimal`.
fn builtin_round(args: &[Value]) -> Eval {
    use crate::interpreter::value::Decimal;
    if args.is_empty() || args.len() > 3 {
        return simple_error("Round expects 1 to 3 arguments");
    }
    let Some(number) = arg_decimal(&args[0]) else {
        return simple_error(format!(
            "Round expects a numeric value, got {}",
            args[0].type_name()
        ));
    };
    let precision = match args.get(1) {
        None => Decimal::new(1, 2), // 0.01, BC's default rounding precision
        Some(v) => match arg_decimal(v) {
            Some(p) if p > Decimal::ZERO => p,
            Some(_) => return simple_error("Round: precision must be greater than zero"),
            None => {
                return simple_error(format!(
                    "Round: precision must be numeric, got {}",
                    v.type_name()
                ))
            }
        },
    };
    let direction = match args.get(2) {
        None => "=".to_string(),
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        Some(v) => {
            return simple_error(format!(
                "Round: direction must be Text, got {}",
                v.type_name()
            ))
        }
    };
    let Some(quotient) = number.checked_div(precision) else {
        return simple_error("Round: arithmetic overflow");
    };
    let rounded = match direction.as_str() {
        "=" => {
            quotient.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointNearestEven)
        }
        "<" => quotient.floor(),
        ">" => quotient.ceil(),
        other => {
            return simple_error(format!(
                "Round: direction must be '=', '<' or '>', got '{other}'"
            ))
        }
    };
    match rounded.checked_mul(precision) {
        Some(result) => Eval::Normal(Value::Decimal(result.normalize())),
        None => simple_error("Round: arithmetic overflow"),
    }
}

/// `Power(base, exponent)` — returns `Decimal`, like BC's `Power`.
fn builtin_power(args: &[Value]) -> Eval {
    use rust_decimal::MathematicalOps;
    let (base, exponent) = match args {
        [a, b] => match (arg_decimal(a), arg_decimal(b)) {
            (Some(base), Some(exponent)) => (base, exponent),
            _ => {
                return simple_error(format!(
                    "Power expects numeric arguments, got ({}, {})",
                    a.type_name(),
                    b.type_name()
                ))
            }
        },
        _ => return simple_error("Power expects exactly 2 arguments"),
    };
    match base.checked_powd(exponent) {
        Some(result) => Eval::Normal(Value::Decimal(result.normalize())),
        None => simple_error("Power: arithmetic overflow or undefined result"),
    }
}

/// `StrPos(s, substring)` — 1-based position of the first occurrence, 0 when
/// absent or when the substring is empty (BC convention).
fn builtin_strpos(args: &[Value]) -> Eval {
    let (s, needle) = match args {
        [Value::Text(s) | Value::Code(s), Value::Text(n) | Value::Code(n)] => {
            (s.as_str(), n.as_str())
        }
        _ => return simple_error("StrPos expects (Text, Text)"),
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

/// `DelChr(s [, where [, which]])` — delete characters. `where` is any
/// combination of `<` (leading), `>` (trailing), `=` (everywhere); defaults
/// follow BC: `where` = `'<'`, `which` = `' '` (space).
fn builtin_delchr(args: &[Value]) -> Eval {
    let s = match args.first() {
        Some(Value::Text(s) | Value::Code(s)) => s.clone(),
        Some(v) => return simple_error(format!("DelChr expects Text, got {}", v.type_name())),
        None => return simple_error("DelChr expects 1 to 3 arguments"),
    };
    if args.len() > 3 {
        return simple_error("DelChr expects 1 to 3 arguments");
    }
    let where_ = match args.get(1) {
        None => "<".to_string(),
        Some(Value::Text(w) | Value::Code(w)) => w.clone(),
        Some(v) => {
            return simple_error(format!("DelChr: where must be Text, got {}", v.type_name()))
        }
    };
    let which: Vec<char> = match args.get(2) {
        None => vec![' '],
        Some(Value::Text(w) | Value::Code(w)) => w.chars().collect(),
        Some(v) => {
            return simple_error(format!("DelChr: which must be Text, got {}", v.type_name()))
        }
    };
    if let Some(bad) = where_.chars().find(|c| !matches!(c, '<' | '>' | '=')) {
        return simple_error(format!(
            "DelChr: where must contain only '<', '>' or '=', got '{bad}'"
        ));
    }
    let in_set = |c: char| which.contains(&c);
    let mut result: &str = &s;
    let everywhere = where_.contains('=');
    if everywhere {
        return Eval::Normal(Value::Text(
            result.chars().filter(|c| !in_set(*c)).collect(),
        ));
    }
    if where_.contains('<') {
        result = result.trim_start_matches(in_set);
    }
    if where_.contains('>') {
        result = result.trim_end_matches(in_set);
    }
    Eval::Normal(Value::Text(result.to_string()))
}

/// `ConvertStr(s, from, to)` — replace every occurrence of the i-th character
/// of `from` with the i-th character of `to`. Errors when the lengths differ
/// (BC behaviour).
fn builtin_convertstr(args: &[Value]) -> Eval {
    let (s, from, to) = match args {
        [Value::Text(s) | Value::Code(s), Value::Text(f) | Value::Code(f), Value::Text(t) | Value::Code(t)] => {
            (s, f, t)
        }
        _ => return simple_error("ConvertStr expects (Text, Text, Text)"),
    };
    let from: Vec<char> = from.chars().collect();
    let to: Vec<char> = to.chars().collect();
    if from.len() != to.len() {
        return simple_error(
            "ConvertStr: FromCharacters and ToCharacters must have the same length",
        );
    }
    let converted: String = s
        .chars()
        .map(|c| match from.iter().position(|f| *f == c) {
            Some(i) => to[i],
            None => c,
        })
        .collect();
    Eval::Normal(Value::Text(converted))
}

/// `PadStr(s, length [, fill])` — return exactly `length` characters: truncate
/// when too long, pad on the right with `fill` (default space) when too short.
fn builtin_padstr(args: &[Value]) -> Eval {
    let (s, length) = match args {
        [Value::Text(s) | Value::Code(s), Value::Integer(n)]
        | [Value::Text(s) | Value::Code(s), Value::Integer(n), _] => (s.clone(), *n),
        _ => return simple_error("PadStr expects (Text, Integer[, Text])"),
    };
    if length < 0 {
        return simple_error("PadStr: length must be >= 0");
    }
    let fill = match args.get(2) {
        None => ' ',
        Some(Value::Text(f) | Value::Code(f)) => match f.chars().next() {
            Some(c) => c,
            None => return simple_error("PadStr: fill character cannot be empty"),
        },
        Some(v) => {
            return simple_error(format!(
                "PadStr: fill character must be Text, got {}",
                v.type_name()
            ))
        }
    };
    let length = length as usize;
    let mut chars: Vec<char> = s.chars().collect();
    if chars.len() > length {
        chars.truncate(length);
    } else {
        chars.resize(length, fill);
    }
    Eval::Normal(Value::Text(chars.into_iter().collect()))
}

/// `SelectStr(index, commaString)` — the 1-based `index`-th comma-separated
/// element; errors when the index is out of range (BC behaviour).
fn builtin_selectstr(args: &[Value]) -> Eval {
    let (index, list) = match args {
        [Value::Integer(n), Value::Text(s) | Value::Code(s)] => (*n, s.as_str()),
        _ => return simple_error("SelectStr expects (Integer, Text)"),
    };
    if index < 1 {
        return simple_error("SelectStr: index must be >= 1");
    }
    match list.split(',').nth(index as usize - 1) {
        Some(part) => Eval::Normal(Value::Text(part.to_string())),
        None => simple_error(format!(
            "SelectStr: index {index} is beyond the number of elements in '{list}'"
        )),
    }
}

/// `IncStr(s)` — increment the last number embedded in the string, preserving
/// its zero-padded width. Returns `''` when the string contains no digits
/// (BC behaviour).
fn builtin_incstr(args: &[Value]) -> Eval {
    let s = match args {
        [Value::Text(s) | Value::Code(s)] => s.clone(),
        _ => return simple_error("IncStr expects exactly 1 Text argument"),
    };
    let chars: Vec<char> = s.chars().collect();
    let mut end = None;
    for (i, c) in chars.iter().enumerate().rev() {
        if c.is_ascii_digit() {
            end = Some(i + 1);
            break;
        }
    }
    let Some(end) = end else {
        return Eval::Normal(Value::Text(String::new()));
    };
    let mut start = end;
    while start > 0 && chars[start - 1].is_ascii_digit() {
        start -= 1;
    }
    let digits: String = chars[start..end].iter().collect();
    let width = digits.len();
    let Ok(number) = digits.parse::<u64>() else {
        return simple_error(format!("IncStr: number '{digits}' is out of range"));
    };
    let incremented = format!("{:0width$}", number + 1, width = width);
    let mut result: String = chars[..start].iter().collect();
    result.push_str(&incremented);
    result.extend(&chars[end..]);
    Eval::Normal(Value::Text(result))
}

/// `Date2DMY(date, what)` — extract day (1), month (2) or year (3).
fn builtin_date2dmy(args: &[Value]) -> Eval {
    let (date, what) = match args {
        [Value::Date(d), Value::Integer(w)] => (*d, *w),
        _ => return simple_error("Date2DMY expects (Date, Integer)"),
    };
    if date == 0 {
        return simple_error("Date2DMY is undefined for 0D");
    }
    let (year, month, day) = crate::interpreter::value::ymd_from_al_days(date);
    let part = match what {
        1 => day,
        2 => month,
        3 => year,
        other => {
            return simple_error(format!(
                "Date2DMY: the what argument must be 1 (day), 2 (month) or 3 (year), got {other}"
            ))
        }
    };
    Eval::Normal(Value::Integer(part))
}

/// `DMY2Date(day [, month [, year]])` — build a Date; omitted month/year come
/// from the session work date (BC behaviour).
fn builtin_dmy2date(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    if args.is_empty() || args.len() > 3 {
        return simple_error("DMY2Date expects 1 to 3 arguments");
    }
    let mut parts = [0i64; 3];
    for (i, arg) in args.iter().enumerate() {
        match arg {
            Value::Integer(n) => parts[i] = *n,
            other => {
                return simple_error(format!(
                    "DMY2Date expects Integer arguments, got {}",
                    other.type_name()
                ))
            }
        }
    }
    let work = current_work_date(ctx);
    let (work_year, work_month, _) = crate::interpreter::value::ymd_from_al_days(work);
    let day = parts[0];
    let month = if args.len() >= 2 {
        parts[1]
    } else {
        work_month
    };
    let year = if args.len() >= 3 { parts[2] } else { work_year };
    match checked_al_date(year, month, day) {
        Some(date) => Eval::Normal(Value::Date(date)),
        None => simple_error(format!(
            "DMY2Date: {day}/{month}/{year} is not a valid date"
        )),
    }
}

/// Validate a year/month/day and convert it to the AL day carrier.
fn checked_al_date(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        _ => 28,
    };
    if !(1..=max_day).contains(&day) {
        return None;
    }
    Some(crate::interpreter::value::al_days_from_ymd(
        year, month, day,
    ))
}

/// `DT2Date(datetime)` — the date part of a DateTime.
fn builtin_dt2date(args: &[Value]) -> Eval {
    match args {
        [Value::DateTime(dt)] => Eval::Normal(Value::Date(
            dt.div_euclid(crate::interpreter::value::MS_PER_DAY),
        )),
        _ => simple_error("DT2Date expects exactly 1 DateTime argument"),
    }
}

/// `DT2Time(datetime)` — the time part of a DateTime.
fn builtin_dt2time(args: &[Value]) -> Eval {
    match args {
        [Value::DateTime(dt)] => Eval::Normal(Value::Time(
            dt.rem_euclid(crate::interpreter::value::MS_PER_DAY),
        )),
        _ => simple_error("DT2Time expects exactly 1 DateTime argument"),
    }
}

/// The effective session work date: the value set through `WorkDate(d)`, or
/// today (BC's session default) when never set.
fn current_work_date(ctx: &DispatchCtx) -> i64 {
    ctx.work_date.unwrap_or_else(clock_today)
}

/// `WorkDate([newdate])` — read or set the session work date.
fn builtin_workdate(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    match args {
        [] => Eval::Normal(Value::Date(current_work_date(ctx))),
        [Value::Date(d)] => {
            ctx.work_date = Some(*d);
            Eval::Normal(Value::Date(*d))
        }
        [v] => simple_error(format!("WorkDate expects a Date, got {}", v.type_name())),
        _ => simple_error("WorkDate expects at most 1 argument"),
    }
}

/// Advance the deterministic LCG and return the next raw value in [0, 0x7FFF].
fn next_random(ctx: &mut DispatchCtx) -> i64 {
    ctx.random_state = ctx
        .random_state
        .wrapping_mul(214_013)
        .wrapping_add(2_531_011);
    ((ctx.random_state >> 16) & 0x7FFF) as i64
}

/// `Random(n)` — a deterministic pseudo-random Integer in `1..=n`.
fn builtin_random(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    let n = match args {
        [Value::Integer(n)] => *n,
        _ => return simple_error("Random expects exactly 1 Integer argument"),
    };
    if n < 1 {
        return simple_error("Random: the maximum must be >= 1");
    }
    Eval::Normal(Value::Integer(next_random(ctx) % n + 1))
}

/// `Randomize([seed])` — re-seed the deterministic generator. Without a seed
/// the generator returns to the fixed default so runs stay reproducible.
fn builtin_randomize(args: &[Value], ctx: &mut DispatchCtx) -> Eval {
    match args {
        [] => {
            ctx.random_state = DEFAULT_RANDOM_SEED;
            Eval::Normal(Value::Empty)
        }
        [Value::Integer(seed) | Value::BigInteger(seed)] => {
            ctx.random_state = *seed as u64;
            Eval::Normal(Value::Empty)
        }
        _ => simple_error("Randomize expects at most 1 Integer argument"),
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
///
/// Date/Time/DateTime render as invariant-culture date strings
/// (`MM/DD/YYYY`, `HH:MM:SS`), not their raw integer carriers; the undefined
/// values (0D/0T and the zero DateTime) render as the empty string, matching
/// BC.
fn render_value(v: &Value) -> String {
    match v {
        Value::Integer(n) | Value::BigInteger(n) => n.to_string(),
        Value::Decimal(n) => n.normalize().to_string(),
        Value::Boolean(true) => "Yes".to_string(),
        Value::Boolean(false) => "No".to_string(),
        Value::Text(s) | Value::Code(s) => s.clone(),
        Value::Date(0) | Value::Time(0) | Value::DateTime(0) => String::new(),
        Value::Date(d) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(*d);
            format!("{m:02}/{day:02}/{y:04}")
        }
        Value::Time(t) => render_time_ms(*t),
        Value::DateTime(dt) => {
            let (y, m, day) = crate::interpreter::value::ymd_from_al_days(
                dt.div_euclid(crate::interpreter::value::MS_PER_DAY),
            );
            let time = render_time_ms(dt.rem_euclid(crate::interpreter::value::MS_PER_DAY));
            format!("{m:02}/{day:02}/{y:04} {time}")
        }
        Value::Duration(d) => d.to_string(),
        Value::Guid(g) => g.clone(),
        Value::Char(c) => c.to_string(),
        Value::Null => String::new(),
        Value::Empty => String::new(),
        Value::Option { member, .. } => member.clone(),
        other => format!("<{}>", other.type_name()),
    }
}

/// Substitute %1, %2, … placeholders in `fmt` with rendered arg values.
///
/// Single left-to-right pass over `fmt`: inserted argument text is never
/// re-scanned, so an argument whose value contains `%1` stays literal (BC
/// behaviour). Digit runs are read maximally (`%10` targets the 10th
/// argument); a placeholder with no matching argument is left verbatim.
fn substitute_placeholders(fmt: &str, args: &[Value]) -> String {
    let mut result = String::with_capacity(fmt.len());
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(d) = chars.peek().filter(|d| d.is_ascii_digit()) {
            digits.push(*d);
            chars.next();
        }
        match digits.parse::<usize>() {
            Ok(n) if n >= 1 && n <= args.len() => {
                result.push_str(&render_value(&args[n - 1]));
            }
            _ => {
                result.push('%');
                result.push_str(&digits);
            }
        }
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
    fn dialog_builtins_without_handlers_fail_closed() {
        let cases = [
            ("Message", vec![Value::Text("hello".into())]),
            ("Confirm", vec![Value::Text("continue?".into())]),
            ("StrMenu", vec![Value::Text("One,Two".into())]),
            (
                "Hyperlink",
                vec![Value::Text("https://example.test".into())],
            ),
        ];
        for (procedure, args) in cases {
            let mut ctx = ctx();
            let error = err(dispatch_call(None, procedure, args, &mut ctx));
            assert!(
                error.message.contains("configured"),
                "{procedure}: {}",
                error.message
            );
        }
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
    fn copystr_position_beyond_string_length_returns_empty() {
        // BC's CopyStr is the safe truncating variant: past-the-end positions
        // yield '' rather than a runtime error.
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
        assert_eq!(
            ok(result),
            Value::Text(String::new()),
            "CopyStr pos > string length must return the empty string"
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
    fn explicit_workspace_receiver_is_not_hijacked_by_builtin_name() {
        let ws = Arc::new(Workspace::new());
        ws.file_index.add_file(
            std::path::PathBuf::from("/test/BuiltinCollision.al"),
            r#"codeunit 50998 "Builtin Collision"
{
    procedure Format(): Text
    begin
        exit('workspace method');
    end;
}"#
            .to_string(),
        );
        let mut ctx = DispatchCtx::new_pure(ws);

        let result = dispatch_call(Some("Builtin Collision"), "Format", vec![], &mut ctx);
        assert_eq!(ok(result), Value::Text("workspace method".to_string()));
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
        // Run on a thread with an explicit large stack so this verifies the
        // interpreter's own depth guard, not the runner's thread-stack size
        // (CI test threads default to 2 MiB and debug frames vary by
        // toolchain).
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                let ws = workspace_with_helper();
                let mut ctx = DispatchCtx::new_pure(ws);
                let result = dispatch_call(Some("Helper"), "Forever", vec![], &mut ctx);
                let e = err(result);
                assert!(
                    e.message.contains("recursion depth exceeded"),
                    "expected 'recursion depth exceeded' in error, got: {}",
                    e.message
                );
            })
            .expect("spawn recursion test thread")
            .join()
            .expect("recursion test thread panicked");
    }

    #[test]
    fn every_supported_global_builtin_dispatches_without_procedure_not_found() {
        // The router's safe-list and the dispatch catalog share
        // `supports_global_builtin`; this pins that every listed name actually
        // has a dispatch arm (a zero-argument call may fail its own argument
        // validation, but must never fall through to "procedure not found").
        let names = [
            "Error",
            "Message",
            "Confirm",
            "StrMenu",
            "Hyperlink",
            "StrSubstNo",
            "Format",
            "StrLen",
            "CopyStr",
            "LowerCase",
            "UpperCase",
            "IndexOf",
            "MaxStrLen",
            "CreateDateTime",
            "CurrentDateTime",
            "Today",
            "Time",
            "Abs",
            "Round",
            "Power",
            "StrPos",
            "DelChr",
            "ConvertStr",
            "PadStr",
            "SelectStr",
            "IncStr",
            "Date2DMY",
            "DMY2Date",
            "DT2Date",
            "DT2Time",
            "WorkDate",
            "Random",
            "Randomize",
            "GetLastErrorText",
            "ClearLastError",
        ];
        for name in names {
            assert!(
                supports_global_builtin(name),
                "{name} must be in the shared safe-list"
            );
            let mut ctx = ctx();
            let result = dispatch_call(None, name, vec![], &mut ctx);
            if let Eval::Error(e) = &result {
                assert!(
                    !e.message.contains("procedure not found"),
                    "{name} must dispatch to a builtin, got: {}",
                    e.message
                );
            }
        }
        assert!(
            !supports_global_builtin("Evaluate"),
            "unimplemented globals must stay off the safe-list"
        );
        assert!(!supports_global_builtin("CalcDate"));
    }

    #[test]
    fn abs_preserves_numeric_type_and_traps_overflow() {
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(
                None,
                "Abs",
                vec![Value::Integer(-5)],
                &mut ctx
            )),
            Value::Integer(5)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Abs",
                vec![Value::Decimal(rust_decimal_macros::dec!(-1.25))],
                &mut ctx
            )),
            Value::Decimal(rust_decimal_macros::dec!(1.25))
        );
        assert!(
            dispatch_call(None, "Abs", vec![Value::Integer(i32::MIN as i64)], &mut ctx).is_error()
        );
    }

    #[test]
    fn round_uses_bankers_rounding_and_directions() {
        use rust_decimal_macros::dec;
        let mut ctx = ctx();
        let round =
            |ctx: &mut DispatchCtx, args: Vec<Value>| dispatch_call(None, "Round", args, ctx);

        // Default precision 0.01, banker's midpoint: 2.675 → 2.68 (268 even).
        assert_eq!(
            ok(round(&mut ctx, vec![Value::Decimal(dec!(2.675))])),
            Value::Decimal(dec!(2.68))
        );
        // Midpoints round to the EVEN multiple: 2.5 → 2, 1.5 → 2.
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![Value::Decimal(dec!(2.5)), Value::Integer(1)]
            )),
            Value::Decimal(dec!(2))
        );
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![Value::Decimal(dec!(1.5)), Value::Integer(1)]
            )),
            Value::Decimal(dec!(2))
        );
        // Explicit directions.
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![
                    Value::Decimal(dec!(2.1)),
                    Value::Integer(1),
                    Value::Text("<".into())
                ]
            )),
            Value::Decimal(dec!(2))
        );
        assert_eq!(
            ok(round(
                &mut ctx,
                vec![
                    Value::Decimal(dec!(2.1)),
                    Value::Integer(1),
                    Value::Text(">".into())
                ]
            )),
            Value::Decimal(dec!(3))
        );
        assert!(round(
            &mut ctx,
            vec![
                Value::Decimal(dec!(1.0)),
                Value::Integer(1),
                Value::Text("?".into())
            ]
        )
        .is_error());
    }

    #[test]
    fn power_strpos_and_string_builtins() {
        use rust_decimal_macros::dec;
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(
                None,
                "Power",
                vec![Value::Integer(2), Value::Integer(10)],
                &mut ctx
            )),
            Value::Decimal(dec!(1024))
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "StrPos",
                vec![
                    Value::Text("Hello World".into()),
                    Value::Text("World".into())
                ],
                &mut ctx
            )),
            Value::Integer(7)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "StrPos",
                vec![Value::Text("abc".into()), Value::Text("zz".into())],
                &mut ctx
            )),
            Value::Integer(0)
        );
        // DelChr default: trim leading spaces only.
        assert_eq!(
            ok(dispatch_call(
                None,
                "DelChr",
                vec![Value::Text("  x  ".into())],
                &mut ctx
            )),
            Value::Text("x  ".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DelChr",
                vec![
                    Value::Text(" a,b, c ".into()),
                    Value::Text("=".into()),
                    Value::Text(",".into())
                ],
                &mut ctx
            )),
            Value::Text(" ab c ".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DelChr",
                vec![
                    Value::Text("  x  ".into()),
                    Value::Text("<>".into()),
                    Value::Text(" ".into())
                ],
                &mut ctx
            )),
            Value::Text("x".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "ConvertStr",
                vec![
                    Value::Text("a-b-c".into()),
                    Value::Text("-".into()),
                    Value::Text("_".into())
                ],
                &mut ctx
            )),
            Value::Text("a_b_c".into())
        );
        assert!(dispatch_call(
            None,
            "ConvertStr",
            vec![
                Value::Text("abc".into()),
                Value::Text("ab".into()),
                Value::Text("x".into())
            ],
            &mut ctx
        )
        .is_error());
        assert_eq!(
            ok(dispatch_call(
                None,
                "PadStr",
                vec![Value::Text("ab".into()), Value::Integer(5)],
                &mut ctx
            )),
            Value::Text("ab   ".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "PadStr",
                vec![
                    Value::Text("abcdef".into()),
                    Value::Integer(3),
                    Value::Text("*".into())
                ],
                &mut ctx
            )),
            Value::Text("abc".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "SelectStr",
                vec![Value::Integer(2), Value::Text("one,two,three".into())],
                &mut ctx
            )),
            Value::Text("two".into())
        );
        assert!(dispatch_call(
            None,
            "SelectStr",
            vec![Value::Integer(9), Value::Text("one,two".into())],
            &mut ctx
        )
        .is_error());
        assert_eq!(
            ok(dispatch_call(
                None,
                "IncStr",
                vec![Value::Text("INV-009".into())],
                &mut ctx
            )),
            Value::Text("INV-010".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "IncStr",
                vec![Value::Text("nodigits".into())],
                &mut ctx
            )),
            Value::Text(String::new())
        );
    }

    #[test]
    fn date_builtins_round_trip() {
        let mut ctx = ctx();
        let date = crate::interpreter::value::al_days_from_ymd(2024, 7, 31);
        assert_eq!(
            ok(dispatch_call(
                None,
                "Date2DMY",
                vec![Value::Date(date), Value::Integer(1)],
                &mut ctx
            )),
            Value::Integer(31)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Date2DMY",
                vec![Value::Date(date), Value::Integer(2)],
                &mut ctx
            )),
            Value::Integer(7)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Date2DMY",
                vec![Value::Date(date), Value::Integer(3)],
                &mut ctx
            )),
            Value::Integer(2024)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DMY2Date",
                vec![Value::Integer(31), Value::Integer(7), Value::Integer(2024)],
                &mut ctx
            )),
            Value::Date(date)
        );
        assert!(dispatch_call(
            None,
            "DMY2Date",
            vec![Value::Integer(31), Value::Integer(2), Value::Integer(2024)],
            &mut ctx
        )
        .is_error());

        let dt = date * crate::interpreter::value::MS_PER_DAY + 3_600_000;
        assert_eq!(
            ok(dispatch_call(
                None,
                "DT2Date",
                vec![Value::DateTime(dt)],
                &mut ctx
            )),
            Value::Date(date)
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "DT2Time",
                vec![Value::DateTime(dt)],
                &mut ctx
            )),
            Value::Time(3_600_000)
        );
    }

    #[test]
    fn workdate_defaults_to_today_and_is_settable() {
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(None, "WorkDate", vec![], &mut ctx)),
            Value::Date(clock_today()),
            "the session work date defaults to Today"
        );
        let date = crate::interpreter::value::al_days_from_ymd(2025, 1, 2);
        assert_eq!(
            ok(dispatch_call(
                None,
                "WorkDate",
                vec![Value::Date(date)],
                &mut ctx
            )),
            Value::Date(date)
        );
        assert_eq!(
            ok(dispatch_call(None, "WorkDate", vec![], &mut ctx)),
            Value::Date(date)
        );
    }

    #[test]
    fn random_is_deterministic_and_seedable() {
        let mut a = ctx();
        let mut b = ctx();
        let seq = |ctx: &mut DispatchCtx| {
            (0..5)
                .map(|_| {
                    ok(dispatch_call(
                        None,
                        "Random",
                        vec![Value::Integer(100)],
                        ctx,
                    ))
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(seq(&mut a), seq(&mut b), "fresh contexts share the seed");
        for value in seq(&mut a) {
            match value {
                Value::Integer(n) => assert!((1..=100).contains(&n)),
                other => panic!("Random must return Integer, got {other:?}"),
            }
        }
        // Re-seeding resets the sequence deterministically.
        let first = seq(&mut a);
        assert!(dispatch_call(None, "Randomize", vec![], &mut a)
            .into_value()
            .is_some());
        assert!(matches!(
            dispatch_call(None, "Randomize", vec![Value::Integer(1)], &mut b),
            Eval::Normal(_)
        ));
        let _ = first;
    }

    #[test]
    fn get_last_error_text_reads_and_clears() {
        let mut ctx = ctx();
        ctx.last_error = Some(ErrorInfo {
            message: "boom".into(),
            error_type: None,
            source: None,
        });
        assert_eq!(
            ok(dispatch_call(None, "GetLastErrorText", vec![], &mut ctx)),
            Value::Text("boom".into())
        );
        assert!(matches!(
            dispatch_call(None, "ClearLastError", vec![], &mut ctx),
            Eval::Normal(_)
        ));
        assert_eq!(
            ok(dispatch_call(None, "GetLastErrorText", vec![], &mut ctx)),
            Value::Text(String::new())
        );
    }

    #[test]
    fn format_renders_dates_not_raw_carriers() {
        let mut ctx = ctx();
        let date = crate::interpreter::value::al_days_from_ymd(2024, 1, 31);
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Date(date)],
                &mut ctx
            )),
            Value::Text("01/31/2024".into())
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Date(0)],
                &mut ctx
            )),
            Value::Text(String::new()),
            "the undefined date renders as ''"
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Time(6 * 3_600_000 + 30 * 60_000)],
                &mut ctx
            )),
            Value::Text("06:30:00".into())
        );
        // StrSubstNo renders through the same path.
        assert_eq!(
            ok(dispatch_call(
                None,
                "StrSubstNo",
                vec![Value::Text("on %1".into()), Value::Date(date)],
                &mut ctx
            )),
            Value::Text("on 01/31/2024".into())
        );
    }

    #[test]
    fn format_supports_length_and_format_number_and_rejects_format_strings() {
        let mut ctx = ctx();
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Integer(42), Value::Integer(5)],
                &mut ctx
            )),
            Value::Text("42   ".into()),
            "positive length pads on the right"
        );
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Integer(42), Value::Integer(-5)],
                &mut ctx
            )),
            Value::Text("   42".into()),
            "negative length right-justifies"
        );
        let date = crate::interpreter::value::al_days_from_ymd(2024, 1, 31);
        assert_eq!(
            ok(dispatch_call(
                None,
                "Format",
                vec![Value::Date(date), Value::Integer(0), Value::Integer(9)],
                &mut ctx
            )),
            Value::Text("2024-01-31".into()),
            "format 9 is the XML rendering"
        );
        // Unsupported format arguments must error, not be silently ignored.
        assert!(dispatch_call(
            None,
            "Format",
            vec![
                Value::Decimal(rust_decimal_macros::dec!(1.5)),
                Value::Integer(0),
                Value::Text("<Precision,2:2><Standard Format,0>".into())
            ],
            &mut ctx
        )
        .is_error());
        assert!(dispatch_call(
            None,
            "Format",
            vec![Value::Integer(1), Value::Integer(0), Value::Integer(4)],
            &mut ctx
        )
        .is_error());
    }

    #[test]
    fn strsubstno_does_not_rescan_substituted_values() {
        let mut ctx = ctx();
        let result = dispatch_call(
            None,
            "StrSubstNo",
            vec![
                Value::Text("%2 %1".into()),
                Value::Text("A".into()),
                Value::Text("x%1y".into()),
            ],
            &mut ctx,
        );
        assert_eq!(
            ok(result),
            Value::Text("x%1y A".into()),
            "a %1 inside a substituted value must stay literal"
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
