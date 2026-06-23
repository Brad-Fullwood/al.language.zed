//! Document symbols query.

use url::Url;

use super::AlDocumentSymbol;
use al_workspace::Workspace;

/// Get document symbols (outline) for a document.
///
/// Returns transport-agnostic `AlDocumentSymbol` values; al-lsp converts to
/// `tower_lsp::lsp_types::DocumentSymbol` at the boundary.
pub fn document_symbols(workspace: &Workspace, uri: &Url) -> Option<Vec<AlDocumentSymbol>> {
    let (text, tree) = al_source::parsing::get_or_parse(&workspace.documents, uri)?;
    let symbols = al_syntax::extract_document_symbols(&tree, &text);
    Some(symbols.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::AlSymbolKind;
    use al_workspace::Workspace;

    #[test]
    fn document_symbols_returns_top_level_object() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/sym.al").expect("test");
        let src = r#"codeunit 50100 "My CU"
{
    procedure DoWork()
    begin
        Message('Hello');
    end;
}"#;
        ws.documents.open(uri.clone(), src.to_string());

        let symbols = document_symbols(&ws, &uri).expect("open document yields Some");
        assert_eq!(
            symbols.len(),
            1,
            "a single object declaration produces one top-level symbol"
        );
        let cu = &symbols[0];
        assert!(
            cu.name.contains("My CU"),
            "top-level symbol name should carry the object name, got {:?}",
            cu.name
        );
        // A codeunit maps to the Class symbol kind via object_types.json.
        assert_eq!(cu.kind, AlSymbolKind::Class);
    }

    #[test]
    fn document_symbols_includes_procedure_children() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/sym_children.al").expect("test");
        let src = r#"codeunit 50100 "Outline"
{
    procedure Alpha()
    begin
    end;

    procedure Beta()
    begin
    end;
}"#;
        ws.documents.open(uri.clone(), src.to_string());

        let symbols = document_symbols(&ws, &uri).expect("open document yields Some");
        let object = &symbols[0];
        let children = object
            .children
            .as_ref()
            .expect("object with procedures has children");
        let names: Vec<&str> = children.iter().map(|c| c.name.as_str()).collect();
        assert!(
            names.iter().any(|n| n.contains("Alpha")),
            "expected an Alpha procedure child, got {:?}",
            names
        );
        assert!(
            names.iter().any(|n| n.contains("Beta")),
            "expected a Beta procedure child, got {:?}",
            names
        );
    }

    #[test]
    fn document_symbols_handles_multiple_objects() {
        // Two object declarations in one file (valid for some AL layouts) must
        // each surface as a distinct top-level symbol.
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/sym_multi.al").expect("test");
        let src = r#"table 50100 "Customer Ext"
{
}

codeunit 50101 "Helper"
{
}"#;
        ws.documents.open(uri.clone(), src.to_string());

        let symbols = document_symbols(&ws, &uri).expect("open document yields Some");
        assert_eq!(
            symbols.len(),
            2,
            "each object declaration yields its own top-level symbol, got {:?}",
            symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    #[test]
    fn document_symbols_missing_uri_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent/file.al").expect("test");
        // No document opened: get_or_parse returns None, so we must propagate None
        // (not Some([])). This distinguishes "unknown document" from "no symbols".
        assert!(
            document_symbols(&ws, &uri).is_none(),
            "an unopened URI must return None"
        );
    }

    #[test]
    fn document_symbols_empty_document_returns_some_empty() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/empty_sym.al").expect("test");
        ws.documents.open(uri.clone(), String::new());

        let result = document_symbols(&ws, &uri);
        assert!(
            result.is_some(),
            "an opened (if empty) document must return Some, not None"
        );
        assert!(
            result.expect("test").is_empty(),
            "an empty document declares no symbols"
        );
    }

    #[test]
    fn document_symbols_non_object_content_returns_some_empty() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/garbage.al").expect("test");
        ws.documents
            .open(uri.clone(), "// just a comment\nfoo bar baz".to_string());

        let result = document_symbols(&ws, &uri).expect("opened document yields Some");
        assert!(
            result.is_empty(),
            "content without an object declaration yields no symbols, got {:?}",
            result.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }
}
