//! Document symbols query.

use url::Url;

use super::Range;
use crate::workspace::Workspace;

/// A document symbol (outline entry).
#[derive(Debug, Clone)]
pub struct DocumentSymbolEntry {
    pub name: String,
    pub detail: Option<String>,
    pub kind: SymbolKind,
    pub range: Range,
    pub selection_range: Range,
    pub children: Vec<DocumentSymbolEntry>,
}

/// Symbol kind (transport-agnostic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    File, Module, Class, Method, Property, Field, Constructor, Enum, Interface,
    Function, Variable, Constant, Struct, Event, Key, EnumMember, Object,
}

/// Get document symbols (outline) for a document.
pub fn document_symbols(workspace: &Workspace, uri: &Url) -> Option<tower_lsp::lsp_types::DocumentSymbolResponse> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let symbols = al_syntax::extract_document_symbols(&tree, &text);
    Some(tower_lsp::lsp_types::DocumentSymbolResponse::Nested(symbols))
}
