//! Format handler — delegates to al-syntax.

use tower_lsp::lsp_types::*;

use crate::server::AlServer;

/// Handle textDocument/formatting.
pub(crate) fn handle_formatting(
    server: &AlServer,
    uri: &Url,
    options: &FormattingOptions,
) -> Option<Vec<TextEdit>> {
    let text = server.documents.get_text(uri)?;

    let format_options = al_syntax::FormatOptions {
        tab_size: options.tab_size as usize,
        insert_spaces: options.insert_spaces,
    };

    let formatted = al_syntax::format_al(&text, &format_options);

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
                character: last_line.len() as u32,
            },
        },
        new_text: formatted,
    }])
}
