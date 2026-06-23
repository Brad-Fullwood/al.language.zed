//! `al-explorer` CLI command implementations that talk to the al-lsp daemon.
//!
//! This was a single ~2965-line `lsp.rs`; it is now split into cohesive
//! submodules grouped by command family. Every `pub fn cmd_*` is re-exported
//! here so `cli::run` keeps calling `lsp::cmd_<name>(...)` unchanged. Private
//! per-family helpers stay inside their submodule.

pub mod env;
pub mod language;
pub mod project;
pub mod quality;
pub mod query;
pub mod refactor;
pub mod reports;
pub mod tests;

pub use env::*;
pub use language::*;
pub use project::*;
pub use quality::*;
pub use query::*;
pub use refactor::*;
pub use reports::*;
pub use tests::*;
