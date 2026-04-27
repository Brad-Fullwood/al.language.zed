use std::path::{Path, PathBuf};

/// FNV-1a 64-bit hash — stable across Rust compiler versions.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    let mut hash = OFFSET;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Compute the deterministic Unix socket path for a project root.
///
/// The path is canonicalized before hashing so that symlinks and relative
/// paths resolve to the same socket. Format: `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`.
///
/// On Linux, if `XDG_RUNTIME_DIR` is unset, we try `/run/user/<uid>` by reading
/// the real UID from `/proc/self/status`. Returns `None` if no secure runtime
/// directory can be determined (e.g. non-Linux, non-XDG environment without
/// `/proc`).
pub fn socket_path(project_root: &Path) -> Option<PathBuf> {
    socket_path_with_runtime_dir(project_root, runtime_dir()?)
}

/// Like `socket_path` but accepts an explicit runtime directory.
/// Useful for testing without modifying environment variables.
pub fn socket_path_with_runtime_dir(
    project_root: &Path,
    runtime_dir: impl AsRef<str>,
) -> Option<PathBuf> {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = format!("{:016x}", fnv1a64(canonical.as_os_str().as_encoded_bytes()));
    Some(PathBuf::from(format!(
        "{}/al-lsp/{}.sock",
        runtime_dir.as_ref(),
        hash
    )))
}

/// Resolve the runtime directory: prefer `XDG_RUNTIME_DIR`, fall back on Linux
/// to `/run/user/<uid>` read from `/proc/self/status` (avoids calling `getuid()`
/// via FFI). Returns `None` when neither source is available.
fn runtime_dir() -> Option<String> {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Some(dir);
    }

    // Linux fallback: read real UID from /proc/self/status without libc.
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            // "Uid:\t<ruid>\t<euid>\t<suid>\t<fsuid>"
            if let Some(rest) = line.strip_prefix("Uid:") {
                let uid = rest.split_whitespace().next()?;
                // Validate that it parses as a number before using it in a path.
                let _: u64 = uid.parse().ok()?;
                return Some(format!("/run/user/{uid}"));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_stable_known_value() {
        assert_eq!(fnv1a64(b"hello"), 0xa430d84680aabd0b);
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
    }

    #[test]
    fn socket_path_is_deterministic() {
        let p = Path::new("/tmp");
        let path1 = socket_path_with_runtime_dir(p, "/tmp")
            .expect("socket_path_with_runtime_dir returned None");
        let path2 = socket_path_with_runtime_dir(p, "/tmp")
            .expect("socket_path_with_runtime_dir returned None");
        assert_eq!(path1, path2);
        assert!(path1.to_str().expect("valid UTF-8").ends_with(".sock"));
        let filename = path1
            .file_name()
            .expect("has filename")
            .to_str()
            .expect("valid UTF-8");
        let hash_part = filename.strip_suffix(".sock").expect("ends with .sock");
        assert_eq!(hash_part.len(), 16);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn socket_path_different_runtime_dirs_differ() {
        let p = Path::new("/tmp");
        let path_a = socket_path_with_runtime_dir(p, "/run/user/1000").expect("returns Some");
        let path_b = socket_path_with_runtime_dir(p, "/run/user/1001").expect("returns Some");
        // Same project hash but different parent directories
        assert_ne!(path_a, path_b);
        assert!(path_a.starts_with("/run/user/1000/al-lsp/"));
        assert!(path_b.starts_with("/run/user/1001/al-lsp/"));
    }
}
