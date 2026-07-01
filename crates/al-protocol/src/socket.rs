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
/// If `XDG_RUNTIME_DIR` is unset, falls back (in order) to `/run/user/<uid>`
/// on Linux, `$TMPDIR` on macOS, and a per-user subdirectory of the system
/// temp dir everywhere else. Returns `None` only if none of those can be
/// determined at all (see `runtime_dir`).
pub fn socket_path(project_root: &Path) -> Option<PathBuf> {
    socket_path_with_runtime_dir(project_root, runtime_dir()?)
}

/// Like `socket_path` but accepts an explicit runtime directory.
/// Useful for testing without modifying environment variables.
pub fn socket_path_with_runtime_dir(
    project_root: &Path,
    runtime_dir: impl AsRef<str>,
) -> Option<PathBuf> {
    // canonicalize() resolves symlinks and ensures every (different real
    // path, different alias) maps to a unique hash. If it fails — usually
    // because the project hasn't been created yet — fall back to the
    // ABSOLUTE path, never the relative input directly. Using an
    // unsanitised `project_root` here would let two callers reach the
    // same socket via different relative paths and accidentally share a
    // daemon, or in a worst case let a local attacker pre-create
    // `./.sock` and intercept JSON-RPC (T034 / sec-022).
    let canonical = project_root.canonicalize().unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|cwd| cwd.join(project_root))
            .unwrap_or_else(|_| project_root.to_path_buf())
    });
    let hash = format!("{:016x}", fnv1a64(canonical.as_os_str().as_encoded_bytes()));
    Some(PathBuf::from(format!(
        "{}/al-lsp/{}.sock",
        runtime_dir.as_ref(),
        hash
    )))
}

/// Resolve the runtime directory: prefer `XDG_RUNTIME_DIR`, fall back on Linux
/// to `/run/user/<uid>` read from `/proc/self/status` (avoids calling `getuid()`
/// via FFI), fall back on macOS to `$TMPDIR`, and fall back everywhere else to
/// a per-user subdirectory of the system temp dir. Returns `None` only if none
/// of these can be determined at all.
fn runtime_dir() -> Option<String> {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Some(dir);
    }

    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            // "Uid:\t<ruid>\t<euid>\t<suid>\t<fsuid>"
            if let Some(rest) = line.strip_prefix("Uid:") {
                let uid = rest.split_whitespace().next()?;
                let _: u64 = uid.parse().ok()?;
                return Some(format!("/run/user/{uid}"));
            }
        }
        return None;
    }

    // macOS: the OS already provisions a private, per-user, per-session temp
    // directory in `$TMPDIR` (e.g. `/var/folders/xx/yyyy/T/`) — the closest
    // equivalent to Linux's `/run/user/<uid>`. `XDG_RUNTIME_DIR` is a
    // Linux/freedesktop convention that's normally unset on macOS, so relying
    // on it alone made the daemon (and everything routed through it —
    // al-explorer, MCP, Zed daemon-backed tasks) fail outright on macOS.
    #[cfg(target_os = "macos")]
    {
        if let Ok(dir) = std::env::var("TMPDIR") {
            let trimmed = dir.trim_end_matches('/');
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    // Last-resort fallback for any other Unix (or macOS without `$TMPDIR`
    // set, which shouldn't normally happen): a per-user subdirectory of the
    // system temp dir, so unrelated users on a shared host don't collide on
    // the same path. `ensure_private_dir` (daemon/mod.rs) still re-asserts
    // 0o700 on the final `al-lsp` directory and fails loudly if it's owned by
    // someone else, so this doesn't weaken the single-owner guarantee even
    // if the temp dir itself is world-writable.
    let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).ok()?;
    if user.is_empty() {
        return None;
    }
    Some(format!("{}/{user}", std::env::temp_dir().display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_stable_known_value() {
        assert_eq!(fnv1a64(b"hello"), 0xa430d84680aabd0b);
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
    }

    // Regression test for the "Cannot determine Unix socket path:
    // XDG_RUNTIME_DIR is not set" failure seen on macOS CI, where
    // XDG_RUNTIME_DIR is normally unset. Mutates process-global env vars, like
    // the existing `socket_path_is_deterministic` test in
    // al-lsp/server/daemon/mod.rs does; accepted here for the same reason.
    #[test]
    fn runtime_dir_falls_back_when_xdg_runtime_dir_unset() {
        // Safety: test-only env mutation; no other test in this crate reads
        // XDG_RUNTIME_DIR/TMPDIR, so there's nothing else in-process to race.
        let prev_xdg = std::env::var("XDG_RUNTIME_DIR").ok();
        std::env::remove_var("XDG_RUNTIME_DIR");

        let dir = runtime_dir();

        if let Some(prev) = prev_xdg {
            std::env::set_var("XDG_RUNTIME_DIR", prev);
        }

        assert!(
            dir.is_some(),
            "runtime_dir() must resolve a fallback when XDG_RUNTIME_DIR is unset"
        );
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
        assert_ne!(path_a, path_b);
        assert!(path_a.starts_with("/run/user/1000/al-lsp/"));
        assert!(path_b.starts_with("/run/user/1001/al-lsp/"));
    }
}
