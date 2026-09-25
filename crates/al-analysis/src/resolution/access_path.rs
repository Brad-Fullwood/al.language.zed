//! From a cursor position to the receiver and member it names.
//!
//! Two paths reach the same answer. The tree path walks the parsed member
//! access node. The text path scans the raw line, which is what an incomplete
//! edit such as `Cust.` leaves behind, and is guarded against comments and
//! string literals by [`position_is_in_comment_or_literal`].

use al_syntax::IdentifierText;
use tree_sitter::Tree;

use crate::queries::Position;

use super::{utf16_col_to_byte_offset, AccessKind, AccessPath};

pub(crate) fn access_path_at(tree: &Tree, text: &str, position: Position) -> Option<AccessPath> {
    // The text shortcut below works on the raw line and knows nothing of
    // comments or literals, so `// Update Cust.Name before posting` and
    // `Error('Cust.Name is required')` both produced a member access and a
    // tooltip. The tree branch cannot fire inside either, so only the shortcut
    // needs the guard.
    if position_is_in_comment_or_literal(tree, text, position) {
        return None;
    }
    if let Some(path) = access_path_from_text(text, position) {
        tracing::debug!(
            receiver = %path.receiver,
            member = %path.member,
            kind = ?path.kind,
            source = "text",
            "access_path_at: found via text-based parsing"
        );
        return Some(path);
    }

    let Some(node) = al_syntax::find_node_at_position(tree, text, position.into()) else {
        tracing::debug!(
            line = position.line,
            character = position.character,
            "access_path_at: no tree-sitter node at position"
        );
        return None;
    };
    let mut current = node;

    loop {
        match current.kind() {
            "member_call_suffix" | "member_suffix" | "scope_call_suffix" | "scope_suffix" => {
                let member_node = current.child_by_field_name("member")?;
                if member_node.start_byte() <= node.start_byte()
                    && member_node.end_byte() >= node.end_byte()
                {
                    let postfix = current.parent()?;
                    if postfix.kind() != "postfix_expression" {
                        tracing::debug!(
                            parent_kind = postfix.kind(),
                            "access_path_at: parent is not postfix_expression"
                        );
                        return None;
                    }
                    let receiver = text[postfix.start_byte()..current.start_byte()]
                        .trim()
                        .to_string();
                    // Non-UTF8 member node text is not a valid identifier
                    let member = member_node
                        .utf8_text(text.as_bytes())
                        .unwrap_or("")
                        .unquote_identifier()
                        .to_string();
                    let kind = if current.kind().starts_with("scope") {
                        AccessKind::Scope
                    } else {
                        AccessKind::Member
                    };
                    tracing::debug!(
                        receiver = %receiver,
                        member = %member,
                        kind = ?kind,
                        source = "tree",
                        node_kind = current.kind(),
                        "access_path_at: found via tree-sitter"
                    );
                    return Some(AccessPath {
                        receiver,
                        member,
                        kind,
                    });
                }
            }
            _ => {}
        }

        current = current.parent()?;
    }
}

pub(crate) fn receiver_chain_before(
    text: &str,
    position: Position,
) -> Option<(String, AccessKind)> {
    let line = text.lines().nth(position.line as usize)?;
    let byte_off = utf16_col_to_byte_offset(line, position.character as usize);
    let prefix = &line[..byte_off];
    let trimmed = prefix.trim_end();

    let (kind, end) = if trimmed.ends_with("::") {
        (AccessKind::Scope, trimmed.len().saturating_sub(2))
    } else if trimmed.ends_with('.') {
        (AccessKind::Member, trimmed.len().saturating_sub(1))
    } else {
        tracing::debug!(
            line = position.line,
            "receiver_chain_before: no trailing '.' or '::'"
        );
        return None;
    };

    let (start, token_end) = token_span_ending_at(trimmed, end)?;
    let mut left_cursor = start;
    let mut receiver_start = start;
    while let Some((_, prev_start, _)) = previous_access_part(trimmed, left_cursor) {
        receiver_start = prev_start;
        left_cursor = prev_start;
    }

    let receiver = trimmed[receiver_start..token_end].trim().to_string();
    tracing::debug!(receiver = %receiver, kind = ?kind, "receiver_chain_before: detected");
    Some((receiver, kind))
}

