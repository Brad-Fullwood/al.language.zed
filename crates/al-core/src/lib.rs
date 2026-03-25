//! al-core: Central engine for AL language analysis.
//!
//! All state, queries, and orchestration live here. Only `al-lsp` imports this crate.
//! Analysis libraries (al-syntax, al-symbols, al-semantic) are standalone dependencies.

pub mod bc_client;
pub mod build;
pub mod config;
pub mod documents;
pub mod errors;
pub mod file_index;
pub mod insight;
pub mod launch;
pub mod native_debug;
pub mod parsing;
pub mod permissions;
pub mod profiling;
pub mod project;
pub mod publish;
pub mod queries;
pub(crate) mod http_auth;
pub(crate) mod resolution;
pub mod generators;
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
        AlParser, ParseResult, SyntaxError,
        format_al, format_range, FormatOptions, BraceStyle, KeywordCasing, BlankLinesBetweenProcedures,
        lint, lint_rules, LintDiagnostic, LintRuleInfo, LintSeverity,
        find_object_declaration, ts_range_to_lsp, sort_members,
    };
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
        AnalyzeRequest, CompileResult, CompletionItem, DiagnosticEntry,
        SemanticBridge,
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
