//! Scopes, call frames, and the interpreter execution stack.
//!
//! The interpreter is a tree-walker over the existing tree-sitter parse
//! tree (`al_syntax::AlParser` output). Each procedure invocation
//! pushes a `CallFrame`; `ScopeStack` is the active list of frames.
//! Variable lookup walks up from the innermost frame through enclosing
//! procedure / object scopes.

use std::collections::HashMap;

use crate::interpreter::value::{
    add_held_bytes, check_held_bytes, hold_bytes, owned_bytes, release_held_bytes, ErrorInfo, Value,
};

/// One activation record on the interpreter stack.
///
/// The text and array elements its variables hold count toward the test's
/// [`MAX_HELD_BYTES`](crate::interpreter::value::MAX_HELD_BYTES) until the
/// variable takes another value or the frame drops.
#[derive(Debug)]
pub struct CallFrame {
    /// Procedure name (for stack traces / DAP).
    pub procedure: String,
    /// Containing object name (codeunit / page / report).
    pub object: String,
    /// Locally-bound variables (parameters + `var` section).
    pub locals: HashMap<String, Value>,
    /// Declared capacities for `Text[N]`/`Code[N]` variables. Runtime string
    /// values intentionally remain plain strings; MaxStrLen consults this
    /// parallel type metadata through the scope stack.
    pub declared_text_lengths: HashMap<String, usize>,
    /// Source location of the call site (file + line) for stack traces.
    /// `None` for the synthetic top frame.
    pub call_site: Option<(String, u32)>,
    /// Table code: bare names that are not variables are fields of `Rec`,
    /// and bare record methods (`Modify()`, `TestField(...)`) act on it.
    pub implicit_record: bool,
    /// For a codeunit's globals frame, the instance the globals belong to.
    pub instance: Option<u64>,
    /// The bytes this frame's variables have added to the test's total.
    held: usize,
}

/// A copy of a frame holds copies of its values, and counts them again.
impl Clone for CallFrame {
    fn clone(&self) -> Self {
        add_held_bytes(self.held);
        Self {
            procedure: self.procedure.clone(),
            object: self.object.clone(),
            locals: self.locals.clone(),
            declared_text_lengths: self.declared_text_lengths.clone(),
            call_site: self.call_site.clone(),
            implicit_record: self.implicit_record,
            instance: self.instance,
            held: self.held,
        }
    }
}

/// A frame that returns, or a codeunit instance's globals that go, give
/// back the bytes their variables held.
impl Drop for CallFrame {
    fn drop(&mut self) {
        release_held_bytes(self.held);
    }
}

impl CallFrame {
    pub fn new(object: impl Into<String>, procedure: impl Into<String>) -> Self {
        Self {
            procedure: procedure.into(),
            object: object.into(),
            locals: HashMap::new(),
            declared_text_lengths: HashMap::new(),
            call_site: None,
            implicit_record: false,
            instance: None,
            held: 0,
        }
    }

    /// AL identifiers are case-insensitive, so the key is lower-cased.
    ///
    /// The value's bytes count toward the test's total in place of those of
    /// the value the name held. The total is checked at the next operation
    /// that adds to it, such as [`Self::check_held_bytes`] when the frame is
    /// a call's.
    pub fn bind(&mut self, name: &str, value: Value) {
        let bytes = owned_bytes(&value);
        add_held_bytes(bytes);
        self.held = self.held.saturating_add(bytes);
        if let Some(old) = self.locals.insert(name.to_ascii_lowercase(), value) {
            release_frame_bytes(&mut self.held, owned_bytes(&old));
        }
    }

    /// Remove the variable `name` and give back the bytes it held.
    pub fn unbind(&mut self, name: &str) -> Option<Value> {
        let old = self.locals.remove(&name.to_ascii_lowercase())?;
        release_frame_bytes(&mut self.held, owned_bytes(&old));
        Some(old)
    }

    /// Refuse when this frame's variables hold bytes and the test's total
    /// is past [`MAX_HELD_BYTES`](crate::interpreter::value::MAX_HELD_BYTES).
    /// `operation` names what bound them, for the message.
    pub fn check_held_bytes(&self, operation: &str) -> Result<(), String> {
        if self.held == 0 {
            return Ok(());
        }
        check_held_bytes(operation)
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.locals.get(&name.to_ascii_lowercase())
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.locals.get_mut(&name.to_ascii_lowercase())
    }

