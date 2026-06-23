//! Workspace symbol search query.
//!
//! Transport-agnostic search over workspace .al file objects.  Both the LSP
//! `workspace/symbol` handler and the daemon `search` RPC call this function;
//! each caller converts the result to its own response type.

use std::path::PathBuf;

use super::{AlSymbolKind, Range};
use al_source::file_index::CachedObjectInfo;
use al_workspace::Workspace;

/// A single workspace-file search result.
#[derive(Debug, Clone)]
pub struct WorkspaceSearchResult {
    /// File path containing the object.
    pub file_path: PathBuf,
    /// Cached object metadata.
    pub info: CachedObjectInfo,
}

/// Case-insensitive ASCII substring check without allocation.
/// `query_lower` must already be lowercase.
fn ascii_contains_ci(haystack: &str, query_lower: &str) -> bool {
    let q = query_lower.as_bytes();
    let h = haystack.as_bytes();
    if q.len() > h.len() {
        return false;
    }
    h.windows(q.len())
        .any(|w| w.iter().zip(q).all(|(a, b)| a.to_ascii_lowercase() == *b))
}

/// Search workspace .al file objects whose name contains `query` (case-insensitive).
///
/// - When `query` is empty, all objects are returned up to `limit`.
/// - When `query` is non-empty, only objects whose name contains the query are returned.
/// - Results are limited to `limit` entries.
pub fn workspace_search(
    workspace: &Workspace,
    query: &str,
    limit: usize,
) -> Vec<WorkspaceSearchResult> {
    let mut results = Vec::new();
    let query_lower = query.to_lowercase();

    for entry in workspace.file_index.object_info.iter() {
        if results.len() >= limit {
            break;
        }
        let file_path = entry.key().clone();
        let info = entry.value().clone();
        if !query.is_empty() && !ascii_contains_ci(&info.name, &query_lower) {
            continue;
        }
        results.push(WorkspaceSearchResult { file_path, info });
    }

    results
}

/// A single child symbol search result (procedure, trigger, event, etc.).
#[derive(Debug, Clone)]
pub struct WorkspaceChildSearchResult {
    /// File path containing the symbol.
    pub file_path: PathBuf,
    /// Symbol name (e.g. procedure name).
    pub name: String,
    /// Transport-agnostic symbol kind.
    pub kind: AlSymbolKind,
    /// Range of the symbol in the file.
    pub range: Range,
    /// Name of the parent object (container), or empty string if unknown.
    pub container_name: String,
}

