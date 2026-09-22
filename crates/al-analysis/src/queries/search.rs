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

/// How well a name matched the query. Lower sorts first.
///
/// An editor's symbol picker shows the first `limit` results, so the rank has
/// to decide *which* matches survive truncation, not just their order. Without
/// it the exact object whose full name the user typed could be cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchRank {
    Exact,
    Prefix,
    Substring,
}

/// The rank of `name` against an already-folded query, or `None` for no match.
///
/// An empty query matches everything at [`MatchRank::Substring`].
///
/// Both sides are folded with `str::to_lowercase`. A per-byte ASCII fold left
/// `Ü` (`0xC3 0x9C`) alone while lowercasing the query to `ü` (`0xC3 0xBC`), so
/// searching `MÜNCHEN` found nothing in Nordic and German projects while
/// `München` did.
fn match_rank(name: &str, query_lower: &str) -> Option<MatchRank> {
    if query_lower.is_empty() {
        return Some(MatchRank::Substring);
    }
    let folded = name.to_lowercase();
    if folded == query_lower {
        Some(MatchRank::Exact)
    } else if folded.starts_with(query_lower) {
        Some(MatchRank::Prefix)
    } else if folded.contains(query_lower) {
        Some(MatchRank::Substring)
    } else {
        None
    }
}

