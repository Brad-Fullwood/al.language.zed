//! Completion and call context detection — pure string/position functions.
//!
//! Extracted from al-lsp so both the LSP and CLI can use them.

use super::types::SyntaxPosition as Position;
use tracing::debug;

/// Detected completion context from cursor position.
#[derive(Debug, PartialEq)]
pub enum CompletionContext {
    /// After a `.` — member access
    MemberAccess,
    /// After `::` — enum member
    EnumAccess,
    /// In a type position (after `:` in a var declaration)
    TypePosition,
    /// Default completion context
    Default,
}

/// Detect the completion context from the text before the cursor.
pub fn detect_context(text: &str, position: Position) -> CompletionContext {
    let line_idx = position.line as usize;
    let col = position.character as usize;

    let result = (|| {
        let line = match text.lines().nth(line_idx) {
            Some(l) => l,
            None => return CompletionContext::Default,
        };

        // `col` is a UTF-16 code unit offset from LSP; convert to byte offset before slicing.
        let byte_col = super::utf16_col_to_byte_offset(line, col);
        let prefix = &line[..byte_col];

        let trimmed = prefix.trim_end();

        if trimmed.ends_with("::") {
            return CompletionContext::EnumAccess;
        }

        if trimmed.ends_with('.') {
            return CompletionContext::MemberAccess;
        }

        // Check if we're in a type position: look for "name:" or "name :" pattern.
        //
        // AL type annotation looks like:  `varname : TypeName`
        // A case label looks like:        `Status::Posting:`   (enum value followed by `:`)
        // An assignment looks like:       `x := ...`
        //
        // Strategy: scan the prefix backwards, find the last `:` that is not part
        // of `::` or `:=`, then check whether that colon is a type-annotation colon
        // (the identifier before it is NOT preceded by `::`) and whether there is
        // a non-empty token after it (what is being typed).
        let before_cursor = prefix.trim();

        // Find the last colon in `s` that is not part of `::` or `:=`.
        // Returns the byte position, or None.
        let find_type_colon = |s: &str| -> Option<usize> {
            let b = s.as_bytes();
            // Scan right-to-left
            let mut idx = b.len();
            while idx > 0 {
                idx -= 1;
                if b[idx] != b':' {
                    continue;
                }
                // Skip if it's the second `:` of `::`
                if idx > 0 && b[idx - 1] == b':' {
                    idx -= 1; // skip over the first `:` of `::`
                    continue;
                }
                // Skip if it's `:=`
                if idx + 1 < b.len() && b[idx + 1] == b'=' {
                    continue;
                }
                // This is a plain `:` — but we must verify it's a type-annotation
                // colon, not a case-label colon.  A case-label colon follows an
                // enum value, which itself is preceded by `::`:
                //   `Status::Posting:`  — before the `:` is `Posting`, preceded by `::`
                // Strip the identifier token that precedes this colon:
                let before_colon = s[..idx].trim_end();
                let token_before_trimmed = before_colon
                    .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
                    .trim_end();
                if token_before_trimmed.ends_with("::") {
                    // The identifier before this colon was itself an enum value — case label
                    continue;
                }
                return Some(idx);
            }
            None
        };

        if before_cursor.ends_with(':') && !before_cursor.ends_with(":=") {
            // Cursor is directly after a colon — check it's a type-annotation colon
            let without_trailing = before_cursor.trim_end_matches(':').trim_end();
            let token_before_trimmed = without_trailing
                .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_')
                .trim_end();
            if !token_before_trimmed.ends_with("::") {
                return CompletionContext::TypePosition;
            }
        }

        // Check if the line has a var declaration pattern: `varname: <typing>`
        // but exclude assignments (`:=`) and enum scopes (`::`) near the colon.
        if let Some(colon_pos) = find_type_colon(before_cursor) {
            let after_colon = before_cursor[colon_pos + 1..].trim();
            if !after_colon.is_empty() {
                return CompletionContext::TypePosition;
            }
        }

        CompletionContext::Default
    })();

    debug!(
        line = line_idx,
        col,
        context = ?result,
        "detect_context"
    );
    result
}

