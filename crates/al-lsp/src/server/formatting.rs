use tower_lsp::lsp_types::*;

use super::AlServer;

fn format_options(
    config: &al_project::config::FormattingConfig,
    client_options: &FormattingOptions,
) -> al_syntax::FormatOptions {
    use al_project::config::{BlankLinesBetweenProcedures, BraceStyle};

    al_syntax::FormatOptions {
        tab_size: client_options.tab_size as usize,
        insert_spaces: client_options.insert_spaces,
        blank_lines_between_procedures: match config.blank_lines_between_procedures {
            BlankLinesBetweenProcedures::Preserve => {
                al_syntax::BlankLinesBetweenProcedures::Preserve
            }
            BlankLinesBetweenProcedures::One => al_syntax::BlankLinesBetweenProcedures::One,
            BlankLinesBetweenProcedures::Two => al_syntax::BlankLinesBetweenProcedures::Two,
        },
        max_line_length: config.max_line_length,
        brace_style: match config.brace_style {
            BraceStyle::SameLine => al_syntax::BraceStyle::SameLine,
            BraceStyle::NextLine => al_syntax::BraceStyle::NextLine,
        },
        sort_properties: config.sort_properties,
        ..al_syntax::FormatOptions::default()
    }
}

pub(crate) fn handle_formatting(
    server: &AlServer,
    uri: &Url,
    options: &FormattingOptions,
) -> Option<Vec<TextEdit>> {
    let config = server.workspace.config.try_read().ok()?;
    handle_formatting_with_config(server, uri, options, &config.formatting)
}

pub(crate) fn handle_formatting_with_config(
    server: &AlServer,
    uri: &Url,
    options: &FormattingOptions,
    config: &al_project::config::FormattingConfig,
) -> Option<Vec<TextEdit>> {
    let text = server.workspace.documents.get_text(uri)?;

    let format_options = format_options(config, options);

    let formatted = al_syntax::format_al(&text, &format_options);

    if formatted == text {
        return Some(Vec::new());
    }

    Some(vec![TextEdit {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: full_document_end(&text),
        },
        new_text: formatted,
    }])
}

/// The LSP position one past the last character of `text`.
///
/// Pairing `text.lines().count()` with the length of the *last content line*
/// produced an end position that does not exist: for a file ending in `\n` the
/// line index is one past the last content line (which is empty), and for a file
/// without a trailing newline the line index is one past the end of the
/// document entirely. Both only worked because clients clamp. Compute the true
/// end instead: the last line index and its UTF-16 length.
fn full_document_end(text: &str) -> Position {
    // `split('\n')` (unlike `lines()`) keeps the empty segment after a trailing
    // newline, which is exactly the final, empty line of the document. A `\r\n`
    // document keeps the `\r` in the segment; it is a real character on that
    // line, so it counts toward the end column.
    let last_line = text.split('\n').count().saturating_sub(1);
    let last_segment = text.split('\n').next_back().unwrap_or("");
    Position {
        line: u32::try_from(last_line).unwrap_or(u32::MAX),
        character: u32::try_from(last_segment.encode_utf16().count()).unwrap_or(u32::MAX),
    }
}

/// Handle textDocument/rangeFormatting.
///
/// Delegates to `al_syntax::format_range` which formats the full document
/// for correct indent context, then returns edits covering only the selected lines.
///
/// **By design:** because the indent of a line in AL depends on the
/// enclosing block structure, the indent emitted for the *selected* lines is
/// derived from a whole-document formatter pass. The selection's own indent is
/// therefore corrected relative to its true block depth, which can differ from
/// the (possibly mis-indented) surrounding lines that are left untouched. The
/// returned edits never span outside the requested range, so unselected lines
/// are never rewritten — they may simply end up at a different indent level than
/// the freshly-formatted selection. This matches the AL formatter convention of
/// always indenting to the structurally-correct depth. The trailing-newline
/// edge case for a last-line selection in a document without a final newline is
/// handled inside `format_range`.
pub(crate) fn handle_range_formatting_with_config(
    server: &AlServer,
    uri: &Url,
    range: Range,
    options: &FormattingOptions,
    config: &al_project::config::FormattingConfig,
) -> Option<Vec<TextEdit>> {
    let text = server.workspace.documents.get_text(uri)?;

    let format_options = format_options(config, options);

    al_syntax::format_range(&text, range.start.line, range.end.line, &format_options).map(|edits| {
        edits
            .into_iter()
            .map(|e| TextEdit {
                range: Range {
                    start: Position {
                        line: e.start_line,
                        character: e.start_character,
                    },
                    end: Position {
                        line: e.end_line,
                        character: e.end_character,
                    },
                },
                new_text: e.new_text,
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_document_end_is_in_bounds_for_a_trailing_newline() {
        // Three content lines plus the empty final line created by the
        // trailing newline: the edit must end at (3, 0), not (3, len("c")).
        let end = full_document_end("a\nbb\nccc\n");
        assert_eq!(
            end,
            Position {
                line: 3,
                character: 0
            }
        );
    }

    #[test]
    fn full_document_end_without_trailing_newline_lands_on_the_last_line() {
        let end = full_document_end("a\nbb\nccc");
        assert_eq!(
            end,
            Position {
                line: 2,
                character: 3
            }
        );
    }

    #[test]
    fn full_document_end_counts_utf16_units() {
        // "é" is one UTF-16 unit but two UTF-8 bytes; "𝄞" is two UTF-16 units.
        let end = full_document_end("codeunit 1 X\n// é𝄞");
        assert_eq!(
            end,
            Position {
                line: 1,
                character: "// ".encode_utf16().count() as u32 + 3
            }
        );
    }

    #[test]
    fn full_document_end_of_empty_document_is_the_origin() {
        assert_eq!(
            full_document_end(""),
            Position {
                line: 0,
                character: 0
            }
        );
    }

    #[test]
    fn settings_advanced_options_are_mapped_to_formatter() {
        let config = al_project::config::FormattingConfig {
            blank_lines_between_procedures: al_project::config::BlankLinesBetweenProcedures::Two,
            max_line_length: 72,
            brace_style: al_project::config::BraceStyle::SameLine,
            sort_properties: true,
        };
        let options = format_options(
            &config,
            &FormattingOptions {
                tab_size: 2,
                insert_spaces: false,
                ..Default::default()
            },
        );

        assert_eq!(options.tab_size, 2);
        assert!(!options.insert_spaces);
        assert!(matches!(
            options.blank_lines_between_procedures,
            al_syntax::BlankLinesBetweenProcedures::Two
        ));
        assert_eq!(options.max_line_length, 72);
        assert!(matches!(
            options.brace_style,
            al_syntax::BraceStyle::SameLine
        ));
        assert!(options.sort_properties);
    }
}
