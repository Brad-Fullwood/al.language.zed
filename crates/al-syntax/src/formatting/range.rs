//! Range formatting: format the whole document for context, then emit edits
//! covering only the requested lines. The passes that change the line count
//! are off for this, so every edit stays inside the requested lines.

use super::indent::format_al_keeping_line_count;
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
    let formatted_full = format_al_keeping_line_count(text, options);

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

    // The edit replaces the selected lines with their line breaks, so a
    // collapsed blank line inside the selection is deleted and no line break
    // is added. On the document's last line with no trailing newline there is
    // no line break to replace, and the new text gets none. `.lines()` drops
    // that distinction, so `text` is consulted directly.
    let end_is_last_line = end == orig_lines.len().saturating_sub(1);
    if end_is_last_line && !text.ends_with('\n') {
        let end_char = crate::byte_col_to_utf16_col(orig_lines[end], orig_lines[end].len());
        return Some(vec![FormatTextEdit {
            start_line,
            start_character: 0,
            end_line: end as u32,
            end_character: end_char,
            new_text: formatted_region.join("\n"),
        }]);
    }

    let mut new_text = formatted_region.join("\n");
    if !formatted_region.is_empty() {
        new_text.push('\n');
    }
    // `end`, not `end_line`: the caller may have named a line past the end of
    // the document. LSP clients clamp `(end + 1, 0)` past the last line to
    // the document end, which is the trailing newline.
    Some(vec![FormatTextEdit {
        start_line,
        start_character: 0,
        end_line: end as u32 + 1,
        end_character: 0,
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

    /// The full format moves `{` onto its own line, which shifts every later
    /// line by one. The edit for line 3 used to carry the new line 3, `begin`.
    #[test]
    fn range_format_with_next_line_brace_edits_only_range() {
        let input =
            "codeunit 50100 Test {\n    procedure A()\n    begin\n    x :=  1;\n    end;\n}\n";
        let edits = format_range(input, 3, 3, &FormatOptions::default()).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!((edits[0].start_line, edits[0].end_line), (3, 4));
        assert_eq!(edits[0].new_text, "        x :=  1;\n");
    }

    /// `One` inserts a blank line between procedures in a full format, and
    /// `SameLine` merges lines. Neither may move the text of a range edit.
    #[test]
    fn range_format_ignores_passes_that_change_the_line_count() {
        let input = "codeunit 50100 Test\n{\n    procedure A()\n    begin\n    end;\n    procedure B()\n    begin\n    x := 1;\n    end;\n}\n";
        for opts in [
            FormatOptions {
                blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
                ..FormatOptions::default()
            },
            FormatOptions {
                brace_style: crate::formatting::BraceStyle::SameLine,
                ..FormatOptions::default()
            },
        ] {
            let edits = format_range(input, 7, 7, &opts).unwrap();
            assert_eq!(edits.len(), 1);
            assert_eq!((edits[0].start_line, edits[0].end_line), (7, 8));
            assert_eq!(edits[0].new_text, "        x := 1;\n");
        }
    }

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
        assert_eq!(edit.end_line, 6);
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
        assert_eq!(edits[0].end_line, last as u32 + 1);
        assert_eq!(edits[0].end_character, 0);
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
        assert_eq!(
            apply_format_edit(input, &edits[0]),
            "codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n}\n"
        );
    }

    #[test]
    fn test_format_range_non_last_line_keeps_trailing_newline() {
        // A selection that does not reach the final line always keeps the
        // bridging trailing newline, regardless of the document's final newline.
        let input = "codeunit 50100 Test\n{\nprocedure X()\nbegin\nend;\n}";
        let opts = FormatOptions::default();
        let edits = format_range(input, 2, 4, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(
            apply_format_edit(input, &edits[0]),
            "codeunit 50100 Test\n{\n    procedure X()\n    begin\n    end;\n}"
        );
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

    /// A single isolated blank line (its neighbour above has real content) is
    /// not the second line of a collapsed run of two blank lines. Selecting a
    /// range that starts on it must keep it.
    #[test]
    fn test_format_range_starting_on_an_isolated_blank_line_is_not_a_collapsed_run() {
        let input = "codeunit 50100 Test\n\
                     {\n\
                     \x20   procedure A()\n\
                     \x20   begin\n\
                     \x20   end;\n\
                     \n\
                     procedure B()\n\
                     begin\n\
                     end;\n\
                     }\n";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
            ..Default::default()
        };

        // Line 5 is the lone blank. Line 4 ("    end;") is not blank, so this
        // is not a run of two blank lines. Range [5, 7] also covers
        // "procedure B()" and "begin", which are indented wrong, so the region
        // does differ from the original and the function reaches the check
        // for a collapsed blank line instead of returning early.
        let edits = format_range(input, 5, 7, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].end_line, 8);
        assert_eq!(edits[0].new_text, "\n    procedure B()\n    begin\n");
        assert_eq!(
            apply_format_edit(input, &edits[0]),
            "codeunit 50100 Test\n{\n    procedure A()\n    begin\n    end;\n\n    procedure B()\n    begin\nend;\n}\n"
        );
    }

    /// A first line of the document that holds only whitespace (`start == 0`)
    /// must short-circuit on `start > 0` before any `start - 1` indexing is
    /// attempted. Selecting just that line, where the trailing whitespace
    /// itself needs stripping, exercises the boundary directly.
    #[test]
    fn test_format_range_leading_whitespace_only_line_at_start_zero() {
        let input = "   \ncodeunit 50100 Test\n{\n}\n";
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::One,
            ..Default::default()
        };
        let edits = format_range(input, 0, 0, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].end_line, 1);
        assert_eq!(edits[0].new_text, "\n");
        assert_eq!(
            apply_format_edit(input, &edits[0]),
            "\ncodeunit 50100 Test\n{\n}\n"
        );
    }

    /// Applying a range edit added an empty line after the selection, because
    /// the edit kept the end line's line break and the new text added one.
    #[test]
    fn applied_range_format_adds_no_blank_line() {
        let cases: [(&str, u32, u32); 4] = [
            (
                "codeunit 50100 Test {\n    procedure A()\n    begin\n    x :=  1;\n    end;\n}\n",
                3,
                3,
            ),
            (
                "codeunit 50100 Test\n{\nprocedure DoSomething()\nbegin\nMessage('Hello');\nend;\n}",
                2,
                5,
            ),
            ("codeunit 50100 Test\n{\nprocedure X()\nbegin\nend;\n}\n", 0, 9999),
            (
                "codeunit 50100 Test\n{\nprocedure Outer()\nbegin\nif x > 0 then\nMessage('yes');\nend;\n}",
                4,
                5,
            ),
        ];
        for (input, start, end) in cases {
            let edits = format_range(input, start, end, &FormatOptions::default()).unwrap();
            assert_eq!(edits.len(), 1, "{input:?}");
            let applied = apply_format_edit(input, &edits[0]);
            assert_eq!(
                applied.lines().count(),
                input.lines().count(),
                "{applied:?}"
            );
            assert_eq!(
                applied.ends_with('\n'),
                input.ends_with('\n'),
                "{applied:?}"
            );
        }
    }

    /// A header ending in `{` is the case where a full format splits the
    /// brace onto its own line. The selected statement must stay on its line.
    #[test]
    fn range_format_after_split_header_brace_keeps_the_statement() {
        let input =
            "codeunit 50100 X {\n    procedure P()\n    begin\n    Message('hello');\n    end;\n}\n";
        let edits = format_range(input, 3, 3, &FormatOptions::default()).unwrap();
        assert_eq!(edits.len(), 1);
        let applied = apply_format_edit(input, &edits[0]);
        let lines: Vec<&str> = applied.lines().collect();
        let input_lines: Vec<&str> = input.lines().collect();
        assert_eq!(
            applied.matches("Message('hello');").count(),
            1,
            "{applied:?}"
        );
        assert_eq!(lines[3], "        Message('hello');");
        assert_eq!(lines[..3], input_lines[..3]);
        assert_eq!(lines[4..], input_lines[4..]);
    }

    /// `SameLine` merges a stand-alone `{` into the header in a full format.
    /// A range edit after the brace keeps its line and touches no other.
    #[test]
    fn range_format_same_line_brace_merge() {
        let input = "codeunit 50100 X\n{\n    procedure P()\n    {\n    begin\n    Message('hello');\n    end;\n}\n";
        let opts = FormatOptions {
            brace_style: crate::formatting::BraceStyle::SameLine,
            ..FormatOptions::default()
        };
        let edits = format_range(input, 5, 5, &opts).unwrap();
        assert_eq!(edits.len(), 1);
        let applied = apply_format_edit(input, &edits[0]);
        let lines: Vec<&str> = applied.lines().collect();
        let input_lines: Vec<&str> = input.lines().collect();
        assert_eq!(lines.len(), input_lines.len(), "{applied:?}");
        assert_eq!(lines[5].trim(), "Message('hello');");
        for (i, (got, want)) in lines.iter().zip(&input_lines).enumerate() {
            if i != 5 {
                assert_eq!(got, want, "line {i} changed");
            }
        }
    }
}
