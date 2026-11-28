//! Folders only this user can replace: the daemon endpoint folder and the
//! cache fallback under the temp dir.
//!
//! Unix only. `what` names the folder in a refusal, for example
//! `the daemon runtime directory`.

use std::path::Path;

/// Refuse a directory anyone but this user, or root, could replace.
///
/// With `XDG_RUNTIME_DIR` unset the socket path falls back to
/// `{temp_dir}/{USER}/al-lsp/<hash>.sock`. `DirBuilder::recursive` applies its
/// mode only to the directories it creates, so on a shared host an attacker who
/// creates `/tmp/<victim>` first owns the parent, can rename the `al-lsp` entry
/// whatever its own mode says, and can bind their own socket where al-explorer
/// and the MCP server connect.
///
/// A component passes when this user owns it, or root owns it and it is either
/// not writable by group or other, or sticky (which is what `/tmp` is).
pub fn check_directory_owner(dir: &Path, what: &str) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let refuse = |message: String| {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            message,
        ))
    };
    // Safety: `geteuid` reads the calling process's own effective uid and
    // cannot fail.
    let me = unsafe { libc::geteuid() };
    let metadata = std::fs::symlink_metadata(dir)?;
    if metadata.file_type().is_symlink() {
        return refuse(format!("{what} '{}' is a symbolic link", dir.display()));
    }
    let owner = metadata.uid();
    if owner == me {
        return Ok(());
    }
    if owner != 0 {
        return refuse(format!(
            "{what} '{}' is owned by uid {owner}, not by you (uid {me})",
            dir.display()
        ));
    }
    let mode = metadata.mode();
    let sticky = mode & 0o1000 != 0;
    if mode & 0o022 != 0 && !sticky {
        return refuse(format!(
            "{what} '{}' is writable by other users (mode {:o})",
            dir.display(),
            mode & 0o7777
        ));
    }
    Ok(())
}

/// Every existing ancestor of `dir`, root first, so a refusal names the
/// outermost directory that fails rather than the leaf inside it.
///
/// A symlinked component is followed rather than refused when root or this
/// user owns the link: its containing directory has already passed, so
/// nobody else can replace it, and every directory on the path it resolves to
/// is checked the same way. macOS needs this, since `TMPDIR` lives under
/// `/var` and `/tmp`, which are root-owned links into `/private`; refusing
/// every link kept the daemon from starting there at all. The caller still
/// refuses a link at the leaf itself.
pub fn check_ancestors(dir: &Path, what: &str) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let mut walked = std::path::PathBuf::new();
    for component in dir.components() {
        walked.push(component.as_os_str());
        let Ok(metadata) = std::fs::symlink_metadata(&walked) else {
            continue;
        };
        if !metadata.file_type().is_symlink() {
            check_directory_owner(&walked, what)?;
            continue;
        }
        // Safety: `geteuid` reads this process's own effective uid and cannot fail.
        let me = unsafe { libc::geteuid() };
        let owner = metadata.uid();
        if owner != 0 && owner != me {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{what} '{}' is a symbolic link owned by uid {owner}, \
                     not by you (uid {me}) or root",
                    walked.display()
                ),
            ));
        }
        let resolved = std::fs::canonicalize(&walked)?;
        let mut prefix = std::path::PathBuf::new();
        for resolved_component in resolved.components() {
            prefix.push(resolved_component.as_os_str());
            check_directory_owner(&prefix, what)?;
        }
        walked = resolved;
    }
    Ok(())
}

/// Create `dir` (and parents) restricted to the owner (0o700), refusing any
/// existing component someone else could replace.
///
/// `DirBuilder::mode` applies the mode only to directories this call creates, so
/// a dir left at a laxer mode by an earlier run (created before this hardening,
/// or under a different umask) would keep its old permissions. We therefore
/// re-assert 0o700 after creation, making the result independent of prior state.
pub fn ensure_private_dir(dir: &Path, what: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    check_ancestors(dir, what)?;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    check_directory_owner(dir, what)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}
