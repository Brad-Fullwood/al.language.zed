//! The main pass: a line-by-line indentation state machine.
//!
//! See the module docs of [`crate::formatting`] for the rules it applies.

use super::casing::{apply_keyword_casing, apply_keyword_casing_with_state};
use super::options::{BlankLinesBetweenProcedures, FormatOptions};
use super::passes::{
    apply_brace_style, normalize_blank_lines_between_procedures, sort_object_properties,
    wrap_long_property_lines,
};
use super::text::{contains_char_outside_strings, strip_comments};

/// See module-level docs for the complete set of rules.
pub fn format_al(text: &str, options: &FormatOptions) -> String {
    // `str::lines` intentionally strips both LF and CRLF terminators. Keep
    // track of the source convention so a no-op format check stays a no-op on
    // Windows checkouts instead of rewriting every line ending to LF.
    let uses_crlf = text.contains("\r\n");
    // The brace merge runs *before* the indentation pass. Merging a stand-alone
    // `{` onto the line above changes what the indentation state machine sees on
    // that line, so a merge applied afterwards would leave the file indented for
    // the pre-merge layout and the next format run would move it again.
    let merged = apply_brace_style(text, options);
    let text: &str = &merged;
    let indent_str = if options.insert_spaces {
        " ".repeat(options.tab_size)
    } else {
        "\t".to_string()
    };

    let mut result = String::with_capacity(text.len());
    let mut indent_level: i32 = 0;
    let mut prev_was_empty = false;

    // Runs of blank lines are collapsed only when a blank-line policy is
    // active. `BlankLinesBetweenProcedures::Preserve` (the default) is
    // documented as a strict no-op, so it must not rewrite files that
    // intentionally use double blank lines.
    let collapse_blank_runs = !matches!(
        options.blank_lines_between_procedures,
        BlankLinesBetweenProcedures::Preserve
    );

    // Track how many single-statement indents are pending (if...then, for...do, while...do)
    let mut single_stmt_depth: i32 = 0;

    // Whether we are inside a `var` section (between `var` and `begin`)
    let mut in_var_section = false;

    // Track unclosed parentheses for multi-line call continuation
    let mut paren_depth: i32 = 0;

    // multi-line property assignments (`Permissions = tabledata A = rm,`)
    // continue on following lines until the terminating `;`. Continuation
    // lines are indented one level past the opener instead of being
    // collapsed to the property's own level.
    let mut in_property_continuation = false;

    // Track case...of nesting for label indentation
    let mut case_depth: i32 = 0;

    // Whether we are inside a case label body (indent +1 for label body)
    let mut in_case_label_body = false;

    // Track begin/end nesting depth within case label bodies.
    // When > 0, an `end;` closes a begin block, not the case label body.
    let mut case_begin_depth: i32 = 0;

    // Block comments do not consume pending single-statement indentation.
    let mut in_block_comment = false;

    for line in text.lines() {
        let trimmed = line.trim();
        let started_in_block_comment = in_block_comment;

        // Preserve comment-interior layout byte-for-byte. A closing line with
        // code after `*/` is different: preserve its physical layout, but feed
        // that code tail through the normal state machine so `*/ end;`, calls,
        // and block openers affect every following line correctly.
        let carried_comment_code = if started_in_block_comment {
            let (code, still_open) = strip_comments(trimmed, true);
            in_block_comment = still_open;
            if code.is_empty() {
                result.push_str(line.trim_end());
                result.push('\n');
                prev_was_empty = false;
                continue;
            }
            Some(code)
        } else {
            None
        };

        if trimmed.is_empty() {
            if collapse_blank_runs && prev_was_empty {
                continue;
            }
            prev_was_empty = true;

            // Blank lines drain single-statement stack (MS behavior)
            if single_stmt_depth > 0 {
                indent_level = (indent_level - single_stmt_depth).max(0);
                single_stmt_depth = 0;
            }
            result.push('\n');
            continue;
        }
        prev_was_empty = false;

        // Block-transition heuristics must look at the code only: a line like
        // `x: Integer; // then begin` must not be treated as ending in
        // `begin`, and neither must `/* … begin … */`. `code`/`code_lower`
        // have every comment span removed, so commented-out braces and
        // keywords can't drive the indent state machine; an unterminated `/*`
        // sets `in_block_comment` for the following lines.
        let code = if let Some(code) = carried_comment_code {
            code
        } else {
            let (code, opens_block_comment) = strip_comments(trimmed, false);
            in_block_comment = opens_block_comment;
            code
        };
        let code_lower = code.to_lowercase();

        // `begin` closes a var section — dedent back to the procedure level
        if in_var_section && (code_lower == "begin" || code_lower.ends_with(" begin")) {
            indent_level = (indent_level - 1).max(0);
            in_var_section = false;
        }

        // An object-level `var` section has no closing `begin`; it
        // ends at the next member declaration: an attribute line
        // (`[EventSubscriber(...)]`) or a procedure/trigger header.
        if in_var_section {
            let is_member_start = code.starts_with('[')
                || code_lower.starts_with("procedure ")
                || code_lower.starts_with("local ")
                || code_lower.starts_with("internal ")
                || code_lower.starts_with("protected ")
                || code_lower.starts_with("trigger ");
            if is_member_start {
                indent_level = (indent_level - 1).max(0);
                in_var_section = false;
            }
        }

        // `begin` after single-statement openers (if...then begin written separately)
        // drains the single-stmt stack since begin starts a block
        if single_stmt_depth > 0 && (code_lower == "begin" || code_lower.ends_with(" begin")) {
            indent_level = (indent_level - single_stmt_depth).max(0);
            single_stmt_depth = 0;
        }

        // Case label: close previous label body before this new label.
        // Be precise: a case label is "<token>:" with no whitespace before the
        // colon — not `Trigger:` inside a field declaration on the same line,
        // and not `OnValidate:` on a trigger header. Reject lines containing
        // anything other than the label token, optional inner spaces (for
        // multi-word string labels like `"Foo Bar"`) and the trailing colon.
        // Use the code portion (comment stripped) and reject disqualifying
        // characters only when they occur OUTSIDE a string, so a quoted label
        // like `'a;b':` is still a label because its semicolon is quoted.
        let label_code = code.as_str();
        let is_case_label = case_depth > 0
            && label_code.ends_with(':')
            && !label_code.ends_with("::")
            // The colon must directly follow the last non-space character —
            // no `;` or `=` etc. before it (outside string literals).
            && !contains_char_outside_strings(
                label_code.trim_end_matches(':'),
                &[';', '=', '(', ')', ','],
            );
        if is_case_label && in_case_label_body {
            // Drain any single-stmt from within the previous label body
            if single_stmt_depth > 0 {
                indent_level = (indent_level - single_stmt_depth).max(0);
                single_stmt_depth = 0;
            }
            indent_level = (indent_level - 1).max(0);
            in_case_label_body = false;
        }

        let is_close = code_lower == "}"
            || code_lower == "end;"
            || code_lower == "end"
            || code_lower.starts_with("end;")
            || code_lower.starts_with("end ");

        if is_close {
            if single_stmt_depth > 0 {
                indent_level = (indent_level - single_stmt_depth).max(0);
                single_stmt_depth = 0;
            }
            // Close var section if still open (e.g., object-level `var` closed by `}`)
            if in_var_section {
                indent_level = (indent_level - 1).max(0);
                in_var_section = false;
            }
            // Inside case label body: determine if this end; closes a begin
            // block within the label, or the label body / case itself
            if in_case_label_body && case_begin_depth > 0 {
                // This end; closes a begin block within the case label body
                case_begin_depth -= 1;
            } else if in_case_label_body {
                // No nested begin — this end; closes the case block itself
                indent_level = (indent_level - 1).max(0);
                in_case_label_body = false;
                if case_depth > 0 {
                    case_depth -= 1;
                }
            } else if case_depth > 0 && (code_lower == "end;" || code_lower.starts_with("end;")) {
                // Case block close without label body
                case_depth -= 1;
            }
            indent_level = (indent_level - 1).max(0);
        }

        // `else` after single-statement if-then: drain remaining single-stmt depth
        if !is_close
            && (code_lower == "else" || code_lower.starts_with("else "))
            && single_stmt_depth > 0
        {
            indent_level = (indent_level - single_stmt_depth).max(0);
            single_stmt_depth = 0;
        }

        if code_lower.starts_with("until ") || code_lower == "until" {
            indent_level = (indent_level - 1).max(0);
        }

        let output_line = if started_in_block_comment {
            line.trim_end()
        } else {
            for _ in 0..indent_level {
                result.push_str(&indent_str);
            }
            trimmed
        };
        // Apply casing only to code spans. On a carried block-comment closing
        // line, the scanner starts in comment state so prose before `*/` stays
        // byte-for-byte while an executable tail still receives normal casing.
        let transformed = if started_in_block_comment {
            apply_keyword_casing_with_state(output_line, &options.keyword_casing, true)
        } else {
            apply_keyword_casing(output_line, &options.keyword_casing)
        };
        result.push_str(&transformed);
        result.push('\n');

        let net_parens = count_net_parens(&code);
        if net_parens > 0 && paren_depth == 0 {
            // Opening parens on this line — indent continuation lines
            indent_level += 1;
            paren_depth += net_parens;
        } else if net_parens > 0 {
            paren_depth += net_parens;
        } else if net_parens < 0 {
            let old_depth = paren_depth;
            paren_depth = (paren_depth + net_parens).max(0);
            if old_depth > 0 && paren_depth == 0 {
                // Parens fully closed — remove continuation indent
                indent_level = (indent_level - 1).max(0);
            }
        }

        // If inside open parens, skip all other indent logic for this line
        if paren_depth > 0 {
            continue;
        }

        // property-assignment continuations. A non-comment line that
        // ends with `,` outside any parens starts (or stays in) a
        // continuation — the following line(s) indent one extra level until
        // the `;` terminator. Trailing commas outside parens are not valid
        // in executable AL, so this only fires on multi-line property
        // values (Permissions, TableRelation, CalcFormula, …).
        // A line whose code portion is empty is pure comment (`// …`,
        // `/* … */`, or the opening line of a multi-line block comment).
        let is_comment = code.is_empty();
        if in_property_continuation && (code.ends_with(';') || is_close) {
            indent_level = (indent_level - 1).max(0);
            in_property_continuation = false;
        } else if !in_property_continuation && !is_comment && code.ends_with(',') {
            indent_level += 1;
            in_property_continuation = true;
        }

        // Drain single-stmt stack: if we just wrote a "normal" statement (not a
        // block opener, closer, comment, or another single-stmt opener), pop
        // the stack. Comment-only lines are not executable statements and must
        // not consume single-stmt-depth.
        let is_block_opener = code.ends_with('{')
            || code_lower == "begin"
            || code_lower.ends_with(" begin")
            || code_lower == "var"
            || code_lower == "repeat";
        let is_single_stmt_opener = is_single_statement_opener(&code_lower);

        if single_stmt_depth > 0
            && !is_block_opener
            && !is_close
            && !is_comment
            && !is_single_stmt_opener
            && !is_case_label
            && code_lower != "else"
            && !code_lower.starts_with("else ")
        {
            // This line consumed one single-statement slot; drain all
            indent_level = (indent_level - single_stmt_depth).max(0);
            single_stmt_depth = 0;
        }

        if code.ends_with('{') {
            indent_level += 1;
        } else if code_lower == "begin" || code_lower.ends_with(" begin") {
            indent_level += 1;
            if in_case_label_body {
                case_begin_depth += 1;
            }
        } else if code_lower == "var" {
            indent_level += 1;
            in_var_section = true;
        } else if code_lower == "repeat" {
            indent_level += 1;
        } else if code_lower == "else" || code_lower.starts_with("else ") {
            // `else` without `begin` on same line — next stmt is single-stmt
            if !code_lower.ends_with(" begin") {
                indent_level += 1;
                single_stmt_depth += 1;
            }
        } else if code_lower.starts_with("case ") && code_lower.ends_with(" of") {
            indent_level += 1;
            case_depth += 1;
        }

        // Case labels: indent the body after a label (separate from single-stmt)
        if is_case_label {
            indent_level += 1;
            in_case_label_body = true;
        }

        // Single-statement openers: if...then, for...do, while...do, with...do
        // Only when not followed by `begin` on the same line
        if is_single_stmt_opener && !code_lower.ends_with(" begin") {
            indent_level += 1;
            single_stmt_depth += 1;
        }
    }

    if !result.ends_with('\n') {
        result.push('\n');
    }

    // The pass order is significant for idempotence: sort properties, normalize
    // procedure gaps, then wrap long properties. The brace merge already ran
    // ahead of the indentation pass.
    let result = sort_object_properties(result, options);
    let result = normalize_blank_lines_between_procedures(result, options);
    let result = wrap_long_property_lines(result, options);

    if uses_crlf {
        result.replace('\n', "\r\n")
    } else {
        result
    }
}

