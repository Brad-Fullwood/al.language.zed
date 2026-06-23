//! AL interpreter — Phase 2 of the test engine.
//!
//! Walks the existing tree-sitter parse tree and evaluates AL constructs
//! directly, with no .NET runtime dependency. Phase 2a covers pure-logic
//! tests (no DB / HTTP / UI ops); Phase 3 will add the mock BC runtime.

pub mod dispatch;
pub mod eval_expr;
pub mod eval_stmt;
pub mod scope;
pub mod value;

#[cfg(test)]
mod tests_adversarial_wave2;

pub use dispatch::{DispatchCtx, DispatchMode};
pub use scope::{CallFrame, Eval, ScopeStack};
pub use value::{ErrorInfo, RecordValue, Value};
