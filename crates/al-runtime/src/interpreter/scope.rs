//! Scopes, call frames, and the interpreter execution stack.
//!
//! The interpreter is a tree-walker over the existing tree-sitter parse
//! tree (`al_syntax::AlParser` output). Each procedure invocation
//! pushes a `CallFrame`; `ScopeStack` is the active list of frames.
//! Variable lookup walks up from the innermost frame through enclosing
//! procedure / object scopes.

use std::collections::HashMap;

use crate::interpreter::value::{ErrorInfo, Value};

/// One activation record on the interpreter stack.
#[derive(Debug, Clone)]
pub struct CallFrame {
    /// Procedure name (for stack traces / DAP).
    pub procedure: String,
    /// Containing object name (codeunit / page / report).
    pub object: String,
    /// Locally-bound variables (parameters + `var` section).
    pub locals: HashMap<String, Value>,
    /// Slot for the procedure return value, populated on `exit(value)`
    /// or by assigning to the procedure name.
    pub return_slot: Option<Value>,
    /// Source location of the call site (file + line) for stack traces.
    /// `None` for the synthetic top frame.
    pub call_site: Option<(String, u32)>,
}

impl CallFrame {
    pub fn new(object: impl Into<String>, procedure: impl Into<String>) -> Self {
        Self {
            procedure: procedure.into(),
            object: object.into(),
            locals: HashMap::new(),
            return_slot: None,
            call_site: None,
        }
    }

    /// AL identifiers are case-insensitive, so the key is lower-cased.
    pub fn bind(&mut self, name: &str, value: Value) {
        self.locals.insert(name.to_ascii_lowercase(), value);
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.locals.get(&name.to_ascii_lowercase())
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Value> {
        self.locals.get_mut(&name.to_ascii_lowercase())
    }
}

/// The interpreter's running stack of call frames. Newest at the back.
#[derive(Debug, Default)]
pub struct ScopeStack {
    frames: Vec<CallFrame>,
    /// Current `eval_expr` recursion depth. Expression evaluation recurses
    /// per AST nesting level; ~400 nested parens overflow a 2 MiB thread
    /// stack (tokio worker default) and ABORT the process. Capped at
    /// `MAX_EXPR_DEPTH` so degenerate input yields `Eval::Error` instead.
    /// Statement nesting has its own cap in `DispatchCtx`.
    expr_depth: usize,
}

/// Maximum `eval_expr` AST recursion depth. Mirrors `eval_stmt`'s
/// MAX_AST_DEPTH rationale: real AL expressions nest ~10 deep; 256 is far
/// beyond anything legitimate and comfortably inside a 2 MiB thread stack.
pub const MAX_EXPR_DEPTH: usize = 256;

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

    pub fn lookup(&self, name: &str) -> Option<&Value> {
        let key = name.to_ascii_lowercase();
        self.frames.iter().rev().find_map(|f| f.locals.get(&key))
    }

    pub fn lookup_mut(&mut self, name: &str) -> Option<&mut Value> {
        let key = name.to_ascii_lowercase();
        for frame in self.frames.iter_mut().rev() {
            if frame.locals.contains_key(&key) {
                return frame.locals.get_mut(&key);
            }
        }
        None
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
    /// Reaching a procedure body (escaping all loops) is a runtime error. (C24)
    Break,
    /// `continue` — unwinds to the nearest enclosing loop, which proceeds to
    /// its next iteration. Escaping all loops is a runtime error. (C24)
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
        // Negative: lookup of an unbound name returns None, not panic.
        let frame = CallFrame::new("Cu", "Proc");
        assert!(frame.get("nope").is_none());
    }

    #[test]
    fn lookup_walks_outer_scopes() {
        let mut stack = ScopeStack::new();
        let mut outer = CallFrame::new("Cu", "Outer");
        outer.bind("g", Value::Integer(1));
        stack.push(outer);
        let inner = CallFrame::new("Cu", "Inner");
        stack.push(inner);

        assert_eq!(stack.lookup("g"), Some(&Value::Integer(1)));
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
        // Negative: Eval::Error -> None when extracted as a value.
        let err = Eval::Error(ErrorInfo {
            message: "boom".into(),
            error_type: None,
            source: None,
        });
        assert!(err.is_error());
        assert!(err.into_value().is_none());
    }

    #[test]
    fn scope_stack_1000_deep_lookup_no_stack_overflow_adversarial_h_3() {
        // AUDIT (no bug): ScopeStack.lookup uses iterative rev().find_map,
        // not recursion — 1000 nested frames cannot cause a call-stack overflow.
        let mut stack = ScopeStack::new();
        for i in 0..1000_usize {
            let mut frame = CallFrame::new("Cu", format!("proc_{i}"));
            if i == 500 {
                frame.bind("deep_var", Value::Integer(500));
            }
            stack.push(frame);
        }
        assert_eq!(
            stack.lookup("deep_var"),
            Some(&Value::Integer(500)),
            "must find value in frame 500 of 1000 without stack overflow"
        );
        assert!(
            stack.lookup_mut("deep_var").is_some(),
            "lookup_mut must also work on 1000-frame stack"
        );
    }
}
