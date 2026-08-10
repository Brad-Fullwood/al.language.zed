//! Namespace and using-directive code actions.

use url::Url;

use super::single_edit_ws;
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

/// Extract the unresolved type name from an AL diagnostic message.
///
/// Handles formats produced by the Microsoft AL compiler:
/// - `"Type 'Foo' could not be found"`
/// - `"The type 'Foo' could not be found"`
/// - `"'Foo' does not exist in the current context"`
/// - `"Foo could not be found"` (no quotes)
pub(super) fn extract_type_name_from_diagnostic(message: &str) -> Option<String> {
    if let Some(start) = message.find('\'') {
        let rest = &message[start + 1..];
        if let Some(end) = rest.find('\'') {
            let name = &rest[..end];
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }

    // Fallback: "TypeName could not be found" — take the first word
    if message.contains("could not be found") {
        let word = message.split_whitespace().next()?;
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if !word.is_empty() {
            return Some(word.to_string());
        }
    }

    None
}

pub(super) fn source_action_add_using(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Vec<CodeActionEntry> {
    let word = extract_word_at_position(text, range);
    if word.is_empty() {
        return Vec::new();
    }

    let matches = workspace.symbols.get_by_name(&word);
    if matches.is_empty() {
        return Vec::new();
    }

    let mut candidate_namespaces: Vec<String> = matches
        .iter()
        .filter(|e| !e.namespace.is_empty())
        .map(|e| e.namespace.clone())
        .collect();
    candidate_namespaces.sort();
    candidate_namespaces.dedup();

    if candidate_namespaces.is_empty() {
        return Vec::new();
    }

    let (existing_usings, insert_line) = parse_using_directives(text);

    candidate_namespaces.retain(|ns| !existing_usings.iter().any(|u| u.eq_ignore_ascii_case(ns)));

    candidate_namespaces
        .into_iter()
        .map(|ns| {
            let new_text = format!("using {};\n", ns);
            let edit = TextEdit {
                range: Range {
                    start: super::Position {
                        line: insert_line,
                        character: 0,
                    },
                    end: super::Position {
                        line: insert_line,
                        character: 0,
                    },
                },
                new_text,
            };
            CodeActionEntry {
                title: format!("Add using {}", ns),
                kind: CodeActionKind::QuickFix,
                edit: Some(single_edit_ws(uri, vec![edit])),
                is_preferred: false,
            }
        })
        .collect()
}

/// Extract the word at the given range position from text.
/// Handles both plain identifiers and quoted identifiers like "Customer".
///
/// LSP positions are UTF-16 code units — we convert to byte offsets before slicing.
fn extract_word_at_position(text: &str, range: Range) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let line_idx = range.start.line as usize;
    if line_idx >= lines.len() {
        return String::new();
    }
    let line = lines[line_idx];

    let start_byte =
        crate::resolution::utf16_col_to_byte_offset(line, range.start.character as usize);
    let end_byte = crate::resolution::utf16_col_to_byte_offset(line, range.end.character as usize);

    // A namespace is never needed for text inside a comment or a string
    // literal, and offering `Add using` there is pure noise.
    if in_comment_or_string_literal(line, start_byte) {
        return String::new();
    }

    if start_byte < end_byte && end_byte <= line.len() {
        return line[start_byte..end_byte].trim_matches('"').to_string();
    }

    // Non-ASCII bytes (>= 0x80) are not alphanumeric, so they act as word boundaries.
    if start_byte >= line.len() {
        return String::new();
    }
    let bytes = line.as_bytes();
    let cursor = start_byte;

    // If cursor is inside a quoted identifier like `"Sales Header"`, expand the full span.
    if let Some(quoted) = try_extract_quoted_identifier(bytes, cursor) {
        return quoted;
    }

    let mut word_start = cursor;
    let mut word_end = cursor;

    while word_start > 0
        && (bytes[word_start - 1].is_ascii_alphanumeric() || bytes[word_start - 1] == b'_')
    {
        word_start -= 1;
    }
    while word_end < bytes.len()
        && (bytes[word_end].is_ascii_alphanumeric() || bytes[word_end] == b'_')
    {
        word_end += 1;
    }

    line[word_start..word_end].to_string()
}

/// Whether byte `offset` on `line` lies inside a `//` comment or a
/// single-quoted AL string literal. Double quotes delimit *identifiers*, not
/// literals, so they do not count.
fn in_comment_or_string_literal(line: &str, offset: usize) -> bool {
    let bytes = line.as_bytes();
    let limit = offset.min(bytes.len());
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0usize;
    while i < limit {
        match bytes[i] {
            b'/' if !in_single && !in_double && bytes.get(i + 1) == Some(&b'/') => return true,
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            _ => {}
        }
        i += 1;
    }
    in_single
}

/// Extract the quoted identifier that *contains* `cursor`.
///
/// Quoted spans are walked left-to-right from the start of the line. Scanning
/// leftwards from the cursor (as this used to) paired the *closing* quote of an
/// earlier identifier with the *opening* quote of the next one, so a cursor
/// between `... "X"; B: Record "Y"` extracted the garbage `; B: Record`.
fn try_extract_quoted_identifier(bytes: &[u8], cursor: usize) -> Option<String> {
    if cursor >= bytes.len() {
        return None;
    }

    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let open_quote = i;
        let mut close = open_quote + 1;
        while close < bytes.len() && bytes[close] != b'"' {
            close += 1;
        }
        if close >= bytes.len() {
            // Unterminated quote — no identifier to extract.
            return None;
        }
        if cursor >= open_quote && cursor <= close {
            let inner = std::str::from_utf8(&bytes[open_quote + 1..close]).ok()?;
            if inner.is_empty() {
                return None;
            }
            return Some(inner.to_string());
        }
        if open_quote > cursor {
            // Spans are ordered; everything from here on starts after the cursor.
            return None;
        }
        i = close + 1;
    }
    None
}

