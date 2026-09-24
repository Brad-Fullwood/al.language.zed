//! AL syntax layer — tree-sitter parsing, AST navigation, formatting, lint.

pub mod complexity;
pub mod context;
pub mod folding;
pub mod formatting;
pub mod language_data;
mod lexical;
pub mod lint;
pub mod navigation;
pub mod parser;
pub mod sort;
pub mod source_lines;
pub mod symbols;
pub mod tokens;
pub mod traversal;
pub mod type_resolver;
pub mod types;

pub use context::{detect_context, extract_last_identifier, find_call_context, CompletionContext};
pub use folding::extract_folding_ranges;
pub use formatting::{
    format_al, format_range, BlankLinesBetweenProcedures, BraceStyle, FormatOptions, KeywordCasing,
};
pub use lint::{lint, lint_rules, LintDiagnostic, LintRuleInfo, LintSeverity};
pub use navigation::{
    collect_call_site_names, collect_call_sites, collect_member_access_names,
    collect_primary_expression_names, collect_primary_expression_names_under,
    count_call_references, count_identifier_occurrences, find_event_subscriber_attributes,
    find_event_subscriber_references, find_node_at_position, find_object_declaration,
    find_object_declarations, find_procedure_at, find_variable_references,
    find_variable_references_under, procedure_keyword_row, EventSubscriberAttribute, ObjectInfo,
    ParameterInfo, ProcedureInfo, SubscriberArgument,
};
pub use parser::{AlParser, ParseResult, SyntaxError};
pub use sort::sort_members;
pub use source_lines::{get_source_line, SourceLines};
pub use symbols::{extract_document_symbols, extract_document_symbols_under};
pub use tokens::{extract_semantic_tokens, SemanticToken};
pub use traversal::{walk_tree, walk_tree_until};
pub use type_resolver::{object_kind_to_al_type, TypeResolver, VariableDecl, VariableScope};
pub use types::{
    SyntaxDocumentSymbol, SyntaxFoldingRange, SyntaxFoldingRangeKind, SyntaxPosition, SyntaxRange,
    SyntaxSymbolKind,
};

/// Clean an attribute argument: trim, strip a leading `Type::` prefix, and strip
/// one surrounding `"`/`'` pair, unescaping the doubled quote inside it. A pure string helper shared by the navigation,
/// insight and query layers.
pub fn clean_attr_arg(s: &str) -> String {
    let s = s.trim();
    // `Codeunit::"Sales-Post"` names the object after the scope operator. A
    // quoted argument is a name in its own right, `::` inside it included.
    let s = match s.find("::") {
        Some(pos) if !s.starts_with('"') && !s.starts_with('\'') => s[pos + 2..].trim(),
        _ => s,
    };
    // One quote pair, with the doubled quote unescaped, so `"My ""Big"" Unit"`
    // matches the name al-syntax reports for that object. An unbalanced value
    // keeps the old lenient strip.
    for quote in ['"', '\''] {
        if s.len() >= 2 && s.starts_with(quote) && s.ends_with(quote) {
            let doubled: String = [quote, quote].iter().collect();
            return s[1..s.len() - 1]
                .replace(&doubled, &quote.to_string())
                .trim()
                .to_string();
        }
    }
    s.trim_matches('"').trim_matches('\'').trim().to_string()
}

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

/// Extract text from a node, stripping surrounding `"` quotes, returning `None` on empty.
///
/// This is the canonical quote-stripping helper used across the crate.
/// Use `node_text_or` when a fallback string is needed instead of `None`.
pub fn node_text_clean(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    clean_identifier_text(node.utf8_text(source).ok()?)
}

/// Clean raw identifier text: strip exactly one surrounding `"` pair (when
/// present) and unescape doubled quotes inside it, returning `None` on empty.
///
/// AL quoted identifiers escape an embedded `"` by doubling it, so
/// `"My ""X"" Field"` names the identifier `My "X" Field`. Stripping quote
/// *runs* (`trim_matches('"')`) would both leave the doubled quotes in place
/// and over-strip a name that legitimately ends in `"`.
pub fn clean_identifier_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let clean = if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].replace("\"\"", "\"")
    } else {
        trimmed.to_string()
    };
    if clean.is_empty() {
        None
    } else {
        Some(clean)
    }
}

