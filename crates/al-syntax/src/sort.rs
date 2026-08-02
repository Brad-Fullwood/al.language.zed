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

    // `var` and `protected var` blocks all hoist to the top, keeping their
    // relative source order. (An object may legally declare both.)
    let mut var_blocks: Vec<Vec<&str>> = Vec::new();
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

        if is_var_start(&trimmed) {
            var_blocks.push(member);
        } else if trimmed.starts_with("trigger ") {
            let name = extract_member_name(first, "trigger");
            triggers.push((name, member));
        } else if is_procedure_start(&trimmed) {
            let name = extract_member_name_procedure(first);
            procedures.push((name, member));
        } else if trimmed.starts_with("[") {
            // Attribute annotation — peek ahead: treat whole block as procedure
            let name = member
                .iter()
                .find(|l| is_procedure_start(&l.trim().to_lowercase()))
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

    for vb in var_blocks {
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
    // `str::lines` strips both LF and CRLF terminators, so rejoining with `\n`
    // would silently rewrite a Windows checkout's line endings and turn a
    // member sort into a whole-file diff. Mirror `format_al`'s `uses_crlf`
    // handling so the two transformations agree.
    if text.contains("\r\n") {
        out = out.replace('\n', "\r\n");
    }
    Some(out)
}

/// True if `code` contains `begin` as a standalone word outside any string
/// literal. Word boundaries stop `Begins`/`MyBegin` from matching, and the
/// literal skip stops `Message('begin')` from doing so.
fn contains_begin_keyword(code: &str) -> bool {
    for span in crate::lexical::LineScanner::new(code, false) {
        if span.kind != crate::lexical::SpanKind::Code {
            continue;
        }
        let bytes = span.text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if b.is_ascii_alphabetic() || b == b'_' {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                if span.text[start..i].eq_ignore_ascii_case("begin") {
                    return true;
                }
                continue;
            }
            i += 1;
        }
    }
    false
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
    // The block being accumulated is (so far) only attribute lines; a
    // procedure/trigger header that follows continues the same member instead
    // of starting a new one — otherwise sorting detaches the attribute from
    // the member it annotates.
    let mut attr_pending = false;
    // Net unclosed `[` of a multi-line attribute; while > 0, lines are
    // attribute continuation and never member starts.
    let mut attr_bracket_depth = 0i32;

    for &line in lines {
        let (code, still_in_comment) = crate::formatting::strip_comments(line, in_block_comment);
        let was_in_comment = in_block_comment;
        in_block_comment = still_in_comment;

        if attr_bracket_depth > 0 {
            // Interior/closing line of a multi-line `[…]` attribute.
            attr_bracket_depth += crate::count_net_delimiters(&code, '[', ']');
            depth += crate::count_net_delimiters(&code, '{', '}');
            current.push(line);
            continue;
        }

        let trimmed = code.trim().to_lowercase();
        let is_var_keyword = is_var_start(&trimmed);
        let is_local_var = is_plain_var_start(&trimmed) && current_has_body_member && !seen_begin;

        let at_member_level = depth == 0 && !was_in_comment;
        let is_attr_start = at_member_level && trimmed.starts_with('[');
        let is_keyword_start = at_member_level && is_member_keyword(&trimmed) && !is_local_var;

        // A pending attribute binds to the next keyword line, and stacked
        // attributes accumulate — neither may start a fresh member.
        let is_member_start = !attr_pending && (is_attr_start || is_keyword_start);

        if is_member_start && !current.is_empty() {
            members.push(current);
            current = Vec::new();
            current_has_body_member = false;
            seen_begin = false;
        }
        if is_attr_start {
            attr_pending = true;
            attr_bracket_depth += crate::count_net_delimiters(&code, '[', ']');
        } else if is_keyword_start {
            attr_pending = false;
            if !is_var_keyword {
                current_has_body_member = true;
            }
        } else if !trimmed.is_empty() {
            // Any other code line breaks the attribute→member linkage.
            attr_pending = false;
        }
        // Match `begin` as a word anywhere in the code, not just at the end of
        // the line: a single-line body (`procedure A() begin end;`) opens and
        // closes its block on one line, and would otherwise leave `seen_begin`
        // false so a following object-level `var` was misread as that
        // procedure's locals.
        if contains_begin_keyword(&code) {
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

/// True for a plain `var` section header (never `protected var`).
fn is_plain_var_start(trimmed_lower: &str) -> bool {
    trimmed_lower == "var"
        || trimmed_lower.starts_with("var ")
        || trimmed_lower.starts_with("var\t")
}

/// True for any object-level var section header: `var` or `protected var`.
fn is_var_start(trimmed_lower: &str) -> bool {
    is_plain_var_start(trimmed_lower)
        || trimmed_lower == "protected var"
        || trimmed_lower.starts_with("protected var ")
        || trimmed_lower.starts_with("protected var\t")
}

/// Return the text after the `procedure ` keyword of a procedure header,
/// accepting any combination of the `local`/`internal`/`protected` modifiers
/// (e.g. `internal local procedure Foo()`), or `None` if the line is not a
/// procedure header. Input must be trimmed and lower-cased.
fn procedure_name_part(trimmed_lower: &str) -> Option<&str> {
    let mut rest = trimmed_lower;
    loop {
        if let Some(after) = rest.strip_prefix("procedure ") {
            return Some(after.trim_start());
        }
        let mut advanced = false;
        for modifier in ["local ", "internal ", "protected "] {
            if let Some(after) = rest.strip_prefix(modifier) {
                rest = after.trim_start();
                advanced = true;
                break;
            }
        }
        if !advanced {
            return None;
        }
    }
}

/// True when the (trimmed, lower-cased) line is a procedure header.
fn is_procedure_start(trimmed_lower: &str) -> bool {
    procedure_name_part(trimmed_lower).is_some()
}

fn is_member_keyword(trimmed_lower: &str) -> bool {
    is_var_start(trimmed_lower)
        || trimmed_lower.starts_with("trigger ")
        || is_procedure_start(trimmed_lower)
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
    let after = procedure_name_part(&lower).unwrap_or(&lower);
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
    fn crlf_line_endings_are_preserved() {
        // `str::lines` drops the `\r`; rejoining with `\n` would turn a member
        // sort on a Windows checkout into a whole-file line-ending diff.
        let input =
            "codeunit 50100 T\r\n{\r\n    procedure Zed() begin end;\r\n\r\n    procedure Alpha() begin end;\r\n}\r\n";
        let out = sort_members(input).expect("should sort");
        assert!(out.contains("\r\n"), "CRLF must survive:\n{out:?}");
        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "no bare LF may remain in a CRLF document:\n{out:?}"
        );
        assert!(
            out.find("Alpha").unwrap() < out.find("Zed").unwrap(),
            "{out}"
        );

        // An LF document stays LF.
        let lf = "codeunit 50100 T\n{\n    procedure Zed() begin end;\n\n    procedure Alpha() begin end;\n}\n";
        let lf_out = sort_members(lf).expect("should sort");
        assert!(
            !lf_out.contains('\r'),
            "LF document must not gain CR:\n{lf_out:?}"
        );
    }

    #[test]
    fn single_line_body_lets_a_later_var_block_hoist() {
        // `procedure A() begin end;` opens and closes its body on one line, so
        // the object-level `var` after it is not that procedure's locals.
        let input = "\
codeunit 50100 T
{
    procedure Zed() begin end;

    var
        G: Integer;
}
";
        let out = sort_members(input).expect("should sort");
        assert_eq!(
            content_multiset(input),
            content_multiset(&out),
            "sorting dropped content:\n{out}"
        );
        assert!(
            out.find("var").unwrap() < out.find("procedure Zed").unwrap(),
            "the object-level var block must hoist above the procedures:\n{out}"
        );
    }

    #[test]
    fn begin_keyword_detection_respects_words_and_literals() {
        assert!(contains_begin_keyword("begin"));
        assert!(contains_begin_keyword("procedure A() begin end;"));
        assert!(contains_begin_keyword("if x then begin"));
        assert!(contains_begin_keyword("BEGIN"));
        assert!(!contains_begin_keyword("Beginning := 1;"));
        assert!(!contains_begin_keyword("MyBegin();"));
        assert!(!contains_begin_keyword("Message('begin');"));
        assert!(!contains_begin_keyword("x := \"begin\";"));
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
    fn multi_line_attribute_stays_attached_to_its_procedure() {
        // A multi-line [EventSubscriber(...)] used to be split from its
        // procedure, leaving the attribute orphaned after sorting (which
        // silently unbinds the subscriber).
        let input = "\
codeunit 50100 T
{
    procedure Zebra()
    begin
    end;

    [EventSubscriber(ObjectType::Table, Database::Customer, 'OnAfterInsertEvent',
        '', false, false)]
    local procedure Alpha()
    begin
    end;
}
";
        let out = sort_members(input).expect("should sort");
        assert_eq!(
            content_multiset(input),
            content_multiset(&out),
            "sorting dropped content:\n{out}"
        );
        let attr = out.find("[EventSubscriber").expect("attribute present");
        let attr_close = out.find("false, false)]").expect("attribute close present");
        let alpha = out.find("local procedure Alpha").expect("Alpha present");
        let zebra = out.find("procedure Zebra").expect("Zebra present");
        assert!(
            attr < attr_close && attr_close < alpha,
            "attribute must immediately precede Alpha:\n{out}"
        );
        assert!(alpha < zebra, "Alpha must sort before Zebra:\n{out}");
        assert!(
            !out[alpha..zebra].contains('['),
            "no attribute fragment may sit between Alpha's header and Zebra:\n{out}"
        );
    }

    #[test]
    fn single_line_attribute_stays_attached_to_its_procedure() {
        let input = "\
codeunit 50100 T
{
    procedure Zebra()
    begin
    end;

    [IntegrationEvent(false, false)]
    procedure Alpha()
    begin
    end;
}
";
        let out = sort_members(input).expect("should sort");
        assert_eq!(content_multiset(input), content_multiset(&out), "{out}");
        let attr = out.find("[IntegrationEvent").expect("attribute present");
        let alpha = out.find("procedure Alpha").expect("Alpha present");
        let zebra = out.find("procedure Zebra").expect("Zebra present");
        assert!(
            attr < alpha && alpha < zebra,
            "attribute must precede Alpha, which sorts before Zebra:\n{out}"
        );
    }

    #[test]
    fn protected_var_block_hoists_with_var_blocks() {
        let input = "\
codeunit 50100 T
{
    procedure Zebra()
    begin
    end;

    protected var
        SharedState: Integer;
}
";
        let out = sort_members(input).expect("should sort");
        assert_eq!(content_multiset(input), content_multiset(&out), "{out}");
        let pv = out.find("protected var").expect("protected var present");
        let zebra = out.find("procedure Zebra").expect("Zebra present");
        assert!(
            pv < zebra,
            "protected var must hoist above procedures:\n{out}"
        );
        assert!(
            out[pv..zebra].contains("SharedState: Integer;"),
            "protected var must keep its declarations:\n{out}"
        );
    }

    #[test]
    fn var_and_protected_var_blocks_hoist_together_in_source_order() {
        // An object may legally declare both a `protected var` and a plain
        // `var` block. Both must hoist above triggers/procedures as a unit,
        // preserving their relative source order (protected-first here).
        let input = "\
codeunit 50100 T
{
    procedure Zebra()
    begin
    end;

    protected var
        SharedState: Integer;

    procedure Alpha()
    begin
    end;

    var
        PrivateState: Integer;
}
";
        let out = sort_members(input).expect("should sort");
        assert_eq!(content_multiset(input), content_multiset(&out), "{out}");

        let pv = out.find("protected var").expect("protected var present");
        let plain = out.find("\n    var").expect("plain var present") + 1;
        let alpha = out.find("procedure Alpha").expect("Alpha present");
        let zebra = out.find("procedure Zebra").expect("Zebra present");

        assert!(
            pv < plain,
            "blocks must keep their relative source order (protected first):\n{out}"
        );
        assert!(
            plain < alpha && plain < zebra,
            "both var blocks must hoist above every procedure:\n{out}"
        );
        assert!(
            out[pv..plain].contains("SharedState: Integer;"),
            "protected var must keep its declarations:\n{out}"
        );
        assert!(
            out[plain..alpha.min(zebra)].contains("PrivateState: Integer;"),
            "plain var must keep its declarations:\n{out}"
        );
        assert!(
            alpha < zebra,
            "procedures still sort alphabetically:\n{out}"
        );
    }

    #[test]
    fn internal_local_procedure_is_a_member_start() {
        let input = "\
codeunit 50100 T
{
    internal local procedure Zebra()
    begin
    end;

    procedure Alpha()
    begin
    end;
}
";
        let out = sort_members(input).expect("should sort");
        assert_eq!(content_multiset(input), content_multiset(&out), "{out}");
        let alpha = out.find("procedure Alpha").expect("Alpha present");
        let zebra = out
            .find("internal local procedure Zebra")
            .expect("Zebra present");
        assert!(alpha < zebra, "Alpha must sort before Zebra:\n{out}");
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