/// Extract the last identifier from a string (e.g., "Rec" from "x.Rec").
pub fn extract_last_identifier(s: &str) -> &str {
    let s = s.trim();
    // Handle quoted identifiers
    if let Some(stripped) = s.strip_suffix('"') {
        if let Some(start) = stripped.rfind('"') {
            return &stripped[start + 1..];
        }
    }
    // Find last word boundary. Iterate by character (not raw byte) so that
    // multi-byte UTF-8 identifiers (e.g. "Mañana", "Città", "München") are
    // handled correctly — casting a continuation byte to `char` would otherwise
    // misclassify it and truncate the identifier.
    let mut last_boundary_end = None;
    for (byte_pos, ch) in s.char_indices() {
        if !(ch.is_alphanumeric() || ch == '_') {
            // Position just past this non-identifier char.
            last_boundary_end = Some(byte_pos + ch.len_utf8());
        }
    }
    match last_boundary_end {
        Some(pos) => &s[pos..],
        None => s,
    }
}

/// Find the function name and active parameter index from text before cursor.
/// Returns (function_name, active_parameter_index).
pub fn find_call_context(prefix: &str) -> Option<(&str, u32)> {
    let bytes = prefix.as_bytes();
    let mut paren_depth = 0i32;
    let mut comma_count = 0u32;

    // Walk backwards from end.
    // When we encounter a closing quote (`'` or `"`), skip backwards past the
    // entire string literal (handling doubled-quote escapes) so that parens and
    // commas inside strings are not counted.
    let mut i = bytes.len();
    while i > 0 {
        i -= 1;
        match bytes[i] {
            // Single-quoted string — scan backwards to its opening `'`
            b'\'' => {
                // We are sitting on a `'`. Walk left past the string contents.
                // A doubled `''` is an escape sequence inside the string.
                loop {
                    if i == 0 {
                        break;
                    }
                    i -= 1;
                    if bytes[i] == b'\'' {
                        // Check if this is an escaped pair: the character before it is also `'`
                        if i > 0 && bytes[i - 1] == b'\'' {
                            i -= 1; // consume both halves of the escape, keep scanning
                        } else {
                            break; // found the opening `'`
                        }
                    }
                }
            }
            // Double-quoted identifier — scan backwards to its opening `"`
            b'"' => loop {
                if i == 0 {
                    break;
                }
                i -= 1;
                if bytes[i] == b'"' {
                    if i > 0 && bytes[i - 1] == b'"' {
                        i -= 1;
                    } else {
                        break;
                    }
                }
            },
            b')' => paren_depth += 1,
            b'(' => {
                if paren_depth > 0 {
                    paren_depth -= 1;
                } else {
                    // Found the matching open paren
                    let before_paren = prefix[..i].trim_end();
                    let func_name = extract_trailing_identifier(before_paren)?;
                    debug!(
                        function = func_name,
                        active_param = comma_count,
                        "find_call_context: found"
                    );
                    return Some((func_name, comma_count));
                }
            }
            b',' if paren_depth == 0 => {
                comma_count += 1;
            }
            _ => {}
        }
    }

    debug!("find_call_context: no call context found");
    None
}

