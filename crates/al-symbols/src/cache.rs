//! Disk cache for parsed symbol packages.
//!
//! Caches the result of parsing SymbolReference.json from .app files.
//! On subsequent loads, checks the .app file's modification time against
//! the cached entry. If unchanged, deserializes from cache instead of
//! re-parsing the ZIP archive.
//!
//! Cache location: `~/.cache/al-lsp/index/`
//! Format: bincode-serialized Vec<SymbolEntry> per package, keyed by .app filename hash.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::debug;

use crate::model::SymbolPackage;

/// Cache header stored alongside each cached package.
#[derive(serde::Serialize, serde::Deserialize)]
struct CacheHeader {
    /// Modification time seconds component (Unix timestamp).
    mtime_secs: u64,
    /// Modification time nanoseconds component (sub-second precision).
    /// Defaults to 0 on older cache files (serde default); when 0 only
    /// seconds are compared during validation.
    #[serde(default)]
    mtime_nanos: u32,
    /// Size of the .app file when it was cached.
    file_size: u64,
    /// Name of the package (from NavxManifest.xml).
    package_name: String,
    /// Publisher from NavxManifest.xml. Defaults to empty string on older caches.
    #[serde(default)]
    publisher: String,
    /// App ID (GUID) from NavxManifest.xml. Defaults to empty string on older caches.
    #[serde(default)]
    app_id: String,
    /// Version from NavxManifest.xml. Defaults to empty string on older caches.
    #[serde(default)]
    version: String,
}

/// The disk cache for symbol packages.
pub struct SymbolCache {
    cache_dir: PathBuf,
}