/// Parse all `using ...;` directives from the file and return the list of namespace strings
/// plus the line number where a new `using` directive should be inserted.
pub(super) fn parse_using_directives(text: &str) -> (Vec<String>, u32) {
    let mut usings = Vec::new();
    let mut last_directive_line: u32 = 0;
    let mut found_any_directive = false;

    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        // AL keywords are case-insensitive per language spec — we
        // only recognised two case variants ("namespace " / "Namespace ",
        // "using " / "Using ") which silently dropped legitimate
        // NAMESPACE / USING / mixed-case forms (e.g. "uSiNg ").
        let leader_is = |kw: &str| {
            trimmed
                .get(..kw.len())
                .map(|p| p.eq_ignore_ascii_case(kw))
                .unwrap_or(false)
                && trimmed[kw.len()..].starts_with(' ')
        };
        if leader_is("namespace") {
            last_directive_line = i as u32;
            found_any_directive = true;
        } else if leader_is("using") {
            // Extract namespace name: "using Foo.Bar;" -> "Foo.Bar"
            // Strip trailing inline comment before the semicolon (e.g. "using Foo; // comment")
            let after_keyword = trimmed.get(6..).unwrap_or("");
            let without_comment = if let Some(pos) = after_keyword.find("//") {
                &after_keyword[..pos]
            } else {
                after_keyword
            };
            let ns = without_comment.trim_end_matches(';').trim();
            if !ns.is_empty() {
                usings.push(ns.to_string());
            }
            last_directive_line = i as u32;
            found_any_directive = true;
        } else if found_any_directive && !trimmed.is_empty() && !trimmed.starts_with("//") {
            break;
        }
    }

    let insert_line = if found_any_directive {
        last_directive_line + 1
    } else {
        0
    };
    (usings, insert_line)
}

#[cfg(test)]
mod tests {
    use super::super::{namespace_quick_fix_for_diagnostic, source_actions, DiagnosticInfo};
    use super::*;
    use al_symbols::{ObjectKind, SymbolEntry};
    use al_workspace::Workspace;
    use url::Url;