/// [`clean_identifier_text`] with an empty string in place of `None`.
///
/// For the call sites that treat an empty name as "no name" rather than
/// branching on it.
pub fn clean_identifier(text: &str) -> String {
    clean_identifier_text(text).unwrap_or_default()
}

/// Identifier cleanup for text that did not come straight from a syntax node.
pub trait IdentifierText {
    /// Trim whitespace, drop one leading and one trailing `"`, and unescape
    /// `""` to `"`.
    ///
    /// Replaces `trim_matches('"')`, which strips quote *runs*: it left the
    /// doubled quote inside `"Cust ""Main"" Rec"` and over-stripped
    /// `"Name"""` to `Name`, so those names never matched the ones al-syntax
    /// reports. Each end is handled on its own, so text cut off mid-name while
    /// the user types (`"Sales Hea`) still loses its opening quote. Borrows
    /// when there is nothing to unescape.
    fn unquote_identifier(&self) -> std::borrow::Cow<'_, str>;
}

impl IdentifierText for str {
    fn unquote_identifier(&self) -> std::borrow::Cow<'_, str> {
        let text = self.trim();
        let text = text.strip_prefix('"').unwrap_or(text);
        let text = text.strip_suffix('"').unwrap_or(text);
        if text.contains("\"\"") {
            std::borrow::Cow::Owned(text.replace("\"\"", "\""))
        } else {
            std::borrow::Cow::Borrowed(text)
        }
    }
}

pub fn node_text_or(node: tree_sitter::Node, source: &[u8], fallback: &str) -> String {
    node_text_clean(node, source).unwrap_or_else(|| fallback.to_string())
}

/// Extract the `name` field's text (quotes/whitespace trimmed), or `fallback`
/// if the field is missing or empty.
pub fn node_name_or(node: tree_sitter::Node, source: &[u8], fallback: &str) -> String {
    node.child_by_field_name("name")
        .and_then(|n| node_text_clean(n, source))
        .unwrap_or_else(|| fallback.to_string())
}

