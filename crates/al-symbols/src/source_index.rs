//! Source indexing for `.app` packages with embedded AL source.
//!
//! Builds a fast map from (ObjectKind, Id, Name) to the internal ZIP path
//! by scanning object headers once per package. Subsequent lookups are instant.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use dashmap::DashMap;
use memmap2::Mmap;
use zip::ZipArchive;

use crate::model::{ObjectKind, SymbolEntry};

const MAX_HEADER_BYTES: usize = 256 * 1024;

/// Cached source index per `.app` file.
///
/// Each entry is paired with a per-path `Mutex<()>` build guard so that
/// concurrent callers for the **same** path serialise on building the index
/// (double-checked locking) while callers for **different** paths remain
/// independent.
static SOURCE_INDEX_CACHE: OnceLock<DashMap<PathBuf, Arc<AppSourceIndex>>> = OnceLock::new();
static SOURCE_BUILD_LOCKS: OnceLock<DashMap<PathBuf, Arc<Mutex<()>>>> = OnceLock::new();

#[derive(Debug)]
pub struct AppSourceIndex {
    modified: SystemTime,
    mmap: Arc<Mmap>,
    zip_offset: usize,
    by_kind_id: HashMap<(ObjectKind, i32), String>,
    by_kind_name: HashMap<(ObjectKind, String), String>,
}

impl AppSourceIndex {
    pub fn from_app_path(app_path: &Path) -> io::Result<Self> {
        let file = File::open(app_path)?;
        let modified = file
            .metadata()?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        // SAFETY: .app files are opened read-only. Concurrent modification is
        // prevented by the staleness check at the call site (package version
        // comparison via `modified` timestamp). On Linux, MAP_PRIVATE means a
        // concurrent file replacement serves stale data rather than UB. On
        // Windows, the file cannot be replaced while it is mapped.
        let mmap = unsafe { Mmap::map(&file)? };
        let zip_offset = crate::app_reader::find_zip_offset(&mmap).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "ZIP signature not found in .app",
            )
        })?;

        let mut archive = ZipArchive::new(Cursor::new(&mmap[zip_offset..]))?;
        let mut by_kind_id = HashMap::new();
        let mut by_kind_name = HashMap::new();

        for i in 0..archive.len() {
            let file = archive.by_index(i)?;
            let name = file.name().to_string();
            if !name.to_lowercase().ends_with(".al") {
                continue;
            }

            let mut buf = Vec::new();
            let mut limited = file.take(MAX_HEADER_BYTES as u64);
            if limited.read_to_end(&mut buf).is_err() {
                continue;
            }

            if let Some((kind, id, obj_name)) = parse_object_header(&buf) {
                by_kind_id.entry((kind, id)).or_insert_with(|| name.clone());
                by_kind_name
                    .entry((kind, obj_name.to_lowercase()))
                    .or_insert_with(|| name.clone());
            }
        }

        Ok(Self {
            modified,
            mmap: Arc::new(mmap),
            zip_offset,
            by_kind_id,
            by_kind_name,
        })
    }

    pub fn source_path_for_entry(&self, entry: &SymbolEntry) -> Option<&str> {
        if entry.id != 0 {
            if let Some(path) = self.by_kind_id.get(&(entry.kind, entry.id)) {
                return Some(path.as_str());
            }
        }
        self.by_kind_name
            .get(&(entry.kind, entry.name.to_lowercase()))
            .map(|s| s.as_str())
    }

    pub fn extract_source_by_path(&self, zip_path: &str) -> Option<String> {
        let mut archive = ZipArchive::new(Cursor::new(&self.mmap[self.zip_offset..])).ok()?;
        let mut file = archive.by_name(zip_path).ok()?;
        let mut content = String::new();
        file.read_to_string(&mut content).ok()?;
        Some(content)
    }

    pub fn extract_source_for_entry(&self, entry: &SymbolEntry) -> Option<String> {
        let path = self.source_path_for_entry(entry)?;
        self.extract_source_by_path(path)
    }
}

