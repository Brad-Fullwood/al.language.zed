//! AL interpreter — Phase 2 of the test engine.
//!
//! Walks the existing tree-sitter parse tree and evaluates AL constructs
//! directly, with no .NET runtime dependency. Phase 2a covers pure-logic
//! tests (no DB / HTTP / UI ops); Phase 3 will add the mock BC runtime.
//!
//! Module map:
//!  - `value`      Value enum (stable contract — see file docs)
//!  - `scope`      CallFrame, ScopeStack, Eval
//!  - `eval_expr`  expression evaluation
//!  - `eval_stmt`  statement evaluation
//!  - `dispatch`   procedure dispatch (intra-file + cross-codeunit)

pub mod eval_expr;
pub mod scope;
pub mod value;

pub use scope::{CallFrame, Eval, ScopeStack};
pub use value::{ErrorInfo, RecordValue, Value};
