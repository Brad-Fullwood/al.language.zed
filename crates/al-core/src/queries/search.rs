//! Workspace symbol search query.
//!
//! Transport-agnostic search over workspace .al file objects.  Both the LSP
//! `workspace/symbol` handler and the daemon `search` RPC call this function;
//! each caller converts the result to its own response type.

use std::path::PathBuf;

use tower_lsp::lsp_types::{Range, SymbolKind};

use crate::file_index::CachedObjectInfo;
use crate::workspace::Workspace;

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
    if q.len() > h.len() { return false; }
    h.windows(q.len()).any(|w| w.iter().zip(q).all(|(a, b)| a.to_ascii_lowercase() == *b))
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
    /// LSP symbol kind.
    pub kind: SymbolKind,
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

    for tree_entry in workspace.file_index.file_trees.iter() {
        if results.len() >= limit {
            break;
        }
        let file_path = tree_entry.key().clone();
        let tree = tree_entry.value().clone();
        drop(tree_entry); // release dashmap lock before accessing files

        let text = match workspace.file_index.files.get(&file_path) {
            Some(t) => t.value().clone(),
            None => continue,
        };

        let doc_symbols = al_syntax::extract_document_symbols(&tree, &text);
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
    use crate::workspace::Workspace;

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
}
