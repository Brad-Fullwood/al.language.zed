//! al-lsp: the language-server binary crate (LSP / daemon / MCP / DAP modes).
//!
//! The engine itself is split across standalone crates (`al-syntax`,
//! `al-symbols`, `al-semantic`, `al-analysis`, `al-insight`, `al-emit`,
//! `al-compile`, `al-runtime`, `al-workspace`, `al-project`, `al-bc`,
//! `al-source`, `al-dap`, and `al-publish`; see `Docs/architecture.md` for the
//! full layering. This crate hosts the `al-lsp` binary, while `al-explorer`
//! communicates with it through the `al-protocol` daemon transport.

pub mod semantic;
pub mod server;
pub mod toolchain;

// Re-exports for al-lsp (thin transport layer should not depend on analysis libs)

/// LSP-bridge helpers: kept outside `al_syntax` so the syntax module stays free of `tower_lsp` references.
pub mod syntax_lsp {
    pub fn ts_range_to_lsp(
        range: &tree_sitter::Range,
        source: &[u8],
    ) -> tower_lsp::lsp_types::Range {
        let sr = al_syntax::ts_range_to_syntax(range, source);
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
