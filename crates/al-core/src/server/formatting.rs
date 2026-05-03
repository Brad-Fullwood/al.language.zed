//! Format handler — delegates to crate::syntax.

use tower_lsp::lsp_types::*;

use super::AlServer;

/// Handle textDocument/formatting.
pub(crate) fn handle_formatting(
    server: &AlServer,
    uri: &Url,
    options: &FormattingOptions,
) -> Option<Vec<TextEdit>> {
    let text = server.workspace.documents.get_text(uri)?;

    let format_options = crate::syntax::FormatOptions {
        tab_size: options.tab_size as usize,
        insert_spaces: options.insert_spaces,
        ..crate::syntax::FormatOptions::default()
    };

    let formatted = crate::syntax::format_al(&text, &format_options);

    // If unchanged, return no edits
    if formatted == text {
        return Some(Vec::new());
    }

    // Replace the entire document
    let line_count = text.lines().count();
    let last_line = text.lines().last().unwrap_or("");

    Some(vec![TextEdit {
        range: Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: Position {
                line: line_count as u32,
                character: last_line.encode_utf16().count() as u32,
            },
        },
        new_text: formatted,
    }])
}

/// Handle textDocument/rangeFormatting.
///
/// Delegates to `crate::syntax::format_range` which formats the full document
/// for correct indent context, then returns edits covering only the selected lines.
pub(crate) fn handle_range_formatting(
    server: &AlServer,
    uri: &Url,
    range: Range,
    options: &FormattingOptions,
) -> Option<Vec<TextEdit>> {
    let text = server.workspace.documents.get_text(uri)?;

    let format_options = crate::syntax::FormatOptions {
        tab_size: options.tab_size as usize,
        insert_spaces: options.insert_spaces,
        ..Default::default()
    };

    crate::syntax::format_range(&text, range.start.line, range.end.line, &format_options).map(
        |edits| {
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
        },
    )
}
