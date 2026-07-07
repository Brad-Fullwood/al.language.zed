use tower_lsp::lsp_types::*;

use super::AlServer;

pub(crate) fn handle_formatting(
    server: &AlServer,
    uri: &Url,
    options: &FormattingOptions,
) -> Option<Vec<TextEdit>> {
    let text = server.workspace.documents.get_text(uri)?;

    let format_options = al_syntax::FormatOptions {
        tab_size: options.tab_size as usize,
        insert_spaces: options.insert_spaces,
        ..al_syntax::FormatOptions::default()
    };

    let formatted = al_syntax::format_al(&text, &format_options);

    if formatted == text {
        return Some(Vec::new());
    }

    let line_count = text.lines().count();
    let last_line = text.lines().last().unwrap_or("");

    Some(vec![TextEdit {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: u32::try_from(line_count).unwrap_or(u32::MAX),
                character: u32::try_from(last_line.encode_utf16().count()).unwrap_or(u32::MAX),
            },
        },
        new_text: formatted,
    }])
}

/// Handle textDocument/rangeFormatting.
///
/// Delegates to `al_syntax::format_range` which formats the full document
/// for correct indent context, then returns edits covering only the selected lines.
///
/// **By design (F-OPEN-112):** because the indent of a line in AL depends on the
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
pub(crate) fn handle_range_formatting(
    server: &AlServer,
    uri: &Url,
    range: Range,
    options: &FormattingOptions,
) -> Option<Vec<TextEdit>> {
    let text = server.workspace.documents.get_text(uri)?;

    let format_options = al_syntax::FormatOptions {
        tab_size: options.tab_size as usize,
        insert_spaces: options.insert_spaces,
        ..Default::default()
    };

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