/// Delegates to the crate-level `count_net_delimiters` which skips string literals.
fn count_net_parens(line: &str) -> i32 {
    crate::count_net_delimiters(line, '(', ')')
}

/// Single-statement openers are loaded from `tree-sitter-al/data/single_stmt_openers.json`
/// via [`crate::language_data::single_stmt_openers`].
fn is_single_statement_opener(trimmed_lower: &str) -> bool {
    crate::language_data::single_stmt_openers().iter().any(|o| {
        trimmed_lower.starts_with(o.prefix.as_str()) && trimmed_lower.ends_with(o.suffix.as_str())
    })
}

// Post-processing passes operate on the fully formatted text and take the
// buffer by value so callers can chain them without cloning.

#[cfg(test)]
mod tests {
    use super::format_al;
    use crate::formatting::{BlankLinesBetweenProcedures, FormatOptions};

    fn fmt(text: &str) -> String {
        format_al(text, &FormatOptions::default())
    }

    #[test]
    fn test_basic_codeunit() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
Message('Hello');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        Message('Hello');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn preserves_crlf_line_endings() {
        let input = "codeunit 50100 Test\r\n{\r\n}\r\n";
        let output = fmt(input);
        assert_eq!(output, input);
        assert!(!output.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn test_if_then_single_statement() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if x > 0 then
Message('positive');
Message('always');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if x > 0 then
            Message('positive');
        Message('always');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_if_then_begin_end() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if x > 0 then begin
Message('a');
Message('b');
end;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if x > 0 then begin
            Message('a');
            Message('b');
        end;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_var_section() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
var
x: Integer;
y: Text;
begin
x := 42;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    var
        x: Integer;
        y: Text;
    begin
        x := 42;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_blank_line_drains_single_stmt() {
        let input = "codeunit 50100 Test\n{\nprocedure DoSomething()\nbegin\nif x > 0 then\n\nMessage('after blank');\nend;\n}";
        let result = fmt(input);
        // After blank line, single-stmt stack should be drained
        // so Message should be at the same level as the if
        assert!(
            result.contains("        Message('after blank');")
                || result.contains("    Message('after blank');")
        );
    }

    #[test]
    fn test_case_statement() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
case x of
1:
Message('one');
2:
Message('two');
end;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        case x of
            1:
                Message('one');
            2:
                Message('two');
        end;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_repeat_until() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
repeat
x += 1;
until x > 10;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        repeat
            x += 1;
        until x > 10;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_nested_if_then() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if a then
if b then
Message('nested');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if a then
            if b then
                Message('nested');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_else_branch() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
if a then
Message('yes')
else
Message('no');
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        if a then
            Message('yes')
        else
            Message('no');
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_double_blank_lines_collapsed_when_blank_policy_active() {
        let input = "codeunit 50100 Test\n{\n\n\nprocedure A()\nbegin\nend;\n}";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
            ..Default::default()
        };
        let result = format_al(input, &opts);
        assert!(!result.contains("\n\n\n"));
    }

    #[test]
    fn test_double_blank_lines_preserved_by_default() {
        // `BlankLinesBetweenProcedures::Preserve` (the default) is documented
        // as a strict no-op: intentional double blank lines must survive.
        let input = "codeunit 50100 Test\n{\n    procedure A()\n    begin\n    end;\n\n\n    procedure B()\n    begin\n    end;\n}\n";
        let result = fmt(input);
        assert_eq!(
            result, input,
            "default options must not collapse blank runs"
        );
    }

    #[test]
    fn test_for_do_single_stmt() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
for i := 1 to 10 do
Message('%1', i);
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        for i := 1 to 10 do
            Message('%1', i);
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_while_do_single_stmt() {
        let input = r#"codeunit 50100 Test
{
procedure DoSomething()
begin
while x > 0 do
x -= 1;
end;
}"#;
        let expected = r#"codeunit 50100 Test
{
    procedure DoSomething()
    begin
        while x > 0 do
            x -= 1;
    end;
}
"#;
        assert_eq!(fmt(input), expected);
    }

    #[test]
    fn test_formatter_is_idempotent_on_simple_input() {
        let input = "\
codeunit 50100 Test
{
    procedure Outer()
    var
        x: Integer;
    begin
        if x > 0 then
            Message('yes')
        else
            Message('no');

        for i := 1 to 10 do
            Message(Format(i));
    end;
}
";
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(
            pass1, pass2,
            "second-pass formatting should be a no-op — pass-1 output is\n{pass1}\nand pass-2 is\n{pass2}"
        );
    }

    #[test]
    fn test_multiline_paren_call_is_idempotent() {
        let input = r#"codeunit 50100 Test
{
    procedure DoWork()
    begin
        Foo(
            1,
            2,
            3);
    end;
}
"#;
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(
            pass1, pass2,
            "second-pass formatting of multi-line paren call should be a no-op\n\
             pass1:\n{pass1}\npass2:\n{pass2}"
        );
    }

    #[test]
    fn test_multiline_paren_in_expression_is_idempotent() {
        // Trickier shape: a call inside an if-condition, opening parens on
        // the if line. Whatever pass1 picks, pass2 must match.
        let input = r#"codeunit 50100 Test
{
    procedure DoWork()
    begin
        if MyFunc(
            a,
            b
        ) then
            Message('hit');
    end;
}
"#;
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2);
    }

