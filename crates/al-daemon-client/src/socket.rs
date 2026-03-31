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
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = format!("{:016x}", fnv1a64(canonical.as_os_str().as_encoded_bytes()));
    let runtime_dir = runtime_dir()?;
    Some(PathBuf::from(format!(
        "{}/al-lsp/{}.sock",
        runtime_dir, hash
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
        // Ensure XDG_RUNTIME_DIR is set so socket_path returns Some.
        std::env::set_var("XDG_RUNTIME_DIR", "/tmp");
        let p = Path::new("/tmp");
        let path1 = socket_path(p).expect("socket_path returned None with XDG_RUNTIME_DIR set");
        let path2 = socket_path(p).expect("socket_path returned None with XDG_RUNTIME_DIR set");
        assert_eq!(path1, path2);
        assert!(path1.to_str().unwrap().ends_with(".sock"));
        let filename = path1.file_name().unwrap().to_str().unwrap();
        let hash_part = filename.strip_suffix(".sock").unwrap();
        assert_eq!(hash_part.len(), 16);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn socket_path_returns_none_without_runtime_dir() {
        // Temporarily clear XDG_RUNTIME_DIR and check behaviour.
        // On Linux with /proc/self/status, the fallback may still succeed;
        // on other platforms it must return None.
        let saved = std::env::var("XDG_RUNTIME_DIR").ok();
        std::env::remove_var("XDG_RUNTIME_DIR");

        let result = socket_path(Path::new("/tmp"));

        // Restore
        if let Some(v) = saved {
            std::env::set_var("XDG_RUNTIME_DIR", v);
        }

        // On Linux we may get Some from /proc fallback; on other OSes expect None.
        #[cfg(not(target_os = "linux"))]
        assert!(
            result.is_none(),
            "Expected None without XDG_RUNTIME_DIR on non-Linux"
        );
        #[cfg(target_os = "linux")]
        let _ = result; // either outcome is valid depending on /proc availability
    }
}
