//! Folding ranges query.

use url::Url;

use super::AlFoldingRange;
use crate::workspace::Workspace;

/// Returns transport-agnostic `AlFoldingRange` values; al-lsp converts to
/// `tower_lsp::lsp_types::FoldingRange` at the boundary.
#[must_use]
pub fn folding_ranges(workspace: &Workspace, uri: &Url) -> Option<Vec<AlFoldingRange>> {
    let (text, tree) = crate::parsing::get_or_parse(&workspace.documents, uri)?;
    let ranges = crate::syntax::extract_folding_ranges(&tree, &text);
    Some(ranges.into_iter().map(Into::into).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use url::Url;

    #[test]
    fn folding_ranges_returns_ranges_for_open_document() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/fold.al").expect("test");
        let src = r#"codeunit 50100 "My CU"
{
    procedure DoWork()
    begin
        Message('Hello');
    end;
}"#;
        ws.documents.open(uri.clone(), src.to_string());
        let ranges = folding_ranges(&ws, &uri);
        assert!(
            ranges.is_some(),
            "Should return Some for a known open document"
        );
        let ranges = ranges.expect("test");
        assert!(
            !ranges.is_empty(),
            "Should produce at least one folding range"
        );
    }

    #[test]
    fn folding_ranges_includes_procedure_region() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/fold_proc.al").expect("test");
        let src = r#"codeunit 50100 "Fold"
{
    procedure Alpha()
    begin
        Message('a');
    end;

    procedure Beta()
    begin
        Message('b');
    end;
}"#;
        ws.documents.open(uri.clone(), src.to_string());
        let ranges = folding_ranges(&ws, &uri).expect("test");
        let region_count = ranges
            .iter()
            .filter(|r| r.kind == Some(crate::queries::AlFoldingRangeKind::Region))
            .count();
        assert!(
            region_count >= 2,
            "Expected at least 2 region folds, got {}",
            region_count
        );
    }

    #[test]
    fn folding_ranges_missing_uri_returns_none() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///nonexistent/file.al").expect("test");
        let result = folding_ranges(&ws, &uri);
        assert!(result.is_none(), "Unknown URI must return None");
    }

    #[test]
    fn folding_ranges_empty_document_returns_some_empty() {
        let ws = Workspace::new();
        let uri = Url::parse("file:///test/empty_fold.al").expect("test");
        ws.documents.open(uri.clone(), String::new());
        let result = folding_ranges(&ws, &uri);
        assert!(
            result.is_some(),
            "Empty document should return Some (not None)"
        );
        let ranges = result.expect("test");
        assert!(ranges.is_empty(), "Empty document should produce no folds");
    }
}
