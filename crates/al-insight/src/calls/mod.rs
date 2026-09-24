//! Call edge extraction from workspace AL source ASTs.
//!
//! Walks tree-sitter parse trees from workspace files to extract three kinds
//! of directed edges for the insight call graph:
//!
//! | Kind | Grammar pattern | Edge |
//! |------|----------------|------|
//! | Direct call | `Foo()` / `Obj.Method()` | `Calls` |
//! | Record op | `Rec.Insert()` / `Rec.Insert(true)` | `Triggers` |
//! | EventSubscriber attribute | `[EventSubscriber(...)]` | `SubscribesTo` |
//!
//! The top-level entry points are:
//! - [`register_workspace_nodes`] — register nodes in InsightGraph before
//!   wrapping in Arc (must be called before [`populate_workspace_call_edges`]).
//! - [`populate_workspace_call_edges`] — score + resolve call edges across all
//!   workspace files.
//! - [`fanout_score`] — count call-suffix nodes in a tree (used for tier ranking).

use al_syntax::IdentifierText;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use al_symbols::{ObjectKind, SymbolEntry, SymbolIndex};

use super::graph::{EventNodeType, InsightEdge, InsightGraph, InsightNode, NodeKey};
use super::index::{CallGraph, EdgeResolutionState, NodeId};
use al_source::file_index::FileIndex;

/// A broken invariant in a source index used to construct a call graph.
///
/// These are not ordinary unresolved calls: they mean a file that was
/// advertised as an indexed AL object could not participate in graph
/// construction. Returning the error prevents callers from presenting a
/// quietly incomplete graph as authoritative.
#[derive(Debug, thiserror::Error)]
pub enum SourceGraphError {
    #[error("indexed AL object '{}' has unsupported kind '{kind}'", path.display())]
    InvalidObjectKind { path: PathBuf, kind: String },
    #[error("indexed AL object '{}' has no numeric object ID", path.display())]
    MissingObjectId { path: PathBuf },
    #[error("indexed AL object '{}' has ID {id}, outside the supported i32 range", path.display())]
    ObjectIdOutOfRange { path: PathBuf, id: i64 },
    #[error(
        "indexed name-scoped AL object '{}' unexpectedly declares numeric ID {id}",
        path.display()
    )]
    UnexpectedObjectId { path: PathBuf, id: i64 },
    #[error("indexed AL object '{}' has no coherent cached source/tree pair", path.display())]
    MissingCachedParse { path: PathBuf },
    #[error(
        "callable graph node for {kind} '{object}'.'{member}' from '{}' was not registered",
        path.display()
    )]
    MissingCallableNode {
        path: PathBuf,
        kind: ObjectKind,
        object: String,
        member: String,
    },
}

fn indexed_object_kind(
    path: &Path,
    info: &al_source::file_index::CachedObjectInfo,
) -> Result<ObjectKind, SourceGraphError> {
    info.kind
        .parse()
        .map_err(|_| SourceGraphError::InvalidObjectKind {
            path: path.to_path_buf(),
            kind: info.kind.clone(),
        })
}

fn indexed_object_id(
    path: &Path,
    info: &al_source::file_index::CachedObjectInfo,
    kind: ObjectKind,
) -> Result<i32, SourceGraphError> {
    kind.normalize_declaration_id(info.id)
        .map_err(|error| match error {
            al_symbols::DeclarationIdError::Missing { .. } => SourceGraphError::MissingObjectId {
                path: path.to_path_buf(),
            },
            al_symbols::DeclarationIdError::OutOfRange { id, .. } => {
                SourceGraphError::ObjectIdOutOfRange {
                    path: path.to_path_buf(),
                    id,
                }
            }
            al_symbols::DeclarationIdError::Unexpected { id, .. } => {
                SourceGraphError::UnexpectedObjectId {
                    path: path.to_path_buf(),
                    id,
                }
            }
        })
}

fn indexed_parse(
    file_index: &FileIndex,
    path: &Path,
) -> Result<(String, tree_sitter::Tree), SourceGraphError> {
    file_index
        .get_cached_parse(path)
        .ok_or_else(|| SourceGraphError::MissingCachedParse {
            path: path.to_path_buf(),
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOp {
    Insert,
    Modify,
    Delete,
    Validate,
}

impl RecordOp {
    /// Parse from a method name (case-insensitive).
    ///
    /// The four operations are the stable AL record-runtime tokens since
    /// NAV 2.0 — they're part of the BC record ABI (each fires OnBefore/OnAfter
    /// table events), not AL *language* keywords or built-in functions. This
    /// set is fixed by Microsoft and has not changed in
    /// 20+ years. Locked in here rather than fetched from `LanguageData` so
    /// the call-graph builder has no runtime dependency on language data load
    /// order.
    pub fn from_method_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "insert" => Some(RecordOp::Insert),
            "modify" => Some(RecordOp::Modify),
            "delete" => Some(RecordOp::Delete),
            "validate" => Some(RecordOp::Validate),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum CallSite {
    BareCall {
        name: String,
    },
    MemberCall {
        object: String,
        method: String,
    },
    RecordOp {
        variable: String,
        op: RecordOp,
        run_trigger: bool,
    },
    /// `Codeunit.Run(Codeunit::"X")` / `Codeunit.RunModal(Codeunit::X)` with a
    /// literal codeunit reference as the first argument. The dispatch
    /// target is `X`'s `OnRun` trigger. Only the *literal* form is captured;
    /// `Codeunit.Run(SomeVariable)` is left unresolved (no sound static target).
    CodeunitRun {
        target: String,
    },
}

mod call_sites;
mod edges;
mod nodes;
mod object_symbols;
#[cfg(test)]
mod tests;
mod var_types;

pub use call_sites::*;
pub use edges::*;
pub use nodes::*;
use object_symbols::*;
pub use var_types::*;