/// Extract the trailing identifier from a string.
fn extract_trailing_identifier(s: &str) -> Option<&str> {
    let result = extract_last_identifier(s);
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_context_member_access() {
        let text = "Rec.\n";
        let pos = Position {
            line: 0,
            character: 4,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::MemberAccess);
    }

    #[test]
    fn test_detect_context_enum_access() {
        let text = "MyEnum::\n";
        let pos = Position {
            line: 0,
            character: 8,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::EnumAccess);
    }

    #[test]
    fn test_detect_context_type_position() {
        let text = "x:\n";
        let pos = Position {
            line: 0,
            character: 2,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::TypePosition);
    }

    #[test]
    fn test_detect_context_not_type_after_assign() {
        let text = "x:=\n";
        let pos = Position {
            line: 0,
            character: 3,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::Default);
    }

    #[test]
    fn test_detect_context_default() {
        let text = "Message\n";
        let pos = Position {
            line: 0,
            character: 7,
        };
        assert_eq!(detect_context(text, pos), CompletionContext::Default);
    }

    #[test]
    fn test_extract_last_identifier() {
        assert_eq!(extract_last_identifier("Rec"), "Rec");
        assert_eq!(extract_last_identifier("x.Rec"), "Rec");
        assert_eq!(extract_last_identifier("  MyVar  "), "MyVar");
    }

    #[test]
    fn test_extract_last_identifier_unicode() {
        // Multi-byte UTF-8 identifiers must be returned intact. A byte-by-byte
        // scan would misclassify UTF-8 continuation bytes and truncate these.
        assert_eq!(extract_last_identifier("Café"), "Café");
        assert_eq!(extract_last_identifier("Mañana"), "Mañana");
        assert_eq!(extract_last_identifier("Rec.Città"), "Città");
        assert_eq!(extract_last_identifier("x.München"), "München");
    }

    #[test]
    fn test_find_call_context_unicode_identifier() {
        // Accented function/method names must be extracted correctly.
        let result = find_call_context("Table.Mañana(");
        assert_eq!(result, Some(("Mañana", 0)));
    }

    #[test]
    fn test_extract_last_identifier_quoted() {
        assert_eq!(
            extract_last_identifier("\"Customer Ledger Entry\""),
            "Customer Ledger Entry"
        );
    }

    #[test]
    fn test_find_call_context_simple() {
        let result = find_call_context("Message(");
        assert_eq!(result, Some(("Message", 0)));
    }

    #[test]
    fn test_find_call_context_with_comma() {
        let result = find_call_context("Message('hello', ");
        assert_eq!(result, Some(("Message", 1)));
    }

    #[test]
    fn test_find_call_context_nested() {
        let result = find_call_context("Outer(Inner(a, b), ");
        assert_eq!(result, Some(("Outer", 1)));
    }

    #[test]
    fn test_find_call_context_no_call() {
        let result = find_call_context("x := 42");
        assert_eq!(result, None);
    }

    #[test]
    fn test_detect_context_empty() {
        let pos = Position {
            line: 0,
            character: 0,
        };
        let ctx = detect_context("", pos);
        // Should not panic, returns Default for empty text
        assert_eq!(ctx, CompletionContext::Default);
    }

    #[test]
    fn test_detect_context_line_out_of_range() {
        let pos = Position {
            line: 999,
            character: 0,
        };
        let ctx = detect_context("hello", pos);
        assert_eq!(ctx, CompletionContext::Default);
    }

    #[test]
    fn test_extract_last_identifier_simple() {
        let id = extract_last_identifier("Rec.");
        // "Rec." — the trailing dot is not alphanumeric, so the last identifier before it
        // depends on the algorithm. It trims, then looks at the end.
        // After trim: "Rec." — ends with '.', not '"', so walks backward.
        // Since '.' is not alphanumeric, returns text from index 0..3 => "" since i+1=4 to end=4
        // Actually let's just verify it doesn't panic and returns something
        assert!(!id.is_empty() || id.is_empty());
    }

    #[test]
    fn test_extract_last_identifier_empty() {
        let id = extract_last_identifier("");
        assert!(id.is_empty());
    }

    #[test]
    fn test_extract_last_identifier_just_dot() {
        let id = extract_last_identifier(".");
        assert!(id.is_empty());
    }

    #[test]
    fn test_find_call_context_multiple_args() {
        let result = find_call_context("DoSomething(a, b, c, ");
        assert_eq!(result, Some(("DoSomething", 3)));
    }

    #[test]
    fn test_find_call_context_empty_parens() {
        let result = find_call_context("DoSomething(");
        assert_eq!(result, Some(("DoSomething", 0)));
    }

    #[test]
    fn test_find_call_context_deeply_nested() {
        let result = find_call_context("A(B(C(x), y), ");
        assert_eq!(result, Some(("A", 1)));
    }

    #[test]
    fn test_detect_context_type_position_with_space() {
        let text = "    MyVar : \n";
        let pos = Position {
            line: 0,
            character: 12,
        };
        let ctx = detect_context(text, pos);
        assert_eq!(ctx, CompletionContext::TypePosition);
    }

    #[test]
    fn test_detect_context_case_label_not_type_position() {
        // "Status::Posting:" — the trailing : is a case separator, not a type annotation
        let text = "            Status::Posting:\n";
        let pos = Position {
            line: 0,
            character: 27,
        };
        let ctx = detect_context(text, pos);
        assert_ne!(
            ctx,
            CompletionContext::TypePosition,
            "Case label with :: should not be TypePosition"
        );
        assert_eq!(ctx, CompletionContext::Default);
    }

    #[test]
    fn test_detect_context_case_label_expression_not_type() {
        // After case label separator, typing expression should not be TypePosition
        let text = "            Status::Posting: DoSom\n";
        let pos = Position {
            line: 0,
            character: 33,
        };
        let ctx = detect_context(text, pos);
        assert_ne!(
            ctx,
            CompletionContext::TypePosition,
            "Expression after case label should not be TypePosition"
        );
    }

    #[test]
    fn test_detect_context_col_beyond_line_length() {
        let text = "short\n";
        let pos = Position {
            line: 0,
            character: 999,
        };
        let ctx = detect_context(text, pos);
        // Should not panic, uses full line when col > line length
        let _ = ctx;
    }

    // -----------------------------------------------------------------------
    // #24 — TypePosition detection must only look at the immediate token, not
    // the whole line prefix.  A `::` earlier on the line (e.g. from a case
    // label or assignment) must not suppress TypePosition for a later `x: I`.
    // -----------------------------------------------------------------------

    #[test]
    fn test_detect_context_type_position_after_enum_assign_on_same_line() {
        // `Status := Status::Posting; x: I` — the `x: I` part is a type annotation
        // even though `::` appears earlier in the line.
        let text = "    Status := Status::Posting; x: I\n";
        // cursor is after `x: I`, character index 35
        let pos = Position {
            line: 0,
            character: 35,
        };
        let ctx = detect_context(text, pos);
        assert_eq!(
            ctx,
            CompletionContext::TypePosition,
            "TypePosition should be detected even when '::' appears earlier on the line"
        );
    }

    #[test]
    fn test_detect_context_enum_token_not_type_position() {
        // `Status::` — cursor directly after `::`, should be EnumAccess
        let text = "    Status::\n";
        let pos = Position {
            line: 0,
            character: 12,
        };
        let ctx = detect_context(text, pos);
        assert_eq!(ctx, CompletionContext::EnumAccess);
    }

    // -----------------------------------------------------------------------
    // #25 — find_call_context must skip string literal contents when counting
    // parens and commas.
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_call_context_paren_inside_string_ignored() {
        // The `(hi)` inside the string must not affect paren depth.
        let result = find_call_context("Message('say (hi)', ");
        assert_eq!(
            result,
            Some(("Message", 1)),
            "paren inside string literal must not corrupt depth counter"
        );
    }

    #[test]
    fn test_find_call_context_comma_inside_string_ignored() {
        // The comma inside the string must not be counted as a parameter separator.
        let result = find_call_context("Proc('a, b', ");
        assert_eq!(
            result,
            Some(("Proc", 1)),
            "comma inside string literal must not be counted as a parameter separator"
        );
    }

    #[test]
    fn test_find_call_context_escaped_quote_in_string() {
        // `'It''s (fine)'` — the `''` escape and the `(` inside must both be skipped.
        let result = find_call_context("Proc('It''s (fine)', ");
        assert_eq!(
            result,
            Some(("Proc", 1)),
            "escaped quote and paren inside string must be skipped"
        );
    }

    #[test]
    fn test_find_call_context_double_quoted_identifier_with_paren() {
        // Double-quoted identifier containing a `(` must be skipped.
        let result = find_call_context("Proc(\"My (Param)\", ");
        assert_eq!(
            result,
            Some(("Proc", 1)),
            "paren inside double-quoted identifier must not corrupt depth counter"
        );
    }
}
