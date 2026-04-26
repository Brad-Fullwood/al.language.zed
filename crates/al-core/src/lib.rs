//! al-core: Central engine for AL language analysis.
//!
//! All state, queries, and orchestration live here. `al-lsp`, `al-cli`, and `al-explorer`
//! (via the daemon) all depend on this crate for business logic.
//! Analysis libraries (al-syntax, al-symbols, al-semantic) are standalone dependencies.

pub mod bc_client;
pub mod build;
pub mod config;
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
pub mod test_runner;
pub mod toolchain;
pub mod workspace;
pub mod xliff;

// ---------------------------------------------------------------------------
// Re-exports for al-lsp (thin transport layer should not depend on analysis libs)
// ---------------------------------------------------------------------------

/// Re-exports from al-syntax for al-lsp consumption.
pub mod syntax {
    pub use al_syntax::{
        find_object_declaration, format_al, format_range, lint, lint_rules, sort_members,
        ts_range_to_syntax, AlParser, BlankLinesBetweenProcedures, BraceStyle, FormatOptions,
        KeywordCasing, LintDiagnostic, LintRuleInfo, LintSeverity, ParseResult, SyntaxError,
    };

    /// Convert a tree-sitter Range to an LSP Range.
    ///
    /// This helper lives in al-core (a transport-aware crate) because converting to
    /// `tower_lsp::lsp_types::Range` is a transport-layer concern. al-syntax only
    /// converts to its own `SyntaxRange`; al-core bridges the gap here.
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

    /// Convert a `tower_lsp::lsp_types::Position` to an `al_syntax::types::SyntaxPosition`.
    ///
    /// This bridge exists in al-core because the orphan rule prevents implementing
    /// `From<tower_lsp::lsp_types::Position> for al_syntax::types::SyntaxPosition` in al-syntax
    /// (al-syntax no longer depends on tower-lsp) and in al-core (both types are external).
    #[inline]
    pub fn lsp_pos_to_syntax(
        pos: tower_lsp::lsp_types::Position,
    ) -> al_syntax::types::SyntaxPosition {
        al_syntax::types::SyntaxPosition {
            line: pos.line,
            character: pos.character,
        }
    }

    pub mod complexity {
        pub use al_syntax::complexity::{compute_complexity, ProcedureComplexity};
    }
    pub mod context {
        pub use al_syntax::context::{detect_context, CompletionContext};
    }
    pub mod tokens {
        pub mod token_types {
            pub use al_syntax::tokens::token_types::LEGEND;
        }
        pub mod token_modifiers {
            pub use al_syntax::tokens::token_modifiers::LEGEND;
        }
    }
}

/// Re-exports from al-symbols for al-lsp consumption.
pub mod symbols {
    pub use al_symbols::{
        EnumValueSymbol, FieldSymbol, MethodSymbol, ObjectKind, ParameterSymbol, SymbolEntry,
        SymbolIndex,
    };
    pub mod cache {
        pub use al_symbols::cache::SymbolCache;
    }
    pub mod virtual_file {
        pub use al_symbols::virtual_file::{app_has_source, cache_dir};
    }
    pub mod bc_server {
        pub use al_symbols::bc_server::{AuthMethod, BcServerClient, MessageSink};
    }
    pub mod nuget {
        pub use al_symbols::nuget::{AppDependency, NuGetClient, NuGetFeed};
    }
    pub mod oauth {
        pub use al_symbols::oauth::{acquire_token, token_cache_path};
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