/// Get a cached index for the given `.app` path, rebuilding if the file changed.
///
/// Uses double-checked locking to avoid redundant builds under concurrent
/// access: after acquiring the per-path build mutex a second staleness check
/// is performed so that a thread that lost the race finds the already-built
/// index and returns it immediately.
pub fn get_or_build(app_path: &Path) -> io::Result<Arc<AppSourceIndex>> {
    let cache = SOURCE_INDEX_CACHE.get_or_init(DashMap::new);

    // --- First check (no lock) ---
    if let Some(existing) = cache.get(app_path) {
        if let Ok(meta) = std::fs::metadata(app_path) {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if existing.modified == modified {
                return Ok(existing.value().clone());
            }
        }
    }

    // --- Serialise concurrent builds for this specific path ---
    let build_locks = SOURCE_BUILD_LOCKS.get_or_init(DashMap::new);
    let lock_arc = {
        // Avoid holding the DashMap shard lock while we await the build mutex.
        build_locks
            .entry(app_path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .value()
            .clone()
    };
    let _guard = lock_arc.lock().unwrap_or_else(|e| e.into_inner());

    // --- Second check (under lock) ---
    if let Some(existing) = cache.get(app_path) {
        if let Ok(meta) = std::fs::metadata(app_path) {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if existing.modified == modified {
                return Ok(existing.value().clone());
            }
        }
    }

    let built = Arc::new(AppSourceIndex::from_app_path(app_path)?);
    cache.insert(app_path.to_path_buf(), built.clone());
    Ok(built)
}

/// Clear all cached source indices, freeing memory.
///
/// Callers should invoke this when switching projects or when cached `.app`
/// files are no longer needed.
pub fn clear_source_index_cache() {
    if let Some(cache) = SOURCE_INDEX_CACHE.get() {
        cache.clear();
    }
    if let Some(locks) = SOURCE_BUILD_LOCKS.get() {
        locks.clear();
    }
}

fn parse_object_header(bytes: &[u8]) -> Option<(ObjectKind, i32, String)> {
    let text = String::from_utf8_lossy(bytes);
    let s = text.as_ref();
    let b = s.as_bytes();
    let mut i = 0usize;

    while i < b.len() {
        skip_ws_and_comments(b, &mut i);
        if i >= b.len() {
            break;
        }
        if b[i] == b'\'' {
            i += 1;
            while i < b.len() {
                if b[i] == b'\'' {
                    if i + 1 < b.len() && b[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if b[i] == b'"' {
            i += 1;
            while i < b.len() {
                if b[i] == b'"' {
                    if i + 1 < b.len() && b[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if is_ident_start(b[i]) {
            let start = i;
            i += 1;
            while i < b.len() && is_ident_char(b[i]) {
                i += 1;
            }
            let ident = &s[start..i];
            if let Ok(kind) = ident.parse::<ObjectKind>() {
                let mut j = i;
                skip_ws_and_comments(b, &mut j);
                let (id, next) = match parse_int(b, j) {
                    Some(v) => v,
                    None => continue,
                };
                let mut k = next;
                skip_ws_and_comments(b, &mut k);
                if let Some((name, _)) = parse_name(s, b, k) {
                    return Some((kind, id, name));
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn skip_ws_and_comments(bytes: &[u8], i: &mut usize) {
    loop {
        while *i < bytes.len() && bytes[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if *i + 1 >= bytes.len() {
            return;
        }
        if bytes[*i] == b'/' && bytes[*i + 1] == b'/' {
            *i += 2;
            while *i < bytes.len() && bytes[*i] != b'\n' {
                *i += 1;
            }
            continue;
        }
        if bytes[*i] == b'/' && bytes[*i + 1] == b'*' {
            *i += 2;
            while *i + 1 < bytes.len() && !(bytes[*i] == b'*' && bytes[*i + 1] == b'/') {
                *i += 1;
            }
            *i = (*i + 2).min(bytes.len());
            continue;
        }
        break;
    }
}

fn parse_int(bytes: &[u8], mut i: usize) -> Option<(i32, usize)> {
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return None;
    }
    let num = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
    Some((num, i))
}

/// Parse a double-quoted AL identifier starting at `start` (which must be the `"` byte).
///
/// Handles escaped double-quotes (`""` → `"`). Returns the parsed string and the position
/// immediately after the closing `"`. Returns `None` if the closing quote is missing.
pub(crate) fn parse_quoted_ident(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    debug_assert_eq!(bytes[start], b'"');
    let s = std::str::from_utf8(bytes).ok()?;
    let mut i = start + 1;
    let mut out = String::new();
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                out.push('"');
                i += 2;
                continue;
            }
            return Some((out, i + 1));
        }
        let ch = s[i..].chars().next()?;
        out.push(ch);
        i += ch.len_utf8();
    }
    None // unterminated quoted identifier
}

fn parse_name(s: &str, bytes: &[u8], mut i: usize) -> Option<(String, usize)> {
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'"' {
        return parse_quoted_ident(bytes, i);
    }

    let start = i;
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'{' {
        i += 1;
    }
    let name = s[start..i].trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some((name, i))
    }
}

pub(crate) fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

pub(crate) fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
