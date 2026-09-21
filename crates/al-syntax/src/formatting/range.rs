//! Range formatting: format the whole document for context, then emit edits
//! covering only the requested lines.

use super::indent::format_al;
use super::options::{BlankLinesBetweenProcedures, FormatOptions};

/// Transport-agnostic text edit emitted by [`format_range`].
///
/// Lines are 0-based; `end_character` is in UTF-16 code units. Callers
/// (al-lsp) convert to `tower_lsp::lsp_types::TextEdit` at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatTextEdit {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
    pub new_text: String,
}

/// Formats the entire document (to derive correct indent context) but returns
/// `FormatTextEdit`s covering only the requested line range.
///
/// `start_line` and `end_line` are 0-based, inclusive.
/// Returns `None` if `start_line` is out of bounds.
/// Returns `Some(vec![])` if the selected lines are already correctly formatted.
pub fn format_range(
    text: &str,
    start_line: u32,
    end_line: u32,
    options: &FormatOptions,
) -> Option<Vec<FormatTextEdit>> {
    let formatted_full = format_al(text, options);

    let orig_lines: Vec<&str> = text.lines().collect();
    let fmt_lines: Vec<&str> = formatted_full.lines().collect();

    let start = start_line as usize;
    if start >= orig_lines.len() {
        return None;
    }
    let end = (end_line as usize).min(orig_lines.len().saturating_sub(1));
    if start > end {
        return None;
    }

    let collapse_blank_runs = !matches!(
        options.blank_lines_between_procedures,
        BlankLinesBetweenProcedures::Preserve
    );
    let formatted_region =
        extract_formatted_region(&orig_lines, &fmt_lines, start, end, collapse_blank_runs);
    let original_region: Vec<&str> = orig_lines[start..=end].to_vec();

    if formatted_region == original_region {
        return Some(Vec::new());
    }

    // When the requested range *starts* inside a blank run whose leading
    // line(s) the collapser removes, the edit must actually delete those
    // lines. The usual replacement span `(start, 0)..(end, len)` excludes the
    // end line's terminating newline, so a removed-only region would emit a
    // no-op (or even insert a newline). Extend the span through the end
    // line's newline — `(end + 1, 0)` — so the removed line count is real.
    // LSP clients clamp an end position past the last line to the document
    // end, which is exactly the terminating newline when one exists.
    let starts_in_collapsed_blank = collapse_blank_runs
        && start > 0
        && orig_lines[start].trim().is_empty()
        && orig_lines[start - 1].trim().is_empty();
    if starts_in_collapsed_blank {
        let mut new_text = formatted_region.join("\n");
        if !formatted_region.is_empty() {
            new_text.push('\n');
        }
        return Some(vec![FormatTextEdit {
            start_line,
            start_character: 0,
            end_line: end as u32 + 1,
            end_character: 0,
            new_text,
        }]);
    }

    let mut new_text = formatted_region.join("\n");
    // Append a trailing newline so the replacement bridges into the line that
    // follows the selection. The one exception is the document's final line
    // when the original text has no trailing newline: appending `\n` there
    // would inject a newline the document never had. `.lines()` discards the
    // trailing-newline distinction, so consult `text` directly.
    let end_is_last_line = end == orig_lines.len().saturating_sub(1);
    if !end_is_last_line || text.ends_with('\n') {
        new_text.push('\n');
    }

    let end_char = orig_lines
        .get(end)
        .map(|l| crate::byte_col_to_utf16_col(l, l.len()))
        .unwrap_or(0);

    // `end`, not `end_line`: the caller may have named a line past the end of
    // the document, and `end_char` is measured on the clamped line.
    Some(vec![FormatTextEdit {
        start_line,
        start_character: 0,
        end_line: end as u32,
        end_character: end_char,
        new_text,
    }])
}

