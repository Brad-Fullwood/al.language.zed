//! shared write/rename helpers that keep the in-memory workspace state
//! (document store, file index, insight graph) in sync with disk writes made
//! by other daemon dispatchers (sort/organize, fixes, etc.).

use std::io::Write;
use std::path::{Path, PathBuf};

use al_workspace::Workspace;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FileRefreshError {
    #[error("AL source path cannot be represented as a file URI: {}", .0.display())]
    InvalidFilePath(PathBuf),
    #[error(transparent)]
    Source(#[from] al_source::file_index::ScanError),
    #[error(transparent)]
    Document(#[from] al_source::documents::DocumentMutationError),
    #[error("cannot rename to '{}': the destination already exists", .0.display())]
    DestinationExists(PathBuf),
    #[error("failed to {operation} '{}': {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "file operation failed after disk mutation and rollback also failed (operation: {operation_error}; rollback: {rollback_error})"
    )]
    RollbackFailed {
        operation_error: String,
        rollback_error: String,
    },
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> FileRefreshError {
    FileRefreshError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

struct ExistingSource {
    content: String,
    permissions: std::fs::Permissions,
}

fn read_existing_source(path: &Path) -> Result<Option<ExistingSource>, FileRefreshError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("inspect", path, source)),
    };
    if !metadata.file_type().is_file() {
        return Err(al_source::file_index::ScanError::NotRegularFile {
            path: path.to_path_buf(),
        }
        .into());
    }
    let content = al_source::file_index::read_source_file(path)?.ok_or_else(|| {
        io_error(
            "read",
            path,
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "source disappeared after metadata inspection",
            ),
        )
    })?;
    Ok(Some(ExistingSource {
        content,
        permissions: metadata.permissions(),
    }))
}

/// Atomically replace `path` with `content` using a same-directory temporary
/// file. Existing permissions are retained when replacing a source.
fn atomic_write_source(
    path: &Path,
    content: &str,
    permissions: Option<&std::fs::Permissions>,
) -> Result<(), FileRefreshError> {
    let parent = path.parent().ok_or_else(|| {
        io_error(
            "derive parent for",
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
        )
    })?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| io_error("create temporary file beside", path, source))?;
    if let Some(permissions) = permissions {
        temporary
            .as_file()
            .set_permissions(permissions.clone())
            .map_err(|source| io_error("set temporary-file permissions for", path, source))?;
    }
    temporary
        .write_all(content.as_bytes())
        .map_err(|source| io_error("write temporary file for", path, source))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| io_error("sync temporary file for", path, source))?;
    temporary
        .persist(path)
        .map_err(|error| io_error("atomically replace", path, error.error))?;
    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error("sync parent directory for", path, source))?;
    Ok(())
}

fn remove_created_source(path: &Path) -> Result<(), FileRefreshError> {
    std::fs::remove_file(path)
        .map_err(|source| io_error("roll back newly created", path, source))?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error("sync rollback directory for", path, source))?;
    }
    Ok(())
}

/// Write `.al` content to disk and refresh the workspace's
/// in-memory state so subsequent daemon queries observe the change
/// without requiring a restart. Updates the document store, the file
/// index, and invalidates the lazy insight graph. Centralised so every
/// daemon write dispatcher can use one consistent refresh sequence.
pub(crate) fn write_al_file_and_refresh(
    workspace: &Workspace,
    path: &Path,
    content: String,
) -> Result<(), FileRefreshError> {
    if content.len() as u64 > al_source::file_index::MAX_AL_FILE_BYTES {
        return Err(al_source::file_index::ScanError::FileTooLarge {
            path: path.to_path_buf(),
            size: content.len() as u64,
            limit: al_source::file_index::MAX_AL_FILE_BYTES,
        }
        .into());
    }
    let uri = url::Url::from_file_path(path)
        .map_err(|()| FileRefreshError::InvalidFilePath(path.to_path_buf()))?;
    workspace.documents.validate_document_text(&uri, &content)?;
    let previous = read_existing_source(path)?;
    atomic_write_source(
        path,
        &content,
        previous.as_ref().map(|source| &source.permissions),
    )?;

    if let Err(document_error) = workspace.documents.replace_or_open(uri, content.clone()) {
        let rollback = match &previous {
            Some(previous) => {
                atomic_write_source(path, &previous.content, Some(&previous.permissions))
            }
            None => remove_created_source(path),
        };
        if let Err(rollback_error) = rollback {
            return Err(FileRefreshError::RollbackFailed {
                operation_error: document_error.to_string(),
                rollback_error: rollback_error.to_string(),
            });
        }
        return Err(document_error.into());
    }

    workspace.file_index.add_file(path.to_path_buf(), content);
    workspace.invalidate_insight_graph();
    workspace.mark_generation_changed();
    Ok(())
}

