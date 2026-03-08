//! AL syntax layer — tree-sitter parsing, AST navigation, formatting, lint.

pub mod parser;
pub mod navigation;
pub mod formatting;
pub mod lint;
pub mod symbols;
pub mod tokens;
pub mod folding;

pub use parser::{AlParser, ParseResult, SyntaxError};
pub use formatting::{format_al, FormatOptions};
pub use lint::{lint, LintDiagnostic, LintSeverity};
pub use symbols::extract_document_symbols;
pub use tokens::{extract_semantic_tokens, SemanticToken};
pub use folding::extract_folding_ranges;
pub use navigation::{
    find_node_at_position, find_object_declaration, find_procedure_at,
    find_variable_references, ObjectInfo, ProcedureInfo, ParameterInfo,
};

/// Convert a tree-sitter Range to an LSP Range.
pub fn ts_range_to_lsp(range: &tree_sitter::Range) -> tower_lsp::lsp_types::Range {
    tower_lsp::lsp_types::Range {
        start: tower_lsp::lsp_types::Position {
            line: range.start_point.row as u32,
            character: range.start_point.column as u32,
        },
        end: tower_lsp::lsp_types::Position {
            line: range.end_point.row as u32,
            character: range.end_point.column as u32,
        },
    }
}
