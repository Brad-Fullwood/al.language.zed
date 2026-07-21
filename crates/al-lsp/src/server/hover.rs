use tower_lsp::lsp_types::*;

use super::AlServer;

pub(crate) async fn handle_hover(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<Hover> {
    let core_pos = position.into();
    let result = al_analysis::queries::hover::hover_full(&server.workspace, uri, core_pos).await?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: result.contents,
        }),
        range: result.range.map(|r| r.into()),
    })
}
