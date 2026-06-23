//! al-runtime: pure-Rust AL test runtime.
//!
//! Phase 2 ships the interpreter (pure-logic tests). Phase 3 fills in the
//! mock BC runtime (records, filters, FlowFields, stubs). Phase 4+ adds the
//! offline DAP server. None of this code transitively requires the .NET CLR
//! or a live BC instance.
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
