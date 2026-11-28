//! Range formatting: format the whole document for context, then emit edits
//! covering only the requested lines. The passes that move or split lines are
//! off for this, so every edit stays inside the requested lines. Blank runs
//! between procedures still follow the blank line policy.

use super::indent::format_al_keeping_line_count;
use super::options::FormatOptions;

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

    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let formatted_region = extract_formatted_region(&orig_lines, &fmt_lines, start, end);
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
            new_text: formatted_region.join(eol),
        }]);
    }

    let mut new_text = formatted_region.join(eol);
    if !formatted_region.is_empty() {
        new_text.push_str(eol);
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

/// Extract the formatted lines for the original lines `start..=end`.
///
/// Keep-line-count formatting leaves the non-blank lines in order and in
/// number. Only the length of a blank run can change. Both texts are split
/// into blank runs and non-blank lines, and the non-blank lines are paired by
/// position. A blank run that lies wholly in the selection takes its formatted
/// length. A run that straddles the selection edge keeps the lines outside the
/// selection, so only the formatted lines left over for the selection are
/// emitted.
fn extract_formatted_region<'a>(
    orig_lines: &[&'a str],
    fmt_lines: &[&'a str],
    start: usize,
    end: usize,
) -> Vec<&'a str> {
    let selected = start..=end;
    let orig_content = non_blank_indexes(orig_lines);
    let fmt_content = non_blank_indexes(fmt_lines);
    debug_assert_eq!(
        orig_content.len(),
        fmt_content.len(),
        "extract_formatted_region: formatter changed the number of non-blank lines"
    );
    if orig_content.len() != fmt_content.len() {
        // No safe line mapping exists, so the selected lines stay as they are.
        return orig_lines[start..=end].to_vec();
    }

    let non_blank = orig_content.len();
    let mut result = Vec::new();
    for j in 0..=non_blank {
        let orig_run = blank_run(&orig_content, orig_lines.len(), j);
        let fmt_run = blank_run(&fmt_content, fmt_lines.len(), j);
        let formatted_len = fmt_run.len();
        let emitted = if orig_run.is_empty() {
            // An empty run lies in the selection when both lines around it do.
            let around_selected = j > 0
                && j < non_blank
                && selected.contains(&orig_content[j - 1])
                && selected.contains(&orig_content[j]);
            if around_selected {
                formatted_len
            } else {
                0
            }
        } else {
            let inside = orig_run.clone().filter(|i| selected.contains(i)).count();
            let outside = orig_run.len() - inside;
            match (inside, outside) {
                (0, _) => 0,
                (_, 0) => formatted_len,
                _ => formatted_len.saturating_sub(outside),
            }
        };
        result.extend_from_slice(&fmt_lines[fmt_run.start..fmt_run.start + emitted]);

        if j < non_blank && selected.contains(&orig_content[j]) {
            result.push(fmt_lines[fmt_content[j]]);
        }
    }
    result
}

/// Indexes of the lines that are not blank after `trim`.
fn non_blank_indexes(lines: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(i, _)| i)
        .collect()
}

/// Index range of blank run `j`: the lines after non-blank line `j - 1` up to
/// non-blank line `j`. Run 0 starts at the top, and the last run ends at `len`.
fn blank_run(non_blank: &[usize], len: usize, j: usize) -> std::ops::Range<usize> {
    let from = if j == 0 { 0 } else { non_blank[j - 1] + 1 };
    let to = non_blank.get(j).copied().unwrap_or(len);
    from..to
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

    /// Applies edits in LSP coordinates: 0-based lines and UTF-16 columns. An
    /// end line past the last line means the end of the text.
    fn apply_edits(text: &str, edits: &[FormatTextEdit]) -> String {
        fn offset(text: &str, line: u32, character: u32) -> usize {
            let mut line_start = 0usize;
            for _ in 0..line {
                match text[line_start..].find('\n') {
                    Some(n) => line_start += n + 1,
                    None => return text.len(),
                }
            }
            let mut utf16 = 0u32;
            for (i, ch) in text[line_start..].char_indices() {
                if utf16 >= character || ch == '\n' || ch == '\r' {
                    return line_start + i;
                }
                utf16 += ch.len_utf16() as u32;
            }
            text.len()
        }
        // Apply the last edit first, so the offsets of earlier edits stay valid.
        let mut sorted: Vec<&FormatTextEdit> = edits.iter().collect();
        sorted.sort_by_key(|e| std::cmp::Reverse((e.start_line, e.start_character)));
        let mut out = text.to_string();
        for edit in sorted {
            let start = offset(&out, edit.start_line, edit.start_character);
            let end = offset(&out, edit.end_line, edit.end_character);
            out.replace_range(start..end, &edit.new_text);
        }
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

    #[test]
    fn range_format_with_sorted_properties_keeps_lines_outside_the_selection() {
        let input = "table 50100 T\n{\n    Caption = 'X';\n    Access = Public;\n    fields\n    {\n    }\n}\n";
        let opts = FormatOptions {
            sort_properties: true,
            ..FormatOptions::default()
        };
        let edits = format_range(input, 2, 2, &opts).unwrap();
        let applied = apply_edits(input, &edits);
        assert_eq!(applied.matches("Caption = 'X';").count(), 1, "{applied}");
        assert_eq!(applied.matches("Access = Public;").count(), 1, "{applied}");
        assert_eq!(applied.lines().nth(3), input.lines().nth(3), "{applied}");
    }

    #[test]
    fn range_format_two_blank_lines_between_procedures_matches_full_format() {
        let opts = FormatOptions {
            blank_lines_between_procedures: BlankLinesBetweenProcedures::Two,
            ..FormatOptions::default()
        };
        let input = "codeunit 50100 Test\n{\n    procedure A()\n    begin\n    end;\n\n\n\n    procedure B()\n    begin\n    end;\n}\n";
        let edits = format_range(input, 4, 8, &opts).unwrap();
        let applied = apply_edits(input, &edits);
        assert_eq!(applied, crate::formatting::format_al(input, &opts));

        let gap_of_two = "codeunit 50100 Test\n{\n    procedure A()\n    begin\n    end;\n\n\n    procedure B()\n    begin\n    end;\n}\n";
        assert_eq!(format_range(gap_of_two, 4, 8, &opts), Some(vec![]));
    }

    #[test]
    fn range_format_keeps_crlf() {
        let input = "codeunit 50100 Test {\r\n    procedure A()\r\n    begin\r\n    x :=  1;\r\n    end;\r\n}\r\n";
        let edits = format_range(input, 3, 3, &FormatOptions::default()).unwrap();
        let applied = apply_edits(input, &edits);
        let bare_lf = applied
            .match_indices('\n')
            .any(|(i, _)| i == 0 || applied.as_bytes()[i - 1] != b'\r');
        assert!(!bare_lf, "bare LF in {applied:?}");
        assert_eq!(
            applied.lines().nth(3),
            Some("        x :=  1;"),
            "{applied:?}"
        );
    }
}