    fn open_doc(ws: &Workspace, uri: &Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
    }

    fn make_entry_with_namespace(kind: ObjectKind, id: i32, name: &str, ns: &str) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            package: "TestPkg".to_string(),
            namespace: ns.to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn add_using_offered_for_unresolved_type_in_known_namespace() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_entry_with_namespace(
            ObjectKind::Table,
            18,
            "Customer",
            "Microsoft.Sales",
        )]);

        // AL file that uses Customer but doesn't have the right `using`
        let al_code = r#"namespace MyCompany.MyApp;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record Customer;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Cursor on line 5: "        Cust: Record Customer;"
        // "Customer" starts at col 21, ends at col 29
        let range = Range {
            start: super::super::Position {
                line: 5,
                character: 21,
            },
            end: super::super::Position {
                line: 5,
                character: 29,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert!(!using_actions.is_empty(), "Should offer 'Add using' action");
        assert!(using_actions[0].title.contains("Microsoft.Sales"));

        let edit = using_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        assert!(edits[0].new_text.contains("using Microsoft.Sales;"));
    }

    #[test]
    fn add_using_not_offered_when_namespace_already_imported() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[make_entry_with_namespace(
            ObjectKind::Table,
            18,
            "Customer",
            "Microsoft.Sales",
        )]);

        // File already has `using Microsoft.Sales;`
        let al_code = r#"namespace MyCompany.MyApp;
