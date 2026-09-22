//! Table field declarations, read from the parse tree.
//!
//! A field is a node whose text begins with `field(`, which is what lets two
//! declarations on one line resolve independently. The header parses as
//! `(<id> ; <name> ; <type…>)`, so the name is the token between the two
//! semicolons and the type is everything after the second one.

use crate::queries::{Position, Range};

use super::type_text::parse_type_expr;
use super::ResolvedType;

/// Every `field(id; "Name"; Type ...)` declaration node in `tree`. A field is
/// a node whose text begins with `field(`, so two `field(...)`
/// on one line each resolve independently, unlike the old per-line text scan
/// which only ever saw the first. We stop descending once matched (the paren
/// child text begins with `(`, not `field(`, so it isn't double-counted).
pub(super) fn field_decl_nodes<'a>(
    tree: &'a tree_sitter::Tree,
    src: &[u8],
) -> Vec<tree_sitter::Node<'a>> {
    field_decl_nodes_under(tree.root_node(), src)
}

/// [`field_decl_nodes`] under one node, so a caller holding a single object
/// declaration in a multi-object file does not see the other objects' fields.
pub(super) fn field_decl_nodes_under<'a>(
    root: tree_sitter::Node<'a>,
    src: &[u8],
) -> Vec<tree_sitter::Node<'a>> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let is_field = node
            .utf8_text(src)
            .map(|t| t.trim_start().starts_with("field("))
            .unwrap_or(false);
        if is_field {
            out.push(node);
            continue;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out
}

/// Byte offset → LSP `Position` (line + UTF-16 column) within `content`.
pub(super) fn byte_to_position(content: &str, byte: usize) -> Position {
    let byte = byte.min(content.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (i, ch) in content.char_indices() {
        if i >= byte {
            break;
        }
        if ch == '\n' {
            line += 1;
            line_start = i + ch.len_utf8();
        }
    }
    let col = content[line_start..byte].encode_utf16().count() as u32;
    Position {
        line,
        character: col,
    }
}

/// Parse a field declaration `node` into `(name_part, type_str)` plus the byte
/// range of the name within `content`.
///
/// The header parses as `(<id> ; <name> ; <type…>)`, so the name is the token
/// between the first and second `;` and the type is everything from the second
/// `;` to the end of the header. Splitting the text instead cut the
/// declaration at its first `)`, which dropped every standard Business Central
/// field named like `"Amount (LCY)"` or `"Qty. (Base)"` and mis-split any name
/// holding a `;`.
pub(super) fn parse_field_node<'a>(
    node: tree_sitter::Node<'_>,
    content: &'a str,
) -> Option<(&'a str, &'a str, usize, usize)> {
    let mut cursor = node.walk();
    let header = node
        .children(&mut cursor)
        .find(|child| child.kind() == "parenthesized_block")?;
    let mut header_cursor = header.walk();
    let parts: Vec<tree_sitter::Node> = header.named_children(&mut header_cursor).collect();
    let mut separators = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| part.kind() == "semicolon")
        .map(|(index, _)| index);
    let after_id = separators.next()? + 1;
    let after_name = separators.next()?;
    if after_id >= after_name {
        return None; // `field(1; ; Integer)`
    }
    let name = parts.get(after_id)?;
    let type_start = parts.get(after_name + 1)?.start_byte();
    let type_end = parts.last()?.end_byte();
    Some((
        content.get(name.start_byte()..name.end_byte())?,
        content.get(type_start..type_end)?,
        name.start_byte(),
        name.end_byte(),
    ))
}

pub(super) fn find_workspace_field(
    content: &str,
    tree: &tree_sitter::Tree,
    field_name: &str,
) -> Option<(ResolvedType, Range)> {
    find_field_under(content, tree.root_node(), field_name)
}

