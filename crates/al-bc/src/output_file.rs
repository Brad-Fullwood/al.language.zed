//! Writing a downloaded snapshot or profile into the output directory.
//!
//! The daemon checks that `outputDir` is inside the project, and the file name
//! the download adds to it is checked here. A project can ship a symbolic link
//! at that name, so the write refuses a link at the destination and any other
//! entry there that is not a regular file. The same approach as
//! `write_no_follow` in al-lsp's `daemon/containment.rs`, which al-bc cannot
//! depend on.

use std::path::Path;

/// Write `contents` to `path`, refusing a symbolic link at `path` and an
/// existing entry there that is not a regular file.
///
/// The bytes go to a fresh sibling (mode 0600 and `O_NOFOLLOW` on Unix) that
/// is renamed over `path`, so an existing link or file at `path` is never
/// opened.
pub async fn write_no_follow(path: &Path, contents: Vec<u8>) -> std::io::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        refuse_existing_non_regular(&path)?;
        write_no_follow_blocking(&path, &contents)
    })
    .await
    .map_err(std::io::Error::other)?
}

fn refuse_existing_non_regular(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(symlink_refusal(path)),
        Ok(metadata) if !metadata.is_file() => Err(not_regular_refusal(path)),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn symlink_refusal(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "refusing to write through the symbolic link at '{}'",
            path.display()
        ),
    )
}

fn not_regular_refusal(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "refusing to write to '{}', which exists and is not a regular file",
            path.display()
        ),
    )
}

/// Distinguishes temp names of writes running at the same time in one process.
static TEMP_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Write the bytes to a fresh sibling `.<name>.<pid>.<n>.tmp`, then rename it
/// over `path`. An existing regular file at `path` is replaced, so a hard link
/// to it keeps the old bytes. The rename replaces a link planted at `path`
/// after the first check instead of following it.
fn write_no_follow_blocking(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path has no parent directory",
        )
    })?;
    let temp = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out"),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(contents)?;
        file.sync_all()
    });
    written
        .and_then(|()| std::fs::rename(&temp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn existing_file_is_replaced_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::write(&a, b"old bytes").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        write_no_follow(&a, b"new".to_vec()).await.unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"new");
        assert_eq!(std::fs::read(&b).unwrap(), b"old bytes");
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "no temp file is left: {left:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_at_the_target_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::write(&outside, b"keep").unwrap();
        let link = dir.path().join("out.zip");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let error = write_no_follow(&link, b"new".to_vec()).await.unwrap_err();
        assert!(error.to_string().contains("symbolic link"), "{error}");
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
    }
}
