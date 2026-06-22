//! al-core: Central engine for AL language analysis.
//!
//! Owns all state, queries, and orchestration. The `al-lsp` binary lives at
//! `src/bin/al-lsp.rs` inside this crate; `al-explorer` consumes it through
//! the daemon over `al-protocol`.
//!
//! Internal layers are organised as modules — `syntax`, `symbols`, `semantic`,
//! `dap`, `server`, `queries`, `insight` — not standalone crates. The legacy
//! `al-syntax` / `al-symbols` / `al-semantic` / `al-cli` / `al-dap-client`
//! crate names are gone (consolidated April 2026).

// Business Central client layer is the standalone `al-bc` crate; re-export its
// modules so existing crate::{bc_client,http_auth,launch,profiling,snapshot}::…
// paths keep resolving.
pub use al_bc::{bc_client, http_auth, launch, profiling, snapshot};
pub mod build;
pub mod config;
pub mod dap;
pub mod documents;
pub mod emit;
pub mod errors;
pub mod file_index;
pub mod generators;
pub mod insight;
pub mod native_debug;
pub mod parsing;
pub mod permissions;
pub mod project;
pub mod publish;
pub mod queries;
pub(crate) mod resolution;
pub mod scaffold;
pub mod semantic;
pub mod server;
pub mod symbols;
// The syntax layer is now the standalone `al-syntax` crate; alias it as
// `crate::syntax` so existing `crate::syntax::…` paths keep resolving.
pub use al_syntax as syntax;
pub mod test_engine;
pub mod test_runner;
pub mod test_runtime;
pub mod test_snapshots;
pub mod toolchain;
pub mod workspace;
pub mod xliff;

// Re-exports for al-lsp (thin transport layer should not depend on analysis libs)

/// LSP-bridge helpers: kept outside `al_core::syntax` so the syntax module stays free of `tower_lsp` references.
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
pub mod ts {
    pub use tree_sitter::{Point, Range, Tree};
}