/// Extract lines from the formatted output corresponding to orig lines [start..=end].
///
/// When a blank-line policy is active (`collapse_blanks`), the formatter
/// collapses consecutive blank lines (two → one). We walk orig and fmt in
/// lockstep, skipping orig-only collapsed blanks without advancing the fmt
/// cursor. Under the default `Preserve` policy blank runs survive verbatim and
/// the two walks advance together.
///
/// Collapsed double-blanks are the only supported line-count difference.
fn extract_formatted_region<'a>(
    orig_lines: &[&str],
    fmt_lines: &[&'a str],
    start: usize,
    end: usize,
    collapse_blanks: bool,
) -> Vec<&'a str> {
    let mut orig_idx = 0usize;
    let mut fmt_idx = 0usize;
    let mut result: Vec<&'a str> = Vec::new();
    let mut prev_orig_blank = false;
    let mut visited_non_collapsed = 0usize;

    while orig_idx <= end && fmt_idx < fmt_lines.len() {
        let orig_is_blank = orig_lines[orig_idx].trim().is_empty();

        if collapse_blanks && orig_is_blank && prev_orig_blank {
            orig_idx += 1;
            continue;
        }

        prev_orig_blank = orig_is_blank;

        if orig_idx >= start {
            result.push(fmt_lines[fmt_idx]);
        }

        orig_idx += 1;
        fmt_idx += 1;
        visited_non_collapsed += 1;
    }

    // Invariant: we advanced fmt_idx exactly as often as we advanced
    // through orig rows that weren't collapsed blanks. Any other formatter
    // asymmetry (drop, duplicate, reorder) would manifest as fmt_idx
    // diverging from visited_non_collapsed.
    debug_assert_eq!(
        fmt_idx, visited_non_collapsed,
        "extract_formatted_region: fmt cursor diverged from orig walk — \
         formatter introduced a line-count asymmetry beyond blank collapse"
    );

    result
}

#[cfg(test)]
mod tests {
    use super::{format_range, FormatTextEdit};
    use crate::formatting::{BlankLinesBetweenProcedures, FormatOptions};

    #[test]
    fn test_format_range_no_change_needed() {
        let input = "codeunit 50100 Test\n{\n    procedure DoSomething()\n    begin\n        Message(\'Hello\');\n    end;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 2, 5, &opts);
        assert!(edits.is_some());
        assert!(edits.unwrap().is_empty());
    }

    #[test]
    fn test_format_range_single_procedure_body() {
        let input =
            "codeunit 50100 Test\n{\nprocedure DoSomething()\nbegin\nMessage(\'Hello\');\nend;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 2, 5, &opts);
        assert!(edits.is_some());
        let edits = edits.unwrap();
        assert!(!edits.is_empty());
        let edit = &edits[0];
        assert_eq!(edit.start_line, 2);
        assert_eq!(edit.end_line, 5);
        assert!(edit.new_text.contains("    procedure DoSomething()"));
        assert!(edit.new_text.contains("    begin"));
        assert!(edit.new_text.contains("        Message(\'Hello\');"));
        assert!(edit.new_text.contains("    end;"));
    }

    #[test]
    fn test_format_range_out_of_bounds_returns_none() {
        let input = "codeunit 50100 Test\n{\n}";
        let opts = FormatOptions::default();
        assert!(format_range(input, 10, 15, &opts).is_none());
    }

