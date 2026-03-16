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
/// paths resolve to the same socket. Format: `$XDG_RUNTIME_DIR/al-lsp/<hash>.sock`
/// (falls back to `/tmp` if XDG_RUNTIME_DIR is unset).
pub fn socket_path(project_root: &Path) -> PathBuf {
    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let hash = format!("{:016x}", fnv1a64(canonical.as_os_str().as_encoded_bytes()));
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{}/al-lsp/{}.sock", runtime_dir, hash))
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
        let path1 = socket_path(p);
        let path2 = socket_path(p);
        assert_eq!(path1, path2);
        assert!(path1.to_str().unwrap().ends_with(".sock"));
        let filename = path1.file_name().unwrap().to_str().unwrap();
        let hash_part = filename.strip_suffix(".sock").unwrap();
        assert_eq!(hash_part.len(), 16);
        assert!(hash_part.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
