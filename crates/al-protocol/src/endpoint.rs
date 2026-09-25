//! Who owns the daemon endpoint, and who is answering on it.
//!
//! The endpoint path is derived from the project path, so it is guessable. On
//! Unix it is a socket file whose directory may sit under a world-writable
//! `/tmp` when `XDG_RUNTIME_DIR` is unset; on Windows it is a named pipe, and
//! pipe names are a global namespace where the first creator owns the name.
//! Either way, another process can be listening where the daemon should be.
//!
//! Two checks answer that. [`ensure_private_dir`] refuses a directory anyone
//! else could replace, which the daemon runs before it creates the socket.
//! [`check_before_connect`] and [`check_peer`] run on the client's side, before
//! it sends anything: the same directory check, plus a refusal of an endpoint
//! that is a symlink or not a socket, plus the peer's own uid read from the
//! kernel. A socket planted at the endpoint path captured a Business Central
//! password in cleartext before these existed.

#[cfg(unix)]
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
#[cfg(unix)]
pub fn check_directory_owner(dir: &Path) -> std::io::Result<()> {
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
        return refuse(format!(
            "the daemon runtime directory '{}' is a symbolic link",
            dir.display()
        ));
    }
    let owner = metadata.uid();
    if owner == me {
        return Ok(());
    }
    if owner != 0 {
        return refuse(format!(
            "the daemon runtime directory '{}' is owned by uid {owner}, not by you (uid {me})",
            dir.display()
        ));
    }
    let mode = metadata.mode();
    let sticky = mode & 0o1000 != 0;
    if mode & 0o022 != 0 && !sticky {
        return refuse(format!(
            "the daemon runtime directory '{}' is writable by other users (mode {:o})",
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
#[cfg(unix)]
fn check_ancestors(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let mut walked = std::path::PathBuf::new();
    for component in dir.components() {
        walked.push(component.as_os_str());
        let Ok(metadata) = std::fs::symlink_metadata(&walked) else {
            continue;
        };
        if !metadata.file_type().is_symlink() {
            check_directory_owner(&walked)?;
            continue;
        }
        // Safety: `geteuid` reads this process's own effective uid and cannot fail.
        let me = unsafe { libc::geteuid() };
        let owner = metadata.uid();
        if owner != 0 && owner != me {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "the daemon runtime directory '{}' is a symbolic link owned by uid {owner}, \
                     not by you (uid {me}) or root",
                    walked.display()
                ),
            ));
        }
        let resolved = std::fs::canonicalize(&walked)?;
        let mut prefix = std::path::PathBuf::new();
        for resolved_component in resolved.components() {
            prefix.push(resolved_component.as_os_str());
            check_directory_owner(&prefix)?;
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
#[cfg(unix)]
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    check_ancestors(dir)?;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    check_directory_owner(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

/// What a client checks about the endpoint before it connects.
///
/// The daemon ran [`ensure_private_dir`] before it created the socket, and the
/// client used to run nothing at all: it connected to whatever was at the path
/// and sent its request, credentials and all. The same directory check belongs
/// here, together with a refusal of an endpoint that is a symlink (which points
/// the connection somewhere else) or not a socket.
#[cfg(unix)]
pub fn check_before_connect(endpoint: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    if let Some(parent) = endpoint.parent() {
        check_ancestors(parent)?;
    }
    let metadata = std::fs::symlink_metadata(endpoint)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "the daemon endpoint '{}' is a symbolic link",
                endpoint.display()
            ),
        ));
    }
    if !file_type.is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "the daemon endpoint '{}' is not a socket",
                endpoint.display()
            ),
        ));
    }
    Ok(())
}

/// Refuse a connection whose other end belongs to another user.
///
/// `SO_PEERCRED` is filled in by the kernel when the connection is made and
/// cannot be chosen by the process on the other end, which is what separates it
/// from the build-identity handshake. It is read before the client sends
/// anything, so a planted socket never receives a request.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn check_peer(stream: &std::os::unix::net::UnixStream) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // Safety: the fd is a connected AF_UNIX socket this process owns, and the
    // buffer and its length match what SO_PEERCRED writes.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(credentials).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    compare_uid(credentials.uid)
}

/// macOS, the BSDs: `getpeereid` is the same answer under another name.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
pub fn check_peer(stream: &std::os::unix::net::UnixStream) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // Safety: the fd is a connected AF_UNIX socket this process owns.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    compare_uid(uid)
}

#[cfg(unix)]
fn compare_uid(peer: u32) -> std::io::Result<()> {
    // Safety: `geteuid` reads this process's own effective uid and cannot fail.
    let me = unsafe { libc::geteuid() };
    if peer == me {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "the process answering on the daemon endpoint runs as uid {peer}, not as you \
             (uid {me}); refusing to send anything to it"
        ),
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("al-endpoint-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_socket_of_this_user_is_accepted() {
        let dir = scratch("own");
        let path = dir.join("d.sock");
        let _listener = UnixListener::bind(&path).expect("bind");

        check_before_connect(&path).expect("this user's own socket must pass");
        let stream = UnixStream::connect(&path).expect("connect");
        check_peer(&stream).expect("this process is its own peer");
    }

    /// A planted regular file, or anything else that is not a socket, is what
    /// an attacker leaves behind when it cannot bind.
    #[test]
    fn a_planted_non_socket_is_refused() {
        let dir = scratch("regular");
        let path = dir.join("d.sock");
        std::fs::write(&path, b"not a socket").expect("write");

        let error = check_before_connect(&path).expect_err("a regular file must be refused");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("not a socket"), "{error}");
    }

    #[test]
    fn a_symlinked_endpoint_is_refused() {
        let dir = scratch("symlink");
        let real = dir.join("real.sock");
        let _listener = UnixListener::bind(&real).expect("bind");
        let link = dir.join("d.sock");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let error = check_before_connect(&link).expect_err("a symlinked endpoint must be refused");
        assert!(error.to_string().contains("symbolic link"), "{error}");
    }

    /// The refusal reads the uid the kernel recorded, so a peer that is not
    /// this user cannot talk its way past it.
    #[test]
    fn a_peer_of_another_user_is_refused() {
        // Safety: `geteuid` cannot fail.
        let me = unsafe { libc::geteuid() };
        let error = compare_uid(me.wrapping_add(1)).expect_err("another uid must be refused");
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("refusing to send"), "{error}");
    }
}
