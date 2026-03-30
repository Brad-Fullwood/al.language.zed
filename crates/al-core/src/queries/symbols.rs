//! Document symbols query.

use url::Url;

use crate::workspace::Workspace;

/// Get document symbols (outline) for a document.
pub fn document_symbols(
    workspace: &Workspace,
    uri: &Url,
) -> Option<tower_lsp::lsp_types::DocumentSymbolResponse> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let symbols = al_syntax::extract_document_symbols(&tree, &text);
    Some(tower_lsp::lsp_types::DocumentSymbolResponse::Nested(
        symbols,
    ))
}
