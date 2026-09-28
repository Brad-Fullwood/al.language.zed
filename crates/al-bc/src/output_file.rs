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
/// On Unix the open uses `O_NOFOLLOW`, so a link planted after the first check
/// is refused by the kernel, and `O_NONBLOCK`, so a FIFO planted after it fails
/// the open instead of blocking. The opened handle is checked to be a regular
/// file before it is truncated. Elsewhere the bytes go to a fresh sibling that
/// is renamed over `path`, which does not open an existing link.
pub(crate) async fn write_no_follow(path: &Path, contents: Vec<u8>) -> std::io::Result<()> {
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

#[cfg(unix)]
fn write_no_follow_blocking(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                symlink_refusal(path)
            } else {
                error
            }
        })?;
    if !file.metadata()?.is_file() {
        return Err(not_regular_refusal(path));
    }
    file.set_len(0)?;
    file.write_all(contents)
}

#[cfg(not(unix))]
fn write_no_follow_blocking(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output path has no parent directory",
        )
    })?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("out"),
        std::process::id()
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(contents)?;
    }
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}
