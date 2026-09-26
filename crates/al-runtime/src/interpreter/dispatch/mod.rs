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

//!
//! The call path lives in [`routing`] and [`workspace_procedure`], the local
//! and global bindings a call frame needs in [`frames`], and the inline
//! builtins in [`dialog`], [`text`], [`numeric`], [`datetime`], [`random`] and
//! [`render`]. This file holds the dispatch context those all take.

pub mod datetime;
pub mod dialog;
pub mod events;
pub mod frames;
pub mod numeric;
pub mod picture;
pub mod random;
pub mod render;
pub mod routing;
pub mod table_code;
pub mod text;
pub mod workspace_procedure;

#[cfg(test)]
pub(crate) mod test_support;

pub use frames::{bind_object_globals, bind_procedure_locals};
pub use routing::{dispatch_call, supports_global_builtin};
pub use workspace_procedure::object_declaration_named;

pub(crate) use datetime::{clock_current_datetime, clock_time, clock_today};
pub(crate) use frames::declared_text_length;
pub(crate) use render::{render_value, substitute_placeholders_with};
pub(crate) use routing::dispatch_call_scoped;

use std::collections::HashMap;
use std::sync::Arc;

use crate::interpreter::records::RecordStore;
use crate::interpreter::value::{ErrorInfo, Value};

/// Stack size for the thread an interpreted AL body runs on.
///
/// Each interpreted call level is a dispatch→eval_stmt→eval_expr native frame
/// cluster costing tens of KiB in debug builds. A tokio blocking worker or a
/// test thread gives 2 MiB, which is what held the call cap at 48 levels —
/// shallower than a BOM explosion or a recursive chart-of-accounts total, so
/// those failed locally and passed on BC. Callers that interpret AL spawn a
/// thread of this size and the caps below are sized against it.
pub const INTERP_STACK_BYTES: usize = 64 * 1024 * 1024;

/// Maximum simultaneous AL call frames. Sized against [`INTERP_STACK_BYTES`]:
/// 48 frames fitted 2 MiB, so 512 leaves several times that margin inside
/// 64 MiB while being deeper than any AL algorithm that recurses over data.
const MAX_RECURSION_DEPTH: usize = 512;

/// Maximum syntactic nesting depth `eval_stmt` will descend into before
/// aborting with an error. The counter is cumulative across nested
/// procedure calls (a call chain stacks ~4 AST levels per frame), so the
/// cap must exceed what `MAX_RECURSION_DEPTH` (512 × ~4 = 2048) can reach
/// via call recursion alone — that way an infinite-call test trips the
/// call cap first (clearer error message) and only truly pathological
/// single-procedure nesting trips this AST cap.
pub const MAX_AST_DEPTH: usize = 2560;

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
    /// iteration. The wrapper source is an `Arc<str>` so cache hits share it
    /// instead of reallocating the string.
    #[doc(hidden)]
    pub expr_fragment_cache: HashMap<String, (Arc<str>, tree_sitter::Tree)>,
    /// The workspace's event subscribers, indexed on first raise.
    #[doc(hidden)]
    pub event_subscribers: Option<Arc<events::SubscriberIndex>>,
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
            event_subscribers: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interpreter::dispatch::test_support::workspace_with_helper;

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
