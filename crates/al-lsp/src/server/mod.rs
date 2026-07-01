//! LSP / daemon transport layer.
//!
//! Lives in the `al-lsp` crate, whose binary entry point is
//! `crates/al-lsp/src/bin/al-lsp.rs`.
//!
//! `crate::queries::*` returns transport-agnostic types; this module is the
//! only place in the crate that imports `tower_lsp::lsp_types::*` (the
//! transport-boundary coding rule).

mod commands;
mod completions;
pub mod daemon;
pub mod dap_mode;
pub mod definition;
pub mod diagnostics;
pub mod formatting;
pub mod handlers;
pub mod hover;
pub mod lsp;
pub mod mcp;
pub mod workspace;

pub use lsp::{run_lsp, AlServer};
