//! AL syntax layer — tree-sitter parsing, AST navigation, formatting, lint.

pub mod parser;
pub mod navigation;
pub mod formatting;
pub mod lint;
pub mod symbols;
pub mod tokens;
pub mod folding;
pub mod type_resolver;
pub mod context;

pub use parser::{AlParser, ParseResult, SyntaxError};
pub use formatting::{format_al, FormatOptions};
pub use lint::{lint, lint_rules, LintDiagnostic, LintRuleInfo, LintSeverity};
pub use symbols::extract_document_symbols;
pub use tokens::{extract_semantic_tokens, SemanticToken};
pub use folding::extract_folding_ranges;
pub use navigation::{
    find_node_at_position, find_object_declaration, find_procedure_at,
    find_variable_references, ObjectInfo, ProcedureInfo, ParameterInfo,
};
pub use type_resolver::{TypeResolver, VariableDecl, VariableScope, object_kind_to_al_type};
pub use context::{detect_context, extract_last_identifier, find_call_context, CompletionContext};

/// Convert a byte-offset column (as produced by tree-sitter) within a UTF-8 line to a
/// UTF-16 code unit column (as required by the LSP specification).
///
/// Tree-sitter `point.column` values are **byte offsets** within the line.  LSP `character`
/// values are **UTF-16 code unit** offsets.  They differ whenever the line contains characters
/// outside the Basic Multilingual Plane (emoji, supplementary CJK, etc.).
pub fn byte_col_to_utf16_col(line: &str, byte_col: usize) -> u32 {
    let clamped = byte_col.min(line.len());
    let mut utf16 = 0u32;
    let mut byte_pos = 0usize;
    for ch in line.chars() {
        if byte_pos >= clamped {
            break;
        }
        utf16 += ch.len_utf16() as u32;
        byte_pos += ch.len_utf8();
    }
    utf16
}

/// Convert a UTF-16 code unit column (as supplied by LSP) to a byte offset within `line`.
/// If `utf16_col` is past the end of the string, the byte length of `line` is returned
/// (clamp-to-end semantics).
pub fn utf16_col_to_byte_offset(line: &str, utf16_col: usize) -> usize {
    let mut remaining = utf16_col;
    for (byte_idx, ch) in line.char_indices() {
        if remaining == 0 {
            return byte_idx;
        }
        remaining = remaining.saturating_sub(ch.len_utf16());
    }
    line.len()
}

/// Convert a tree-sitter Range to an LSP Range.
///
/// `source` must be the complete source bytes of the file so that tree-sitter byte-offset
/// columns (`point.column`) can be converted to LSP UTF-16 code unit columns correctly.
pub fn ts_range_to_lsp(range: &tree_sitter::Range, source: &[u8]) -> tower_lsp::lsp_types::Range {
    // Extract the source line, then convert byte column to UTF-16 column.
    let get_line = |row: usize| -> &str {
        // Split on newlines and take the nth line.  The `splitn` limit keeps allocation minimal.
        source
            .splitn(row + 2, |&b| b == b'\n')
            .nth(row)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    };

    let start_line = get_line(range.start_point.row);
    let end_line = if range.end_point.row == range.start_point.row {
        start_line
    } else {
        get_line(range.end_point.row)
    };

    tower_lsp::lsp_types::Range {
        start: tower_lsp::lsp_types::Position {
            line: range.start_point.row as u32,
            character: byte_col_to_utf16_col(start_line, range.start_point.column),
        },
        end: tower_lsp::lsp_types::Position {
            line: range.end_point.row as u32,
            character: byte_col_to_utf16_col(end_line, range.end_point.column),
        },
    }
}
