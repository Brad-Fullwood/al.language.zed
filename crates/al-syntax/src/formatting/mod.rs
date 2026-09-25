//! AL code formatter — line-by-line indentation state machine.
//!
//! This text-based formatter operates on raw AL source lines.
//!
//! Key rules:
//! - Indent after: begin, {, (, var, trigger body, procedure body, repeat,
//!   case...of, single-statement openers (if...then, for...do, while...do, with...do)
//! - Dedent before: end, }, ), until
//! - **Comments don't count as statement bodies** for single-statement tracking
//! - **Split end/else**: bare `end` on one line + `else begin` on next
//! - **Multi-line paren close**: `) then` mid-line — track net parens per line
//! - **Blank lines drain single-statement stack**
//! - **Property continuation**: preserve whitespace when `prev_ended_with_comma`
//!
//! Block-structure terminals such as `begin`, `end`, and `var` are matched
//! directly. Versioned language data remains the source for the keyword surface
//! that changes across Business Central releases.

mod casing;
mod indent;
mod options;
mod passes;
mod range;
mod text;

pub use indent::format_al;
pub use options::{BlankLinesBetweenProcedures, BraceStyle, FormatOptions, KeywordCasing};
pub use range::{format_range, FormatTextEdit};

pub(crate) use text::strip_comments;

#[cfg(test)]
mod tests {
    use super::{format_al, BlankLinesBetweenProcedures, BraceStyle, FormatOptions};

    #[test]
    fn all_options_together_are_idempotent() {
        // Combined run exercises pass ordering and cross-pass idempotency.
        let input = "\
table 50100 Test
{
    Zulu = 1;
    Permissions = tabledata Aaaaaaaaaaaaaaaaaaaaaa = rm, tabledata Bbbbbbbbbbbbbbb = r;
    Alpha = 2;

    fields
    {
        field(1; \"No.\"; Code[20])
        {
        }
    }

    procedure A()
    begin
    end;
    procedure B()
    begin
    end;
}
";
        let opts = FormatOptions {
            sort_properties: true,
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            max_line_length: 60,
            brace_style: BraceStyle::SameLine,
            ..Default::default()
        };
        let pass1 = format_al(input, &opts);
        let pass2 = format_al(&pass1, &opts);
        assert_eq!(pass1, pass2, "combined A13 passes must be idempotent");
    }
}
