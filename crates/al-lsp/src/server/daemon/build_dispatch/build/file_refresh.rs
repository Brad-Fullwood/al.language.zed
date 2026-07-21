//! shared write/rename helpers that keep the in-memory workspace state
//! (document store, file index, insight graph) in sync with disk writes made
//! by other daemon dispatchers (sort/organize, fixes, etc.).

use al_workspace::Workspace;

/// Write `.al` content to disk and refresh the workspace's
/// in-memory state so subsequent daemon queries observe the change
/// without requiring a restart. Updates the document store, the file
/// index, and invalidates the lazy insight graph. Centralised so every
/// daemon write dispatcher can use one consistent refresh sequence.
pub(crate) fn write_al_file_and_refresh(
    workspace: &Workspace,
    path: &std::path::Path,
    content: String,
) -> std::io::Result<()> {
    std::fs::write(path, &content)?;
    if let Ok(uri) = url::Url::from_file_path(path) {
        // If the editor already has this file open, don't call `open()`:
        // it resets the internal version to 0, stomping the LSP-synced state and
        // breaking the version/tree-cache pairing until the next did_change.
        // Route the write through a full-replace so versioning stays monotonic.
        if workspace.documents.contains(&uri) {
            let change = al_source::documents::TextChange {
                range: None,
                text: content.clone(),
            };
            workspace.documents.apply_changes_and_get(&uri, &[change]);
        } else {
            workspace.documents.open(uri, content.clone());
        }
    }
    workspace.file_index.add_file(path.to_path_buf(), content);
    workspace.invalidate_insight_graph();
    Ok(())
}

/// Whether `a` and `b` resolve to the same file on disk (e.g. a case-only
/// rename on a case-insensitive filesystem). Both are expected to exist.
fn is_same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// Claim `new` without an exists-then-rename race. On Windows, the native
/// rename operation already fails atomically when the destination exists. On
/// Unix, create the destination directory entry with `link(2)` (which is an
/// atomic create-if-absent operation), then unlink the old name. AL sources are
/// regular files on one filesystem, so this provides no-overwrite semantics
/// without a platform-specific syscall dependency.
fn rename_no_replace(old: &std::path::Path, new: &std::path::Path) -> std::io::Result<()> {
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
    old: &std::path::Path,
    new: &std::path::Path,
) -> std::io::Result<()> {
    let old_uri = url::Url::from_file_path(old).ok();
    let new_uri = url::Url::from_file_path(new).ok();
    if let (Some(old_uri), Some(new_uri)) = (&old_uri, &new_uri) {
        if workspace.documents.contains(old_uri) && workspace.documents.contains(new_uri) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("rename target {} is already open", new.display()),
            ));
        }
    }

    // A case-only rename to the same file is allowed. Every other rename uses
    // an atomic create-if-absent primitive, so a destination created after this
    // advisory check still cannot be overwritten.
    let same_file = new != old && new.exists() && is_same_file(old, new);
    if new != old && new.exists() && !same_file {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "refusing to overwrite existing file {} (rename target already exists)",
                new.display()
            ),
        ));
    }
    if old != new {
        if same_file {
            std::fs::rename(old, new)?;
        } else {
            rename_no_replace(old, new)?;
        }
    }

    if let (Some(old_uri), Some(new_uri)) = (old_uri, new_uri) {
        workspace.documents.rename(&old_uri, new_uri);
    }
    workspace.file_index.remove_file(old);
    if let Ok(content) = std::fs::read_to_string(new) {
        workspace.file_index.add_file(new.to_path_buf(), content);
    }
    workspace.invalidate_insight_graph();
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
        ws.documents.open(old_uri.clone(), content.clone());
        ws.documents.apply_changes_and_get(
            &old_uri,
            &[al_source::documents::TextChange {
                range: None,
                text: content.clone(),
            }],
        );
        ws.documents.set_client_version(&old_uri, 17);

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
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
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
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);

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
            .open(uri.clone(), "codeunit 50100 A { }".to_string());
        ws.documents.apply_changes_and_get(
            &uri,
            &[al_source::documents::TextChange {
                range: None,
                text: "codeunit 50100 A { v1 }".to_string(),
            }],
        );
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
}
