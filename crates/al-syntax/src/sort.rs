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

    let body_open = lines.iter().position(|l| l.trim() == "{")?;
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
        return Some(text.to_string());
    }

    // At most one object-level `var` block is legal; anything further is either
    // malformed input or a mis-split. Keep the first and pass the rest through
    // as `other` so no line can be dropped on the floor.
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
            if var_block.is_none() {
                var_block = Some(member);
            } else {
                other.push(member);
            }
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

    triggers.sort_by_key(|a| a.0.to_lowercase());
    procedures.sort_by_key(|a| a.0.to_lowercase());

    let mut result_lines: Vec<&str> = header.to_vec();

    if let Some(vb) = var_block {
        for l in vb {
            result_lines.push(l);
        }
    }

    for (_, member) in triggers {
        for l in member {
            result_lines.push(l);
        }
    }

    for (_, member) in procedures {
        for l in member {
            result_lines.push(l);
        }
    }

    for member in other {
        for l in member {
            result_lines.push(l);
        }
    }

    for l in footer {
        result_lines.push(l);
    }

    // Sorting is a pure reordering: every input line must appear in the output
    // exactly as many times as it did on the way in. Bail out (no edit) rather
    // than hand back a mangled object if the member split ever mis-segments —
    // this runs behind an editor command, so a silent drop is lost user code.
    if !is_pure_reordering(&lines, &result_lines) {
        return None;
    }

    let mut out = result_lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// True when `after` is a permutation of `before` — same lines, same counts.
fn is_pure_reordering(before: &[&str], after: &[&str]) -> bool {
    if before.len() != after.len() {
        return false;
    }
    let mut a: Vec<&str> = before.to_vec();
    let mut b: Vec<&str> = after.to_vec();
    a.sort_unstable();
    b.sort_unstable();
    a == b
}

/// Split body lines into member blocks.
///
/// A member starts at a member keyword that is at the object's own level —
/// outside any `{ … }` sub-block (`fields`, `keys`, `layout`, `actions`, …) and
/// outside any comment.
///
/// `var` needs one extra condition. AL procedure bodies are delimited by
/// `begin`/`end`, not braces, so brace depth alone cannot tell an object-level
/// `var` block from a procedure's *local* `var` section — and treating the
/// latter as a member start splits a procedure in half, orphaning its header
/// from its body and (because only one `var` member survives) silently dropping
/// code.
///
/// A procedure's local declarations are exactly the `var` section between its
/// header and its `begin`. So a `var` is local — and therefore not a member
/// start — only while the current block has a procedure/trigger header whose
/// `begin` has not been seen yet. An object-level `var` written after a
/// procedure's body still starts a member. `procedure`/`trigger`/`[attribute]`
/// are unambiguous member starts either way.
fn split_into_members<'a>(lines: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut members: Vec<Vec<&'a str>> = Vec::new();
    let mut current: Vec<&'a str> = Vec::new();
    let mut depth = 0i32;
    let mut in_block_comment = false;
    // Does the block being accumulated have a procedure/trigger header …
    let mut current_has_body_member = false;
    // … and has that member's body opened yet?
    let mut seen_begin = false;

    for &line in lines {
        let (code, still_in_comment) = crate::formatting::strip_comments(line, in_block_comment);
        let was_in_comment = in_block_comment;
        in_block_comment = still_in_comment;

        let trimmed = code.trim().to_lowercase();
        let is_var_keyword =
            trimmed == "var" || trimmed.starts_with("var ") || trimmed.starts_with("var\t");
        let is_local_var = is_var_keyword && current_has_body_member && !seen_begin;

        let is_member_start =
            depth == 0 && !was_in_comment && is_member_keyword(&trimmed) && !is_local_var;

        if is_member_start && !current.is_empty() {
            members.push(current);
            current = Vec::new();
            current_has_body_member = false;
            seen_begin = false;
        }
        if is_member_start && !is_var_keyword {
            current_has_body_member = true;
        }
        if trimmed == "begin" || trimmed.ends_with(" begin") {
            seen_begin = true;
        }

        // Brace depth from the comment-stripped code; `count_net_delimiters`
        // also skips `'…'` literals and `"…"` identifiers, so a brace inside
        // `Caption = '{'` or a quoted name cannot corrupt the counter.
        depth += crate::count_net_delimiters(&code, '{', '}');

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

    /// Non-whitespace characters, sorted — sorting is a reordering, so this
    /// multiset must be identical before and after.
    fn content_multiset(s: &str) -> Vec<char> {
        let mut v: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
        v.sort_unstable();
        v
    }

    #[test]
    fn procedures_with_local_vars_keep_their_bodies() {
        // A procedure's local `var` section used to be treated as an
        // object-level member start. That split each procedure into a bodiless
        // header plus an orphaned `var` member — and since only one `var`
        // member was kept, every other one was silently discarded.
        let input = "\
codeunit 50104 \"T\"
{
    var
        GlobalCounter: Integer;

    procedure Zed()
    var
        LocalVar: Integer;
    begin
        LocalVar := 1;
        GlobalCounter += LocalVar;
    end;

    procedure Alpha()
    var
        Other: Text;
    begin
        Other := 'x';
    end;
}
";
        let out = sort_members(input).expect("should sort");
        assert_eq!(
            content_multiset(input),
            content_multiset(&out),
            "sorting dropped content:\n{out}"
        );
        assert!(out.contains("GlobalCounter: Integer;"), "{out}");

        let alpha = out.find("procedure Alpha()").expect("Alpha present");
        let zed = out.find("procedure Zed()").expect("Zed present");
        assert!(alpha < zed, "Alpha must sort before Zed:\n{out}");
        let alpha_body = &out[alpha..zed];
        assert!(
            alpha_body.contains("Other: Text;") && alpha_body.contains("Other := 'x';"),
            "Alpha lost its local var / body:\n{out}"
        );
        let zed_body = &out[zed..];
        assert!(
            zed_body.contains("LocalVar: Integer;") && zed_body.contains("LocalVar := 1;"),
            "Zed lost its local var / body:\n{out}"
        );
    }

    #[test]
    fn sorting_is_always_a_pure_reordering() {
        let cases = [
            "codeunit 50100 T\n{\n}\n",
            "codeunit 50100 T\n{\n    /* procedure Fake()\n       var x: Integer;\n    */\n    procedure B() begin end;\n    procedure A() begin end;\n}\n",
            "codeunit 50100 T\n{\n    procedure B()\n    begin\n        Message('var');\n    end;\n\n    procedure A() begin end;\n}\n",
            "table 50100 T\n{\n    fields\n    {\n        field(1; A; Integer) { }\n    }\n\n    var\n        G: Integer;\n\n    procedure B()\n    var\n        L: Integer;\n    begin\n    end;\n\n    procedure A() begin end;\n}\n",
            "interface IThing\n{\n    procedure Zed(): Text;\n    procedure Alpha();\n}\n",
        ];
        for input in cases {
            let Some(out) = sort_members(input) else {
                continue;
            };
            assert_eq!(
                content_multiset(input),
                content_multiset(&out),
                "sorting dropped content for {input:?}:\n{out}"
            );
            let twice = sort_members(&out).expect("second pass must still sort");
            assert_eq!(out, twice, "sorting is not idempotent for {input:?}");
        }
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