/// Extract the object name from an `object_declaration` node.
///
/// The name is on the `name:` field as
/// `(name_or_keyword (name (identifier | quoted_identifier)))`; read it
/// directly. The positional scan is retained only as a defensive fallback for
/// any caller that passes a node without the field.
pub fn extract_object_name(node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    if let Some(name) = node
        .child_by_field_name("name")
        .and_then(|n| node_text_clean(n, source))
    {
        return Some(name);
    }
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
/// Delimiters that are not code are skipped: single-quoted string literals
/// (`'…'`, with `''` as the escape), double-quoted identifiers (`"…"` — a BC
/// name may legitimately contain `'`, `{` or `(`), `//` line comments, and
/// `/* … */` block comments.
///
/// The scan is per-line and starts outside any comment, so a block comment
/// spanning several lines is only handled up to the end of this one. Callers
/// that walk multi-line text should strip comments with their own carried
/// state before calling.
///
/// Common uses:
/// - `count_net_delimiters(line, '(', ')')` — net parentheses
/// - `count_net_delimiters(line, '{', '}')` — net braces
pub fn count_net_delimiters(line: &str, open: char, close: char) -> i32 {
    let mut depth = 0i32;
    for span in lexical::LineScanner::new(line, false) {
        if span.kind != lexical::SpanKind::Code {
            continue;
        }
        for ch in span.text.chars() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
            }
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

/// UTF-16 code unit column of the position `byte_offset` bytes into `source`,
/// given tree-sitter's byte `column` for the same position.
///
/// A tree-sitter point carries the byte offset of the position and the byte
/// column within its line, so the line begins at `byte_offset - column` and the
/// conversion only has to read that one line prefix. Reaching the line by
/// splitting the file on `\n` instead would walk from byte 0 on every call,
/// which is what made `documentSymbol` on a large table quadratic in file size.
fn utf16_col_at(source: &[u8], byte_offset: usize, column: usize) -> u32 {
    let line_start = byte_offset.saturating_sub(column);
    let Some(prefix) = source.get(line_start..byte_offset) else {
        return 0;
    };
    match std::str::from_utf8(prefix) {
        Ok(prefix) => byte_col_to_utf16_col(prefix, prefix.len()),
        // A position inside a multi-byte character has no UTF-16 column of its
        // own; the byte column is the closest honest answer.
        Err(_) => column as u32,
    }
}

/// Convert a tree-sitter Range to a transport-agnostic [`types::SyntaxRange`].
///
/// `source` must be the complete source bytes of the file so that tree-sitter byte-offset
/// columns (`point.column`) can be converted to UTF-16 code unit columns correctly.
pub fn ts_range_to_syntax(range: &tree_sitter::Range, source: &[u8]) -> types::SyntaxRange {
    types::SyntaxRange {
        start: types::SyntaxPosition {
            line: range.start_point.row as u32,
            character: utf16_col_at(source, range.start_byte, range.start_point.column),
        },
        end: types::SyntaxPosition {
            line: range.end_point.row as u32,
            character: utf16_col_at(source, range.end_byte, range.end_point.column),
        },
    }
}

#[cfg(test)]
mod range_conversion_tests {
    use super::{byte_col_to_utf16_col, get_source_line, ts_range_to_syntax};

    /// The conversion `ts_range_to_syntax` replaced: reach the line by
    /// splitting the file, then count UTF-16 units up to the byte column.
    fn by_line_scan(range: &tree_sitter::Range, source: &[u8]) -> (u32, u32) {
        let start_line = get_source_line(source, range.start_point.row);
        let end_line = get_source_line(source, range.end_point.row);
        (
            byte_col_to_utf16_col(start_line, range.start_point.column),
            byte_col_to_utf16_col(end_line, range.end_point.column),
        )
    }

    #[test]
    fn byte_arithmetic_agrees_with_a_line_scan_including_multibyte_lines() {
        let mut source = String::new();
        for row in 0..400 {
            // Emoji are outside the BMP, so the UTF-16 column differs from both
            // the byte column and the character column.
            source.push_str(&format!("    Message('café 🚀 row {row}');\n"));
        }
        let bytes = source.as_bytes();

        let parsed = crate::parser::AlParser::parse_quick(&source);
        let mut checked = 0;
        crate::walk_tree(parsed.tree.root_node(), &mut |node| {
            let range = node.range();
            let expected = by_line_scan(&range, bytes);
            let actual = ts_range_to_syntax(&range, bytes);
            assert_eq!(
                (actual.start.character, actual.end.character),
                expected,
                "{} at {:?}",
                node.kind(),
                range.start_point
            );
            checked += 1;
        });
        assert!(
            checked > 400,
            "expected a real tree, walked {checked} nodes"
        );
    }

    #[test]
    fn a_range_at_the_end_of_a_large_file_reads_only_its_own_line() {
        // The conversion cost must not grow with the number of lines before
        // the range. Ten thousand identical lines, then one range on the last:
        // a from-byte-0 scan would read the whole file for it.
        let line = "    Message('x');\n";
        let source = line.repeat(10_000);
        let bytes = source.as_bytes();
        let last_start = source.len() - line.len();

        let range = tree_sitter::Range {
            start_byte: last_start + 4,
            end_byte: last_start + 11,
            start_point: tree_sitter::Point {
                row: 9_999,
                column: 4,
            },
            end_point: tree_sitter::Point {
                row: 9_999,
                column: 11,
            },
        };

        let converted = ts_range_to_syntax(&range, bytes);

        assert_eq!(converted.start.line, 9_999);
        assert_eq!(converted.start.character, 4);
        assert_eq!(converted.end.character, 11);
    }
}

#[cfg(test)]
mod clean_identifier_tests {
    use super::clean_identifier_text;

    #[test]
    fn strips_one_quote_pair_and_unescapes_doubled_quotes() {
        assert_eq!(
            clean_identifier_text(r#""My ""X"" Field""#),
            Some(r#"My "X" Field"#.to_string())
        );
        assert_eq!(
            clean_identifier_text(r#""My Table""#),
            Some("My Table".to_string())
        );
        assert_eq!(clean_identifier_text("Plain"), Some("Plain".to_string()));
    }

    #[test]
    fn does_not_over_strip_names_ending_in_a_quote() {
        // `"Name"""` is the identifier `Name"` — trim_matches('"') used to
        // strip every trailing quote and yield `Name`.
        assert_eq!(
            clean_identifier_text(r#""Name""""#),
            Some(r#"Name""#.to_string())
        );
    }

    #[test]
    fn empty_results_are_none() {
        assert_eq!(clean_identifier_text(""), None);
        assert_eq!(clean_identifier_text(r#""""#), None);
        assert_eq!(clean_identifier_text("   "), None);
    }
}

#[cfg(test)]
mod unquote_identifier_tests {
    use super::IdentifierText;

    #[test]
    fn strips_one_quote_at_each_end_and_unescapes() {
        assert_eq!(
            "\"Cust \"\"Main\"\" Rec\"".unquote_identifier(),
            "Cust \"Main\" Rec"
        );
        assert_eq!("\"Name\"\"\"".unquote_identifier(), "Name\"");
        assert_eq!("  \"Sales Header\" ".unquote_identifier(), "Sales Header");
        assert_eq!("Customer".unquote_identifier(), "Customer");
    }

    #[test]
    fn an_unbalanced_quote_is_still_dropped() {
        assert_eq!("\"Sales Hea".unquote_identifier(), "Sales Hea");
        assert_eq!("\"\"".unquote_identifier(), "");
    }

    #[test]
    fn borrows_when_nothing_is_unescaped() {
        assert!(matches!(
            "\"Sales Header\"".unquote_identifier(),
            std::borrow::Cow::Borrowed("Sales Header")
        ));
    }
}

#[cfg(test)]
mod clean_attr_arg_tests {
    use super::clean_attr_arg;

    #[test]
    fn strips_the_scope_prefix() {
        assert_eq!(clean_attr_arg("ObjectType::Codeunit"), "Codeunit");
        assert_eq!(clean_attr_arg(" Codeunit::\"Sales-Post\" "), "Sales-Post");
    }

    #[test]
    fn unescapes_a_doubled_quote_inside_one_quote_pair() {
        assert_eq!(
            clean_attr_arg("\"My \"\"Big\"\" Codeunit\""),
            "My \"Big\" Codeunit"
        );
        assert_eq!(clean_attr_arg("'Don''t'"), "Don't");
        assert_eq!(clean_attr_arg("\"Name\"\"\""), "Name\"");
    }

    #[test]
    fn keeps_a_scope_operator_inside_a_quoted_name() {
        assert_eq!(clean_attr_arg("\"A::B\""), "A::B");
    }

    #[test]
    fn plain_and_unbalanced_values_keep_the_lenient_strip() {
        assert_eq!(clean_attr_arg("OnAfterPost"), "OnAfterPost");
        assert_eq!(clean_attr_arg("'OnAfterPost"), "OnAfterPost");
        assert_eq!(clean_attr_arg("''"), "");
    }
}

#[cfg(test)]
mod delimiter_tests {
    use super::count_net_delimiters;

    #[test]
    fn counts_plain_delimiters() {
        assert_eq!(count_net_delimiters("f(a, b)", '(', ')'), 0);
        assert_eq!(count_net_delimiters("f(a,", '(', ')'), 1);
        assert_eq!(count_net_delimiters("b);", '(', ')'), -1);
        assert_eq!(count_net_delimiters("{", '{', '}'), 1);
    }

    #[test]
    fn skips_single_quoted_string_literals() {
        assert_eq!(count_net_delimiters("Message('(')", '(', ')'), 0);
        // `''` is an escaped quote, not a terminator.
        assert_eq!(count_net_delimiters("Message('it''s (ok)')", '(', ')'), 0);
    }

    #[test]
    fn skips_double_quoted_identifiers() {
        // A BC name may contain an apostrophe. Treating `"…"` as opaque is what
        // stops the scanner from entering string state and swallowing the
        // closing paren of the enclosing call.
        assert_eq!(
            count_net_delimiters(r#"field(1; "Cust's Name"; Text[50])"#, '(', ')'),
            0
        );
        assert_eq!(count_net_delimiters(r#"x := "A{B";"#, '{', '}'), 0);
        assert_eq!(count_net_delimiters(r#"x := "A(B";"#, '(', ')'), 0);
    }

    #[test]
    fn skips_comments() {
        assert_eq!(count_net_delimiters("x := 1; // (", '(', ')'), 0);
        assert_eq!(count_net_delimiters("x := 1; /* ( */", '(', ')'), 0);
        assert_eq!(count_net_delimiters("x := 1; /* ( */ f()", '(', ')'), 0);
        assert_eq!(count_net_delimiters("{ /* } */", '{', '}'), 1);
        // Unterminated `/*` swallows the rest of the line.
        assert_eq!(count_net_delimiters("x; /* (", '(', ')'), 0);
    }
}
