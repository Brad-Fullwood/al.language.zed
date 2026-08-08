//! Coherent workspace source snapshots for whole-project queries.
//!
//! Whole-workspace reports must never silently *lose* a file because an
//! index/cache invariant was broken: a partial report is indistinguishable from
//! a complete one at the wire boundary. Incoherence — an indexed path missing
//! from the parse cache, or the workspace changing mid-collection — therefore
//! still fails the whole query.
//!
//! A file that simply has no AL object in it is a different matter. Real
//! projects contain scratch files, comment-only stubs, and work-in-progress
//! sources with a syntax error; failing the entire query for one of those made
//! dead-code, sql-scan, impact, duplicates and both audits permanently
//! unavailable. Those files are skipped individually with a warning instead.

use std::path::PathBuf;

use al_symbols::ObjectKind;
use al_workspace::Workspace;

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceObjectDeclaration {
    pub info: al_syntax::ObjectInfo,
    pub kind: ObjectKind,
    pub normalized_id: i32,
}

#[derive(Debug, Clone)]
pub(crate) struct WorkspaceSource {
    pub path: PathBuf,
    pub text: String,
    pub tree: tree_sitter::Tree,
    pub object: WorkspaceObjectDeclaration,
}

#[derive(Debug, Clone)]
pub(crate) struct CoherentWorkspaceSource {
    pub path: PathBuf,
    pub text: String,
    pub tree: tree_sitter::Tree,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WorkspaceSourceError {
    #[error("workspace source '{}' has no matching cached parse tree", path.display())]
    MissingCachedParse { path: PathBuf },
    #[error("workspace source '{}' contains AL syntax errors: {details}", path.display())]
    ParseSource { path: PathBuf, details: String },
    #[error("workspace source '{}' has no AL object declaration", path.display())]
    MissingObjectDeclaration { path: PathBuf },
    #[error("workspace source '{}' declares an unsupported object kind: {reason}", path.display())]
    InvalidObjectKind { path: PathBuf, reason: String },
    #[error("workspace source '{}' has an invalid object ID: {reason}", path.display())]
    InvalidObjectId { path: PathBuf, reason: String },
    #[error(
        "workspace sources changed while a complete project snapshot was being collected; retry the query"
    )]
    GenerationChanged,
}

impl WorkspaceSourceError {
    pub(crate) fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::MissingCachedParse { path }
            | Self::ParseSource { path, .. }
            | Self::MissingObjectDeclaration { path }
            | Self::InvalidObjectKind { path, .. }
            | Self::InvalidObjectId { path, .. } => Some(path),
            Self::GenerationChanged => None,
        }
    }
}

/// A workspace file that carries no usable AL object declaration and was
/// therefore left out of the snapshot: `(path, reason)`.
pub(crate) type SkippedSource = (PathBuf, String);

/// Every workspace file that declares a usable AL object.
///
/// Files without a usable declaration are skipped (see
/// [`snapshot_with_skipped`]); only workspace *incoherence* fails the query.
pub(crate) fn snapshot(
    workspace: &Workspace,
) -> Result<Vec<WorkspaceSource>, WorkspaceSourceError> {
    Ok(snapshot_with_skipped(workspace)?.0)
}

/// Like [`snapshot`], but also returns the per-file skips so a caller can
/// surface them.
pub(crate) fn snapshot_with_skipped(
    workspace: &Workspace,
) -> Result<(Vec<WorkspaceSource>, Vec<SkippedSource>), WorkspaceSourceError> {
    let coherent = coherent_snapshot(workspace)?;
    let mut sources = Vec::with_capacity(coherent.len());
    let mut skipped = Vec::new();
    for source in coherent {
        match validate_object_source(source) {
            Ok(valid) => sources.push(valid),
            Err(error) => {
                // Every error `validate_object_source` can produce names a
                // single file; there is no whole-workspace failure to escalate.
                let Some(path) = error.path().map(std::path::Path::to_path_buf) else {
                    return Err(error);
                };
                tracing::warn!(
                    path = %path.display(),
                    reason = %error,
                    "workspace snapshot: skipping file without a usable AL object declaration"
                );
                skipped.push((path, error.to_string()));
            }
        }
    }
    Ok((sources, skipped))
}

/// Capture every indexed source as a coherent text/tree pair.
///
/// Unlike [`snapshot`], this deliberately permits syntax errors and missing
/// object declarations so diagnostics can report those conditions. It still
/// fails when an indexed path has disappeared from the parse cache or the
/// workspace generation changes during collection; returning a partial set in
/// either case would make a whole-workspace diagnostic pass look complete.
pub(crate) fn coherent_snapshot(
    workspace: &Workspace,
) -> Result<Vec<CoherentWorkspaceSource>, WorkspaceSourceError> {
    let revision = workspace.generation_revision();
    let mut paths = workspace
        .file_index
        .files
        .iter()
        .map(|entry| entry.key().clone())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();

    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let (text, tree) = workspace
            .file_index
            .get_cached_parse(&path)
            .ok_or_else(|| WorkspaceSourceError::MissingCachedParse { path: path.clone() })?;
        sources.push(CoherentWorkspaceSource { path, text, tree });
    }

    if workspace.generation_revision() != revision {
        return Err(WorkspaceSourceError::GenerationChanged);
    }
    Ok(sources)
}

