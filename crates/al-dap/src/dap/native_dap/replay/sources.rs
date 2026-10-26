//! Writing the `.al` sources of a snapshot where the editor can open them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use al_bc::snapshot::archive::{ObjectType, SnapshotArchive};

/// Folder name for a snapshot: the zip's file stem reduced to plain characters.
pub(super) fn folder_name(zip: &Path) -> String {
    let stem = zip
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(80)
        .collect();
    if clean.is_empty() {
        "snapshot".to_string()
    } else {
        clean
    }
}

/// The file name of an object's source, `Page%42.al`.
pub(super) fn file_name(object_type: ObjectType, number: i32) -> String {
    format!("{}%{}.al", object_type.name(), number)
}

/// Parse `Page%42.al` back to the object it names.
pub(super) fn object_of_file_name(name: &str) -> Option<(ObjectType, i32)> {
    let stem = name.strip_suffix(".al")?;
    let (kind, id) = stem.split_once('%')?;
    Some((ObjectType::from_name(kind)?, id.parse().ok()?))
}

/// Write every source of the archive into `dir`, read-only on Unix, and
/// return where each object's file is.
///
/// The write refuses a symbolic link at the destination (al_bc's no follow
/// write). Files from an earlier replay are removed first because they are
/// read-only. A std route for this does not exist: `fs::write` follows links.
pub(super) async fn write_sources(
    dir: &Path,
    archive: &SnapshotArchive,
) -> std::io::Result<HashMap<(ObjectType, i32), PathBuf>> {
    tokio::fs::create_dir_all(dir).await?;
    let mut files = HashMap::new();
    for (object_type, number) in archive.source_ids() {
        let Some(text) = archive.source(object_type, number) else {
            continue;
        };
        let path = dir.join(file_name(object_type, number));
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        al_bc::output_file::write_no_follow(&path, text.as_bytes().to_vec()).await?;
        make_read_only(&path).await;
        files.insert((object_type, number), path);
    }
    Ok(files)
}

#[cfg(unix)]
async fn make_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    // Only a regular file we just wrote: a link that appeared since is left alone.
    if tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|m| m.is_file())
    {
        let _ = tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).await;
    }
}

#[cfg(not(unix))]
async fn make_read_only(path: &Path) {
    if let Ok(meta) = tokio::fs::metadata(path).await {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = tokio::fs::set_permissions(path, perms).await;
    }
}
