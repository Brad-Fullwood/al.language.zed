//! Pure-Rust AL test runtime.
//!
//! Includes the tree-walking interpreter, in-memory record and FlowField
//! support, and native test-library stubs. It does not require the .NET CLR or
//! a live Business Central instance.
//!
//! Procedure lookup for nested workspace calls is abstracted behind
//! `al_types::ProcedureSource`; the production implementation lives in
//! `al-source` (`impl ProcedureSource for FileIndex`), which depends on this
//! crate. The crate's own unit tests use a lightweight in-crate mock (see
//! `test_support`).

pub mod interpreter;
pub mod mock;
pub mod stubs;

pub use interpreter::{CallFrame, Eval, ScopeStack, Value};

#[cfg(test)]
mod test_support;
