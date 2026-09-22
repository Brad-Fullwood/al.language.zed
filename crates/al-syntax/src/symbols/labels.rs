//! Label declarations, recovered by scanning the `var` section text.
//!
//! The grammar does not give labels a node of their own, so they are found
//! by masking comments and strings out of the section text and reading what
//! is left.

use crate::ts_range_to_syntax as ts_range_to_lsp;
use crate::types::{
    SyntaxDocumentSymbol as DocumentSymbol, SyntaxRange, SyntaxSymbolKind as SymbolKind,
};
use tree_sitter::Node;

pub(super) fn collect_label_symbols_from_text(
    node: Node,
    source: &[u8],
    symbols: &mut Vec<DocumentSymbol>,
) {
    let Ok(section_text) = node.utf8_text(source) else {
        return;
    };

    let mut seen: std::collections::HashSet<String> = symbols
        .iter()
        .filter(|symbol| symbol.kind == SymbolKind::Variable)
        .map(|symbol| symbol.name.to_lowercase())
        .collect();

    let mut in_block_comment = false;
    for (offset, line) in section_text.lines().enumerate() {
        // Mask comments and string literals (offset-preserving) so a
        // commented-out `// MyLbl: Label 'disabled';` or a trailing `// …`
        // comment cannot produce a bogus label symbol. `"…"` quoted
        // identifiers stay visible: a quoted label name (`"My Lbl": Label …`)
        // must survive for name extraction.
        let (masked, still_in_block_comment) =
            crate::lint::mask_non_code_keep_quoted_identifiers(line, in_block_comment);
        in_block_comment = still_in_block_comment;

        let trimmed = masked.trim();
        if !trimmed.ends_with(';') {
            continue;
        }
        let Some((name_part, rest)) = split_label_name(trimmed) else {
            continue;
        };
        if !rest.trim_start().starts_with("Label ") && !rest.trim_start().starts_with("Label\t") {
            continue;
        }

        let name_part = name_part.trim();
        let Some(name) = crate::clean_identifier_text(name_part) else {
            continue;
        };
        if !seen.insert(name.to_lowercase()) {
            continue;
        }

        let line_no = node.start_position().row as u32 + offset as u32;
        // `masked` preserves byte offsets, so the position found there is
        // valid in the original `line` too.
        let Some(start_byte) = masked.find(name_part) else {
            continue;
        };
        let start_col = crate::byte_col_to_utf16_col(line, start_byte);
        let end_col = crate::byte_col_to_utf16_col(line, start_byte + name_part.len());
        symbols.push(DocumentSymbol {
            name,
            detail: Some("Label".to_string()),
            kind: SymbolKind::Variable,
            range: ts_range_to_lsp(&node.range(), source),
            selection_range: SyntaxRange {
                start: crate::types::SyntaxPosition {
                    line: line_no,
                    character: start_col,
                },
                end: crate::types::SyntaxPosition {
                    line: line_no,
                    character: end_col,
                },
            },
            children: None,
        });
    }
}

