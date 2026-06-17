//! Disk cache for builtins and error codes.
//!
//! Keyed by AL toolchain version. Builtins extraction takes ~500ms via .NET;
//! with the cache it's <1ms from disk.

use std::path::PathBuf;

use serde::{de::DeserializeOwned, Serialize};
use tracing::{debug, info, warn};

use super::{BuiltinType, ErrorCodeInfo};

/// Maximum length for the version-derived portion of a cache filename.
/// Real AL toolchain versions are <20 chars (e.g. "17.0.34.45391"); 64 is
/// well past anything legitimate while keeping the filename well within the
/// OS NAME_MAX (typically 255). A pathological caller passing a multi-KB
/// "version" string could otherwise build a filename the OS rejects.
const MAX_SANITIZED_VERSION_LEN: usize = 64;

/// Sanitize a version string for use as part of a file name.
///
/// Replaces any character that is not an ASCII alphanumeric or `.` with `_`,
/// then truncates at `MAX_SANITIZED_VERSION_LEN` so the resulting filename
/// cannot exceed the OS limit.
fn sanitize_version(v: &str) -> String {
    let mut s: String = v
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.len() > MAX_SANITIZED_VERSION_LEN {
        s.truncate(MAX_SANITIZED_VERSION_LEN);
    }
    s
}

fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("semantic")
}

fn read_cache<T: DeserializeOwned>(version: &str, name: &str) -> Option<T> {
    let version = sanitize_version(version);
    let path = cache_dir().join(format!("{name}-{version}.json"));
    match std::fs::read_to_string(&path) {
        Ok(json) => match serde_json::from_str(&json) {
            Ok(data) => {
                debug!(version, path = %path.display(), "Loaded {name} from cache");
                Some(data)
            }
            Err(e) => {
                warn!(error = %e, "Corrupt {name} cache, will regenerate");
                let _ = std::fs::remove_file(&path);
                None
            }
        },
        Err(_) => None,
    }
}

fn write_cache<T: Serialize + ?Sized>(version: &str, name: &str, data: &T, count: usize) {
    let version = sanitize_version(version);
    let dir = cache_dir();
    // On Unix, create with 0o700 (owner-only) to mirror the crate::symbols cache:
    // semantic results may include error messages with file paths from the
    // workspace, which are minor information leaks if world-readable.
    #[cfg(unix)]
    let dir_create = {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
    };
    #[cfg(not(unix))]
    let dir_create = std::fs::create_dir_all(&dir);
    if let Err(e) = dir_create {
        warn!(error = %e, "Failed to create cache directory");
        return;
    }
    let path = dir.join(format!("{name}-{version}.json"));
    match serde_json::to_string(data) {
        Ok(json) => {
            // Write to a tmp file then atomically rename so concurrent readers
            // never see a partial JSON document (e.g. process killed mid-write).
            let tmp_path = path.with_extension("json.tmp");
            if let Err(e) = std::fs::write(&tmp_path, &json) {
                warn!(error = %e, "Failed to write {name} cache (tmp)");
                return;
            }
            if let Err(e) = std::fs::rename(&tmp_path, &path) {
                warn!(error = %e, "Failed to rename {name} cache into place");
                let _ = std::fs::remove_file(&tmp_path);
                return;
            }
            info!(version, entries = count, "Cached {name} to disk");
        }
        Err(e) => warn!(error = %e, "Failed to serialize {name} for cache"),
    }
}

pub fn read_builtins(version: &str) -> Option<Vec<BuiltinType>> {
    read_cache(version, "builtins")
}

pub fn write_builtins(version: &str, types: &[BuiltinType]) {
    write_cache(version, "builtins", types, types.len());
}

pub fn read_error_codes(version: &str) -> Option<Vec<ErrorCodeInfo>> {
    read_cache(version, "error_codes")
}

pub fn write_error_codes(version: &str, codes: &[ErrorCodeInfo]) {
    write_cache(version, "error_codes", codes, codes.len());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::BuiltinMethod;

    #[test]
    fn test_builtins_cache_roundtrip() {
        let types = vec![BuiltinType {
            name: "Text".to_string(),
            methods: vec![BuiltinMethod {
                name: "StrLen".to_string(),
                parameters: vec![],
                return_type: Some("Integer".to_string()),
                documentation: "Returns length".to_string(),
            }],
            enum_values: vec![],
        }];

        let version = "test-roundtrip-001";
        write_builtins(version, &types);
        let loaded = read_builtins(version);
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Text");
        assert_eq!(loaded[0].methods[0].name, "StrLen");

        let path = cache_dir().join(format!("builtins-{version}.json"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sanitize_version_replaces_unsafe_chars() {
        assert_eq!(sanitize_version("17.0.34.45391"), "17.0.34.45391");
        assert_eq!(sanitize_version("v17.0/beta"), "v17.0_beta");
        assert_eq!(sanitize_version("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize_version("v🙂1.0"), "v_1.0");
    }

    #[test]
    fn sanitize_version_truncates_to_cap() {
        // A pathological multi-KB "version" must not produce a multi-KB
        // filename that the OS would reject.
        let huge: String = "a".repeat(10_000);
        let sanitized = sanitize_version(&huge);
        assert_eq!(sanitized.len(), MAX_SANITIZED_VERSION_LEN);
        assert!(sanitized.chars().all(|c| c == 'a'));
    }

    #[test]
    fn test_error_codes_cache_roundtrip() {
        let codes = vec![ErrorCodeInfo {
            code: "AL0001".to_string(),
            message: "Syntax error".to_string(),
            severity: "Error".to_string(),
        }];

        let version = "test-roundtrip-002";
        write_error_codes(version, &codes);
        let loaded = read_error_codes(version);
        assert!(loaded.is_some());
        let loaded = loaded.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].code, "AL0001");

        let path = cache_dir().join(format!("error_codes-{version}.json"));
        let _ = std::fs::remove_file(path);
    }
}
