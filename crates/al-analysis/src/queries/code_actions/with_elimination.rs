//! With-statement elimination source action.

use url::Url;

use super::{detect_indent, single_edit_ws};
use super::{CodeActionEntry, CodeActionKind, Range, TextEdit};
use al_workspace::Workspace;

/// Collect every field name reachable through `record_var`'s table.
///
/// This must include the fields contributed by *table extensions* of that
/// table: after the `with` is removed those fields are just as unqualified as
/// the base table's, and leaving them alone produced code that no longer
/// compiles. Base-table fields come first, then each extension's, and duplicate
/// names (a base field re-listed by an extension) are collapsed.
fn resolve_with_field_names(
    workspace: &Workspace,
    tree: &tree_sitter::Tree,
    text: &str,
    record_var: &str,
) -> Vec<String> {
    let source = text.as_bytes();
    let table_name = find_record_type_for_var(tree.root_node(), source, record_var);
    let table_name = match table_name {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut names: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push_fields = |fields: &[al_symbols::FieldSymbol], names: &mut Vec<String>| {
        for field in fields {
            if field.name.is_empty() {
                continue;
            }
            if seen.insert(field.name.to_lowercase()) {
                names.push(field.name.clone());
            }
        }
    };

    for entry in &workspace.symbols.get_by_name(&table_name) {
        if entry.kind == al_symbols::ObjectKind::Table {
            push_fields(&entry.fields, &mut names);
        }
    }
    for entry in &workspace.symbols.get_extensions_of(&table_name) {
        if entry.kind == al_symbols::ObjectKind::TableExtension {
            push_fields(&entry.fields, &mut names);
        }
    }
    if names.is_empty() {
        // Fall back to a directly named tableextension entry (some symbol
        // sources record the extension under its own name only).
        for entry in &workspace.symbols.get_by_name(&table_name) {
            if entry.kind == al_symbols::ObjectKind::TableExtension {
                push_fields(&entry.fields, &mut names);
            }
        }
    }

    names
}

fn find_record_type_for_var(
    root: tree_sitter::Node,
    source: &[u8],
    var_name: &str,
) -> Option<String> {
    let var_lower = var_name.to_lowercase();
    find_record_type_recursive(root, source, &var_lower)
}

fn find_record_type_recursive(
    root: tree_sitter::Node,
    source: &[u8],
    var_lower: &str,
) -> Option<String> {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();

        if kind == "regular_variable_declaration" || kind == "parameter" {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name_text) = name_node.utf8_text(source) {
                    let name_clean = name_text.trim_matches('"').trim();
                    if name_clean.to_lowercase() == *var_lower {
                        if let Some(type_node) = node.child_by_field_name("type") {
                            return extract_record_subtype(type_node, source);
                        }
                    }
                }
            }
        }

        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }

    None
}