using Microsoft.Sales;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record Customer;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // Line 6: "        Cust: Record Customer;" — Customer at col 21-29
        let range = Range {
            start: super::super::Position {
                line: 6,
                character: 21,
            },
            end: super::super::Position {
                line: 6,
                character: 29,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert!(
            using_actions.is_empty(),
            "Should NOT offer 'Add using' when already imported"
        );
    }

    #[test]
    fn add_using_not_offered_when_no_matching_symbol() {
        let ws = Workspace::new();

        let al_code = r#"namespace MyCompany.MyApp;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record NonExistentTable;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // "NonExistentTable" at col 21-37
        let range = Range {
            start: super::super::Position {
                line: 5,
                character: 21,
            },
            end: super::super::Position {
                line: 5,
                character: 37,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert!(
            using_actions.is_empty(),
            "Should NOT offer 'Add using' for unknown types"
        );
    }

    #[test]
    fn add_using_offers_multiple_namespaces() {
        let ws = Workspace::new();

        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
            make_entry_with_namespace(ObjectKind::Table, 50100, "Customer", "MyCompany.CRM"),
        ]);

        let al_code = r#"namespace MyCompany.MyApp;

codeunit 50100 "My Codeunit"
{
    var
        Cust: Record Customer;
}
"#;
        let uri = Url::parse("file:///test/MyCU.al").unwrap();
        open_doc(&ws, &uri, al_code);

        // "Customer" at col 21-29
        let range = Range {
            start: super::super::Position {
                line: 5,
                character: 21,
            },
            end: super::super::Position {
                line: 5,
                character: 29,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let using_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("using"))
            .collect();

        assert_eq!(
            using_actions.len(),
            2,
            "Should offer one action per namespace"
        );
    }

    #[test]
    fn insertion_point_after_existing_using_directives() {
        let source =
            "namespace MyApp;\nusing Foo.Bar;\nusing Baz.Qux;\n\ncodeunit 50100 Test { }\n";
        let (usings, insert_line) = parse_using_directives(source);
        assert_eq!(usings, vec!["Foo.Bar", "Baz.Qux"]);
        // Should insert after line 2 (0-indexed), i.e. insert_line == 3
        assert_eq!(insert_line, 3, "should insert after last using line");
    }

    #[test]
    fn insertion_point_after_namespace_when_no_usings() {
        let source = "namespace MyApp;\n\ncodeunit 50100 Test { }\n";
        let (usings, insert_line) = parse_using_directives(source);
        assert!(usings.is_empty());
        // Namespace line is 0, so insert after it at line 1
        assert_eq!(insert_line, 1, "should insert after namespace line");
    }

    #[test]
    fn insertion_point_at_top_when_no_header() {
        let source = "codeunit 50100 Test { }\n";
        let (usings, insert_line) = parse_using_directives(source);
        assert!(usings.is_empty());
        assert_eq!(insert_line, 0, "should insert at top when no header");
    }

    #[test]
    fn extract_type_name_from_quoted_al0185_message() {
        assert_eq!(
            extract_type_name_from_diagnostic("Type 'SalesHeader' could not be found"),
            Some("SalesHeader".to_string()),
        );
    }

    #[test]
    fn extract_type_name_from_unquoted_message() {
        assert_eq!(
            extract_type_name_from_diagnostic("MyTable could not be found"),
            Some("MyTable".to_string()),
        );
    }

    #[test]
    fn extract_type_name_from_does_not_exist_message() {
        assert_eq!(
            extract_type_name_from_diagnostic(
                "'PostingGroup' does not exist in the current context"
            ),
            Some("PostingGroup".to_string()),
        );
    }

    #[test]
    fn diagnostic_quick_fix_offered_for_al0185() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_entry_with_namespace(
            ObjectKind::Table,
            18,
            "Customer",
            "Microsoft.Sales",
        )]);

        let al_code = "namespace MyApp;\n\ncodeunit 50100 Test { var c: Record Customer; }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'Customer' could not be found".to_string(),
            code: Some("AL0185".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].title.contains("Microsoft.Sales"));
        assert!(actions[0].is_preferred, "single match should be preferred");

        let edit = actions[0].edit.as_ref().unwrap();
        let (_, edits) = &edit.changes[0];
        assert!(edits[0].new_text.contains("using Microsoft.Sales;"));
        assert_eq!(edits[0].range.start.line, 1);
    }

    #[test]
    fn diagnostic_quick_fix_not_preferred_when_multiple_namespaces() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            make_entry_with_namespace(ObjectKind::Table, 18, "Customer", "Microsoft.Sales"),
            make_entry_with_namespace(ObjectKind::Table, 50100, "Customer", "MyCompany.CRM"),
        ]);

        let al_code = "namespace MyApp;\n\ncodeunit 50100 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'Customer' could not be found".to_string(),
            code: Some("AL0185".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert_eq!(actions.len(), 2);
        for a in &actions {
            assert!(
                !a.is_preferred,
                "should not be preferred when multiple options"
            );
        }
    }

    #[test]
    fn diagnostic_quick_fix_skipped_when_already_imported() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_entry_with_namespace(
            ObjectKind::Table,
            18,
            "Customer",
            "Microsoft.Sales",
        )]);

        let al_code = "namespace MyApp;\nusing Microsoft.Sales;\n\ncodeunit 50100 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'Customer' could not be found".to_string(),
            code: Some("AL0185".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert!(
            actions.is_empty(),
            "should not offer when namespace already imported"
        );
    }

    #[test]
    fn diagnostic_quick_fix_ignored_for_unrelated_diagnostic() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_entry_with_namespace(
            ObjectKind::Table,
            18,
            "Customer",
            "Microsoft.Sales",
        )]);

        let al_code = "namespace MyApp;\n\ncodeunit 50100 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Some other compile error".to_string(),
            code: Some("AL-L001".to_string()),
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert!(
            actions.is_empty(),
            "should return nothing for unrelated diagnostics"
        );
    }

    #[test]
    fn diagnostic_quick_fix_works_via_message_without_code() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_entry_with_namespace(
            ObjectKind::Codeunit,
            50100,
            "PostingSetup",
            "MyNs.Finance",
        )]);

        let al_code = "namespace MyApp;\n\ncodeunit 50200 Test { }\n";
        let uri = Url::parse("file:///test/T.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let diag = DiagnosticInfo {
            range: Range::default(),
            message: "Type 'PostingSetup' could not be found".to_string(),
            code: None, // no code — message-based detection
        };

        let actions = namespace_quick_fix_for_diagnostic(&ws, &uri, al_code, &diag);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].title.contains("MyNs.Finance"));
    }

    // Bug: extract_word_at_position closing quote
    // When cursor is on a closing `"` of a quoted identifier, the word
    // extracted should be the full inner name, not empty.
    #[test]
    fn extract_word_at_cursor_on_closing_quote_returns_inner_name() {
        // Line: `        SH: Record "Sales Header";`
        // Positions (0-based):
        //   `"` open  at col 19
        //   `S` at col 20
        //   `r` at col 31
        //   `"` close at col 32
        let text = "        SH: Record \"Sales Header\";\n";
        let range = Range {
            start: super::super::Position {
                line: 0,
                character: 32,
            },
            end: super::super::Position {
                line: 0,
                character: 32,
            },
        };

        let word = extract_word_at_position(text, range);
        assert_eq!(
            word, "Sales Header",
            "Should extract 'Sales Header' when cursor is on closing quote"
        );
    }

    fn cursor_word(text: &str, character: u32) -> String {
        extract_word_at_position(
            text,
            Range {
                start: super::super::Position { line: 0, character },
                end: super::super::Position { line: 0, character },
            },
        )
    }

    /// A cursor *between* two quoted identifiers used to pair the closing quote
    /// of the first with the opening quote of the second, yielding garbage.
    #[test]
    fn cursor_between_two_quoted_identifiers_extracts_nothing_bogus() {
        //                0         1         2         3         4
        //                0123456789012345678901234567890123456789012345
        let text = "    A: Record \"X\"; B: Record \"Y\";\n";
        let semicolon = text.find(';').unwrap() as u32;
        let word = cursor_word(text, semicolon);
        assert!(
            word.is_empty() || word == "X" || word == "Y",
            "must not synthesize a cross-identifier word, got {word:?}"
        );
        assert!(
            !word.contains("Record"),
            "must not span from one identifier to the next, got {word:?}"
        );
        // Inside each identifier the correct name is still extracted.
        let x_pos = text.find("\"X\"").unwrap() as u32 + 1;
        assert_eq!(cursor_word(text, x_pos), "X");
        let y_pos = text.find("\"Y\"").unwrap() as u32 + 1;
        assert_eq!(cursor_word(text, y_pos), "Y");
    }

    #[test]
    fn words_inside_comments_and_literals_are_ignored() {
        let comment = "    // Customer is used here\n";
        let offset = comment.find("Customer").unwrap() as u32 + 2;
        assert_eq!(cursor_word(comment, offset), "");

        let literal = "    Message('Customer was posted');\n";
        let offset = literal.find("Customer").unwrap() as u32 + 2;
        assert_eq!(cursor_word(literal, offset), "");

        // A real identifier on the same line is still extracted.
        let code = "    Cust: Record Customer; // Customer record\n";
        let offset = code.find("Record Customer").unwrap() as u32 + 8;
        assert_eq!(cursor_word(code, offset), "Customer");
    }

    #[test]
    fn add_using_not_offered_for_a_type_name_inside_a_comment() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[make_entry_with_namespace(
            ObjectKind::Table,
            18,
            "Customer",
            "Microsoft.Sales",
        )]);

        let al_code =
            "namespace MyApp;\n\ncodeunit 50100 Test\n{\n    // Customer lives elsewhere\n}\n";
        let uri = Url::parse("file:///test/Comment.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let character = al_code.lines().nth(4).unwrap().find("Customer").unwrap() as u32 + 2;
        let range = Range {
            start: super::super::Position { line: 4, character },
            end: super::super::Position { line: 4, character },
        };
        assert!(source_action_add_using(&ws, &uri, al_code, range).is_empty());
    }
}