    pub fn is_object_globals(&self) -> bool {
        self.procedure == "<globals>"
    }

    pub fn bind_declared_text_length(&mut self, name: &str, length: usize) {
        self.declared_text_lengths
            .insert(name.to_ascii_lowercase(), length);
    }
}

/// Take `bytes` off a frame's count and the test's total, at most what the
/// frame counts.
fn release_frame_bytes(held: &mut usize, bytes: usize) {
    let bytes = bytes.min(*held);
    *held -= bytes;
    release_held_bytes(bytes);
}

/// A variable and the count of the frame it lives in. A write that changes
/// the text or array elements the variable holds goes through
/// [`Slot::store`] or [`Slot::recount`], so the frame counts what the
/// variable holds.
pub struct Slot<'a> {
    value: &'a mut Value,
    held: &'a mut usize,
}

impl Slot<'_> {
    /// The variable's value.
    pub fn value(&self) -> &Value {
        self.value
    }

    /// The variable's value, for a change the caller counts with
    /// [`Self::recount`].
    pub fn value_mut(&mut self) -> &mut Value {
        self.value
    }

    /// Count `new` bytes for the variable in place of `old`, or refuse when
    /// the test's total would pass
    /// [`MAX_HELD_BYTES`](crate::interpreter::value::MAX_HELD_BYTES).
    /// `operation` names the write, for the message.
    pub fn recount(&mut self, operation: &str, old: usize, new: usize) -> Result<(), String> {
        hold_bytes(operation, new)?;
        *self.held = self.held.saturating_add(new);
        release_frame_bytes(self.held, old);
        Ok(())
    }

    /// Store `value` in the variable, counting its bytes in place of those
    /// of the value it held. A refused store leaves the variable as it was.
    pub fn store(&mut self, operation: &str, value: Value) -> Result<(), String> {
        self.recount(operation, owned_bytes(self.value), owned_bytes(&value))?;
        *self.value = value;
        Ok(())
    }
}

/// The interpreter's running stack of call frames. Newest at the back.
#[derive(Debug, Default)]
pub struct ScopeStack {
    frames: Vec<CallFrame>,
    /// Current `eval_expr` recursion depth. Expression evaluation recurses
    /// per AST nesting level and does not reset across a call, so a recursive
    /// AL procedure whose recursive call sits inside an expression — the usual
    /// `exit(1 + Walk(n - 1))` shape — spends a couple of levels per frame.
    /// Capped at `MAX_EXPR_DEPTH` so degenerate input yields `Eval::Error`
    /// rather than aborting the process on a stack overflow. Statement nesting
    /// has its own cap in `DispatchCtx`.
    expr_depth: usize,
}

/// Maximum `eval_expr` AST recursion depth. Mirrors `eval_stmt`'s
/// MAX_AST_DEPTH rationale: real AL expressions nest ~10 deep, and the cap has
/// to clear what `MAX_RECURSION_DEPTH` call frames spend on their way down, so
/// an over-deep recursion reports the call limit rather than this one. Sized
/// against `INTERP_STACK_BYTES`, like the other two caps.
pub const MAX_EXPR_DEPTH: usize = 2560;

impl ScopeStack {
    pub fn new() -> Self {
        Self {
            frames: Vec::new(),
            expr_depth: 0,
        }
    }

    /// Enter one `eval_expr` nesting level. Returns `false` when the cap is
    /// exceeded — the caller must return an error WITHOUT calling
    /// [`Self::exit_expr`] (the failed enter did not increment).
    pub fn enter_expr(&mut self) -> bool {
        if self.expr_depth >= MAX_EXPR_DEPTH {
            return false;
        }
        self.expr_depth += 1;
        true
    }

    pub fn exit_expr(&mut self) {
        self.expr_depth = self.expr_depth.saturating_sub(1);
    }

    /// Push a frame; returns its 0-indexed depth.
    pub fn push(&mut self, frame: CallFrame) -> usize {
        let idx = self.frames.len();
        self.frames.push(frame);
        idx
    }

    pub fn pop(&mut self) -> Option<CallFrame> {
        self.frames.pop()
    }

    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    pub fn top(&self) -> Option<&CallFrame> {
        self.frames.last()
    }