fn extract_record_subtype(type_node: tree_sitter::Node, source: &[u8]) -> Option<String> {
    let mut found_record_kw = false;
    let mut cursor = type_node.walk();
    for child in type_node.children(&mut cursor) {
        let kind = child.kind();
        if !found_record_kw {
            if kind.starts_with("kw_") || kind == "identifier" || kind == "name_or_keyword" {
                if let Ok(text) = child.utf8_text(source) {
                    if text.trim().to_lowercase() == "record" {
                        found_record_kw = true;
                    }
                }
            }
        } else {
            if let Ok(text) = child.utf8_text(source) {
                let trimmed = text.trim().trim_matches('"').trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Eliminate a `with` statement by qualifying field references with the record variable.
/// Detects `with X do begin...end` and replaces with the body, prefixing unqualified
/// identifiers with `X.` (AA0205 compliance).
pub(super) fn source_action_eliminate_with(
    workspace: &Workspace,
    uri: &Url,
    text: &str,
    range: Range,
) -> Option<CodeActionEntry> {
    let (_, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
    let root = tree.root_node();
    let source = text.as_bytes();

    // LSP positions use UTF-16 code units; tree-sitter uses byte offsets.
    let cursor_line = text.lines().nth(range.start.line as usize)?;
    let col_bytes =
        crate::resolution::utf16_col_to_byte_offset(cursor_line, range.start.character as usize);
    let point = tree_sitter::Point::new(range.start.line as usize, col_bytes);
    let with_node = find_with_at_point(root, point)?;

    let value_node = with_node.child_by_field_name("value")?;
    let record_var = value_node.utf8_text(source).ok()?.trim().to_string();
    if record_var.is_empty() {
        return None;
    }

    let body_node = with_node.child_by_field_name("body")?;

    let (body_text, _is_begin_end) = extract_with_body(body_node, source);

    let indent = detect_indent(text, with_node.start_position().row as u32);

    let field_names = resolve_with_field_names(workspace, &tree, text, &record_var);
    let own_procedures = collect_own_procedure_names(root, source);

    let qualified_body = qualify_with_references(
        &body_text,
        &record_var,
        &indent,
        &field_names,
        &own_procedures,
    );

    let edit = TextEdit {
        range: Range {
            start: super::Position {
                line: with_node.start_position().row as u32,
                character: 0,
            },
            end: super::Position {
                line: with_node.end_position().row as u32 + 1,
                character: 0,
            },
        },
        new_text: qualified_body,
    };

    Some(CodeActionEntry {
        title: format!("Eliminate with {} (AA0205)", record_var),
        kind: CodeActionKind::Refactor,
        edit: Some(single_edit_ws(uri, vec![edit])),
        is_preferred: false,
    })
}

fn find_with_at_point(
    root: tree_sitter::Node,
    point: tree_sitter::Point,
) -> Option<tree_sitter::Node> {
    let mut node = root.descendant_for_point_range(point, point)?;
    while node.kind() != "with_statement" {
        node = node.parent()?;
    }
    Some(node)
}

/// Extract the body text from a with statement body node.
/// Returns (body text, whether it was a begin..end block).
fn extract_with_body(body_node: tree_sitter::Node, source: &[u8]) -> (String, bool) {
    let inner = if body_node.kind() == "statement" && body_node.child_count() == 1 {
        body_node.child(0).unwrap_or(body_node)
    } else {
        body_node
    };

    if inner.kind() == "begin_end_block" {
        if let Some(stmt_list) = inner.child_by_field_name("body") {
            let text = stmt_list.utf8_text(source).unwrap_or("").to_string();
            return (text, true);
        }
        let mut cursor = inner.walk();
        for child in inner.children(&mut cursor) {
            if child.kind() == "statement_list" {
                let text = child.utf8_text(source).unwrap_or("").to_string();
                return (text, true);
            }
        }
    }

    let text = body_node.utf8_text(source).unwrap_or("").to_string();
    (text, false)
}

/// Qualify field references in with-body text by prepending the record variable.
///
/// Two-pass approach:
/// 1. Structural: if a line starts with an unqualified identifier followed by `:=` or `(`,
///    prepend `record_var.` to the whole line.
/// 2. Field-name: for each known field name, substitute unqualified occurrences with
///    `record_var.field` anywhere in the line (word-boundary, not already qualified).
fn qualify_with_references(
    body: &str,
    record_var: &str,
    indent: &str,
    field_names: &[String],
    own_procedures: &std::collections::HashSet<String>,
) -> String {
    let mut result = String::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            result.push_str(&format!("{}{}\n", indent, trimmed));
            continue;
        }

        let qualified_line =
            qualify_line_with_procs(trimmed, record_var, field_names, own_procedures);
        result.push_str(&format!("{}{}\n", indent, qualified_line));
    }

    result
}

/// Names of procedures/triggers declared by the object that contains the
/// `with` statement.
///
/// The structural pass qualifies any line starting with `Ident(`, which turned
/// a call to the object's *own* helper — `with Cust do begin Helper(); end` —
/// into `Cust.Helper();`, code the compiler rejects. Collecting the object's
/// declarations lets those calls be left alone.
fn collect_own_procedure_names(
    root: tree_sitter::Node,
    source: &[u8],
) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "procedure_declaration" | "trigger_declaration" | "event_procedure_declaration"
        ) {
            if let Some(name) = node
                .child_by_field_name("name")
                .and_then(|n| n.utf8_text(source).ok())
            {
                names.insert(name.trim().trim_matches('"').to_lowercase());
            }
            // Nested declarations do not exist in AL; skip the body.
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    names
}

/// Qualify a single line's identifiers with the record variable.
///
/// Two-pass approach:
/// 1. **Structural pass**: if the line starts with an unqualified identifier followed by
///    `:=` or `(`, prepend `record_var.` to the whole line.  Keywords like `if`, `begin`,
///    `end`, etc. are detected with word-boundary matching so field names that start with
///    a keyword prefix (e.g. `EndDate`, `FormatText`) are still qualified correctly.
/// 2. **Field-name pass**: for each known field name, substitute unqualified occurrences
///    anywhere in the line with `record_var.field` (word-boundary, not already preceded
///    by `.` or `:`).  This handles `if Field > 0`, `filter(Field = ...)`, etc.
#[cfg(test)]
fn qualify_line(line: &str, record_var: &str, field_names: &[String]) -> String {
    qualify_line_with_procs(
        line,
        record_var,
        field_names,
        &std::collections::HashSet::new(),
    )
}

fn qualify_line_with_procs(
    line: &str,
    record_var: &str,
    field_names: &[String],
    own_procedures: &std::collections::HashSet<String>,
) -> String {
    let trimmed = line.trim();

    // Skip lines that start with AL keywords (whole-word match).
    // Does NOT skip field names that begin with a keyword prefix (e.g. EndDate, IfFlag).
    // Keywords are looked up via LanguageData (loaded from tree-sitter-al/data/keywords.json).
    // "//" (line comment) and "end;" are not grammar keywords but must also be skipped here.
    let lower = trimmed.to_lowercase();
    let starts_with_keyword = {
        let keyword_match = lower
            .split_once(|c: char| !c.is_alphanumeric() && c != '_')
            .map(|(word, _)| al_syntax::language_data::is_keyword(word))
            .unwrap_or_else(|| al_syntax::language_data::is_keyword(&lower));
        // Also skip "//" (line comment start) and "end;" (not a grammar keyword but structural)
        let special_match = lower.starts_with("//") || lower.starts_with("end;");
        keyword_match || special_match
    };

    let structurally_qualified = if starts_with_keyword {
        // Cannot structurally qualify a keyword-led line — fall through to field-name pass.
        trimmed.to_string()
    } else if let Some(after_open) = trimmed.strip_prefix('"') {
        if let Some(end_quote) = after_open.find('"') {
            let after = after_open[end_quote + 1..].trim_start();
            if after.starts_with(":=") || after.starts_with('(') {
                format!("{}.{}", record_var, trimmed)
            } else {
                trimmed.to_string()
            }
        } else {
            trimmed.to_string()
        }
    } else {
        let first_word_end = trimmed
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(trimmed.len());
        let first_word = &trimmed[..first_word_end];
        if first_word.is_empty() {
            trimmed.to_string()
        } else {
            let after_word = trimmed[first_word_end..].trim_start();
            let word_lower = first_word.to_lowercase();
            // A call to the object's own procedure (or an AL built-in
            // function) is not a field access — qualifying it with the record
            // variable produces code that does not compile.
            let is_own_call = after_word.starts_with('(')
                && (own_procedures.contains(&word_lower)
                    || al_syntax::language_data::is_builtin_function(first_word));
            if after_word.starts_with('.') || after_word.starts_with("::") || is_own_call {
                trimmed.to_string()
            } else if after_word.starts_with(":=") || after_word.starts_with('(') {
                format!("{}.{}", record_var, trimmed)
            } else {
                trimmed.to_string()
            }
        }
    };

    if field_names.is_empty() {
        return structurally_qualified;
    }

    substitute_field_names(&structurally_qualified, record_var, field_names)
}

/// Replace all unqualified occurrences of known field names in `line` with
/// `record_var.field_name`.  An occurrence is unqualified when the character
/// immediately before it is NOT `.` or `:`.
///
/// Matching is case-insensitive; the replacement uses the original field name
/// casing from `field_names`.
///
/// Two properties this must hold:
/// * **String literals and line comments are never rewritten.** AL captions and
///   messages routinely contain field words — `Message('Name: %1', Name)` must
///   only qualify the *argument*, never the text inside `'...'`. The same
///   applies to everything after a `//`.
/// * **Linear cost.** The previous implementation lowercased the whole line
///   tail at every byte position for every field name (O(len² × fields) per
///   line). The line is now ASCII-lowercased once; because
///   `to_ascii_lowercase` is length-preserving, byte indices stay aligned with
///   the original text. AL's own identifier comparison is ASCII-case-insensitive,
///   so this is also the semantically correct fold.
fn substitute_field_names(line: &str, record_var: &str, field_names: &[String]) -> String {
    if field_names.is_empty() {
        return line.to_string();
    }

    // Sort longest-first to avoid shorter names shadowing longer ones.
    let mut sorted: Vec<(&str, String)> = field_names
        .iter()
        .filter(|f| !f.is_empty())
        .map(|f| (f.as_str(), f.to_ascii_lowercase()))
        .collect();
    sorted.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    if sorted.is_empty() {
        return line.to_string();
    }

    let lower = line.to_ascii_lowercase();
    debug_assert_eq!(lower.len(), line.len());
    let bytes = line.as_bytes();

    let mut output = String::with_capacity(line.len());
    let mut i = 0usize;

    while i < line.len() {
        // Everything from a `//` outside a literal to end of line is a comment.
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'/') {
            output.push_str(&line[i..]);
            return output;
        }

        // Single-quoted AL string literal — copied verbatim, including the
        // doubled-quote escape. `Message('Name: %1', Name)` must qualify only
        // the argument, never the text inside the literal.
        if bytes[i] == b'\'' {
            let end = end_of_string_literal(line, i);
            output.push_str(&line[i..end]);
            i = end;
            continue;
        }

        // Double-quoted AL *identifier* — a real field reference such as
        // `"No."`. Match the whole quoted span against the field list rather
        // than its interior, so the replacement stays a legal identifier.
        if bytes[i] == b'"' {
            let close = line[i + 1..].find('"').map(|offset| i + 1 + offset);
            let end = close.map(|c| c + 1).unwrap_or(line.len());
            let inner = &line[i + 1..close.unwrap_or(line.len())];
            if close.is_some() && prev_char_allows_match(line, i) {
                if let Some((name, _)) = sorted
                    .iter()
                    .find(|(name, _)| inner.eq_ignore_ascii_case(name))
                {
                    output.push_str(record_var);
                    output.push('.');
                    output.push('"');
                    output.push_str(name);
                    output.push('"');
                    i = end;
                    continue;
                }
            }
            output.push_str(&line[i..end]);
            i = end;
            continue;
        }

        if prev_char_allows_match(line, i) {
            let mut matched = None;
            for (name, name_lower) in &sorted {
                if !lower[i..].starts_with(name_lower.as_str()) {
                    continue;
                }
                let match_end = i + name.len();
                let next_ok = match line[match_end..].chars().next() {
                    None => true,
                    Some(next_char) => !next_char.is_alphanumeric() && next_char != '_',
                };
                if next_ok {
                    matched = Some((*name, match_end));
                    break;
                }
            }
            if let Some((name, match_end)) = matched {
                output.push_str(record_var);
                output.push('.');
                output.push_str(name);
                i = match_end;
                continue;
            }
        }

        let step = char_len_at(line, i);
        output.push_str(&line[i..i + step]);
        i += step;
    }

    output
}

/// Word-boundary check: the character before byte `i` must not be `.`, `:`, an
/// alphanumeric, or `_` — otherwise the occurrence is already qualified or part
/// of a longer identifier.
fn prev_char_allows_match(line: &str, i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let prev_char = line[..i].chars().next_back().unwrap_or(' ');
    prev_char != '.' && prev_char != ':' && !prev_char.is_alphanumeric() && prev_char != '_'
}

/// Byte offset one past the closing quote of the AL string literal starting at
/// `start` (which must be a `'`). Doubled quotes (`''`) are escapes, not
/// terminators. An unterminated literal consumes the rest of the line.
fn end_of_string_literal(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let mut j = start + 1;
    while j < line.len() {
        if bytes[j] == b'\'' {
            if bytes.get(j + 1) == Some(&b'\'') {
                j += 2;
                continue;
            }
            return j + 1;
        }
        j += char_len_at(line, j);
    }
    line.len()
}

/// UTF-8 length of the character starting at byte `i`.
fn char_len_at(s: &str, i: usize) -> usize {
    s[i..].chars().next().map(char::len_utf8).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::code_actions::source_actions;
    use al_workspace::Workspace;
    use url::Url;

    fn open_doc(ws: &al_workspace::Workspace, uri: &url::Url, al_code: &str) {
        ws.documents.open(uri.clone(), al_code.to_string()).unwrap();
    }

    #[test]
    fn with_elimination_offered_on_with_statement() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    var
        Cust: Record Customer;
    begin
        with Cust do begin
            Name := 'Test';
            "No." := '10000';
        end;
    end;
}
"#;
        let uri = Url::parse("file:///test/With.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 6,
                character: 8,
            },
            end: super::super::Position {
                line: 6,
                character: 8,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let with_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("with"))
            .collect();

        assert!(
            !with_actions.is_empty(),
            "Should offer 'Eliminate with' action"
        );

        let edit = with_actions[0].edit.as_ref().expect("should have edit");
        let (_, edits) = &edit.changes[0];
        let new_text = &edits[0].new_text;
        assert!(
            new_text.contains("Cust.Name"),
            "Should qualify Name with Cust"
        );
        assert!(
            new_text.contains("Cust.\"No.\""),
            "Should qualify \"No.\" with Cust"
        );
        assert!(
            !new_text.contains("with Cust do"),
            "Should remove with wrapper"
        );
    }

    #[test]
    fn qualify_line_does_not_skip_keyword_prefixed_identifiers() {
        // Field names that start with AL keywords must still be qualified.
        // e.g. EndDate, FormatText, CaseNo must not be silently dropped by
        // the whole-word keyword guard.
        assert!(
            qualify_line("EndDate := Today;", "Rec", &[]).starts_with("Rec."),
            "EndDate starts with 'end' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("FormatText := 'X';", "Rec", &[]).starts_with("Rec."),
            "FormatText starts with 'for' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("CaseNo := 1;", "Rec", &[]).starts_with("Rec."),
            "CaseNo starts with 'case' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("IfFlag := true;", "Rec", &[]).starts_with("Rec."),
            "IfFlag starts with 'if' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("MessageText := '';", "Rec", &[]).starts_with("Rec."),
            "MessageText starts with 'message' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("ExitCode := 0;", "Rec", &[]).starts_with("Rec."),
            "ExitCode starts with 'exit' but is not a keyword — must be qualified"
        );
        assert!(
            qualify_line("ErrorText := '';", "Rec", &[]).starts_with("Rec."),
            "ErrorText starts with 'error' but is not a keyword — must be qualified"
        );
    }

    #[test]
    fn qualify_line_skips_al_keywords_exactly() {
        assert_eq!(qualify_line("end;", "Rec", &[]), "end;");
        assert_eq!(qualify_line("begin", "Rec", &[]), "begin");
        assert_eq!(qualify_line("end", "Rec", &[]), "end");
        assert!(qualify_line("if x = 1 then", "Rec", &[]).starts_with("if"));
        assert!(qualify_line("for i := 1 to 10 do", "Rec", &[]).starts_with("for"));
        assert!(qualify_line("while x > 0 do", "Rec", &[]).starts_with("while"));
        assert!(qualify_line("case x of", "Rec", &[]).starts_with("case"));
    }

    #[test]
    fn qualify_line_skips_already_qualified_references() {
        // Rec.Field := X should not become Rec2.Rec.Field := X
        assert_eq!(
            qualify_line("Rec.Name := 'X';", "Rec2", &[]),
            "Rec.Name := 'X';"
        );
        assert_eq!(
            qualify_line("Customer.\"No.\" := '100';", "Rec", &[]),
            "Customer.\"No.\" := '100';"
        );
    }

    #[test]
    fn with_elimination_not_offered_outside_with() {
        let ws = Workspace::new();

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    begin
        Message('Hello');
    end;
}
"#;
        let uri = Url::parse("file:///test/NoWith.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 4,
                character: 8,
            },
            end: super::super::Position {
                line: 4,
                character: 8,
            },
        };

        let actions = source_actions(&ws, &uri, range);
        let with_actions: Vec<_> = actions
            .iter()
            .filter(|a| a.title.contains("with"))
            .collect();

        assert!(with_actions.is_empty(), "Should NOT offer on non-with code");
    }

    fn field(name: &str) -> al_symbols::FieldSymbol {
        al_symbols::FieldSymbol {
            id: 0,
            name: name.to_string(),
            type_name: "Text".to_string(),
            properties: Vec::new(),
        }
    }

    fn table_entry(
        kind: al_symbols::ObjectKind,
        name: &str,
        extends: Option<&str>,
        fields: Vec<al_symbols::FieldSymbol>,
    ) -> al_symbols::SymbolEntry {
        al_symbols::SymbolEntry {
            synthetic: false,
            kind,
            id: 50100,
            name: name.to_string(),
            extends: extends.map(str::to_string),
            implements: Vec::new(),
            package: "Test".to_string(),
            namespace: String::new(),
            methods: Vec::new(),
            fields,
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    /// Field words appear all the time inside AL captions and messages;
    /// rewriting them corrupts user-visible text.
    #[test]
    fn substitute_field_names_leaves_string_literals_and_comments_alone() {
        let fields = vec!["Name".to_string()];
        assert_eq!(
            substitute_field_names("Message('Name: %1', Name);", "Cust", &fields),
            "Message('Name: %1', Cust.Name);"
        );
        assert_eq!(
            substitute_field_names("x := 1; // set the Name here", "Cust", &fields),
            "x := 1; // set the Name here"
        );
        assert_eq!(
            substitute_field_names("Message('it''s a Name', Name);", "Cust", &fields),
            "Message('it''s a Name', Cust.Name);"
        );
    }

    #[test]
    fn substitute_field_names_qualifies_quoted_identifiers_once() {
        let fields = vec!["No.".to_string()];
        assert_eq!(
            substitute_field_names("\"No.\" := '10000';", "Cust", &fields),
            "Cust.\"No.\" := '10000';"
        );
        // Already qualified — must not be re-qualified.
        assert_eq!(
            substitute_field_names("Cust.\"No.\" := '10000';", "Cust", &fields),
            "Cust.\"No.\" := '10000';"
        );
    }

    /// The structural pass qualified any line starting with `Ident(`, so a call
    /// to the object's own helper became `Cust.Helper();`, which does not compile.
    #[test]
    fn with_elimination_does_not_qualify_own_procedure_calls() {
        let ws = Workspace::new();
        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    var
        Cust: Record Customer;
    begin
        with Cust do begin
            Helper();
            Message('done');
        end;
    end;

    local procedure Helper()
    begin
    end;
}
"#;
        let uri = Url::parse("file:///test/WithOwnProc.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 6,
                character: 8,
            },
            end: super::super::Position {
                line: 6,
                character: 8,
            },
        };
        let action = source_action_eliminate_with(&ws, &uri, al_code, range)
            .expect("eliminate-with action should be offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "with_elimination own procedure",
        );
        assert!(
            !updated.contains("Cust.Helper"),
            "own-procedure call must not be qualified: {updated}"
        );
        assert!(updated.contains("Helper();"), "{updated}");
    }

    /// Fields contributed by a tableextension are just as unqualified as the
    /// base table's once the `with` is gone.
    #[test]
    fn with_elimination_qualifies_table_extension_fields() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[
            table_entry(
                al_symbols::ObjectKind::Table,
                "Customer",
                None,
                vec![field("Name")],
            ),
            table_entry(
                al_symbols::ObjectKind::TableExtension,
                "Customer Ext",
                Some("Customer"),
                vec![field("Loyalty Points")],
            ),
        ]);

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    var
        Cust: Record Customer;
    begin
        with Cust do begin
            Name := 'X';
            "Loyalty Points" := 1;
        end;
    end;
}
"#;
        let uri = Url::parse("file:///test/WithExtFields.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 6,
                character: 8,
            },
            end: super::super::Position {
                line: 6,
                character: 8,
            },
        };
        let action = source_action_eliminate_with(&ws, &uri, al_code, range)
            .expect("eliminate-with action should be offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "with_elimination tableextension fields",
        );
        assert!(updated.contains("Cust.Name"), "{updated}");
        assert!(
            updated.contains("Cust.\"Loyalty Points\""),
            "tableextension field must be qualified: {updated}"
        );
    }

    #[test]
    fn with_elimination_edit_reparses_cleanly() {
        let ws = Workspace::new();
        ws.symbols.add_entries(&[table_entry(
            al_symbols::ObjectKind::Table,
            "Customer",
            None,
            vec![field("Name"), field("No.")],
        )]);

        let al_code = r#"codeunit 50100 "My Codeunit"
{
    procedure DoStuff()
    var
        Cust: Record Customer;
    begin
        with Cust do begin
            Name := 'Test';
            "No." := '10000';
            Message('Name is %1', Name);
        end;
    end;
}
"#;
        let uri = Url::parse("file:///test/WithApply.al").unwrap();
        open_doc(&ws, &uri, al_code);

        let range = Range {
            start: super::super::Position {
                line: 6,
                character: 8,
            },
            end: super::super::Position {
                line: 6,
                character: 8,
            },
        };
        let action = source_action_eliminate_with(&ws, &uri, al_code, range)
            .expect("eliminate-with action should be offered");
        let updated = super::super::test_support::assert_action_applies_cleanly(
            al_code,
            &action,
            "with_elimination",
        );
        assert!(updated.contains("Cust.Name := 'Test';"), "{updated}");
        assert!(updated.contains("Cust.\"No.\" := '10000';"), "{updated}");
        assert!(
            updated.contains("Message('Name is %1', Cust.Name);"),
            "literal must be untouched: {updated}"
        );
    }
}