/// Split a masked `Name: Label …;` line into name part and remainder.
///
/// A plain identifier splits at the first `:`. A `"…"` quoted identifier may
/// legally contain `:` (and doubled `""` escapes), so the quoted form scans to
/// the closing quote first and then requires the `:` separator after it.
fn split_label_name(trimmed: &str) -> Option<(&str, &str)> {
    if !trimmed.starts_with('"') {
        return trimmed.split_once(':');
    }
    let bytes = trimmed.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                i += 2;
                continue;
            }
            break;
        }
        i += 1;
    }
    if i >= bytes.len() {
        // Unterminated quoted identifier.
        return None;
    }
    let name_end = i + 1;
    let rest = trimmed[name_end..].trim_start().strip_prefix(':')?;
    Some((&trimmed[..name_end], rest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::test_support::*;
    use crate::types::SyntaxSymbolKind as SymbolKind;
    use crate::AlParser;

    #[test]
    fn test_label_symbols_extracted_from_var_section() {
        let src = r#"codeunit 50100 Test
{
    var
        ErrMsg: Label 'Something went wrong';
        InfoTxt: Label 'All good';
        RecVar: Record Customer;
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"ErrMsg"), "got {:?}", names);
        assert!(names.contains(&"InfoTxt"), "got {:?}", names);
        assert!(names.contains(&"RecVar"), "got {:?}", names);

        let err = children.iter().find(|c| c.name == "ErrMsg").unwrap();
        assert_eq!(err.kind, SymbolKind::Variable);
    }

    #[test]
    fn test_quoted_label_name_with_comment_survives_masking() {
        // `"…"` quoted identifiers used to be masked away like string
        // literals, so a quoted label name lost its text and the symbol was
        // dropped. The comment on the same line must still be masked.
        let src = r#"codeunit 50100 Test
{
    var
        "My Lbl": Label 'text'; // MyFake: Label 'from a comment';
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(
            !names.contains(&"MyFake"),
            "commented-out label must stay masked: {names:?}"
        );

        let lbl = children
            .iter()
            .find(|c| c.name == "My Lbl")
            .unwrap_or_else(|| panic!("quoted label name must produce a symbol: {names:?}"));
        assert_eq!(lbl.kind, SymbolKind::Variable);
        assert_eq!(lbl.detail.as_deref(), Some("Label"));

        // The selection range covers the quoted identifier (including quotes)
        // on the declaration line.
        assert_eq!(lbl.selection_range.start.line, 3);
        assert_eq!(lbl.selection_range.start.character, 8);
        assert_eq!(
            lbl.selection_range.end.character,
            8 + "\"My Lbl\"".len() as u32
        );
    }

    #[test]
    fn test_label_text_scan_fallback_keeps_quoted_names() {
        // Regression: the text-scan fallback masked `"…"` quoted identifiers
        // like string literals, so a quoted label name was lost and the
        // symbol dropped. Exercise the fallback directly (the grammar path is
        // bypassed) with a quoted name plus a comment on the same line.
        let src = r#"codeunit 50100 Test
{
    var
        "My Lbl": Label 'text'; // MyFake: Label 'from a comment';
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(src);
        let mut symbols = Vec::new();
        collect_label_symbols_from_text(result.tree.root_node(), src.as_bytes(), &mut symbols);

        let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            !names.contains(&"MyFake"),
            "commented-out label must stay masked: {names:?}"
        );
        let lbl = symbols
            .iter()
            .find(|s| s.name == "My Lbl")
            .unwrap_or_else(|| panic!("quoted label name must survive the fallback: {names:?}"));
        assert_eq!(lbl.detail.as_deref(), Some("Label"));
        // Selection range covers the quoted identifier on its line.
        assert_eq!(lbl.selection_range.start.line, 3);
        assert_eq!(lbl.selection_range.start.character, 8);
        assert_eq!(
            lbl.selection_range.end.character,
            8 + "\"My Lbl\"".len() as u32
        );
    }

    #[test]
    fn test_label_not_duplicated_when_already_a_variable() {
        // collect_label_symbols_from_text dedupes against existing Variable
        // symbols (case-insensitive). A label parsed by the grammar must not be
        // emitted a second time by the text-scan fallback.
        let src = r#"codeunit 50100 Test
{
    var
        GreetingLbl: Label 'Hello';
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let count = children.iter().filter(|c| c.name == "GreetingLbl").count();
        assert_eq!(count, 1, "label must appear exactly once, not duplicated");
    }

    #[test]
    fn test_commented_out_label_produces_no_symbol() {
        // The label text-scan must skip comment lines: a commented-out
        // declaration used to produce a bogus symbol named "// DisabledLbl".
        let src = r#"codeunit 50100 Test
{
    var
        // DisabledLbl: Label 'disabled';
        /* BlockLbl: Label 'also disabled'; */
        ActiveLbl: Label 'active'; // TrailingLbl: Label 'nope';
}"#;
        let symbols = parse_symbols(src);
        let children = symbols[0].children.as_ref().expect("var children");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.contains(&"ActiveLbl"),
            "real label must survive: {names:?}"
        );
        assert!(
            !names
                .iter()
                .any(|n| n.contains("DisabledLbl") || n.contains("BlockLbl")),
            "commented-out labels must not become symbols: {names:?}"
        );
    }
}