    #[test]
    fn test_format_range_end_line_is_clamped_to_the_document() {
        // A client that selects "to the end" passes a large end line. The edit
        // must end on the last real line, not on the line the caller named.
        let input = "codeunit 50100 Test\n{\nprocedure X()\nbegin\nend;\n}\n";
        let last = input.lines().count() - 1;
        let opts = FormatOptions::default();
        let edits = format_range(input, 0, 9999, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].end_line, last as u32);
    }

    #[test]
    fn test_format_range_last_line_no_trailing_newline() {
        // when the selection ends on the document's final line and
        // the document has no trailing newline, the emitted edit must NOT append
        // a spurious trailing newline.
        let input = "codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n        }";
        assert!(!input.ends_with('\n'));
        let opts = FormatOptions::default();
        let edits = format_range(input, 5, 5, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        let edit = &edits[0];
        assert_eq!(edit.start_line, 5);
        assert_eq!(edit.end_line, 5);
        // The over-indented `}` is corrected to column 0, with no extra newline.
        assert_eq!(edit.new_text, "}");
        assert!(!edit.new_text.ends_with('\n'));

        // Applying the edit must reproduce a document that still has no trailing
        // newline.
        let mut applied =
            String::from("codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n");
        applied.push_str(&edit.new_text);
        assert!(!applied.ends_with('\n'));
    }

    #[test]
    fn test_format_range_last_line_with_trailing_newline() {
        // Counterpart to the above: when the document DOES end with a newline,
        // the trailing newline must be preserved in the edit.
        let input = "codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n        }\n";
        assert!(input.ends_with('\n'));
        let opts = FormatOptions::default();
        let edits = format_range(input, 5, 5, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "}\n");
    }

    #[test]
    fn test_format_range_non_last_line_keeps_trailing_newline() {
        // A selection that does not reach the final line always keeps the
        // bridging trailing newline, regardless of the document's final newline.
        let input = "codeunit 50100 Test\n{\nprocedure X()\nbegin\nend;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 2, 4, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert!(edits[0].new_text.ends_with('\n'));
    }

    /// Apply a `FormatTextEdit` to `text` the way an LSP client would:
    /// replace the byte span addressed by the (line, character) range with
    /// `new_text`, clamping a position past the last line to the document end.
    /// Test inputs are ASCII, so UTF-16 columns equal byte columns.
    fn apply_format_edit(text: &str, edit: &FormatTextEdit) -> String {
        fn position_offset(text: &str, line: u32, character: u32) -> usize {
            let mut idx = 0usize;
            for _ in 0..line {
                match text[idx..].find('\n') {
                    Some(n) => idx += n + 1,
                    None => return text.len(),
                }
            }
            (idx + character as usize).min(text.len())
        }
        let start = position_offset(text, edit.start_line, edit.start_character);
        let end = position_offset(text, edit.end_line, edit.end_character);
        let mut out = String::with_capacity(text.len() + edit.new_text.len());
        out.push_str(&text[..start]);
        out.push_str(&edit.new_text);
        out.push_str(&text[end..]);
        out
    }

    #[test]
    fn test_format_range_starting_inside_collapsed_blank_run_deletes_line() {
        // With a collapsing blank-line policy, a range that starts on the
        // *second* blank line of a double-blank run used to emit "\n" for the
        // removed line instead of deleting it (a no-op or even growth once
        // applied). The edit must actually remove the collapsed blank line.
        let input = "codeunit 50100 Test\n\
                     {\n\
                     \x20   procedure A()\n\
                     \x20   begin\n\
                     \x20   end;\n\
                     \n\
                     \n\
                     \x20   procedure B()\n\
                     \x20   begin\n\
                     \x20   end;\n\
                     }\n";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
            ..Default::default()
        };

        // Select only line 6 — the second blank of the run.
        let edits = format_range(input, 6, 6, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        let applied = apply_format_edit(input, &edits[0]);
        let expected = "codeunit 50100 Test\n\
                        {\n\
                        \x20   procedure A()\n\
                        \x20   begin\n\
                        \x20   end;\n\
                        \n\
                        \x20   procedure B()\n\
                        \x20   begin\n\
                        \x20   end;\n\
                        }\n";
        assert_eq!(
            applied, expected,
            "the collapsed blank line must be deleted, got edit {:?}",
            edits[0]
        );
    }

    #[test]
    fn test_format_range_starting_in_blank_run_extending_past_it() {
        // Same setup, but the range continues beyond the blank run: the
        // collapsed blank must be deleted while the following lines survive.
        let input = "codeunit 50100 Test\n\
                     {\n\
                     \x20   procedure A()\n\
                     \x20   begin\n\
                     \x20   end;\n\
                     \n\
                     \n\
                     \x20   procedure B()\n\
                     \x20   begin\n\
                     \x20   end;\n\
                     }\n";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
            ..Default::default()
        };

        let edits = format_range(input, 6, 7, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        let applied = apply_format_edit(input, &edits[0]);
        let expected = "codeunit 50100 Test\n\
                        {\n\
                        \x20   procedure A()\n\
                        \x20   begin\n\
                        \x20   end;\n\
                        \n\
                        \x20   procedure B()\n\
                        \x20   begin\n\
                        \x20   end;\n\
                        }\n";
        assert_eq!(
            applied, expected,
            "blank collapsed and procedure B kept, got edit {:?}",
            edits[0]
        );
    }

    #[test]
    fn test_format_range_preserves_context_indent() {
        let input = "codeunit 50100 Test\n{\nprocedure Outer()\nbegin\nif x > 0 then\nMessage(\'yes\');\nend;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 4, 5, &opts);
        assert!(edits.is_some());
        let edits = edits.unwrap();
        assert!(!edits.is_empty());
        assert!(edits[0].new_text.contains("        if x > 0 then"));
        assert!(edits[0].new_text.contains("            Message(\'yes\');"));
    }
}
