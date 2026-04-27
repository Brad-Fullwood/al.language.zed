//! al-core: Central engine for AL language analysis.
//!
//! All state, queries, and orchestration live here. `al-lsp`, `al-cli`, and `al-explorer`
//! (via the daemon) all depend on this crate for business logic.
//! Analysis libraries (al-syntax, al-symbols, al-semantic) are standalone dependencies.

pub mod bc_client;
pub mod build;
pub mod config;
pub mod dap;
pub mod documents;
pub mod errors;
pub mod file_index;
pub mod generators;
pub(crate) mod http_auth;
pub mod insight;
pub mod launch;
pub mod native_debug;
pub mod parsing;
pub mod permissions;
pub mod profiling;
pub mod project;
pub mod publish;
pub mod queries;
pub(crate) mod resolution;
pub mod scaffold;
pub mod semantic;
pub mod snapshot;
pub mod symbols;
pub mod syntax;
pub mod test_runner;
pub mod toolchain;
pub mod workspace;
pub mod xliff;

// ---------------------------------------------------------------------------
// Re-exports for al-lsp (thin transport layer should not depend on analysis libs)
// ---------------------------------------------------------------------------

/// LSP-bridge helpers that connect `al_core::syntax` types to `tower_lsp::lsp_types`.
///
/// These live at this module path (rather than inside `al_core::syntax`) so the
/// syntax module stays free of `tower_lsp` references — preserving the option to
/// lift it back out if ever needed, and matching the transport-boundary coding rule.
pub mod syntax_lsp {
    use crate::syntax::types::SyntaxPosition;

    /// Convert a tree-sitter Range to an LSP Range.
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

    /// Convert a `tower_lsp::lsp_types::Position` to a `SyntaxPosition`.
    #[inline]
    pub fn lsp_pos_to_syntax(pos: tower_lsp::lsp_types::Position) -> SyntaxPosition {
        SyntaxPosition {
            line: pos.line,
            character: pos.character,
        }
    }
}

/// Re-exports from al-semantic for al-lsp consumption.
pub mod semantic_types {
    pub use al_semantic::{
        AnalyzeRequest, CompileResult, CompletionItem, DiagnosticEntry, SemanticBridge,
        SemanticError,
    };
    pub mod cache {
        pub use al_semantic::cache::{read_builtins, read_error_codes};
    }
}

/// Re-exports from tree-sitter for al-lsp test code.
#[cfg(test)]
pub mod ts {
    pub use tree_sitter::{Point, Range, Tree};
}
