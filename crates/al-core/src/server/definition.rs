//! Definition, references, and rename handlers — thin wrappers over al-core.

use tower_lsp::lsp_types::*;

use super::AlServer;

/// Handle textDocument/definition.
pub(crate) fn handle_definition(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let core_pos = position.into();
    let locations = crate::queries::definition::definition(&server.workspace, uri, core_pos)?;
    if locations.len() == 1 {
        // Safety: len == 1 guarantees next() returns Some.
        let loc = locations.into_iter().next()?;
        Some(GotoDefinitionResponse::Scalar(loc.into()))
    } else {
        Some(GotoDefinitionResponse::Array(
            locations.into_iter().map(Into::into).collect(),
        ))
    }
}

/// Handle textDocument/references.
pub(crate) fn handle_references(
    server: &AlServer,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let core_pos = position.into();
    let locations = crate::queries::references::references(
        &server.workspace,
        uri,
        core_pos,
        include_declaration,
    );
    if locations.is_empty() {
        None
    } else {
        Some(locations.into_iter().map(Into::into).collect())
    }
}

/// Handle textDocument/rename.
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

/// Handle textDocument/prepareRename.
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
