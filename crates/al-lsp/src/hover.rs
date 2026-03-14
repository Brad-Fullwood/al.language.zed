//! Hover handler — thin wrapper over al-core::queries::hover.

use tower_lsp::lsp_types::*;

use crate::server::AlServer;

/// Handle textDocument/hover.
pub(crate) fn handle_hover(server: &AlServer, uri: &Url, position: Position) -> Option<Hover> {
    let core_pos = al_core::queries::Position { line: position.line, character: position.character };
    let result = al_core::queries::hover::hover(&server.workspace, uri, core_pos)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: result.contents,
        }),
        range: result.range.map(|r| r.into()),
    })
}