/// Whether `a` and `b` resolve to the same file on disk (e.g. a case-only
/// rename on a case-insensitive filesystem). Both are expected to exist.
fn is_same_file(a: &Path, b: &Path) -> Result<bool, FileRefreshError> {
    let canonical_a =
        std::fs::canonicalize(a).map_err(|source| io_error("canonicalize", a, source))?;
    let canonical_b =
        std::fs::canonicalize(b).map_err(|source| io_error("canonicalize", b, source))?;
    Ok(canonical_a == canonical_b)
}

/// Claim `new` without an exists-then-rename race. On Windows, the native
/// rename operation already fails atomically when the destination exists. On
/// Unix, create the destination directory entry with `link(2)` (which is an
/// atomic create-if-absent operation), then unlink the old name. AL sources are
/// regular files on one filesystem, so this provides no-overwrite semantics
/// without a platform-specific syscall dependency.
fn rename_no_replace(old: &Path, new: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::fs::rename(old, new)
    }

    #[cfg(not(windows))]
    {
        std::fs::hard_link(old, new)?;
        if let Err(error) = std::fs::remove_file(old) {
            // Best-effort rollback: preserve the original name if unlinking it
            // fails after the destination was claimed.
            let _ = std::fs::remove_file(new);
            return Err(error);
        }
        Ok(())
    }
}

