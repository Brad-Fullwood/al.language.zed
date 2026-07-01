//! al-lsp: the language-server binary crate (LSP / daemon / MCP / DAP modes).
//!
//! The engine itself is split across standalone crates (`al-syntax`,
//! `al-symbols`, `al-semantic`, `al-analysis`, `al-insight`, `al-emit`,
//! `al-compile`, `al-runtime`, `al-workspace`, `al-project`, `al-bc`,
//! `al-source`, `al-dap`, `al-publish` — see `Docs/01-architecture.md` for the
//! full layering). This crate hosts the `al-lsp` binary
//! (`src/bin/al-lsp.rs`) and re-exports those crates under their pre-split
//! module names below (`crate::syntax`, `crate::symbols`, `crate::build`,
//! …) so internal call sites written before the crate split keep resolving
//! without a large mechanical rename. `al-explorer` talks to this binary
//! through the daemon over `al-protocol`.

// Business Central client layer is the standalone `al-bc` crate; re-export its
// modules so existing crate::{bc_client,http_auth,launch,profiling,snapshot}::…
// paths keep resolving.
pub use al_bc::{bc_client, http_auth, launch, profiling, snapshot};
pub use al_symbols as symbols;
// Project discovery, workspace config, and the error hierarchy now live in the
// standalone `al-project` crate; re-export so crate::{project,config,errors}::…
// keep resolving. (`toolchain` stays a thin al-lsp facade module — see
// src/toolchain.rs — that re-exports al_project::toolchain::* and parks the
// Workspace-coupled `doctor()`.)
pub use al_dap::{dap, native_debug};
pub use al_project::{config, errors, project};
// Keeps crate::dap::… and crate::native_debug::… resolving for call sites
// written before al-dap was split out.
pub use al_analysis::{generators, permissions, queries, resolution, scaffold, xliff};
pub use al_compile as build;
pub use al_emit as emit;
pub use al_insight as insight;
pub use al_publish as publish;
pub use al_runtime as test_runtime;
pub use al_source::{documents, file_index, parsing};
pub use al_workspace as workspace;
pub mod semantic;
pub mod server;
// The syntax layer is now the standalone `al-syntax` crate; alias it as
// `crate::syntax` so existing `crate::syntax::…` paths keep resolving.
pub use al_syntax as syntax;
pub mod toolchain;

// Re-exports for al-lsp (thin transport layer should not depend on analysis libs)

/// LSP-bridge helpers: kept outside `crate::syntax` so the syntax module stays free of `tower_lsp` references.
pub mod syntax_lsp {
    pub fn ts_range_to_lsp(
        range: &tree_sitter::Range,
        source: &[u8],
    ) -> tower_lsp::lsp_types::Range {
        let sr = crate::syntax::ts_range_to_syntax(range, source);
        tower_lsp::lsp_types::Range {
            start: tower_lsp::lsp_types::Position::new(sr.start.line, sr.start.character),
            end: tower_lsp::lsp_types::Position::new(sr.end.line, sr.end.character),
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)] // crate re-export facades follow (pre-split module names re-exported for compatibility)
pub mod ts {
    pub use tree_sitter::{Point, Range, Tree};
}

pub use al_test as test_engine;
pub use al_test::test_runner;

pub use al_snapshot as test_snapshots;
