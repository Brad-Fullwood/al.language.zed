//! AL syntax layer — tree-sitter parsing, AST navigation, formatting, lint.

pub mod language_data;
pub mod parser;
pub mod navigation;
pub mod formatting;
pub mod lint;
pub mod symbols;
pub mod tokens;
pub mod folding;
pub mod type_resolver;
pub mod context;
pub mod complexity;
pub mod sort;
pub mod traversal;

pub use parser::{AlParser, ParseResult, SyntaxError};
pub use formatting::{format_al, format_range, FormatOptions, KeywordCasing, BlankLinesBetweenProcedures, BraceStyle};
pub use sort::sort_members;
pub use lint::{lint, lint_rules, LintDiagnostic, LintRuleInfo, LintSeverity};
pub use symbols::extract_document_symbols;
pub use tokens::{extract_semantic_tokens, SemanticToken};
pub use folding::extract_folding_ranges;
pub use navigation::{
    find_node_at_position, find_object_declaration, find_procedure_at,
    find_variable_references, find_call_references, ObjectInfo, ProcedureInfo, ParameterInfo,
};
pub use type_resolver::{TypeResolver, VariableDecl, VariableScope, object_kind_to_al_type};
pub use context::{detect_context, extract_last_identifier, find_call_context, CompletionContext};
pub use traversal::{walk_tree, walk_tree_until};

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

// ── Shared AST helpers ───────────────────────────────────────────────

/// Extract text from a node, stripping surrounding `"` quotes, returning `None` on empty.
///
/// This is the canonical quote-stripping helper used across the crate.
/// Use `node_text_or` when a fallback string is needed instead of `None`.
pub fn node_text_clean(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let text = node.utf8_text(source).ok()?;
    let clean = text.trim_matches('"').trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

/// Extract text from a node, stripping surrounding `"` quotes, returning `fallback` on empty.
pub fn node_text_or(node: tree_sitter::Node, source: &[u8], fallback: &str) -> String {
    node_text_clean(node, source).unwrap_or_else(|| fallback.to_string())
}

/// Extract the object name from an `object_declaration` node.
///
/// The grammar does not assign a field name to the object name, so we scan
/// children looking for `identifier`, `quoted_identifier`, `string`, `name`,
/// or `name_or_keyword` nodes and return the first non-empty, unquoted value.
pub fn extract_object_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        match c.kind() {
            "identifier" | "quoted_identifier" | "string" | "name" | "name_or_keyword" => {
                if let Some(name) = node_text_clean(c, source) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Count net occurrences of `open` minus `close` delimiters on `line`.
///
/// Characters inside single-quoted string literals are skipped so that
/// delimiter characters inside strings do not affect the count.
///
/// Common uses:
/// - `count_net_delimiters(line, '(', ')')` — net parentheses
/// - `count_net_delimiters(line, '{', '}')` — net braces
pub fn count_net_delimiters(line: &str, open: char, close: char) -> i32 {
    let mut depth = 0i32;
    let mut in_string = false;
    for ch in line.chars() {
        if ch == '\'' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
        }
    }
    depth
}

/// Return the previous *named* sibling of `node`, skipping anonymous/punctuation nodes.
pub fn prev_named_sibling(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    let mut sibling = node.prev_sibling()?;
    loop {
        if sibling.is_named() {
            return Some(sibling);
        }
        sibling = sibling.prev_sibling()?;
    }
}

/// Return `true` if any ancestor of `node` has the given node kind.
pub fn has_ancestor_kind(node: tree_sitter::Node, kind: &str) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == kind {
            return true;
        }
        current = parent.parent();
    }
    false
}

/// Walk ancestor nodes from `node`'s parent upward, returning the first node
/// for which `predicate` returns `true`, or `None` if the root is reached.
pub fn find_ancestor(
    node: tree_sitter::Node,
    predicate: impl Fn(tree_sitter::Node) -> bool,
) -> Option<tree_sitter::Node> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if predicate(parent) {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}

/// Return the text of a single source line by zero-based `row` index.
///
/// Returns an empty string if `row` is out of range or the bytes are not valid UTF-8.
/// Uses `splitn` to avoid scanning past the requested line.
pub fn get_source_line(source: &[u8], row: usize) -> &str {
    source
        .splitn(row + 2, |&b| b == b'\n')
        .nth(row)
        .and_then(|b| std::str::from_utf8(b).ok())
        .unwrap_or("")
}

/// Convert a tree-sitter Range to an LSP Range.
///
/// `source` must be the complete source bytes of the file so that tree-sitter byte-offset
/// columns (`point.column`) can be converted to LSP UTF-16 code unit columns correctly.
pub fn ts_range_to_lsp(range: &tree_sitter::Range, source: &[u8]) -> tower_lsp::lsp_types::Range {
    let get_line = |row: usize| -> &str { get_source_line(source, row) };

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