fn access_path_from_text(text: &str, position: Position) -> Option<AccessPath> {
    let line = text.lines().nth(position.line as usize)?;
    let bytes = line.as_bytes();
    if bytes.is_empty() {
        tracing::trace!("access_path_from_text: empty line");
        return None;
    }

    let byte_col = utf16_col_to_byte_offset(line, position.character as usize);
    let mut idx = byte_col.min(bytes.len().saturating_sub(1));
    if !is_access_char(bytes[idx]) {
        if idx > 0 && is_access_char(bytes[idx - 1]) {
            idx -= 1;
        } else {
            tracing::trace!(
                line = position.line,
                character = position.character,
                "access_path_from_text: not on access char"
            );
            return None;
        }
    }

    let (token_start, token_end) = token_span_at(line, idx)?;
    let mut parts = vec![(token_start, token_end)];
    let mut separators = Vec::new();

    let mut left_cursor = token_start;
    while let Some((kind, prev_start, prev_end)) = previous_access_part(line, left_cursor) {
        parts.insert(0, (prev_start, prev_end));
        separators.insert(0, kind);
        left_cursor = prev_start;
    }

    let mut right_cursor = token_end;
    while let Some((kind, next_start, next_end)) = next_access_part(line, right_cursor) {
        separators.push(kind);
        parts.push((next_start, next_end));
        right_cursor = next_end;
    }

    let part_index = parts
        .iter()
        .position(|(start, end)| idx >= *start && idx < *end)?;

    tracing::debug!(
        parts = parts.len(),
        separators = separators.len(),
        part_index = part_index,
        "access_path_from_text: parsed chain"
    );

    if part_index == 0 {
        tracing::debug!(
            "access_path_from_text: cursor on part 0 (receiver position), returning None"
        );
        return None;
    }

    let receiver_start = parts.first()?.0;
    let receiver_end = parts[part_index - 1].1;
    let member = clean_access_text(&line[parts[part_index].0..parts[part_index].1]);

    Some(AccessPath {
        receiver: line[receiver_start..receiver_end].trim().to_string(),
        member,
        kind: separators[part_index - 1],
    })
}

