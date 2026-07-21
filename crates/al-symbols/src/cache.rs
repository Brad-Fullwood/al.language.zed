//! Disk cache for parsed symbol packages.
//!
//! Caches the result of parsing SymbolReference.json from .app files.
//! On subsequent loads, checks the .app file's modification time against
//! the cached entry. If unchanged, deserializes from cache instead of
//! re-parsing the ZIP archive.
//!
//! Cache location: `~/.cache/al-lsp/index/`
//! Format: `[4-byte LE header_len][JSON header][JSON Vec<SymbolEntry>]` per package, keyed by .app filename hash.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::debug;

use super::model::SymbolPackage;

/// Cache header stored alongside each cached package.
/// Bump whenever `SymbolEntry`'s semantics change in a way that defaults
/// can't repair — old caches are then rejected and the .app re-parsed.
/// Version 1 added the `synthetic` flag. Older caches contain
/// fabricated Option-enums that would deserialize as `synthetic: false`
/// and reappear in search results.
const CACHE_SCHEMA_VERSION: u32 = 1;
/// A cache serializes the already-capped (200 MB) SymbolReference payload plus
/// a small header. Refuse pathological/corrupt files before allocating them.
const MAX_CACHE_FILE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheHeader {
    /// Cache schema version; pre-versioning caches default to 0.
    #[serde(default)]
    schema_version: u32,
    mtime_secs: u64,
    /// Modification time nanoseconds component (sub-second precision).
    /// Defaults to 0 on older cache files; those files also carry an older
    /// schema version and are rejected before use.
    #[serde(default)]
    mtime_nanos: u32,
    file_size: u64,
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

pub struct SymbolCache {
    cache_dir: PathBuf,
}

impl SymbolCache {
    pub fn default_location() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("al-lsp")
            .join("index");
        Self { cache_dir }
    }

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

        let cache_size = fs::metadata(&cache_path).ok()?.len();
        if cache_size > MAX_CACHE_FILE_BYTES {
            tracing::warn!(
                path = %cache_path.display(),
                size = cache_size,
                limit = MAX_CACHE_FILE_BYTES,
                "symbol cache file exceeds safety limit; ignoring"
            );
            return None;
        }
        let mut cache_data = Vec::with_capacity(cache_size as usize);
        fs::File::open(&cache_path)
            .ok()?
            .take(MAX_CACHE_FILE_BYTES + 1)
            .read_to_end(&mut cache_data)
            .ok()?;
        if cache_data.len() as u64 > MAX_CACHE_FILE_BYTES {
            return None;
        }

        let (header, objects_data) = decode_cache(&cache_data)?;

        let mtime_duration = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?;
        let mtime_secs = mtime_duration.as_secs();
        let mtime_nanos = mtime_duration.subsec_nanos();

        let mtime_matches = header.mtime_secs == mtime_secs && header.mtime_nanos == mtime_nanos;

        if header.schema_version != CACHE_SCHEMA_VERSION {
            debug!(
                app = %app_path.display(),
                cached = header.schema_version,
                current = CACHE_SCHEMA_VERSION,
                "Cache schema version mismatch — re-parsing .app"
            );
            return None;
        }

        if !mtime_matches || header.file_size != file_size {
            debug!(
                app = %app_path.display(),
                "Cache stale (mtime or size mismatch)"
            );
            return None;
        }

        // An incompatible or corrupt cache is rebuilt from the package.
        let objects: Vec<super::model::SymbolEntry> = match serde_json::from_slice(objects_data) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    path = %cache_path.display(),
                    error = %e,
                    "symbol cache: failed to deserialize SymbolEntry list — falling back to .app re-parse"
                );
                return None;
            }
        };

        let pkg = SymbolPackage {
            app_id: header.app_id.clone(),
            name: header.package_name.clone(),
            publisher: header.publisher.clone(),
            version: header.version.clone(),
            object_count: objects.len(),
            objects,
        };

        debug!(
            name = %pkg.name,
            objects = pkg.objects.len(),
            "Loaded package from cache"
        );

        Some(pkg)
    }

    /// Deletes stale temporary cache files.
    fn cleanup_stale_tmp(&self) {
        let entries = match fs::read_dir(&self.cache_dir) {
            Ok(e) => e,
            Err(error) => {
                debug!(%error, path = %self.cache_dir.display(), "Could not scan symbol cache");
                return;
            }
        };
        let cutoff = Duration::from_secs(60);
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.contains(".tmp.") {
                continue;
            }
            if let Ok(meta) = fs::metadata(&path) {
                if let Ok(age) = now.duration_since(meta.modified().unwrap_or(now)) {
                    if age > cutoff {
                        if let Err(error) = fs::remove_file(&path) {
                            debug!(%error, path = %path.display(), "Could not remove stale cache file");
                        }
                    }
                }
            }
        }
    }

    pub fn save(&self, app_path: &Path, pkg: &SymbolPackage) -> Result<(), std::io::Error> {
        let cache_path = self.cache_path_for(app_path);
        let meta = fs::metadata(app_path)?;
        let mtime = meta
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(std::io::Error::other)?;

        let header = CacheHeader {
            schema_version: CACHE_SCHEMA_VERSION,
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
        let header_len = u32::try_from(header_json.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "header too large")
        })?;
        let mut data = Vec::with_capacity(4 + header_json.len() + objects_json.len());
        data.extend_from_slice(&header_len.to_le_bytes());
        data.extend_from_slice(&header_json);
        data.extend_from_slice(&objects_json);

        // Cache may contain proprietary symbol data from private packages.
        // Restrict the directory to user-only read/write/execute (0o700) on
        // Unix; on other platforms fall back to the OS default since chmod
        // semantics are not portable.
        //
        // Use DirBuilder.mode() so the directory is created with 0o700 from the
        // start; `create_dir_all + set_permissions` would leave a TOCTOU window
        // where the dir exists with the umask default (typically 0o755).
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            builder.mode(0o700);
            builder.create(&self.cache_dir)?;
            // Ensure existing dirs (created earlier with default mode) are
            // tightened too. Log on failure; silently ignoring it can leave the
            // cache dir at whatever default mode the umask produced.
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = fs::set_permissions(&self.cache_dir, fs::Permissions::from_mode(0o700))
            {
                tracing::warn!(
                    path = %self.cache_dir.display(),
                    error = %e,
                    "failed to tighten cache dir permissions to 0o700 — \
                     existing entries may be world-readable"
                );
            }
        }
        #[cfg(not(unix))]
        fs::create_dir_all(&self.cache_dir)?;
        self.cleanup_stale_tmp();

        // Write to a temp file in the same directory, then atomically rename.
        // This prevents concurrent readers from seeing a partial write and
        // prevents corruption if the process is killed mid-write.
        static CACHE_WRITE_SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let sequence = CACHE_WRITE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp_path =
            cache_path.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
        fs::write(&tmp_path, &data)?;
        if let Err(error) = fs::rename(&tmp_path, &cache_path) {
            // Unix atomically replaces the destination. Windows rename does
            // not, so fall back to a remove+rename; a concurrent reader may see
            // a harmless cache miss during this narrow window.
            if cache_path.exists() {
                fs::remove_file(&cache_path)?;
                fs::rename(&tmp_path, &cache_path)?;
            } else {
                let _ = fs::remove_file(&tmp_path);
                return Err(error);
            }
        }

        debug!(
            name = %pkg.name,
            objects = pkg.objects.len(),
            cache = %cache_path.display(),
            "Saved package to cache"
        );

        Ok(())
    }

    pub fn clear(&self) -> Result<(), std::io::Error> {
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)?;
        }
        Ok(())
    }

    fn cache_path_for(&self, app_path: &Path) -> PathBuf {
        let filename = app_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown");
        let hash = simple_hash(app_path);
        self.cache_dir.join(format!("{filename}.{hash:016x}.cache"))
    }
}

