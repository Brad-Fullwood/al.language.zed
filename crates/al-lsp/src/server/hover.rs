use tower_lsp::lsp_types::*;

use super::AlServer;

pub(crate) async fn handle_hover(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Result<Option<Hover>, String> {
    let core_pos = position.into();
    let Some(result) =
        al_analysis::queries::hover::hover_full(&server.workspace, uri, core_pos).await?
    else {
        return Ok(None);
    };
    Ok(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: result.contents,
        }),
        range: result.range.map(|r| r.into()),
    }))
}