/// Search workspace .al file child symbols (procedures, triggers, events) whose
/// name contains `query` (case-insensitive).
///
/// Uses cached parse trees from the file index to avoid re-parsing.
/// - When `query` is empty, all child symbols are returned up to `limit`.
/// - When `query` is non-empty, only symbols whose name contains the query are returned.
/// - Results are limited to `limit` entries.
pub fn workspace_search_children(
    workspace: &Workspace,
    query: &str,
    limit: usize,
) -> Vec<WorkspaceChildSearchResult> {
    let mut results = Vec::new();
    let query_lower = query.to_lowercase();

    for entry in workspace.file_index.files.iter() {
        if results.len() >= limit {
            break;
        }
        let file_path = entry.key().clone();
        drop(entry); // release dashmap lock before accessing symbols

        let doc_symbols: Vec<super::AlDocumentSymbol> =
            match workspace.file_index.get_cached_symbols(&file_path) {
                Some(s) => s.into_iter().map(Into::into).collect(),
                None => continue,
            };
        for sym in &doc_symbols {
            let container_name = sym.name.clone();
            if let Some(children) = &sym.children {
                for child in children {
                    if results.len() >= limit {
                        break;
                    }
                    if !query.is_empty() && !ascii_contains_ci(&child.name, &query_lower) {
                        continue;
                    }
                    results.push(WorkspaceChildSearchResult {
                        file_path: file_path.clone(),
                        name: child.name.clone(),
                        kind: child.kind,
                        range: child.range,
                        container_name: container_name.clone(),
                    });
                }
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    #[test]
    fn ascii_contains_ci_matches_case_insensitively() {
        assert!(ascii_contains_ci("CustomerLedgerEntry", "customer"));
        assert!(ascii_contains_ci("customerledgerentry", "customer"));
        assert!(ascii_contains_ci("CUSTOMERLEDGERENTRY", "customer"));
    }

    #[test]
    fn ascii_contains_ci_matches_interior_and_boundary_substrings() {
        assert!(ascii_contains_ci("MyTestTable", "my"));
        assert!(ascii_contains_ci("MyTestTable", "test"));
        assert!(ascii_contains_ci("MyTestTable", "table"));
    }

    #[test]
    fn ascii_contains_ci_rejects_non_substring() {
        assert!(!ascii_contains_ci("MyTestTable", "vendor"));
    }

    #[test]
    fn ascii_contains_ci_query_longer_than_haystack_is_false() {
        assert!(!ascii_contains_ci("abc", "abcd"));
        assert!(!ascii_contains_ci("", "x"));
    }

    #[test]
    fn ascii_contains_ci_single_byte_query_boundary() {
        // `windows(0)` panics; callers guard `!query.is_empty()` so the minimum input is 1 byte.
        assert!(ascii_contains_ci("Foo", "f"));
        assert!(ascii_contains_ci("Foo", "o"));
        assert!(!ascii_contains_ci("Foo", "z"));
    }

    #[test]
    fn ascii_contains_ci_full_string_equality_matches() {
        assert!(ascii_contains_ci("Foo", "foo"));
    }

    fn ws_with_objects() -> Workspace {
        let ws = Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/proj/CustomerCard.al"),
            r#"page 50100 "Customer Card" { }"#.to_string(),
        );
        ws.file_index.add_file(
            PathBuf::from("/proj/CustomerLedger.al"),
            r#"table 50101 "Customer Ledger" { fields { field(1; "No."; Code[20]) { } } }"#
                .to_string(),
        );
        ws.file_index.add_file(
            PathBuf::from("/proj/VendorCard.al"),
            r#"page 50102 "Vendor Card" { }"#.to_string(),
        );
        ws
    }

    #[test]
    fn workspace_search_empty_query_returns_all_up_to_limit() {
        let ws = Workspace::new();
        // No files loaded — should return empty without panic.
        let results = workspace_search(&ws, "", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn workspace_search_with_query_filters_by_name() {
        let ws = Workspace::new();
        let results = workspace_search(&ws, "Customer", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn workspace_search_empty_query_returns_all_objects() {
        let ws = ws_with_objects();
        let results = workspace_search(&ws, "", 100);
        assert_eq!(results.len(), 3, "empty query returns every indexed object");
    }

    #[test]
    fn workspace_search_filters_by_name_case_insensitively() {
        let ws = ws_with_objects();
        let results = workspace_search(&ws, "customer", 100);
        assert_eq!(results.len(), 2, "two objects contain 'customer'");
        for r in &results {
            assert!(r.info.name.to_lowercase().contains("customer"));
        }
    }

    #[test]
    fn workspace_search_no_match_returns_empty() {
        let ws = ws_with_objects();
        let results = workspace_search(&ws, "Item", 100);
        assert!(results.is_empty(), "no object name contains 'Item'");
    }

    #[test]
    fn workspace_search_respects_limit() {
        let ws = ws_with_objects();
        let results = workspace_search(&ws, "", 2);
        assert_eq!(results.len(), 2, "limit must cap the number of results");
    }

    #[test]
    fn workspace_search_zero_limit_returns_nothing() {
        let ws = ws_with_objects();
        let results = workspace_search(&ws, "", 0);
        assert!(
            results.is_empty(),
            "a limit of 0 must short-circuit before pushing any result"
        );
    }

    #[test]
    fn workspace_search_result_carries_path_and_info() {
        let ws = ws_with_objects();
        let results = workspace_search(&ws, "Vendor", 100);
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.file_path, PathBuf::from("/proj/VendorCard.al"));
        assert_eq!(r.info.name, "Vendor Card");
        assert_eq!(r.info.kind, "page");
    }

    fn ws_with_children() -> Workspace {
        let ws = Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/proj/MathUtil.al"),
            r#"codeunit 50100 "Math Util"
{
    procedure AddNumbers(a: Integer; b: Integer): Integer
    begin
    end;

    procedure SubtractNumbers(a: Integer; b: Integer): Integer
    begin
    end;
}
"#
            .to_string(),
        );
        ws
    }

    #[test]
    fn workspace_search_children_empty_query_returns_all_procedures() {
        let ws = ws_with_children();
        let results = workspace_search_children(&ws, "", 100);
        assert_eq!(
            results.len(),
            2,
            "both procedures should be returned for an empty query"
        );
        let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"AddNumbers"));
        assert!(names.contains(&"SubtractNumbers"));
    }

    #[test]
    fn workspace_search_children_filters_case_insensitively() {
        let ws = ws_with_children();
        let results = workspace_search_children(&ws, "subtract", 100);
        assert_eq!(results.len(), 1, "only SubtractNumbers contains 'subtract'");
        assert_eq!(results[0].name, "SubtractNumbers");
    }

    #[test]
    fn workspace_search_children_sets_container_name() {
        let ws = ws_with_children();
        let results = workspace_search_children(&ws, "AddNumbers", 100);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].container_name, "Math Util",
            "child result must record its parent object name"
        );
        assert_eq!(results[0].file_path, PathBuf::from("/proj/MathUtil.al"));
    }

    #[test]
    fn workspace_search_children_no_match_returns_empty() {
        let ws = ws_with_children();
        let results = workspace_search_children(&ws, "Divide", 100);
        assert!(results.is_empty(), "no procedure named like 'Divide'");
    }

    #[test]
    fn workspace_search_children_respects_limit() {
        let ws = ws_with_children();
        let results = workspace_search_children(&ws, "", 1);
        assert_eq!(
            results.len(),
            1,
            "limit must cap child results even when more exist"
        );
    }

    #[test]
    fn workspace_search_children_empty_workspace_is_empty() {
        let ws = Workspace::new();
        let results = workspace_search_children(&ws, "", 100);
        assert!(results.is_empty());
    }

    #[test]
    fn workspace_search_children_returns_all_child_kinds_not_just_procedures() {
        // Unlike the file_index procedure reverse-index (which filters to
        // Function/Event kinds), workspace_search_children returns *every* child
        // symbol of every top-level object. A table field is such a child, so an
        // empty query surfaces it. This pins the actual (broad) behavior so a
        // future change that silently narrows it is caught.
        let ws = Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/proj/PlainTable.al"),
            r#"table 50100 "Plain Table" { fields { field(1; "No."; Code[20]) { } } }"#.to_string(),
        );
        let all = workspace_search_children(&ws, "", 100);
        assert!(
            !all.is_empty(),
            "table children (fields) are returned by the child search"
        );
        for r in &all {
            assert_eq!(
                r.container_name, "Plain Table",
                "every child reports the table as its container"
            );
        }
        let none = workspace_search_children(&ws, "ZZZ_no_such_child", 100);
        assert!(
            none.is_empty(),
            "name filter excludes non-matching children"
        );
    }
}