    pub fn top_mut(&mut self) -> Option<&mut CallFrame> {
        self.frames.last_mut()
    }

    /// The frame at `index`, as [`Self::push`] returned it.
    pub fn frame_mut(&mut self, index: usize) -> Option<&mut CallFrame> {
        self.frames.get_mut(index)
    }

    pub fn has_object_globals(&self, object: &str) -> bool {
        self.frames
            .iter()
            .any(|frame| frame.is_object_globals() && frame.object.eq_ignore_ascii_case(object))
    }

    /// The instance of `object` whose globals the running code reads: that
    /// of the innermost globals frame of `object`.
    pub fn object_instance(&self, object: &str) -> Option<u64> {
        self.frames
            .iter()
            .rev()
            .find(|frame| frame.is_object_globals() && frame.object.eq_ignore_ascii_case(object))
            .and_then(|frame| frame.instance)
    }

    pub fn lookup(&self, name: &str) -> Option<&Value> {
        let key = name.to_ascii_lowercase();
        let current = self.frames.last()?;
        current.locals.get(&key).or_else(|| {
            self.frames.iter().rev().skip(1).find_map(|frame| {
                (frame.is_object_globals() && frame.object.eq_ignore_ascii_case(&current.object))
                    .then(|| frame.locals.get(&key))
                    .flatten()
            })
        })
    }

    /// The variable `name`, for a change that leaves the text and array
    /// elements it holds as they were, such as a method call on a handle.
    /// Any other write goes through [`Self::lookup_slot_mut`].
    pub fn lookup_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.lookup_slot_mut(name).map(|slot| slot.value)
    }

    /// The variable `name` and the count of the frame it lives in: the
    /// running procedure's frame, or its object's globals.
    pub fn lookup_slot_mut(&mut self, name: &str) -> Option<Slot<'_>> {
        let key = name.to_ascii_lowercase();
        let top_index = self.frames.len().checked_sub(1)?;
        let index = if self.frames[top_index].locals.contains_key(&key) {
            top_index
        } else {
            let object = &self.frames[top_index].object;
            (0..top_index).rev().find(|index| {
                let frame = &self.frames[*index];
                frame.is_object_globals()
                    && frame.object.eq_ignore_ascii_case(object)
                    && frame.locals.contains_key(&key)
            })?
        };
        let frame = &mut self.frames[index];
        Some(Slot {
            value: frame.locals.get_mut(&key)?,
            held: &mut frame.held,
        })
    }

    pub fn declared_text_length(&self, name: &str) -> Option<usize> {
        let key = name.to_ascii_lowercase();
        let current = self.frames.last()?;
        current
            .declared_text_lengths
            .get(&key)
            .copied()
            .or_else(|| {
                self.frames.iter().rev().skip(1).find_map(|frame| {
                    (frame.is_object_globals()
                        && frame.object.eq_ignore_ascii_case(&current.object))
                    .then(|| frame.declared_text_lengths.get(&key).copied())
                    .flatten()
                })
            })
    }

    pub fn stack_trace(&self) -> Vec<String> {
        self.frames
            .iter()
            .map(|f| match &f.call_site {
                Some((file, line)) => format!("{}:{} {}::{}", file, line, f.object, f.procedure),
                None => format!("{}::{}", f.object, f.procedure),
            })
            .collect()
    }
}

/// Result of evaluating one statement / expression.
///
/// `Normal(value)` is the typical case. `Error` carries an AL `ErrorInfo`
/// emitted by `Error(...)` or a runtime fault; `asserterror` blocks catch
/// it. `Exit(value)` unwinds the current procedure with the given return
/// value (AL `exit(v)` keyword).
#[derive(Debug, Clone)]
pub enum Eval {
    Normal(Value),
    Error(ErrorInfo),
    Exit(Value),
    /// `break` — unwinds to the nearest enclosing loop, which stops iterating.
    /// Reaching a procedure body after escaping all loops is a runtime error.
    Break,
    /// `continue` — unwinds to the nearest enclosing loop, which proceeds to
    /// its next iteration. Escaping all loops is a runtime error.
    Continue,
}