/// [`find_workspace_field`] restricted to one object declaration.
pub(crate) fn find_field_under(
    content: &str,
    root: tree_sitter::Node<'_>,
    field_name: &str,
) -> Option<(ResolvedType, Range)> {
    let src = content.as_bytes();
    for node in field_decl_nodes_under(root, src) {
        let Some((name_part, ty, start, end)) = parse_field_node(node, content) else {
            continue;
        };
        if !al_syntax::clean_identifier(name_part).eq_ignore_ascii_case(field_name) {
            continue;
        }
        let range = Range {
            start: byte_to_position(content, start),
            end: byte_to_position(content, end),
        };
        return Some((parse_type_expr(ty), range));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_syntax::byte_col_to_utf16_col;

    use crate::resolution::test_support::tree_of;

    #[test]
    fn workspace_field_position_is_ascii_byte_equals_utf16() {
        let text =
            "table 50100 T\n{\n    fields\n    {\n        field(1; Name; Text[50]) { }\n    }\n}";
        let (_ty, range) = find_workspace_field(text, &tree_of(text), "Name").expect("field found");
        let line = text.lines().nth(4).unwrap();
        let expected = line.find("Name").unwrap() as u32;
        assert_eq!(range.start.character, expected);
        assert_eq!(range.end.character, expected + "Name".len() as u32);
        assert_eq!(range.start.line, 4);
    }

    #[test]
    fn compact_table_two_fields_on_one_line() {
        // two field declarations on the same line must both resolve. The
        // old per-line scanner only ever saw the first.
        let text =
        "table 1 T\n{\n    fields\n    { field(1; Amount; Decimal) { } field(2; Qty; Integer) { } }\n}";
        let tree = tree_of(text);
        let (ty_amount, _) = find_workspace_field(text, &tree, "Amount").expect("Amount resolves");
        assert_eq!(ty_amount.type_name, "Decimal");
        let (ty_qty, _) = find_workspace_field(text, &tree, "Qty").expect("Qty resolves");
        assert_eq!(ty_qty.type_name, "Integer");
    }

    #[test]
    fn workspace_field_position_non_ascii_uses_utf16_columns() {
        // Field name starting with a 2-byte UTF-8 char ('Ø' = U+00D8, 1 UTF-16
        // unit, 2 UTF-8 bytes). The reported columns must be UTF-16 code units,
        // not byte offsets.
        let text = "table 50100 T\n{\n    fields\n    {\n        field(1; \"Ørnamental\"; Text[50]) { }\n    }\n}";
        let (_ty, range) =
            find_workspace_field(text, &tree_of(text), "Ørnamental").expect("field found");
        let line = text.lines().nth(4).unwrap();
        // name_part includes the surrounding quotes: "Ørnamental"
        let name_part = "\"Ørnamental\"";
        let byte_start = line.find(name_part).unwrap();
        let expected_start = byte_col_to_utf16_col(line, byte_start);
        let expected_end = byte_col_to_utf16_col(line, byte_start + name_part.len());
        assert_eq!(range.start.character, expected_start);
        assert_eq!(range.end.character, expected_end);
        // The byte length (13) exceeds the UTF-16 length (12) by exactly one
        // (the extra UTF-8 byte of 'Ø'), proving the conversion happened.
        assert_eq!(name_part.len(), 13);
        assert_eq!(expected_end - expected_start, 12);
    }

    #[test]
    fn workspace_field_position_multibyte_in_middle() {
        // 'ü' (U+00FC) mid-name: 2 UTF-8 bytes, 1 UTF-16 unit.
        let text =
        "table 1 T\n{\n    fields\n    {\n        field(1; \"München\"; Code[20]) { }\n    }\n}";
        let (_ty, range) =
            find_workspace_field(text, &tree_of(text), "München").expect("field found");
        let line = text.lines().nth(4).unwrap();
        let name_part = "\"München\"";
        let byte_start = line.find(name_part).unwrap();
        assert_eq!(
            range.start.character,
            byte_col_to_utf16_col(line, byte_start)
        );
        assert_eq!(
            range.end.character,
            byte_col_to_utf16_col(line, byte_start + name_part.len())
        );
        // "München" with quotes = 10 bytes (ü is 2), 9 UTF-16 units.
        assert_eq!(name_part.len(), 10);
        assert_eq!(range.end.character - range.start.character, 9);
    }

    /// `(name, type)` for every field in a one-table source, in source order.
    fn parsed_fields(content: &str) -> Vec<(&str, &str)> {
        let tree = tree_of(content);
        let mut fields: Vec<(usize, &str, &str)> = field_decl_nodes(&tree, content.as_bytes())
            .into_iter()
            .filter_map(|node| {
                let (name, ty, start, _) = parse_field_node(node, content)?;
                Some((start, name, ty))
            })
            .collect();
        fields.sort_by_key(|(start, _, _)| *start);
        fields.into_iter().map(|(_, name, ty)| (name, ty)).collect()
    }

    #[test]
    fn parse_field_node_extracts_name_and_type() {
        let content =
            "table 1 T\n{\n    fields\n    {\n        field(1; Name; Text[50]) { }\n    }\n}";
        assert_eq!(parsed_fields(content), vec![("Name", "Text[50]")]);
    }

    /// A parenthesis, a percent sign and a `;` all appear in standard Business
    /// Central field names, and all three used to cut the declaration short.
    #[test]
    fn parse_field_node_handles_punctuation_in_a_quoted_name() {
        let content = "table 1 T\n{\n    fields\n    {\n        field(50; \"Amount (LCY)\"; Decimal) { }\n        field(51; \"Line Discount %\"; Decimal) { }\n        field(52; \"A;B\"; Text[10]) { }\n    }\n}";
        assert_eq!(
            parsed_fields(content),
            vec![
                ("\"Amount (LCY)\"", "Decimal"),
                ("\"Line Discount %\"", "Decimal"),
                ("\"A;B\"", "Text[10]"),
            ]
        );
    }

    /// A multi-token type keeps every token, up to the closing paren.
    #[test]
    fn parse_field_node_keeps_a_multi_token_type() {
        let content = "table 1 T\n{\n    fields\n    {\n        field(53; Kind; Enum \"My Enum\") { }\n        field(54; Items; array[10] of Text) { }\n    }\n}";
        assert_eq!(
            parsed_fields(content),
            vec![("Kind", "Enum \"My Enum\""), ("Items", "array[10] of Text")]
        );
    }

    #[test]
    fn parse_field_node_none_when_name_or_type_is_missing() {
        let content = "table 1 T\n{\n    fields\n    {\n        field(1; ; Integer) { }\n        field(2) { }\n    }\n}";
        assert!(parsed_fields(content).is_empty());
    }

    /// A field a punctuated name reaches hover and go-to-definition, which is
    /// what the text split dropped.
    #[test]
    fn find_workspace_field_resolves_a_punctuated_name() {
        let content = "table 1 T\n{\n    fields\n    {\n        field(50; \"Amount (LCY)\"; Decimal) { }\n    }\n}";
        let (resolved, range) =
            find_workspace_field(content, &tree_of(content), "Amount (LCY)").expect("resolved");
        assert_eq!(resolved.type_name, "Decimal");
        assert_eq!(range.start.line, 4);
    }
}
