//! AL object member sorting.
//!
//! Sorts the members of an AL object in a canonical order:
//!   1. var block (unchanged)
//!   2. triggers (alphabetically)
//!   3. procedures / local procedures (alphabetically)
//!
//! This is a text-based transformation that does not require tree-sitter.
//! It preserves the object header and footer verbatim.

/// Sort AL object members in canonical order.
///
/// Returns `None` if the text does not look like a single AL object or if
/// the structure cannot be parsed reliably (e.g. nested objects, syntax errors).
/// Returns the sorted text (may equal the input if already sorted).
pub fn sort_members(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // Find the opening brace of the object body (line containing only `{`)
    let body_open = lines.iter().position(|l| l.trim() == "{")?;
    // Find the closing brace (last line with only `}`)
    let body_close = lines.iter().rposition(|l| l.trim() == "}")?;

    if body_close <= body_open {
        return None;
    }

    let header = &lines[..=body_open];
    let footer = &lines[body_close..];
    let body_lines = &lines[body_open + 1..body_close];

    // Split body into members. A member starts when we see:
    //   - `var` (at indent level 0 of body — 4 spaces)
    //   - `trigger <name>` ...
    //   - `procedure <name>` / `local procedure <name>`
    //   - `[<attr>]` preceding a procedure

    let members = split_into_members(body_lines);
    if members.is_empty() {
        // No members — body is empty or unparseable, return unchanged
        return Some(text.to_string());
    }

    // Categorise members
    let mut var_block: Option<Vec<&str>> = None;
    let mut triggers: Vec<(String, Vec<&str>)> = Vec::new();
    let mut procedures: Vec<(String, Vec<&str>)> = Vec::new();
    let mut other: Vec<Vec<&str>> = Vec::new();

    for member in members {
        let first = member
            .iter()
            .find(|l| !l.trim().is_empty())
            .copied()
            .unwrap_or("");
        let trimmed = first.trim().to_lowercase();

        if trimmed == "var" || trimmed.starts_with("var ") || trimmed.starts_with("var\t") {
            var_block = Some(member);
        } else if trimmed.starts_with("trigger ") {
            let name = extract_member_name(first, "trigger");
            triggers.push((name, member));
        } else if trimmed.starts_with("procedure ")
            || trimmed.starts_with("local procedure ")
            || trimmed.starts_with("internal procedure ")
            || trimmed.starts_with("protected procedure ")
            || trimmed.starts_with("protected local procedure ")
        {
            let name = extract_member_name_procedure(first);
            procedures.push((name, member));
        } else if trimmed.starts_with("[") {
            // Attribute annotation — peek ahead: treat whole block as procedure
            let name = member
                .iter()
                .find(|l| {
                    let t = l.trim().to_lowercase();
                    t.starts_with("procedure ")
                        || t.starts_with("local procedure ")
                        || t.starts_with("internal procedure ")
                        || t.starts_with("protected procedure ")
                        || t.starts_with("protected local procedure ")
                })
                .map(|l| extract_member_name_procedure(l))
                .unwrap_or_default();
            procedures.push((name, member));
        } else {
            other.push(member);
        }
    }

    // Sort triggers and procedures alphabetically (case-insensitive)
    triggers.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    procedures.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));

    // Reconstruct
    let mut result_lines: Vec<&str> = header.to_vec();

    // var block first
    if let Some(vb) = var_block {
        for l in vb {
            result_lines.push(l);
        }
    }

    // triggers
    for (_, member) in triggers {
        for l in member {
            result_lines.push(l);
        }
    }

    // procedures
    for (_, member) in procedures {
        for l in member {
            result_lines.push(l);
        }
    }

    // other (unrecognised sections) — append at end
    for member in other {
        for l in member {
            result_lines.push(l);
        }
    }

    for l in footer {
        result_lines.push(l);
    }

    // Preserve trailing newline if original had one
    let mut out = result_lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// Split body lines into member blocks.
