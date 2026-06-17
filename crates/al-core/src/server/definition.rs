//! Definition, references, and rename handlers — thin wrappers over al-core.

use tower_lsp::lsp_types::*;

use super::AlServer;

pub(crate) fn handle_definition(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let core_pos = position.into();
    let locations = crate::queries::definition::definition(&server.workspace, uri, core_pos)?;
    // Return `LocationLink`s carrying an `originSelectionRange` — the full
    // identifier under the cursor. With it, Zed highlights the whole symbol on
    // Ctrl-hover (including multi-word quoted names like "Sales Shipment
    // Header"); without it Zed falls back to its own single-word boundary.
    let origin = origin_selection_range(server, uri, core_pos);
    let links: Vec<LocationLink> = locations
        .into_iter()
        .map(|loc| {
            let l: Location = loc.into();
            LocationLink {
                origin_selection_range: origin,
                target_uri: l.uri,
                target_range: l.range,
                target_selection_range: l.range,
            }
        })
        .collect();
    Some(GotoDefinitionResponse::Link(links))
}

/// The range of the identifier at `pos` — used as the go-to-definition
/// `originSelectionRange`. Uses the tree-sitter node span, so a quoted
/// identifier reports its whole `"…"` extent rather than one word.
fn origin_selection_range(
    server: &AlServer,
    uri: &Url,
    pos: crate::queries::Position,
) -> Option<Range> {
    let (text, tree) = crate::parsing::get_or_parse(&server.workspace.documents, uri)?;
    let node = crate::syntax::find_node_at_position(&tree, &text, pos.into())?;
    let core: crate::queries::Range =
        crate::syntax::ts_range_to_syntax(&node.range(), text.as_bytes()).into();
    Some(core.into())
}

// T028: handle_references was inlined into the LanguageServer::references impl
// and wrapped in spawn_blocking so it can be cancel-friendly. Removed.

pub(crate) fn handle_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
    new_name: String,
) -> Option<WorkspaceEdit> {
    let core_pos = position.into();
    let result = crate::queries::rename::rename(&server.workspace, uri, core_pos, &new_name)?;
    Some(super::handlers::core_workspace_edit_to_lsp(result))
}

pub(crate) fn handle_prepare_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let core_pos = position.into();
    let (range, placeholder) =
        crate::queries::rename::prepare_rename(&server.workspace, uri, core_pos)?;
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: range.into(),
        placeholder,
    })
}
