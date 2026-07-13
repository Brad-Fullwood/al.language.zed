//! F-011: shared write/rename helpers that keep the in-memory workspace state
//! (document store, file index, insight graph) in sync with disk writes made
//! by other daemon dispatchers (sort/organize, fixes, etc.).

use al_workspace::Workspace;

/// F-011: Write `.al` content to disk and refresh the workspace's
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
        workspace.documents.open(uri, content.clone());
    }
    workspace.file_index.add_file(path.to_path_buf(), content);
    workspace.invalidate_insight_graph();
    Ok(())
}
pub(crate) fn rename_al_file_and_refresh(
    workspace: &Workspace,
    old: &std::path::Path,
    new: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::rename(old, new)?;
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

    /// F-011 positive: write_al_file_and_refresh writes to disk AND
    /// updates documents + file_index + invalidates insight graph.
    #[test]
    fn f011_write_helper_refreshes_documents_and_file_index() {
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

    /// F-011 positive: rename_al_file_and_refresh moves the file on disk
    /// AND drops the old file_index entry while adding the new one.
    #[test]
    fn f011_rename_helper_refreshes_file_index_for_old_and_new_paths() {
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

    /// F-011 negative: write_al_file_and_refresh propagates I/O errors
    /// instead of silently succeeding. A path under a non-existent
    /// directory must surface the underlying io::Error.
    #[test]
    fn f011_write_helper_returns_io_error_for_unwritable_path() {
        let ws = empty_ws();
        let bogus = std::path::PathBuf::from("/nonexistent/parent/dir/Foo.al");
        let err = write_al_file_and_refresh(&ws, &bogus, "x".to_string()).expect_err("must error");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        ));
    }
}