impl Eval {
    /// Treat any non-Error result as a normal value, returning `None`
    /// for `Exit`. Convenient for expression contexts.
    pub fn into_value(self) -> Option<Value> {
        match self {
            Eval::Normal(v) | Eval::Exit(v) => Some(v),
            Eval::Error(_) | Eval::Break | Eval::Continue => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Eval::Error(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_are_case_insensitive() {
        let mut frame = CallFrame::new("Cu", "Proc");
        frame.bind("MyVar", Value::Integer(42));
        assert_eq!(frame.get("myvar"), Some(&Value::Integer(42)));
        assert_eq!(frame.get("MYVAR"), Some(&Value::Integer(42)));
        assert_eq!(frame.get("MyVar"), Some(&Value::Integer(42)));
    }

    #[test]
    fn missing_binding_returns_none() {
        let frame = CallFrame::new("Cu", "Proc");
        assert!(frame.get("nope").is_none());
    }

    #[test]
    fn lookup_walks_from_procedure_to_object_globals() {
        let mut stack = ScopeStack::new();
        let mut outer = CallFrame::new("Cu", "<globals>");
        outer.bind("g", Value::Integer(1));
        stack.push(outer);
        let inner = CallFrame::new("Cu", "Inner");
        stack.push(inner);

        assert_eq!(stack.lookup("g"), Some(&Value::Integer(1)));
    }

    #[test]
    fn nested_procedure_cannot_read_caller_locals() {
        let mut stack = ScopeStack::new();
        stack.push(CallFrame::new("Cu", "<globals>"));
        let mut caller = CallFrame::new("Cu", "Caller");
        caller.bind("private_local", Value::Integer(42));
        stack.push(caller);
        stack.push(CallFrame::new("Cu", "Callee"));

        assert_eq!(stack.lookup("private_local"), None);
        assert_eq!(stack.lookup_mut("private_local"), None);
    }

    #[test]
    fn procedure_cannot_read_another_objects_globals() {
        let mut stack = ScopeStack::new();
        let mut caller_globals = CallFrame::new("Caller", "<globals>");
        caller_globals.bind("shared_name", Value::Integer(42));
        stack.push(caller_globals);
        stack.push(CallFrame::new("Callee", "Run"));

        assert_eq!(stack.lookup("shared_name"), None);
    }

    #[test]
    fn shadowing_inner_wins() {
        let mut stack = ScopeStack::new();
        let mut outer = CallFrame::new("Cu", "Outer");
        outer.bind("x", Value::Integer(1));
        stack.push(outer);
        let mut inner = CallFrame::new("Cu", "Inner");
        inner.bind("x", Value::Integer(99));
        stack.push(inner);

        assert_eq!(stack.lookup("x"), Some(&Value::Integer(99)));
    }

    #[test]
    fn pop_returns_topmost() {
        let mut stack = ScopeStack::new();
        stack.push(CallFrame::new("A", "a"));
        stack.push(CallFrame::new("B", "b"));
        let popped = stack.pop().unwrap();
        assert_eq!(popped.procedure, "b");
        assert_eq!(stack.depth(), 1);
    }

    #[test]
    fn stack_trace_renders_frames() {
        let mut stack = ScopeStack::new();
        stack.push(CallFrame::new("MyCU", "Outer"));
        let mut inner = CallFrame::new("MyCU", "Inner");
        inner.call_site = Some(("src/MyCU.al".into(), 42));
        stack.push(inner);
        let trace = stack.stack_trace();
        assert_eq!(trace.len(), 2);
        assert!(trace[1].contains("src/MyCU.al:42"));
    }

    #[test]
    fn eval_into_value_handles_error_negative() {
        let err = Eval::Error(ErrorInfo {
            message: "boom".into(),
            error_type: None,
            source: None,
        });
        assert!(err.is_error());
        assert!(err.into_value().is_none());
    }

    #[test]
    fn scope_stack_1000_deep_lookup_does_not_overflow() {
        let mut stack = ScopeStack::new();
        let mut globals = CallFrame::new("Cu", "<globals>");
        globals.bind("deep_var", Value::Integer(500));
        stack.push(globals);
        for i in 1..1000_usize {
            stack.push(CallFrame::new("Cu", format!("proc_{i}")));
        }
        assert_eq!(
            stack.lookup("deep_var"),
            Some(&Value::Integer(500)),
            "must find object global beneath 999 call frames without stack overflow"
        );
        assert!(
            stack.lookup_mut("deep_var").is_some(),
            "lookup_mut must also work on 1000-frame stack"
        );
    }
}