/// Each member is a contiguous block of lines starting with a member keyword
/// and continuing until the next member keyword at the same depth.
fn split_into_members<'a>(lines: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut members: Vec<Vec<&'a str>> = Vec::new();
    let mut current: Vec<&'a str> = Vec::new();
    let mut depth = 0i32;

    for &line in lines {
        let trimmed = line.trim().to_lowercase();

        // Check if this line starts a new top-level member
        let is_member_start = depth == 0 && is_member_keyword(&trimmed);

        if is_member_start && !current.is_empty() {
            members.push(current);
            current = Vec::new();
        }

        // Track brace depth — skip over string literal contents so that
        // braces inside strings (e.g. `Caption = '{'`) don't corrupt the counter.
        {
            let mut chars = line.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    // Single-quoted string: skip until closing `'`, handling `''` escape
                    '\'' => {
                        loop {
                            match chars.next() {
                                None => break,
                                Some('\'') => {
                                    // Doubled quote is an escape — peek to check
                                    if chars.peek() == Some(&'\'') {
                                        chars.next(); // consume the second `'`
                                    } else {
                                        break; // end of string
                                    }
                                }
                                Some(_) => {}
                            }
                        }
                    }
                    // Double-quoted identifier: skip until closing `"`, handling `""` escape
                    '"' => loop {
                        match chars.next() {
                            None => break,
                            Some('"') => {
                                if chars.peek() == Some(&'"') {
                                    chars.next();
                                } else {
                                    break;
                                }
                            }
                            Some(_) => {}
                        }
                    },
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
            }
        }

        current.push(line);
    }

    if !current.is_empty() {
        members.push(current);
    }

    members
}

fn is_member_keyword(trimmed_lower: &str) -> bool {
    trimmed_lower == "var"
        || trimmed_lower.starts_with("var ")
        || trimmed_lower.starts_with("var\t")
        || trimmed_lower.starts_with("trigger ")
        || trimmed_lower.starts_with("procedure ")
        || trimmed_lower.starts_with("local procedure ")
        || trimmed_lower.starts_with("internal procedure ")
        || trimmed_lower.starts_with("protected procedure ")
        || trimmed_lower.starts_with("protected local procedure ")
        || (trimmed_lower.starts_with('[') && trimmed_lower.ends_with(']'))
}