fn validate_object_source(
    source: CoherentWorkspaceSource,
) -> Result<WorkspaceSource, WorkspaceSourceError> {
    let CoherentWorkspaceSource { path, text, tree } = source;
    if tree.root_node().has_error() {
        let parsed = al_syntax::AlParser::parse_quick(&text);
        let details = parsed
            .errors
            .iter()
            .take(3)
            .map(|error| {
                format!(
                    "{} at {}:{}",
                    error.message,
                    error.range.start_point.row + 1,
                    error.range.start_point.column + 1
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(WorkspaceSourceError::ParseSource {
            path,
            details: if details.is_empty() {
                "tree-sitter reported an error node".to_string()
            } else {
                details
            },
        });
    }

    let info = al_syntax::find_object_declaration(&tree, &text)
        .ok_or_else(|| WorkspaceSourceError::MissingObjectDeclaration { path: path.clone() })?;
    let kind = info.kind.parse::<ObjectKind>().map_err(|reason| {
        WorkspaceSourceError::InvalidObjectKind {
            path: path.clone(),
            reason,
        }
    })?;
    let normalized_id = kind.normalize_declaration_id(info.id).map_err(|error| {
        WorkspaceSourceError::InvalidObjectId {
            path: path.clone(),
            reason: error.to_string(),
        }
    })?;
    Ok(WorkspaceSource {
        path,
        text,
        tree,
        object: WorkspaceObjectDeclaration {
            info,
            kind,
            normalized_id,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One broken scratch file must not take the whole workspace query down.
    #[test]
    fn snapshot_skips_unparsable_files_and_keeps_the_rest() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/project/Broken.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/project/Good.al"),
            "codeunit 50101 Good { procedure Run() begin end; }".to_string(),
        );

        let (sources, skipped) = snapshot_with_skipped(&workspace).expect("snapshot must succeed");
        assert_eq!(sources.len(), 1, "the parsable file must still be returned");
        assert_eq!(sources[0].object.info.name, "Good");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, PathBuf::from("/project/Broken.al"));
        assert!(skipped[0].1.contains("syntax errors"), "{skipped:?}");
    }

    /// A comment-only / declaration-free file is skipped, not fatal.
    #[test]
    fn snapshot_skips_files_without_an_object_declaration() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/project/Notes.al"),
            "// scratch notes, no object here\n".to_string(),
        );
        workspace.file_index.add_file(
            PathBuf::from("/project/Good.al"),
            "codeunit 50101 Good { procedure Run() begin end; }".to_string(),
        );

        let (sources, skipped) = snapshot_with_skipped(&workspace).expect("snapshot must succeed");
        assert_eq!(sources.len(), 1);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0, PathBuf::from("/project/Notes.al"));
    }

    /// Incoherence (index/cache mismatch) is still fatal — a partial report
    /// there would be indistinguishable from a complete one.
    #[test]
    fn snapshot_still_fails_on_a_missing_cached_parse() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/project/Good.al"),
            "codeunit 50101 Good { procedure Run() begin end; }".to_string(),
        );
        workspace.file_index.files.insert(
            PathBuf::from("/project/Ghost.al"),
            "codeunit 50102 Ghost { }".to_string(),
        );

        assert!(matches!(
            snapshot(&workspace),
            Err(WorkspaceSourceError::MissingCachedParse { .. })
        ));
    }

    #[test]
    fn coherent_snapshot_retains_syntax_errors_for_diagnostics() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/project/Broken.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );

        let sources = coherent_snapshot(&workspace).unwrap();
        assert_eq!(sources.len(), 1);
        assert!(sources[0].tree.root_node().has_error());
    }

    #[test]
    fn snapshot_normalizes_name_scoped_objects() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/project/Contract.al"),
            r#"interface "Contract" { procedure Run(); }"#.to_string(),
        );

        let sources = snapshot(&workspace).unwrap();
        let object = &sources[0].object;
        assert_eq!(object.kind, ObjectKind::Interface);
        assert_eq!(object.normalized_id, 0);
    }

    #[test]
    fn snapshot_skips_objects_with_a_missing_numbered_id() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/project/MissingId.al"),
            r#"codeunit "Missing Id" { procedure Run() begin end; }"#.to_string(),
        );

        let (sources, skipped) = snapshot_with_skipped(&workspace).expect("snapshot must succeed");
        assert!(sources.is_empty());
        assert_eq!(skipped.len(), 1);
    }
}
