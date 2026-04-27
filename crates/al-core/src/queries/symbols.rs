//! Document symbols query.

use url::Url;

use super::AlDocumentSymbol;
use crate::workspace::Workspace;

/// Get document symbols (outline) for a document.
///
/// Returns transport-agnostic `AlDocumentSymbol` values; al-lsp converts to
/// `tower_lsp::lsp_types::DocumentSymbol` at the boundary.
pub fn document_symbols(workspace: &Workspace, uri: &Url) -> Option<Vec<AlDocumentSymbol>> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let symbols = crate::syntax::extract_document_symbols(&tree, &text);
    Some(symbols.into_iter().map(Into::into).collect())
}