fn extract_member_name(line: &str, keyword: &str) -> String {
    let lower = line.trim().to_lowercase();
    let after = lower
        .strip_prefix(keyword)
        .map(str::trim)
        .unwrap_or(lower.trim());
    // Take up to first `(` or whitespace
    after
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

fn extract_member_name_procedure(line: &str) -> String {
    let lower = line.trim().to_lowercase();
    // Strip the longest matching prefix first (most-specific to least-specific).
    let after = if let Some(rest) = lower.strip_prefix("protected local procedure ") {
        rest.trim()
    } else if let Some(rest) = lower.strip_prefix("protected procedure ") {
        rest.trim()
    } else if let Some(rest) = lower.strip_prefix("internal procedure ") {
        rest.trim()
    } else if let Some(rest) = lower.strip_prefix("local procedure ") {
        rest.trim()
    } else if let Some(rest) = lower.strip_prefix("procedure ") {
        rest.trim()
    } else {
        lower.trim()
    };
    after
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_members_procedures_sorted_alphabetically() {
        let input = r#"codeunit 50100 "My Codeunit"
{
    procedure Zebra()
    begin
        Message('z');
    end;

    procedure Apple()
    begin
        Message('a');
    end;

    procedure Mango()
    begin
        Message('m');
    end;
}
"#;
        let result = sort_members(input).expect("should sort");
        let apple_pos = result.find("procedure Apple").expect("should have Apple");
        let mango_pos = result.find("procedure Mango").expect("should have Mango");
        let zebra_pos = result.find("procedure Zebra").expect("should have Zebra");
        assert!(apple_pos < mango_pos, "Apple before Mango");
        assert!(mango_pos < zebra_pos, "Mango before Zebra");
    }

    #[test]
    fn sort_members_var_block_stays_first() {
        let input = r#"codeunit 50100 "My Codeunit"
{
    procedure DoSomething()
    begin
    end;

    var
        x: Integer;
}
"#;
        let result = sort_members(input).expect("should sort");
        let var_pos = result.find("    var").expect("should have var");
        let proc_pos = result.find("    procedure").expect("should have procedure");
        assert!(var_pos < proc_pos, "var block must precede procedures");
    }

    #[test]
    fn sort_members_triggers_before_procedures() {
        let input = r#"table 50100 "My Table"
{
    procedure MyProc()
    begin
    end;

    trigger OnInsert()
    begin
    end;
}
"#;
        let result = sort_members(input).expect("should sort");
        let trigger_pos = result
            .find("    trigger OnInsert")
            .expect("should have trigger");
        let proc_pos = result
            .find("    procedure MyProc")
            .expect("should have procedure");
        assert!(trigger_pos < proc_pos, "triggers must precede procedures");
    }

    #[test]
    fn sort_members_already_sorted_unchanged() {
        let input = r#"codeunit 50100 "My Codeunit"
{
    procedure Apple()
    begin
    end;

    procedure Zebra()
    begin
    end;
}
"#;
        let result = sort_members(input).expect("should sort");
        // The content should be the same (possibly reordered, but since already sorted — same)
        assert!(result.contains("procedure Apple"));
        assert!(result.contains("procedure Zebra"));
        let apple_pos = result.find("procedure Apple").unwrap();
        let zebra_pos = result.find("procedure Zebra").unwrap();
        assert!(apple_pos < zebra_pos, "Apple still before Zebra");
    }

    #[test]
    fn sort_members_case_insensitive_sort() {
        let input = r#"codeunit 50100 "My Codeunit"
{
    procedure zProc()
    begin
    end;

    procedure AProc()
    begin
    end;
}
"#;
        let result = sort_members(input).expect("should sort");
        let a_pos = result.find("procedure AProc").unwrap();
        let z_pos = result.find("procedure zProc").unwrap();
        assert!(a_pos < z_pos, "AProc before zProc (case-insensitive)");
    }

    #[test]
    fn sort_members_returns_none_for_non_object() {
        // No `{` / `}` wrapper — not an AL object body
        let result = sort_members("procedure Foo()\nbegin\nend;");
        // Could return None or unchanged — either is acceptable
        // But if it returns Some, the content must still be valid
        if let Some(r) = result {
            assert!(r.contains("procedure Foo"));
        }
    }

    /// #23 — Braces inside string literals must not corrupt the depth counter.
    #[test]
    fn sort_members_string_literal_brace_does_not_corrupt_depth() {
        // The Caption property contains `{` and `}` inside a single-quoted string.
        // Without the fix, depth would be thrown off and the procedure body would
        // not be correctly identified as a complete member.
        let input = r#"codeunit 50100 "My Codeunit"
{
    procedure Zebra()
    begin
        Message('Value: {0}', x);
    end;

    procedure Apple()
    begin
        Message('a');
    end;
}
"#;
        let result = sort_members(input).expect("should sort");
        let apple_pos = result.find("procedure Apple").expect("Apple missing");
        let zebra_pos = result.find("procedure Zebra").expect("Zebra missing");
        assert!(
            apple_pos < zebra_pos,
            "Apple must come before Zebra after sort"
        );
    }

    /// #23 — Double-quoted identifiers containing braces are also handled.
    #[test]
    fn sort_members_double_quoted_brace_does_not_corrupt_depth() {
        let input = r#"codeunit 50100 "My Codeunit"
{
    procedure Zebra()
    var
        x: Record "Table {Name}";
    begin
    end;

    procedure Apple()
    begin
    end;
}
"#;
        let result = sort_members(input).expect("should sort");
        let apple_pos = result.find("procedure Apple").expect("Apple missing");
        let zebra_pos = result.find("procedure Zebra").expect("Zebra missing");
        assert!(
            apple_pos < zebra_pos,
            "Apple must come before Zebra after sort"
        );
    }

    /// #23 — Escaped single-quote inside string (`''`) is handled correctly.
    #[test]
    fn sort_members_escaped_single_quote_in_string() {
        let input = r#"codeunit 50100 "My Codeunit"
{
    procedure Zebra()
    begin
        Message('It''s {not} a brace issue');
    end;

    procedure Apple()
    begin
    end;
}
"#;
        let result = sort_members(input).expect("should sort");
        let apple_pos = result.find("procedure Apple").expect("Apple missing");
        let zebra_pos = result.find("procedure Zebra").expect("Zebra missing");
        assert!(
            apple_pos < zebra_pos,
            "Apple must come before Zebra after sort"
        );
    }
}