fn previous_access_part(line: &str, current_start: usize) -> Option<(AccessKind, usize, usize)> {
    if current_start == 0 {
        return None;
    }

    let bytes = line.as_bytes();
    let (kind, prev_end) = if bytes.get(current_start.wrapping_sub(1)) == Some(&b'.') {
        (AccessKind::Member, current_start - 1)
    } else if current_start >= 2 && &bytes[current_start - 2..current_start] == b"::" {
        (AccessKind::Scope, current_start - 2)
    } else {
        return None;
    };

    // `Token.AsValue().AsText`: the part before the `.` is a call, so step
    // back over its argument list to the name.
    if kind == AccessKind::Member && bytes.get(prev_end.wrapping_sub(1)) == Some(&b')') {
        let mut depth = 0usize;
        let mut open = None;
        for index in (0..prev_end).rev() {
            match bytes[index] {
                b')' => depth += 1,
                b'(' => {
                    depth -= 1;
                    if depth == 0 {
                        open = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        let (prev_start, _) = token_span_ending_at(line, open?)?;
        return Some((kind, prev_start, prev_end));
    }
    let (prev_start, _) = token_span_ending_at(line, prev_end)?;
    Some((kind, prev_start, prev_end))
}

fn next_access_part(line: &str, current_end: usize) -> Option<(AccessKind, usize, usize)> {
    let bytes = line.as_bytes();
    let (kind, next_start) = if bytes.get(current_end) == Some(&b'.') {
        (AccessKind::Member, current_end + 1)
    } else if bytes.get(current_end) == Some(&b':') && bytes.get(current_end + 1) == Some(&b':') {
        (AccessKind::Scope, current_end + 2)
    } else {
        return None;
    };

    let (_, next_end) = token_span_at(line, next_start)?;
    Some((kind, next_start, next_end))
}

fn token_span_at(line: &str, idx: usize) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let ch = *bytes.get(idx)?;
    if ch == b'"' || inside_quoted_identifier(line, idx) {
        return quoted_span_at(line, idx);
    }
    if !is_identifier_char(ch) {
        return None;
    }

    let mut start = idx;
    while start > 0 && is_identifier_char(bytes[start - 1]) {
        start -= 1;
    }

    let mut end = idx + 1;
    while end < bytes.len() && is_identifier_char(bytes[end]) {
        end += 1;
    }

    Some((start, end))
}

fn token_span_ending_at(line: &str, end: usize) -> Option<(usize, usize)> {
    if end == 0 {
        return None;
    }

    let idx = end - 1;
    token_span_at(line, idx).filter(|(_, token_end)| *token_end == end)
}

fn quoted_span_at(line: &str, idx: usize) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut start = idx;
    while start > 0 {
        start -= 1;
        if bytes[start] == b'"' {
            break;
        }
    }
    if bytes.get(start) != Some(&b'"') {
        return None;
    }

    let mut end = start + 1;
    while end < bytes.len() {
        if bytes[end] == b'"' {
            return Some((start, end + 1));
        }
        end += 1;
    }

    None
}

fn inside_quoted_identifier(line: &str, idx: usize) -> bool {
    let bytes = line.as_bytes();
    let mut quote_count = 0usize;
    for ch in &bytes[..idx] {
        if *ch == b'"' {
            quote_count += 1;
        }
    }
    quote_count % 2 == 1
}

fn is_access_char(ch: u8) -> bool {
    is_identifier_char(ch) || ch == b'"'
}

fn is_identifier_char(ch: u8) -> bool {
    ch.is_ascii_alphanumeric() || ch == b'_' || ch >= 0x80
}

fn clean_access_text(value: &str) -> String {
    value.unquote_identifier().into_owned()
}

/// Whether `position` sits inside a comment or a string literal.
fn position_is_in_comment_or_literal(tree: &Tree, text: &str, position: Position) -> bool {
    al_syntax::find_node_at_position(tree, text, position.into()).is_some_and(|node| {
        matches!(
            node.kind(),
            "comment" | "string" | "verbatim_string" | "inactive_code"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_syntax::AlParser;

    #[test]
    fn access_path_detects_flat_member_chain() {
        let source = r#"report 1 Test
{
    trigger OnPreReport()
    begin
        this.APIHelper.SchedulePost(StagingRec);
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(source);

        let api_helper = access_path_at(
            &result.tree,
            source,
            Position {
                line: 4,
                character: 13,
            },
        )
        .expect("member access on APIHelper");
        assert_eq!(api_helper.receiver, "this");
        assert_eq!(api_helper.member, "APIHelper");
        assert_eq!(api_helper.kind, AccessKind::Member);

        let schedule_post = access_path_at(
            &result.tree,
            source,
            Position {
                line: 4,
                character: 23,
            },
        )
        .expect("member access on SchedulePost");
        assert_eq!(schedule_post.receiver, "this.APIHelper");
        assert_eq!(schedule_post.member, "SchedulePost");
        assert_eq!(schedule_post.kind, AccessKind::Member);
    }

    #[test]
    fn access_path_detects_scope_chain() {
        let source = r#"codeunit 1 Test
{
    procedure Run()
    begin
        if Staging.Status = Staging.Status::Posting then;
    end;
}"#;
        let mut parser = AlParser::new();
        let result = parser.parse(source);

        let posting = access_path_at(
            &result.tree,
            source,
            Position {
                line: 4,
                character: 44,
            },
        )
        .expect("scope access on Posting");
        assert_eq!(posting.receiver, "Staging.Status");
        assert_eq!(posting.member, "Posting");
        assert_eq!(posting.kind, AccessKind::Scope);
    }

    #[test]
    fn receiver_chain_before_trailing_dot_ignores_outer_syntax() {
        let source = "field(status; this.)";
        let (receiver, kind) = receiver_chain_before(
            source,
            Position {
                line: 0,
                character: source.len() as u32 - 1,
            },
        )
        .expect("receiver before trailing dot");
        assert_eq!(receiver, "this");
        assert_eq!(kind, AccessKind::Member);
    }

    #[test]
    fn utf16_col_to_byte_offset_ascii_only() {
        let line = "Hello.World";
        // All ASCII: UTF-16 col == byte offset.
        assert_eq!(utf16_col_to_byte_offset(line, 5), 5);
        assert_eq!(utf16_col_to_byte_offset(line, 0), 0);
        assert_eq!(utf16_col_to_byte_offset(line, 11), 11);
    }

    #[test]
    fn utf16_col_to_byte_offset_multibyte() {
        // "Ø" is U+00D8: 2 UTF-8 bytes, 1 UTF-16 code unit.
        // "Ønske" → bytes: [0xC3, 0x98, 'n', 's', 'k', 'e']
        //                  byte 0        2    3    4    5
        // UTF-16 cols:        0           1    2    3    4    5
        let line = "Ønske.Foo";
        assert_eq!(utf16_col_to_byte_offset(line, 0), 0); // start of 'Ø'
        assert_eq!(utf16_col_to_byte_offset(line, 1), 2); // 'n' (after 2-byte Ø)
        assert_eq!(utf16_col_to_byte_offset(line, 5), 6); // '.' at byte 6
        assert_eq!(utf16_col_to_byte_offset(line, 6), 7); // 'F' at byte 7
    }

    #[test]
    fn utf16_col_to_byte_offset_past_end_clamps() {
        let line = "abc";
        assert_eq!(utf16_col_to_byte_offset(line, 100), 3);
    }

    #[test]
    fn receiver_chain_before_non_ascii_prefix() {
        // The line starts with a 2-byte UTF-8 character ('ÿ', U+00FF) followed by
        // an ASCII identifier and a trailing dot.
        //
        //   "ÿRec."
        //   bytes:    [0xC3, 0xBF, 'R', 'e', 'c', '.']   (6 bytes)
        //   UTF-16:     0            1    2    3    4   -> utf16_len = 5
        //
        // OLD (buggy) code: prefix = &line[..5] = "ÿRec" (byte 5 is before '.')
        //   trimmed has no trailing '.', returns None  <- wrong
        //
        // NEW (fixed) code: utf16_col_to_byte_offset converts col 5 -> byte 6
        //   prefix = &line[..6] = "ÿRec." -> trimmed ends with '.'
        //   is_identifier_char includes bytes >= 0x80, so the full token "ÿRec"
        //   is extracted as the receiver.
        let source = "ÿRec.";
        let utf16_len: usize = source.chars().map(|c| c.len_utf16()).sum();
        // Sanity: 'ÿ' is 2 UTF-8 bytes but 1 UTF-16 unit, so lengths differ.
        assert_eq!(source.len(), 6, "6 bytes");
        assert_eq!(utf16_len, 5, "5 UTF-16 code units");

        let (receiver, kind) = receiver_chain_before(
            source,
            Position {
                line: 0,
                character: utf16_len as u32,
            },
        )
        .expect("receiver before trailing dot when line has multi-byte prefix");
        assert_eq!(receiver, "ÿRec");
        assert_eq!(kind, AccessKind::Member);
    }

    /// Hovering an identifier inside a comment or a string literal used to
    /// reach the text shortcut, which knows nothing of either, and produced a
    /// member access and a tooltip.
    #[test]
    fn access_path_ignores_comments_and_string_literals() {
        let source = r#"codeunit 50100 "Test"
{
    procedure Run()
    var
        Cust: Record Customer;
    begin
        // Update Cust.Name before posting
        Error('Cust.Name is required');
        Cust.Name := 'X';
    end;
}"#;
        let mut parser = AlParser::new();
        let parsed = parser.parse(source);

        let at = |line: u32, character: u32| {
            access_path_at(&parsed.tree, source, Position { line, character })
        };
        assert!(at(6, 19).is_none(), "comment produced an access path");
        assert!(
            at(7, 20).is_none(),
            "string literal produced an access path"
        );
        let real = at(8, 14).expect("the real member access still resolves");
        assert_eq!(real.receiver, "Cust");
        assert_eq!(real.member, "Name");
    }

    /// A method on a call's result: the receiver is the call expression.
    #[test]
    fn a_member_after_a_call_has_the_call_as_its_receiver() {
        let source = "codeunit 50100 X\n{\n    procedure P()\n    var\n        Token: JsonToken;\n        V: Text;\n    begin\n        V := Token.AsValue().AsText();\n    end;\n}";
        let mut parser = AlParser::new();
        let parsed = parser.parse(source);
        let line = source.lines().nth(7).unwrap();
        let column = line.find("AsText").unwrap() as u32 + 2;
        let path = access_path_at(
            &parsed.tree,
            source,
            Position {
                line: 7,
                character: column,
            },
        )
        .expect("access path");
        assert_eq!(path.receiver, "Token.AsValue()");
        assert_eq!(path.member, "AsText");
    }
}