/// Search workspace .al file objects whose name contains `query` (case-insensitive).
///
/// - When `query` is empty, all objects are returned up to `limit`.
/// - When `query` is non-empty, only objects whose name contains the query are returned.
/// - Every match is collected and ranked (exact, then prefix, then substring,
///   then by name and path) before the list is cut to `limit`, so the same
///   query over an unchanged workspace always returns the same results in the
///   same order. Truncating a DashMap iteration instead returned an arbitrary
///   subset in shard order.
pub fn workspace_search(
    workspace: &Workspace,
    query: &str,
    limit: usize,
) -> Vec<WorkspaceSearchResult> {
    let query_lower = query.to_lowercase();
    let mut ranked: Vec<(MatchRank, WorkspaceSearchResult)> = Vec::new();

    // `object_infos`, not `object_info`: the singular map holds the first
    // declaration of each file, so the second and later objects of a
    // multi-object file were not findable by `workspace/symbol` at all.
    for entry in workspace.file_index.object_infos.iter() {
        let file_path = entry.key();
        for info in entry.value() {
            let Some(rank) = match_rank(&info.name, &query_lower) else {
                continue;
            };
            ranked.push((
                rank,
                WorkspaceSearchResult {
                    file_path: file_path.clone(),
                    info: info.clone(),
                },
            ));
        }
    }

    ranked.sort_by(|(left_rank, left), (right_rank, right)| {
        left_rank.cmp(right_rank).then_with(|| {
            (
                left.info.name.to_lowercase(),
                &left.info.name,
                &left.file_path,
                left.info.range.start_byte,
            )
                .cmp(&(
                    right.info.name.to_lowercase(),
                    &right.info.name,
                    &right.file_path,
                    right.info.range.start_byte,
                ))
        })
    });
    ranked.truncate(limit);
    ranked.into_iter().map(|(_, result)| result).collect()
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
/// - Ranked and truncated the same way as [`workspace_search`].
pub fn workspace_search_children(
    workspace: &Workspace,
    query: &str,
    limit: usize,
) -> Vec<WorkspaceChildSearchResult> {
    let query_lower = query.to_lowercase();
    let mut ranked: Vec<(MatchRank, WorkspaceChildSearchResult)> = Vec::new();

    let paths: Vec<PathBuf> = workspace
        .file_index
        .files
        .iter()
        .map(|entry| entry.key().clone())
        .collect();
    for file_path in paths {
        let doc_symbols: Vec<super::AlDocumentSymbol> =
            match workspace.file_index.get_cached_symbols(&file_path) {
                Some(s) => s.into_iter().map(Into::into).collect(),
                None => continue,
            };
        for sym in &doc_symbols {
            let container_name = sym.name.clone();
            let Some(children) = &sym.children else {
                continue;
            };
            for child in children {
                let Some(rank) = match_rank(&child.name, &query_lower) else {
                    continue;
                };
                ranked.push((
                    rank,
                    WorkspaceChildSearchResult {
                        file_path: file_path.clone(),
                        name: child.name.clone(),
                        kind: child.kind,
                        range: child.range,
                        container_name: container_name.clone(),
                    },
                ));
            }
        }
    }

    ranked.sort_by(|(left_rank, left), (right_rank, right)| {
        left_rank.cmp(right_rank).then_with(|| {
            (
                left.name.to_lowercase(),
                &left.name,
                &left.container_name,
                &left.file_path,
                left.range.start.line,
            )
                .cmp(&(
                    right.name.to_lowercase(),
                    &right.name,
                    &right.container_name,
                    &right.file_path,
                    right.range.start.line,
                ))
        })
    });
    ranked.truncate(limit);
    ranked.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_workspace::Workspace;
    use std::path::PathBuf;

    #[test]
    fn match_rank_orders_exact_then_prefix_then_substring() {
        assert_eq!(match_rank("Customer", "customer"), Some(MatchRank::Exact));
        assert_eq!(
            match_rank("Customer Ledger", "customer"),
            Some(MatchRank::Prefix)
        );
        assert_eq!(
            match_rank("Posted Customer Entry", "customer"),
            Some(MatchRank::Substring)
        );
        assert_eq!(match_rank("Vendor", "customer"), None);
        assert_eq!(match_rank("Anything", ""), Some(MatchRank::Substring));
    }

    /// The query was folded with Unicode-aware `to_lowercase` and the haystack
    /// with a per-byte ASCII fold, so `MÜNCHEN` found nothing while `München`
    /// did. Nordic and German BC projects hit this on every accented name.
    #[test]
    fn match_rank_folds_non_ascii_names() {
        assert_eq!(
            match_rank("München Setup", &"MÜNCHEN".to_lowercase()),
            Some(MatchRank::Prefix)
        );
        assert_eq!(
            match_rank("Ørnamental Entry", &"ØRNAMENTAL".to_lowercase()),
            Some(MatchRank::Prefix)
        );
        assert_eq!(
            match_rank("Æble Setup", &"ÆBLE".to_lowercase()),
            Some(MatchRank::Prefix)
        );
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

    /// Truncating a DashMap iteration returned an arbitrary subset in shard
    /// order, so the object whose full name the user typed could be cut and
    /// the list reordered between identical requests.
    #[test]
    fn workspace_search_ranks_before_it_truncates() {
        let ws = Workspace::new();
        for index in 0..40 {
            ws.file_index.add_file(
                PathBuf::from(format!("/proj/Sales{index}.al")),
                format!(r#"page {} "Posted Sales {index}" {{ }}"#, 50200 + index),
            );
        }
        ws.file_index.add_file(
            PathBuf::from("/proj/Sales.al"),
            r#"table 50100 "Sales" { fields { field(1; "No."; Code[20]) { } } }"#.to_string(),
        );
        ws.file_index.add_file(
            PathBuf::from("/proj/SalesHeader.al"),
            r#"table 50101 "Sales Header" { fields { field(1; "No."; Code[20]) { } } }"#
                .to_string(),
        );

        let first = workspace_search(&ws, "Sales", 3);
        assert_eq!(
            first
                .iter()
                .map(|result| result.info.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Sales", "Sales Header", "Posted Sales 0"],
            "exact, then prefix, then substring"
        );

        let second = workspace_search(&ws, "Sales", 3);
        assert_eq!(
            first
                .iter()
                .map(|result| result.info.name.clone())
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|result| result.info.name.clone())
                .collect::<Vec<_>>(),
            "identical requests must return an identical list"
        );
    }

    /// `object_info` holds the first declaration of each file, so the second
    /// object of a multi-object file was not findable at all.
    #[test]
    fn workspace_search_finds_a_second_object_in_a_file() {
        let ws = Workspace::new();
        ws.file_index.add_file(
            PathBuf::from("/proj/Setup.al"),
            "table 50100 \"Ship Setup\" { fields { field(1; Key1; Code[10]) { } } }\n\
             page 50101 \"Ship Setup Card\" { PageType = Card; }\n"
                .to_string(),
        );

        let results = workspace_search(&ws, "Ship Setup Card", 10);
        assert_eq!(results.len(), 1, "{results:?}");
        assert_eq!(results[0].info.name, "Ship Setup Card");
        assert_eq!(results[0].info.kind, "page");
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
