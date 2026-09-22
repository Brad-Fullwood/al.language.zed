//! Post-processing passes over the fully formatted buffer.
//!
//! Each takes the buffer by value so the main pass can chain them without
//! cloning, each is a strict no-op at its option default, and each is
//! idempotent.

use super::options::{indent_unit, BlankLinesBetweenProcedures, BraceStyle, FormatOptions};
use super::text::{
    first_top_level, has_line_comment_outside_strings, join_lines, leading_ws, strip_comments,
};

/// Byte offset of the property-assignment `=` (the first top-level `=` that is
/// not part of `:=`, `<=`, `>=`, `<>`, `!=`, or `==`). `None` if absent.
fn property_eq_pos(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    for span in crate::lexical::LineScanner::new(s, false) {
        if span.kind != crate::lexical::SpanKind::Code {
            continue;
        }
        for (offset, b) in span.text.bytes().enumerate() {
            let absolute = span.start + offset;
            match b {
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'[' => bracket += 1,
                b']' => bracket -= 1,
                b'=' if paren == 0 && bracket == 0 => {
                    let prev = if absolute > 0 { bytes[absolute - 1] } else { 0 };
                    let next = bytes.get(absolute + 1).copied().unwrap_or(0);
                    if !matches!(prev, b':' | b'<' | b'>' | b'!' | b'=') && next != b'=' {
                        return Some(absolute);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// A property name is a single ASCII identifier (`Caption`, `TableRelation`,
/// `Permissions`, …): non-empty, only alphanumerics and underscores.
fn is_property_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// True if `trimmed` (an already-trimmed line) opens a property statement —
/// `Name = value …`. Rejects comments, attributes, sub-block openers (`fields`
/// + `{`), and executable assignments (`x := …`) via the name/`=` checks.
fn is_property_opener(trimmed: &str) -> bool {
    if trimmed.is_empty()
        || trimmed.starts_with("//")
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with('[')
        || trimmed.ends_with('{')
    {
        return false;
    }
    match property_eq_pos(trimmed) {
        Some(pos) => is_property_name(trimmed[..pos].trim()),
        None => false,
    }
}

/// Case-insensitive sort key for a property statement: its name (the text
/// before the top-level `=`), lower-cased.
fn property_sort_key(first_line: &str) -> String {
    let t = first_line.trim();
    match property_eq_pos(t) {
        Some(pos) => t[..pos].trim().to_ascii_lowercase(),
        None => t.to_ascii_lowercase(),
    }
}

/// PASS 1 — `sort_properties`. Within each object body, sort every contiguous
/// run of object-level (brace depth 1) property statements case-insensitively
/// by property name. Multi-line property values move as a single unit; nothing
/// below depth 1 (fields/keys/layout/actions/triggers/procedures) is touched.
pub(super) fn sort_object_properties(text: String, options: &FormatOptions) -> String {
    if !options.sort_properties {
        return text;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    // Each pending run element is the full set of physical lines of one
    // property statement.
    let mut run: Vec<Vec<String>> = Vec::new();
    let mut depth: i32 = 0; // brace depth BEFORE the current line
    let mut i = 0;
    // Block-comment state carried across lines, so a `{` in commented-out text
    // does not shift `depth` and pull unrelated lines into a property run.
    let mut in_block = false;
    let brace_delta = |line: &str, in_block: &mut bool| -> i32 {
        let (code, still_open) = strip_comments(line, *in_block);
        *in_block = still_open;
        crate::count_net_delimiters(&code, '{', '}')
    };

    fn flush(run: &mut Vec<Vec<String>>, out: &mut Vec<String>) {
        if run.is_empty() {
            return;
        }
        // Stable sort keeps equal-named properties in source order.
        run.sort_by(|a, b| property_sort_key(&a[0]).cmp(&property_sort_key(&b[0])));
        for prop in run.drain(..) {
            out.extend(prop);
        }
    }

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if depth == 1 && !in_block && is_property_opener(trimmed) {
            // Collect this property's lines: from the opener until the line
            // that carries the statement-terminating top-level `;`.
            let mut j = i;
            let mut prop = vec![line.to_string()];
            while first_top_level(lines[j], b';').is_none() && j + 1 < lines.len() {
                j += 1;
                prop.push(lines[j].to_string());
            }
            // Properties carry no braces, so depth is unchanged; account for
            // any stray braces defensively.
            for l in &prop {
                depth += brace_delta(l, &mut in_block);
            }
            depth = depth.max(0);
            run.push(prop);
            i = j + 1;
            continue;
        }

        // Any non-property line ends the current run.
        flush(&mut run, &mut out);
        depth += brace_delta(line, &mut in_block);
        depth = depth.max(0);
        out.push(line.to_string());
        i += 1;
    }
    flush(&mut run, &mut out);
    join_lines(out)
}

/// Member-start lines that the blank-line pass treats as "the next procedure":
/// an attribute block, or a procedure/trigger header.
fn is_procedure_member_start(trimmed: &str) -> bool {
    if trimmed.starts_with('[') {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("procedure ")
        || lower.starts_with("local procedure ")
        || lower.starts_with("internal procedure ")
        || lower.starts_with("protected procedure ")
        || lower.starts_with("trigger ")
}

/// Normalize the blank-line gap
/// between a member-level `end;` and the following procedure/trigger member
/// (or its leading attribute block) to the configured count. Inserts blanks
/// when none exist; leaves everything else alone.
pub(super) fn normalize_blank_lines_between_procedures(
    text: String,
    options: &FormatOptions,
) -> String {
    let desired = match options.blank_lines_between_procedures {
        BlankLinesBetweenProcedures::Preserve => return text,
        BlankLinesBetweenProcedures::One => 1,
        BlankLinesBetweenProcedures::Two => 2,
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        out.push(line.to_string());

        // A member-closing `end;` is `<indent>end;` (procedure/trigger bodies
        // sit one indent inside their object; nested `end;` is deeper, so the
        // sibling-indent check below filters those out anyway).
        if line.trim().eq_ignore_ascii_case("end;") {
            let end_indent = leading_ws(line);
            let mut j = i + 1;
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            if j < lines.len() {
                let next = lines[j];
                if leading_ws(next) == end_indent && is_procedure_member_start(next.trim()) {
                    for _ in 0..desired {
                        out.push(String::new());
                    }
                    i = j; // skip the original blank gap; emit `next` next loop
                    continue;
                }
            }
        }
        i += 1;
    }
    join_lines(out)
}

/// PASS 3 — `max_line_length`. Wrap over-long single-line property statements
/// by splitting at top-level commas (never inside `()`, `[]`, or a string).
/// Continuation lines are indented one unit past the opener — matching how the
/// main pass indents an already-multi-line property — so re-formatting is a
/// no-op. A property with no top-level comma (one long token) is left intact.
pub(super) fn wrap_long_property_lines(text: String, options: &FormatOptions) -> String {
    let max = options.max_line_length;
    if max == 0 {
        return text;
    }
    let unit = indent_unit(options);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut in_continuation = false;
    for line in lines {
        let trimmed = line.trim();
        let is_comment =
            trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*');

        let wrappable = !in_continuation
            && trimmed.ends_with(';')
            && is_property_opener(trimmed)
            && line.chars().count() > max;
        if wrappable {
            out.extend(wrap_property_line(line, &unit));
        } else {
            out.push(line.to_string());
        }

        // Mirror the main pass's property-continuation tracking so a wrapped
        // (now multi-line) property is not re-examined as a fresh opener.
        if in_continuation && trimmed.ends_with(';') {
            in_continuation = false;
        } else if !in_continuation && !is_comment && trimmed.ends_with(',') {
            in_continuation = true;
        }
    }
    join_lines(out)
}

/// Split one over-long property line at top-level commas. Returns the original
/// line unchanged (as a single element) when there is nothing to split.
fn wrap_property_line(line: &str, unit: &str) -> Vec<String> {
    let opener_indent = leading_ws(line);
    let content = line.trim();

    // Collect top-level comma positions (paren/bracket/string aware).
    let mut cuts: Vec<usize> = Vec::new();
    let mut paren = 0i32;
    let mut bracket = 0i32;
    for span in crate::lexical::LineScanner::new(content, false) {
        if span.kind != crate::lexical::SpanKind::Code {
            continue;
        }
        for (offset, b) in span.text.bytes().enumerate() {
            match b {
                b'(' => paren += 1,
                b')' => paren -= 1,
                b'[' => bracket += 1,
                b']' => bracket -= 1,
                b',' if paren == 0 && bracket == 0 => cuts.push(span.start + offset),
                _ => {}
            }
        }
    }

    if cuts.is_empty() {
        return vec![line.to_string()];
    }

    let cont_indent = format!("{opener_indent}{unit}");
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut seg_start = 0;
    for (idx, &cut) in cuts.iter().enumerate() {
        // Include the comma at the end of this segment.
        let seg = content[seg_start..=cut].trim();
        let indent = if idx == 0 {
            opener_indent
        } else {
            &cont_indent
        };
        out.push(format!("{indent}{seg}"));
        seg_start = cut + 1;
    }
    // Final segment (carries the terminating `;`).
    let seg = content[seg_start..].trim();
    out.push(format!("{cont_indent}{seg}"));
    out
}

/// True if a stand-alone `{` may be merged onto `prev`. Rejects block keywords
/// (`begin`/`end`), closers (`}`), comment lines, lines already ending in `{`,
/// and — critically — any line carrying a trailing `//` comment (merging there
/// would comment the brace out and produce invalid AL).
///
/// It also rejects every line that the indentation pass classifies as a block
/// opener in its own right: a statement terminator (`;`), a single-statement
/// opener (`then`, `do`), `case … of`, a case label (`:`), `repeat`, `var` and
/// `else`. A stand-alone `{` never follows one of those in valid AL, and
/// merging there would change what the indentation pass sees on a re-run, so
/// the pass would not be idempotent.
fn is_mergeable_brace_target(prev: &str) -> bool {
    let t = prev.trim();
    if t.is_empty()
        || t == "}"
        || t.ends_with('{')
        || t.ends_with(';')
        || t.ends_with(':')
        || t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with('*')
    {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    if lower == "begin"
        || lower.ends_with(" begin")
        || lower == "end"
        || lower == "end;"
        || lower.starts_with("end ")
        || lower.starts_with("end;")
    {
        return false;
    }
    for kw in ["then", "do", "of", "repeat", "var", "else"] {
        if lower == kw || lower.ends_with(&format!(" {kw}")) {
            return false;
        }
    }
    // CRITICAL: never merge onto a line with a trailing line comment.
    !has_line_comment_outside_strings(t)
}

/// PRE-PASS — `brace_style`. For `SameLine`, merge a stand-alone `{` line onto
/// the preceding mergeable opener line. `NextLine` (default) is a no-op; this
/// pass never moves `begin`/`end` or a closing `}`.
///
/// Runs before the indentation pass so the indentation is computed for the
/// final line layout. See the call site in `format_al`.
pub(super) fn apply_brace_style(text: &str, options: &FormatOptions) -> String {
    if !matches!(options.brace_style, BraceStyle::SameLine) {
        return text.to_string();
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for line in lines {
        if line.trim() == "{" {
            let do_merge = out
                .last()
                .map(|p| is_mergeable_brace_target(p))
                .unwrap_or(false);
            if do_merge {
                let prev = out.last().unwrap().trim_end().to_string();
                *out.last_mut().unwrap() = format!("{prev} {{");
                continue;
            }
        }
        out.push(line.to_string());
    }
    join_lines(out)
}

#[cfg(test)]
mod tests {
    use crate::formatting::{format_al, BlankLinesBetweenProcedures, BraceStyle, FormatOptions};

    const SORT_INPUT: &str = "\
table 50100 Test
{
    DataPerCompany = true;
    Caption = 'Test';
    DataClassification = ToBeClassified;

    fields
    {
        field(1; \"No.\"; Code[20])
        {
            Zzz = 1;
            Aaa = 2;
        }
    }
}
";

    #[test]
    fn sort_properties_orders_object_level_run() {
        let opts = FormatOptions {
            sort_properties: true,
            ..Default::default()
        };
        let out = format_al(SORT_INPUT, &opts);
        // Object-level run is sorted case-insensitively: Caption, Data..., Data...
        let cap = out.find("Caption").unwrap();
        let dcl = out.find("DataClassification").unwrap();
        let dpc = out.find("DataPerCompany").unwrap();
        assert!(cap < dcl && dcl < dpc, "object props not sorted:\n{out}");
        // Field-level (depth 3) properties must NOT be reordered.
        assert!(
            out.find("Zzz = 1;").unwrap() < out.find("Aaa = 2;").unwrap(),
            "depth-3 field properties must be left untouched:\n{out}"
        );
    }

    #[test]
    fn sort_properties_default_is_noop() {
        let opts = FormatOptions::default();
        let out = format_al(SORT_INPUT, &opts);
        // Default preserves source order.
        assert!(
            out.find("DataPerCompany").unwrap() < out.find("Caption").unwrap(),
            "default must preserve property source order:\n{out}"
        );
    }

    #[test]
    fn sort_properties_is_idempotent() {
        let opts = FormatOptions {
            sort_properties: true,
            ..Default::default()
        };
        let pass1 = format_al(SORT_INPUT, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "sort_properties must be idempotent");
    }

    #[test]
    fn sort_properties_keeps_multiline_value_intact() {
        // A multi-line property must move as one unit, keeping its
        // continuation line attached.
        let input = "\
codeunit 50100 T
{
    Zulu = 1;
    Permissions = tabledata A = rm,
        tabledata B = r;
    Alpha = 2;
}
";
        let opts = FormatOptions {
            sort_properties: true,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        // Order: Alpha, Permissions, Zulu — and the Permissions continuation
        // stays directly under its opener.
        assert!(out.find("Alpha").unwrap() < out.find("Permissions").unwrap());
        assert!(out.find("Permissions").unwrap() < out.find("Zulu").unwrap());
        assert!(
            out.contains("Permissions = tabledata A = rm,\n        tabledata B = r;"),
            "multi-line value must stay intact:\n{out}"
        );
        let pass2 = format_al(&out, &opts);
        assert_eq!(out, pass2, "must remain idempotent with multi-line values");
    }

    // blank_lines_between_procedures

    const PROC_NO_GAP: &str = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;
    procedure B()
    begin
    end;
}
";

    #[test]
    fn blank_lines_two_inserts_two() {
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            ..Default::default()
        };
        let out = format_al(PROC_NO_GAP, &opts);
        assert!(
            out.contains("    end;\n\n\n    procedure B()"),
            "expected two blank lines between procedures:\n{out}"
        );
    }

    #[test]
    fn blank_lines_one_normalizes_existing_gap() {
        // Three existing blanks (collapsed to one by the main pass) become one.
        let input = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;



    procedure B()
    begin
    end;
}
";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        assert!(
            out.contains("    end;\n\n    procedure B()"),
            "expected exactly one blank line:\n{out}"
        );
        assert!(
            !out.contains("    end;\n\n\n    procedure B()"),
            "must not leave two blanks:\n{out}"
        );
    }

    #[test]
    fn blank_lines_counts_from_attribute_block() {
        // The gap is measured to the leading attribute, not the procedure.
        let input = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;
    [IntegrationEvent(false, false)]
    procedure B()
    begin
    end;
}
";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        assert!(
            out.contains("    end;\n\n\n    [IntegrationEvent(false, false)]"),
            "blanks must be inserted before the attribute block:\n{out}"
        );
    }

    #[test]
    fn blank_lines_default_is_noop() {
        let opts = FormatOptions::default();
        let out = format_al(PROC_NO_GAP, &opts);
        // Preserve: no blank inserted between A's end; and procedure B.
        assert!(
            out.contains("    end;\n    procedure B()"),
            "default must preserve the (absent) gap:\n{out}"
        );
    }

    #[test]
    fn blank_lines_is_idempotent() {
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            ..Default::default()
        };
        let pass1 = format_al(PROC_NO_GAP, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "blank_lines must be idempotent");
    }

    // max_line_length

    const LONG_PROP: &str = "\
codeunit 50100 T
{
    Permissions = tabledata Aaaaaaaaaaaaaaaaaaaaaaaaa = rm, tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;
}
";

    #[test]
    fn max_line_length_wraps_at_top_level_comma() {
        let opts = FormatOptions {
            max_line_length: 60,
            ..Default::default()
        };
        let out = format_al(LONG_PROP, &opts);
        assert!(
            out.contains("= rm,\n        tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;"),
            "long property must wrap at the top-level comma:\n{out}"
        );
    }

    #[test]
    fn max_line_length_does_not_split_inside_string() {
        // The only comma is inside a string literal — leave the line intact
        // even though it exceeds the limit.
        let input = "\
codeunit 50100 T
{
    Caption = 'Hello, World this is a very long caption indeed yes';
}
";
        let opts = FormatOptions {
            max_line_length: 20,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        assert!(
            out.contains("    Caption = 'Hello, World this is a very long caption indeed yes';"),
            "must not split inside a string literal:\n{out}"
        );
    }

    #[test]
    fn max_line_length_default_is_noop() {
        let opts = FormatOptions::default(); // max_line_length = 0
        let out = format_al(LONG_PROP, &opts);
        assert!(
            out.contains(
                "    Permissions = tabledata Aaaaaaaaaaaaaaaaaaaaaaaaa = rm, tabledata Bbbbbbbbbbbbbbbbbbbbbb = r;"
            ),
            "max_line_length=0 must leave long lines intact:\n{out}"
        );
    }

    #[test]
    fn max_line_length_is_idempotent() {
        let opts = FormatOptions {
            max_line_length: 60,
            ..Default::default()
        };
        let pass1 = format_al(LONG_PROP, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "max_line_length must be idempotent");
    }

    // brace_style

    const BRACE_INPUT: &str = "\
table 50100 Test
{
    fields
    {
        field(1; \"No.\"; Code[20])
        {
        }
    }
}
";

    #[test]
    fn brace_style_same_line_merges_openers() {
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let out = format_al(BRACE_INPUT, &opts);
        assert!(out.contains("table 50100 Test {"), "object brace:\n{out}");
        assert!(out.contains("    fields {"), "sub-block brace:\n{out}");
        assert!(
            out.contains("        field(1; \"No.\"; Code[20]) {"),
            "field brace:\n{out}"
        );
    }

    #[test]
    fn brace_style_default_is_noop() {
        let opts = FormatOptions::default(); // NextLine
        let out = format_al(BRACE_INPUT, &opts);
        assert!(
            out.contains("table 50100 Test\n{"),
            "NextLine default must leave braces on their own line:\n{out}"
        );
    }

    #[test]
    fn brace_style_is_idempotent() {
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let pass1 = format_al(BRACE_INPUT, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "brace_style must be idempotent");
    }

    #[test]
    fn brace_style_does_not_merge_onto_a_single_statement_opener() {
        // Found by `property_formatting::fixtures::mutated_fixture_every_option_is_idempotent`.
        // `if … then` opens a single-statement indent. Merging the `{` onto it removed
        // that opener on the next run, so the body dedented by one level every pass.
        let input = "\
        if Rec.Status = Rec.Status::Posted then
                {
                    ApplicationArea = All;
";
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let pass1 = format_al(input, &opts);
        assert!(
            !pass1.contains("then {"),
            "a `{{` must not merge onto `if … then`:\n{pass1}"
        );
        assert_eq!(pass1, format_al(&pass1, &opts));
    }

    #[test]
    fn brace_style_does_not_merge_onto_a_statement_terminator() {
        let input = "\
    Caption = 'x';
    {
    }
";
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let pass1 = format_al(input, &opts);
        assert!(!pass1.contains("; {"), "merged past a `;`:\n{pass1}");
        assert_eq!(pass1, format_al(&pass1, &opts));
    }

    #[test]
    fn brace_style_leaves_begin_end_untouched() {
        let input = "\
codeunit 50100 T
{
    procedure A()
    begin
    end;
}
";
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        // `begin` keeps its own line; only the object `{` merged onto header.
        assert!(out.contains("codeunit 50100 T {"), "header merge:\n{out}");
        assert!(out.contains("    begin\n"), "begin untouched:\n{out}");
        assert!(out.contains("    end;\n"), "end; untouched:\n{out}");
    }

    #[test]
    fn brace_style_same_line_does_not_comment_out_brace() {
        // CRITICAL: a `{` after a line that ends in a `//` comment must stay on
        // its own line, otherwise the brace gets commented out -> invalid AL.
        let input = "\
table 50100 Test
{
    fields
    {
        field(1; \"No.\"; Code[20]) // pk
        {
        }
    }
}
";
        let opts = FormatOptions {
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let out = format_al(input, &opts);
        // The brace must NOT be appended after the comment.
        assert!(
            !out.contains("// pk {"),
            "must not comment out the brace:\n{out}"
        );
        // The comment line is intact and the brace remains on its own line.
        assert!(
            out.contains("field(1; \"No.\"; Code[20]) // pk\n"),
            "comment line must be preserved:\n{out}"
        );
        assert!(
            out.contains("// pk\n        {"),
            "brace must remain on its own line:\n{out}"
        );
        // Other (comment-free) braces still merge.
        assert!(out.contains("table 50100 Test {"));
        assert!(out.contains("    fields {"));
        // And the whole thing is still idempotent.
        let pass2 = format_al(&out, &opts);
        assert_eq!(out, pass2, "comment-before-brace case must be idempotent");
    }
}