pub(crate) fn rename_al_file_and_refresh(
    workspace: &Workspace,
    old: &Path,
    new: &Path,
) -> Result<(), FileRefreshError> {
    let old_uri = url::Url::from_file_path(old)
        .map_err(|()| FileRefreshError::InvalidFilePath(old.to_path_buf()))?;
    let new_uri = url::Url::from_file_path(new)
        .map_err(|()| FileRefreshError::InvalidFilePath(new.to_path_buf()))?;
    let old_document_open = workspace.documents.contains(&old_uri);
    if old_uri != new_uri && workspace.documents.contains(&new_uri) {
        return Err(
            al_source::documents::DocumentMutationError::RenameDestinationOpen {
                source_uri: Box::new(old_uri),
                destination_uri: Box::new(new_uri),
            }
            .into(),
        );
    }
    let source = read_existing_source(old)?.ok_or_else(|| {
        io_error(
            "rename",
            old,
            std::io::Error::new(std::io::ErrorKind::NotFound, "source does not exist"),
        )
    })?;

    // A case-only rename to the same file is allowed. Every other rename uses
    // an atomic create-if-absent primitive, so a destination created after this
    // advisory check still cannot be overwritten.
    let destination_exists = match std::fs::symlink_metadata(new) {
        Ok(_) => true,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => return Err(io_error("inspect rename destination", new, source)),
    };
    let same_file = new != old && destination_exists && is_same_file(old, new)?;
    if new != old && destination_exists && !same_file {
        return Err(FileRefreshError::DestinationExists(new.to_path_buf()));
    }
    if old != new {
        if same_file {
            std::fs::rename(old, new).map_err(|source| io_error("rename", old, source))?;
        } else {
            rename_no_replace(old, new).map_err(|source| io_error("rename", old, source))?;
        }
    }

    if old_document_open && old_uri != new_uri {
        if let Err(document_error) = workspace.documents.rename(&old_uri, new_uri) {
            let rollback = std::fs::rename(new, old)
                .map_err(|source| io_error("roll back rename", new, source));
            if let Err(rollback_error) = rollback {
                return Err(FileRefreshError::RollbackFailed {
                    operation_error: document_error.to_string(),
                    rollback_error: rollback_error.to_string(),
                });
            }
            return Err(document_error.into());
        }
    }
    workspace.file_index.remove_file(old);
    workspace
        .file_index
        .add_file(new.to_path_buf(), source.content);
    workspace.invalidate_insight_graph();
    workspace.mark_generation_changed();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    /// positive: write_al_file_and_refresh writes to disk AND
    /// updates documents + file_index + invalidates insight graph.
    #[test]
    fn write_helper_refreshes_documents_and_file_index() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("Foo.al");
        let content = r#"codeunit 50100 "Foo" { }"#.to_string();
        write_al_file_and_refresh(&ws, &path, content.clone()).expect("helper succeeds");
        let on_disk = std::fs::read_to_string(&path).expect("file written");
        assert_eq!(on_disk, content);
        let uri = url::Url::from_file_path(&path).unwrap();
        assert_eq!(
            ws.documents.get_text(&uri).as_deref(),
            Some(content.as_str())
        );
        assert_eq!(
            ws.file_index.get_content(&path).as_deref(),
            Some(content.as_str())
        );
    }

    /// positive: rename_al_file_and_refresh moves the file on disk
    /// AND drops the old file_index entry while adding the new one.
    #[test]
    fn rename_helper_refreshes_file_index_for_old_and_new_paths() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let old = tmp.path().join("Old.al");
        let new = tmp.path().join("New.al");
        let content = r#"codeunit 50101 "Renamed" { }"#.to_string();
        std::fs::write(&old, &content).unwrap();
        ws.file_index.add_file(old.clone(), content.clone());
        assert!(ws.file_index.get_content(&old).is_some());

        rename_al_file_and_refresh(&ws, &old, &new).expect("helper succeeds");

        assert!(!old.exists(), "old file removed from disk");
        assert!(new.exists(), "new file present on disk");
        assert!(
            ws.file_index.get_content(&old).is_none(),
            "old file_index entry must be dropped"
        );
        assert_eq!(
            ws.file_index.get_content(&new).as_deref(),
            Some(content.as_str()),
            "new path must be re-indexed"
        );
    }

    #[test]
    fn rename_transfers_open_document_without_resetting_versions() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let old = tmp.path().join("OpenOld.al");
        let new = tmp.path().join("OpenNew.al");
        let content = r#"codeunit 50101 "Renamed" { }"#.to_string();
        std::fs::write(&old, &content).unwrap();
        ws.file_index.add_file(old.clone(), content.clone());

        let old_uri = url::Url::from_file_path(&old).unwrap();
        let new_uri = url::Url::from_file_path(&new).unwrap();
        ws.documents
            .open_with_client_version(old_uri.clone(), content.clone(), 17)
            .unwrap();
        ws.documents
            .apply_changes_and_get(
                &old_uri,
                &[al_source::documents::TextChange {
                    range: None,
                    text: content.clone(),
                }],
            )
            .unwrap();
        rename_al_file_and_refresh(&ws, &old, &new).unwrap();

        assert!(!ws.documents.contains(&old_uri));
        assert_eq!(
            ws.documents.get_text(&new_uri).as_deref(),
            Some(content.as_str())
        );
        assert_eq!(ws.documents.get_version(&new_uri), Some(1));
        assert_eq!(ws.documents.get_client_version(&new_uri), Some(17));
    }

    /// `write_al_file_and_refresh` propagates I/O errors
    /// instead of silently succeeding. A path under a non-existent
    /// directory must surface the underlying io::Error.
    #[test]
    fn write_helper_returns_io_error_for_unwritable_path() {
        let ws = empty_ws();
        let bogus = std::path::PathBuf::from("/nonexistent/parent/dir/Foo.al");
        let err = write_al_file_and_refresh(&ws, &bogus, "x".to_string()).expect_err("must error");
        assert!(matches!(
            err,
            FileRefreshError::Io {
                source,
                ..
            } if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            )
        ));
    }

    #[test]
    fn rename_refuses_to_overwrite_existing_file() {
        // Two source files exist; renaming one onto the other must NOT destroy
        // the destination. Both files must survive and an error is returned.
        let ws = empty_ws();
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("A.al");
        let new = dir.path().join("B.al");
        std::fs::write(&old, "codeunit 50100 A { }").unwrap();
        std::fs::write(&new, "codeunit 50101 B { }").unwrap();

        let err = rename_al_file_and_refresh(&ws, &old, &new)
            .expect_err("rename onto an existing file must fail");
        assert!(matches!(err, FileRefreshError::DestinationExists(path) if path == new));

        // Both files survive with their original contents.
        assert_eq!(
            std::fs::read_to_string(&old).unwrap(),
            "codeunit 50100 A { }"
        );
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "codeunit 50101 B { }"
        );
    }

    #[test]
    fn write_to_open_doc_keeps_version_monotonic() {
        // When the editor has the file open, a daemon-side write must not reset
        // the document version to 0 (which stomps the LSP-synced state); it must
        // advance monotonically via a full-replace.
        let ws = empty_ws();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Open.al");
        let uri = url::Url::from_file_path(&path).unwrap();

        ws.documents
            .open(uri.clone(), "codeunit 50100 A { }".to_string())
            .unwrap();
        ws.documents
            .apply_changes_and_get(
                &uri,
                &[al_source::documents::TextChange {
                    range: None,
                    text: "codeunit 50100 A { v1 }".to_string(),
                }],
            )
            .unwrap();
        let v_before = ws.documents.get_version(&uri).unwrap();

        write_al_file_and_refresh(&ws, &path, "codeunit 50100 A { v2 }".to_string()).unwrap();

        let v_after = ws.documents.get_version(&uri).unwrap();
        assert!(
            v_after > v_before,
            "version must stay monotonic, not reset (got {v_before} -> {v_after})"
        );
        assert!(ws.documents.get_text(&uri).unwrap().contains("v2"));
    }

    #[test]
    fn rename_to_free_destination_succeeds() {
        let ws = empty_ws();
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("A.al");
        let new = dir.path().join("Renamed.al");
        std::fs::write(&old, "codeunit 50100 A { }").unwrap();

        rename_al_file_and_refresh(&ws, &old, &new).expect("rename to a free path must succeed");
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(&new).unwrap(),
            "codeunit 50100 A { }"
        );
    }

    #[test]
    fn write_rejected_by_document_cap_leaves_disk_and_indexes_unchanged() {
        let ws = empty_ws();
        ws.documents.set_max_doc_bytes(Some(8));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Bounded.al");
        let old = "short";
        std::fs::write(&path, old).unwrap();
        ws.file_index.add_file(path.clone(), old.to_string());

        let error = write_al_file_and_refresh(&ws, &path, "this is too long".to_string())
            .expect_err("content above the document cap must be rejected");

        assert!(matches!(
            error,
            FileRefreshError::Document(
                al_source::documents::DocumentMutationError::TooLarge { .. }
            )
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
        assert_eq!(ws.file_index.get_content(&path).as_deref(), Some(old));
        let uri = url::Url::from_file_path(&path).unwrap();
        assert!(!ws.documents.contains(&uri));
    }

    #[cfg(unix)]
    #[test]
    fn write_refuses_to_replace_a_symlinked_source() {
        use std::os::unix::fs::symlink;

        let ws = empty_ws();
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Target.al");
        let link = dir.path().join("Link.al");
        std::fs::write(&target, "original").unwrap();
        symlink(&target, &link).unwrap();

        let error = write_al_file_and_refresh(&ws, &link, "replacement".to_string())
            .expect_err("symlinked sources must not be replaced");

        assert!(matches!(
            error,
            FileRefreshError::Source(al_source::file_index::ScanError::NotRegularFile { .. })
        ));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
