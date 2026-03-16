//! Definition, references, and rename handlers — thin wrappers over al-core.

use tower_lsp::lsp_types::*;

use crate::server::AlServer;

/// Handle textDocument/definition.
pub(crate) fn handle_definition(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<GotoDefinitionResponse> {
    let core_pos = al_core::queries::Position { line: position.line, character: position.character };
    let locations = al_core::queries::definition::definition(&server.workspace, uri, core_pos)?;
    if locations.len() == 1 {
        // Safety: len == 1 guarantees next() returns Some.
        let loc = locations.into_iter().next()?;
        Some(GotoDefinitionResponse::Scalar(loc.into()))
    } else {
        Some(GotoDefinitionResponse::Array(locations.into_iter().map(Into::into).collect()))
    }
}

/// Handle textDocument/references.
pub(crate) fn handle_references(
    server: &AlServer,
    uri: &Url,
    position: Position,
    include_declaration: bool,
) -> Option<Vec<Location>> {
    let core_pos = al_core::queries::Position { line: position.line, character: position.character };
    let locations = al_core::queries::references::references(&server.workspace, uri, core_pos, include_declaration);
    if locations.is_empty() { None } else { Some(locations.into_iter().map(Into::into).collect()) }
}

/// Handle textDocument/rename.
pub(crate) fn handle_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
    new_name: String,
) -> Option<WorkspaceEdit> {
    let core_pos = al_core::queries::Position { line: position.line, character: position.character };
    let result = al_core::queries::rename::rename(&server.workspace, uri, core_pos, &new_name)?;
    let mut changes = std::collections::HashMap::new();
    for (uri, edits) in result.changes {
        let lsp_edits: Vec<TextEdit> = edits.into_iter().map(|e| TextEdit {
            range: e.range.into(),
            new_text: e.new_text,
        }).collect();
        changes.insert(uri, lsp_edits);
    }
    Some(WorkspaceEdit { changes: Some(changes), ..Default::default() })
}

/// Handle textDocument/prepareRename.
pub(crate) fn handle_prepare_rename(
    server: &AlServer,
    uri: &Url,
    position: Position,
) -> Option<PrepareRenameResponse> {
    let core_pos = al_core::queries::Position { line: position.line, character: position.character };
    let (range, placeholder) = al_core::queries::rename::prepare_rename(&server.workspace, uri, core_pos)?;
    Some(PrepareRenameResponse::RangeWithPlaceholder {
        range: range.into(),
        placeholder,
    })
}