impl SymbolCache {
    /// Create a cache at the default location (`~/.cache/al-lsp/index/`).
    pub fn default_location() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("al-lsp")
            .join("index");
        Self { cache_dir }
    }

    /// Create a cache at a specific directory (useful for testing).
    pub fn at(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Try to load a cached package for the given .app file.
    ///
    /// Returns `Some(SymbolPackage)` if the cache is valid, `None` if cache
    /// is missing, stale, or corrupt.
    pub fn load(&self, app_path: &Path) -> Option<SymbolPackage> {
        let cache_path = self.cache_path_for(app_path);
        let meta = fs::metadata(app_path).ok()?;
        let mtime = meta.modified().ok()?;
        let file_size = meta.len();

        let cache_data = fs::read(&cache_path).ok()?;

        // First read the header to validate freshness
        let (header, objects_data) = decode_cache(&cache_data)?;

        let mtime_duration = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?;
        let mtime_secs = mtime_duration.as_secs();
        let mtime_nanos = mtime_duration.subsec_nanos();

        // Compare mtime (seconds + nanos) and file size.
        // If the header has mtime_nanos == 0 (old cache format), only compare seconds.
        let mtime_matches = header.mtime_secs == mtime_secs
            && (header.mtime_nanos == 0 || header.mtime_nanos == mtime_nanos);

        if !mtime_matches || header.file_size != file_size {
            debug!(
                app = %app_path.display(),
                "Cache stale (mtime or size mismatch)"
            );
            return None;
        }

        // Deserialize the objects
        let objects: Vec<crate::model::SymbolEntry> = serde_json::from_slice(objects_data).ok()?;

        // Reconstruct the SymbolPackage from cached manifest fields + objects.
        let pkg = SymbolPackage {
            app_id: header.app_id.clone(),
            name: header.package_name.clone(),
            publisher: header.publisher.clone(),
            version: header.version.clone(),
            objects,
        };

        debug!(
            name = %pkg.name,
            objects = pkg.objects.len(),
            "Loaded package from cache"
        );

        Some(pkg)
    }

    /// Delete orphaned `.tmp.*` files left by processes that crashed mid-write.
    ///
    /// Files matching `*.tmp.*` that are older than 60 seconds are removed.
    /// Errors are silently ignored — cleanup is best-effort.
    fn cleanup_stale_tmp(&self) {
        let entries = match fs::read_dir(&self.cache_dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        let cutoff = Duration::from_secs(60);
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            // Match files like "foo.cache.tmp.12345"
            if !name.contains(".tmp.") {
                continue;
            }
            if let Ok(meta) = fs::metadata(&path) {
                if let Ok(age) = now.duration_since(meta.modified().unwrap_or(now)) {
                    if age > cutoff {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
    }

    /// Save a parsed package to the disk cache.
    pub fn save(&self, app_path: &Path, pkg: &SymbolPackage) -> Result<(), std::io::Error> {
        let cache_path = self.cache_path_for(app_path);
        let meta = fs::metadata(app_path)?;
        let mtime = meta
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(std::io::Error::other)?;

        let header = CacheHeader {
            mtime_secs: mtime.as_secs(),
            mtime_nanos: mtime.subsec_nanos(),
            file_size: meta.len(),
            package_name: pkg.name.clone(),
            publisher: pkg.publisher.clone(),
            app_id: pkg.app_id.clone(),
            version: pkg.version.clone(),
        };

        let header_json = serde_json::to_vec(&header)?;
        let objects_json = serde_json::to_vec(&pkg.objects)?;

        // Format: [4 bytes header_len][header_json][objects_json]
        let header_len = u32::try_from(header_json.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "header too large"))?;
        let mut data = Vec::with_capacity(4 + header_json.len() + objects_json.len());
        data.extend_from_slice(&header_len.to_le_bytes());
        data.extend_from_slice(&header_json);
        data.extend_from_slice(&objects_json);

        fs::create_dir_all(&self.cache_dir)?;
        self.cleanup_stale_tmp();

        // Write to a temp file in the same directory, then atomically rename.
        // This prevents concurrent readers from seeing a partial write and
        // prevents corruption if the process is killed mid-write.
        let tmp_path = cache_path.with_extension(format!("tmp.{}", std::process::id()));
        fs::write(&tmp_path, &data)?;
        fs::rename(&tmp_path, &cache_path)?;

        debug!(
            name = %pkg.name,
            objects = pkg.objects.len(),
            cache = %cache_path.display(),
            "Saved package to cache"
        );

        Ok(())
    }

    /// Clear the entire cache directory.
    pub fn clear(&self) -> Result<(), std::io::Error> {
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        Ok(())
    }

    /// Get the cache file path for a given .app file.
    fn cache_path_for(&self, app_path: &Path) -> PathBuf {
        let filename = app_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown");
        // Use a simple hash of the full path to avoid collisions
        let hash = simple_hash(app_path);
        self.cache_dir.join(format!("{filename}.{hash:016x}.cache"))
    }
}

/// Decode cache data into (header, objects_data_slice).
fn decode_cache(data: &[u8]) -> Option<(CacheHeader, &[u8])> {
    if data.len() < 4 {
        return None;
    }
    let header_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if data.len() < 4 + header_len {
        return None;
    }
    let header: CacheHeader = serde_json::from_slice(&data[4..4 + header_len]).ok()?;
    let objects_data = &data[4 + header_len..];
    Some((header, objects_data))
}

/// FNV-1a 64-bit hash of a path for cache file naming.
///
/// Uses a stable algorithm (FNV-1a) whose output is deterministic across Rust
/// versions, unlike `DefaultHasher` which may change with compiler upgrades.
fn simple_hash(path: &Path) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;
    let mut hash = OFFSET;
    for &b in path.as_os_str().as_encoded_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn make_test_app(dir: &Path, name: &str) -> (PathBuf, SymbolPackage) {
        // Create a minimal .app file (NAVX header + ZIP with manifest + symbols)
        let manifest = format!(
            r#"<?xml version="1.0" encoding="utf-8"?><Package><App Id="test-id" Name="{}" Publisher="Test" Version="1.0.0.0" /></Package>"#,
            name
        );
        let symbols = r#"{"Tables":[{"Id":1,"Name":"TestTable","Fields":[{"Id":1,"Name":"No.","TypeDefinition":{"Name":"Code"}}]}]}"#;

        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]);

        let mut zip_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(symbols.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);

        let app_path = dir.join(format!("{}.app", name));
        fs::write(&app_path, &data).unwrap();

        let pkg = SymbolPackage {
            app_id: "test-id".to_string(),
            name: name.to_string(),
            publisher: "Test".to_string(),
            version: "1.0.0.0".to_string(),
            objects: vec![SymbolEntry {
                kind: ObjectKind::Table,
                id: 1,
                name: "TestTable".to_string(),
                extends: None,
                implements: Vec::new(),
                namespace: String::new(),
                package: name.to_string(),
                methods: Vec::new(),
                fields: vec![FieldSymbol {
                    id: 1,
                    name: "No.".to_string(),
                    type_name: "Code".to_string(),
                    properties: vec![],
                }],
                controls: Vec::new(),
                enum_values: Vec::new(),
                keys: Vec::new(),
                properties: Vec::new(),
                variables: Vec::new(),
            }],
        };

        (app_path, pkg)
    }

    #[test]
    fn cache_miss_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = SymbolCache::at(dir.path().join("cache"));
        let app_path = dir.path().join("nonexistent.app");

        assert!(cache.load(&app_path).is_none());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let cache = SymbolCache::at(dir.path().join("cache"));
        let (app_path, pkg) = make_test_app(dir.path(), "TestPkg");

        cache.save(&app_path, &pkg).unwrap();
        let loaded = cache.load(&app_path).unwrap();

        assert_eq!(loaded.name, "TestPkg");
        assert_eq!(loaded.objects.len(), 1);
        assert_eq!(loaded.objects[0].name, "TestTable");
        assert_eq!(loaded.objects[0].fields.len(), 1);
    }

    #[test]
    fn stale_cache_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = SymbolCache::at(dir.path().join("cache"));
        let (app_path, pkg) = make_test_app(dir.path(), "StalePkg");

        cache.save(&app_path, &pkg).unwrap();

        // Modify the .app file to invalidate the cache
        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(&app_path, b"NAVX modified content").unwrap();

        assert!(cache.load(&app_path).is_none());
    }

    #[test]
    fn clear_removes_cache() {
        let dir = TempDir::new().unwrap();
        let cache = SymbolCache::at(dir.path().join("cache"));
        let (app_path, pkg) = make_test_app(dir.path(), "ClearPkg");

        cache.save(&app_path, &pkg).unwrap();
        assert!(cache.load(&app_path).is_some());

        cache.clear().unwrap();
        assert!(cache.load(&app_path).is_none());
    }

    #[test]
    fn corrupt_cache_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = SymbolCache::at(dir.path().join("cache"));
        let (app_path, pkg) = make_test_app(dir.path(), "CorruptPkg");

        cache.save(&app_path, &pkg).unwrap();

        // Corrupt the cache file
        let cache_path = cache.cache_path_for(&app_path);
        fs::write(&cache_path, b"corrupted data").unwrap();

        assert!(cache.load(&app_path).is_none());
    }

    #[test]
    fn multiple_packages_cached_independently() {
        let dir = TempDir::new().unwrap();
        let cache = SymbolCache::at(dir.path().join("cache"));
        let (app1, pkg1) = make_test_app(dir.path(), "Pkg1");
        let (app2, pkg2) = make_test_app(dir.path(), "Pkg2");

        cache.save(&app1, &pkg1).unwrap();
        cache.save(&app2, &pkg2).unwrap();

        let loaded1 = cache.load(&app1).unwrap();
        let loaded2 = cache.load(&app2).unwrap();

        assert_eq!(loaded1.name, "Pkg1");
        assert_eq!(loaded2.name, "Pkg2");
    }
}
