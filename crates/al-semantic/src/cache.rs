//! Disk cache for builtins and error codes.
//!
//! Keyed by AL toolchain version. Builtins extraction takes ~500ms via .NET;
//! with the cache it's <1ms from disk.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::{BuiltinType, ErrorCodeInfo};

/// Maximum length for the version-derived portion of a cache filename.
/// Real AL toolchain versions are <20 chars (e.g. "17.0.34.45391"); 64 is
/// well past anything legitimate while keeping the filename well within the
/// OS NAME_MAX (typically 255). A pathological caller passing a multi-KB
/// "version" string could otherwise build a filename the OS rejects.
const MAX_SANITIZED_VERSION_LEN: usize = 64;
const CACHE_SCHEMA_VERSION: u32 = 1;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheEnvelope<T> {
    schema_version: u32,
    toolchain_version: String,
    kind: String,
    data: T,
}

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
    let root = dirs::cache_dir()
        .map(|dir| dir.join("al-lsp"))
        .unwrap_or_else(|| {
            // If the platform exposes no per-user cache location, use a
            // process-private temp directory rather than a predictable shared
            // `/tmp/al-lsp` path that another user could pre-create or symlink.
            std::env::temp_dir().join(format!("al-lsp-{}", std::process::id()))
        });
    root.join("semantic")
}

fn read_cache<T: DeserializeOwned>(version: &str, name: &str) -> Option<T> {
    let safe_version = sanitize_version(version);
    let path = cache_dir().join(format!("{name}-{safe_version}.json"));
    let json = match std::fs::metadata(&path) {
        Ok(metadata) if metadata.len() > MAX_CACHE_BYTES => {
            warn!(path = %path.display(), bytes = metadata.len(), "Oversized {name} cache, will regenerate");
            let _ = std::fs::remove_file(&path);
            return None;
        }
        Ok(_) => std::fs::read_to_string(&path),
        Err(_) => return None,
    };
    match json {
        Ok(json) => match serde_json::from_str::<CacheEnvelope<T>>(&json) {
            Ok(envelope)
                if envelope.schema_version == CACHE_SCHEMA_VERSION
                    && envelope.toolchain_version == version
                    && envelope.kind == name =>
            {
                debug!(version, path = %path.display(), "Loaded {name} from cache");
                Some(envelope.data)
            }
            Ok(_) => {
                warn!(path = %path.display(), "Stale or mismatched {name} cache, will regenerate");
                let _ = std::fs::remove_file(&path);
                None
            }
            Err(e) => {
                warn!(error = %e, path = %path.display(), "Corrupt {name} cache, will regenerate");
                let _ = std::fs::remove_file(&path);
                None
            }
        },
        Err(_) => None,
    }
}

fn write_cache<T: Serialize + ?Sized>(version: &str, name: &str, data: &T, count: usize) {
    let safe_version = sanitize_version(version);
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
    #[cfg(unix)]
    if let Err(e) =
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
    {
        warn!(error = %e, "Failed to restrict cache directory permissions");
        return;
    }

    let path = dir.join(format!("{name}-{safe_version}.json"));
    let envelope = CacheEnvelope {
        schema_version: CACHE_SCHEMA_VERSION,
        toolchain_version: version.to_string(),
        kind: name.to_string(),
        data,
    };
    match serde_json::to_vec(&envelope) {
        Ok(json) => {
            // Write to a tmp file then atomically rename so concurrent readers
            // never see a partial JSON document (e.g. process killed mid-write).
            let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let tmp_path =
                path.with_extension(format!("json.{}.{}.tmp", std::process::id(), sequence));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let write_result = options.open(&tmp_path).and_then(|mut file| {
                file.write_all(&json)?;
                file.sync_all()
            });
            if let Err(e) = write_result {
                warn!(error = %e, "Failed to write {name} cache (tmp)");
                let _ = std::fs::remove_file(&tmp_path);
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
    use crate::BuiltinMethod;

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
    fn sanitized_filename_collision_never_returns_wrong_toolchain_data() {
        let first_version = "collision/version";
        let second_version = "collision_version";
        assert_eq!(
            sanitize_version(first_version),
            sanitize_version(second_version),
            "test versions must exercise the same cache filename"
        );

        let types = vec![BuiltinType {
            name: "Text".to_string(),
            methods: Vec::new(),
            enum_values: Vec::new(),
        }];
        write_builtins(first_version, &types);
        assert!(
            read_builtins(second_version).is_none(),
            "the envelope must reject data written for a different raw version"
        );

        let path = cache_dir().join(format!("builtins-{}.json", sanitize_version(first_version)));
        let _ = std::fs::remove_file(path);
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