    #[test]
    fn attribute_after_object_var_and_property_continuation() {
        let input = r#"codeunit 50104 "AUK Data Management Event Subs"
{
    InherentPermissions = x;
    Permissions = tabledata "Warehouse Shipment Header" = rm,
                  tabledata "Warehouse Shipment Line" = r;

    var
        FieldValueCalcHelper: Codeunit "AUK Field Value Calc Helper";

    [EventSubscriber(ObjectType::Table, Database::"Warehouse Shipment Line", OnAfterInsertEvent, '', false, false)]
    local procedure OnAfterWarehouseShipmentLineInsert(var Rec: Record "Warehouse Shipment Line"; RunTrigger: Boolean)
    begin
        if not RunTrigger then
            exit;
    end;
}
"#;
        let opts = FormatOptions::default();
        let pass1 = format_al(input, &opts);

        // The attribute and procedure header sit at member level (4), not
        // var-entry level (8).
        assert!(
            pass1.contains("\n    [EventSubscriber(ObjectType::Table"),
            "attribute must be at member indentation, got:\n{pass1}"
        );
        assert!(
            pass1.contains("\n    local procedure OnAfterWarehouseShipmentLineInsert"),
            "procedure header must be at member indentation, got:\n{pass1}"
        );
        // The Permissions continuation stays indented past the opener.
        assert!(
            pass1.contains("\n        tabledata \"Warehouse Shipment Line\" = r;"),
            "property continuation must keep extra indentation, got:\n{pass1}"
        );
        // And the property after the continuation is unaffected.
        assert!(
            pass1.contains("\n    var\n"),
            "var keyword must stay at member level, got:\n{pass1}"
        );

        // Idempotent: formatting the formatted output changes nothing.
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "format must be idempotent");
    }

    // sort_properties
}
