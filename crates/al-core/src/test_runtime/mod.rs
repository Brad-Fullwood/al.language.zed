//! Pure-Rust AL test runtime.
//!
//! Phase 2 ships the interpreter (pure-logic tests). Phase 3 fills in
//! the mock BC runtime (records, filters, FlowFields, stubs). Phase 4+
//! adds the offline DAP server. None of this code transitively requires
//! the .NET CLR or a live BC instance.

pub mod interpreter;
pub mod mock;
pub mod stubs;

pub use interpreter::{CallFrame, Eval, ScopeStack, Value};
