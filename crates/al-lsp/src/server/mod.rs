//! LSP / daemon transport layer.
//!
//! Folded into al-core in stage 7 of the crate consolidation; previously the
//! standalone `al-lsp` crate. The binary entry point lives at
//! `crates/al-core/src/bin/al-lsp.rs`.
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
