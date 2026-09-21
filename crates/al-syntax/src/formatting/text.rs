//! Line-level text scanning shared by the indentation pass and the
//! post-passes.
//!
//! Every helper here is comment- and string-literal aware, which is what
//! keeps commented-out braces and keywords out of the block-structure state
//! machine.

/// Leading whitespace of `line` (everything before the first non-space char).
pub(super) fn leading_ws(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// Re-join post-pass output lines into a buffer terminated by a single `\n`.
///
/// `text.lines()` followed by `lines.join("\n")` plus a trailing `\n`
/// round-trips any buffer the main pass produces (which always ends in `\n`),
/// so a pass that does not modify its `lines` reproduces its input verbatim.
pub(super) fn join_lines(lines: Vec<String>) -> String {
    let mut result = lines.join("\n");
    result.push('\n');
    result
}

/// Byte offset of the first occurrence of `target` at the statement's top
/// level — i.e. outside single/double-quoted spans, outside `()`/`[]`, and
/// before any `//` line comment. `None` if no such occurrence exists.
pub(super) fn first_top_level(s: &str, target: u8) -> Option<usize> {
    let mut paren = 0i32;
    let mut bracket = 0i32;
    for span in crate::lexical::LineScanner::new(s, false) {
        if span.kind != crate::lexical::SpanKind::Code {
            continue;
        }
        for (offset, b) in span.text.bytes().enumerate() {
            match b {
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'[' => bracket += 1,
                b']' => bracket -= 1,
                c if c == target && paren == 0 && bracket == 0 => {
                    return Some(span.start + offset);
                }
                _ => {}
            }
        }
    }
    None
}

/// True if `s` contains a `//` line comment that is OUTSIDE any single- or
/// double-quoted span. Used by the brace-merge pass to avoid commenting out a
/// `{` that would be merged onto a line ending in a trailing comment.
/// Byte index where a `//` line comment begins outside any string literal, or
/// `None` if the line has no such comment. Doubled single-quotes (`''`) inside
/// a `'...'` string are escapes, not string terminators.
fn line_comment_start(s: &str) -> Option<usize> {
    crate::lexical::LineScanner::new(s, false)
        .find(|span| span.kind == crate::lexical::SpanKind::LineComment)
        .map(|span| span.start)
}

pub(super) fn has_line_comment_outside_strings(s: &str) -> bool {
    line_comment_start(s).is_some()
}

/// Strip every comment span from `line`, returning the code-only text plus
/// whether the line ends inside an unterminated block comment.
///
/// `in_block` is the block-comment state on entry. Both `//` tails and
/// `/* … */` spans are removed — including a block comment opened part-way
/// through a line and one spanning several lines — while string literals are
/// honoured, so a `//` or `/*` inside `'…'` / `"…"` stays as content.
///
/// This is what keeps commented-out text out of the block-structure state
/// machine: `/* … { … */` must not open a brace level, and `} // done` must
/// still read as a closing brace.
///
/// A removed span collapses to a single space so `end;/*x*/` does not fuse
/// its neighbours into one token; the result is trimmed.
pub(crate) fn strip_comments(line: &str, in_block: bool) -> (String, bool) {
    let mut out = String::with_capacity(line.len());
    let mut scanner = crate::lexical::LineScanner::new(line, in_block);
    for span in scanner.by_ref() {
        match span.kind {
            crate::lexical::SpanKind::Code | crate::lexical::SpanKind::String => {
                out.push_str(span.text);
            }
            crate::lexical::SpanKind::LineComment => {}
            crate::lexical::SpanKind::BlockComment => {
                // Preserve the existing separator contract exactly: the old
                // scanner inserted one space for each `/*` and one for each
                // `*/`. Surrounding source whitespace is copied separately.
                if span.text.starts_with("/*") {
                    out.push(' ');
                }
                if span.text.ends_with("*/") {
                    out.push(' ');
                }
            }
        }
    }
    let in_block = scanner.ends_in_block_comment();
    (out.trim().to_string(), in_block)
}

/// True if any of `needles` appears in `s` **outside** a string literal.
/// Lets case-label detection accept quoted labels like `'a;b'` whose `;`
/// lives inside the quotes.
pub(super) fn contains_char_outside_strings(s: &str, needles: &[char]) -> bool {
    crate::lexical::LineScanner::new(s, false).any(|span| {
        span.kind == crate::lexical::SpanKind::Code
            && span.text.chars().any(|ch| needles.contains(&ch))
    })
}

#[cfg(test)]
mod tests {
    use super::strip_comments;
    use crate::formatting::{format_al, FormatOptions};

    fn fmt(text: &str) -> String {
        format_al(text, &FormatOptions::default())
    }

    /// Leading whitespace of the first line whose trimmed content starts with
    /// `needle` (matching the line start avoids matching the same token inside a
    /// trailing comment).
    fn indent_of(text: &str, needle: &str) -> usize {
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with(needle))
            .unwrap_or_else(|| panic!("needle {needle:?} not found in:\n{text}"));
        line.len() - line.trim_start().len()
    }

    #[test]
    fn test_comments_dont_drain_single_stmt() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if x > 0 then
// This is a comment
Message('positive');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if x > 0 then
            // This is a comment
            Message('positive');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_block_comment_between_if_then_and_body_preserves_indent() {
        let input = "\
codeunit 50100 Test
{
    procedure Outer()
    begin
        if x > 0 then
            /* explain
               what's going on */
            Message('yes');
    end;
}
";
        let opts = FormatOptions::default();
        let out = format_al(input, &opts);
        assert!(
            out.contains("            Message('yes');"),
            "block comment must not collapse single-stmt indent — got:\n{out}"
        );
    }

    #[test]
    fn test_formatter_is_idempotent_with_block_comments() {
        let input = "\
codeunit 50100 Test
{
    /* class-level comment
       spans two lines */
    procedure Outer()
    begin
        if x > 0 then
            /* inline block
               comment */
            Message('yes');
    end;
}
";
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(
            pass1, pass2,
            "second-pass formatting with block comments should be a no-op"
        );
    }

    #[test]
    fn braces_inside_a_block_comment_do_not_change_indentation() {
        // A `{` in commented-out text used to open a real brace level, shifting
        // every following line — and the object's own closing `}` — one level
        // deeper, permanently.
        let input = "\
codeunit 50100 T
{
    /* example: obj {
       nested }
    */
    procedure P()
    begin
        Foo();
    end;
}
";
        let out = format_al(input, &FormatOptions::default());
        assert_eq!(indent_of(&out, "procedure P"), 4, "got:\n{out}");
        assert_eq!(indent_of(&out, "Foo()"), 8, "got:\n{out}");
        // The object's own closing brace must land back at column 0.
        let last = out.lines().rfind(|l| !l.trim().is_empty());
        assert_eq!(last, Some("}"), "got:\n{out}");
    }

    #[test]
    fn block_comment_opened_mid_line_is_tracked() {
        // `/*` after code on the same line was never detected, so the comment
        // body was analysed (and re-indented) as if it were executable AL.
        let input = "\
codeunit 50100 T
{
    procedure P() /* returns { nothing */
    begin
        Foo();
    end;
}
";
        let out = format_al(input, &FormatOptions::default());
        assert_eq!(indent_of(&out, "begin"), 4, "got:\n{out}");
        assert_eq!(indent_of(&out, "Foo()"), 8, "got:\n{out}");
    }

    #[test]
    fn block_comment_interior_layout_is_preserved() {
        // The interior of a `/* … */` block belongs to the author: aligned
        // tables, ASCII art and indented examples must survive a format.
        let input = "\
codeunit 50100 T
{
    /* layout:
         +-----+-----+
         |  a  |  b  |
    */
}
";
        let out = format_al(input, &FormatOptions::default());
        assert!(out.contains("         +-----+-----+"), "got:\n{out}");
        assert!(out.contains("         |  a  |  b  |"), "got:\n{out}");
    }

    #[test]
    fn code_after_block_comment_closer_updates_indentation_state() {
        let closing_end = "\
codeunit 50100 T
{
    procedure P()
    begin
        Foo();
        /* trailing
        */ end;

    procedure Q()
    begin
        Bar();
    end;
}
";
        let out = format_al(closing_end, &FormatOptions::default());
        assert_eq!(indent_of(&out, "procedure Q"), 4, "got:\n{out}");
        assert_eq!(
            out.lines().rfind(|line| !line.trim().is_empty()),
            Some("}"),
            "got:\n{out}"
        );
        assert_eq!(format_al(&out, &FormatOptions::default()), out);

        // A statement after `*/` must consume a pending single-statement body;
        // otherwise the next statement drifts one level deeper.
        let closing_call = "\
codeunit 50100 T
{
    procedure P()
    begin
        if Ready then
            /* why
            */ Foo();
        Bar();
    end;
}
";
        let out = format_al(closing_call, &FormatOptions::default());
        assert_eq!(indent_of(&out, "Bar()"), 8, "got:\n{out}");
        assert_eq!(format_al(&out, &FormatOptions::default()), out);
    }

    #[test]
    fn closing_brace_with_a_trailing_line_comment_still_dedents() {
        // `is_close` matched the raw trimmed line, so `}` was recognised but
        // `} // note` was not — every following line drifted one level deeper.
        let input = "\
table 50100 T
{
    fields
    {
        field(1; A; Integer) { }
    } // end fields
    keys
    {
        key(PK; A) { }
    }
}
";
        let out = format_al(input, &FormatOptions::default());
        assert_eq!(indent_of(&out, "keys"), 4, "got:\n{out}");
    }

    #[test]
    fn comment_markers_inside_string_literals_are_content() {
        let input = "\
codeunit 50100 T
{
    procedure P()
    begin
        Message('a /* b */ c // d');
        Foo();
    end;
}
";
        let out = format_al(input, &FormatOptions::default());
        assert!(out.contains("'a /* b */ c // d'"), "got:\n{out}");
        assert_eq!(indent_of(&out, "Foo()"), 8, "got:\n{out}");
    }

    #[test]
    fn apostrophe_in_a_quoted_name_does_not_open_a_paren_continuation() {
        // BC names are double-quoted and may contain an apostrophe. The
        // delimiter scanner used to enter string state on that `'` and miss the
        // call's closing paren, so every following line was indented as a
        // paren continuation.
        let input = "\
table 50100 T
{
    fields
    {
        field(1; \"Cust's Name\"; Text[50]) { }
        field(2; Other; Integer) { }
    }
}
";
        let out = format_al(input, &FormatOptions::default());
        assert_eq!(indent_of(&out, "field(2"), 8, "got:\n{out}");
    }

    #[test]
    fn strip_comments_removes_every_comment_shape() {
        assert_eq!(
            strip_comments("end; // note", false),
            ("end;".into(), false)
        );
        assert_eq!(strip_comments("} // note", false), ("}".into(), false));
        // A removed span collapses to a space so neighbours never fuse into
        // one token; the exact run of blanks does not matter to any caller.
        assert_eq!(
            strip_comments("a /* x */ b", false),
            ("a    b".into(), false)
        );
        assert_eq!(strip_comments("end;/*x*/", false), ("end;".into(), false));
        assert_eq!(strip_comments("code /* open", false), ("code".into(), true));
        assert_eq!(strip_comments("still inside", true), (String::new(), true));
        assert_eq!(
            strip_comments("closing */ tail", true),
            ("tail".into(), false)
        );
        // Comment markers inside literals are content, not comments.
        assert_eq!(
            strip_comments("Message('// not a comment')", false),
            ("Message('// not a comment')".into(), false)
        );
        assert_eq!(
            strip_comments("Message('/* not a comment */')", false),
            ("Message('/* not a comment */')".into(), false)
        );
        // An unterminated `/*` swallows the rest of the line, not the universe.
        assert_eq!(strip_comments("/*", false), (String::new(), true));
    }

    #[test]
    fn comment_ending_in_begin_does_not_corrupt_var_section() {
        // A `// … begin` comment tail on a var line must not trip the
        // var-section/block `ends_with("begin")` transition and dedent the rest
        // of the file.
        let input = "\
codeunit 50100 Test
{
    procedure P()
    var
        x: Integer; // then begin
        y: Integer;
    begin
        x := 1;
    end;
}
";
        let out = fmt(input);
        // Both var declarations sit at the same indent; the comment did not end
        // the var section early.
        assert_eq!(
            indent_of(&out, "x: Integer"),
            indent_of(&out, "y: Integer"),
            "var lines must share indent:\n{out}"
        );
        // `begin` is one level shallower than the var entries.
        assert!(
            indent_of(&out, "begin") < indent_of(&out, "y: Integer"),
            "begin must be shallower than var entries:\n{out}"
        );
        // Idempotent.
        assert_eq!(fmt(&out), out);
    }

    #[test]
    fn quoted_case_label_with_semicolon_indents_as_label() {
        // A quoted case label whose text contains `;` (`'a;b':`) must be treated
        // as a label, so its body is indented one level deeper.
        let input = "\
codeunit 50100 Test
{
    procedure P()
    begin
        case s of
            'a;b':
                Message('x');
        end;
    end;
}
";
        let out = fmt(input);
        assert!(
            indent_of(&out, "Message('x')") > indent_of(&out, "'a;b':"),
            "label body must be deeper than the label:\n{out}"
        );
        assert_eq!(fmt(&out), out, "must be idempotent");
    }

    // KeywordCasing wiring
}