fn decode_cache(data: &[u8]) -> Option<(CacheHeader, &[u8])> {
    if data.len() < 4 {
        return None;
    }
    let header_len = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let objects_start = 4usize.checked_add(header_len)?;
    if data.len() < objects_start {
        return None;
    }
    let header: CacheHeader = serde_json::from_slice(&data[4..objects_start]).ok()?;
    let objects_data = &data[objects_start..];
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
            object_count: 1,
            objects: vec![SymbolEntry {
                synthetic: false,
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

        let cache_path = cache.cache_path_for(&app_path);
        fs::write(&cache_path, b"corrupted data").unwrap();

        assert!(cache.load(&app_path).is_none());
    }

    #[test]
    fn oversized_sparse_cache_is_rejected_without_reading_it() {
        let dir = TempDir::new().unwrap();
        let cache = SymbolCache::at(dir.path().join("cache"));
        let (app_path, pkg) = make_test_app(dir.path(), "HugeCachePkg");
        cache.save(&app_path, &pkg).unwrap();

        let cache_path = cache.cache_path_for(&app_path);
        fs::OpenOptions::new()
            .write(true)
            .open(&cache_path)
            .unwrap()
            .set_len(MAX_CACHE_FILE_BYTES + 1)
            .unwrap();

        assert!(cache.load(&app_path).is_none());
    }

    #[test]
    fn concurrent_saves_use_independent_temp_files() {
        let dir = TempDir::new().unwrap();
        let cache = std::sync::Arc::new(SymbolCache::at(dir.path().join("cache")));
        let (app_path, pkg) = make_test_app(dir.path(), "ConcurrentPkg");
        let pkg = std::sync::Arc::new(pkg);
        let mut threads = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let app_path = app_path.clone();
            let pkg = pkg.clone();
            threads.push(std::thread::spawn(move || cache.save(&app_path, &pkg)));
        }
        for thread in threads {
            thread.join().unwrap().unwrap();
        }

        assert!(cache.load(&app_path).is_some());
        let leftovers: Vec<_> = fs::read_dir(&cache.cache_dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
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
