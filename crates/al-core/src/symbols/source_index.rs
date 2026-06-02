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

use super::model::{ObjectKind, SymbolEntry};

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
        // SAFETY: .app files are opened read-only. memmap2::Mmap uses
        // MAP_SHARED on Linux (not MAP_PRIVATE — earlier comment versions of
        // this file had that wrong), so concurrent file replacement may
        // produce stale or partial data, but never UB; corruption surfaces as
        // a ZIP-decode error and is caught by the InvalidData branch below.
        // On Windows, the file cannot be replaced while it is mapped, so the
        // race doesn't exist there. Concurrent modification is prevented at
        // the call site by the staleness check (package version comparison
        // via `modified` timestamp) — we never re-mmap a file we've already
        // accepted as fresh.
        let mmap = unsafe { Mmap::map(&file)? };
        let zip_offset = super::app_reader::find_zip_offset(&mmap).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "ZIP signature not found in .app",
            )
        })?;

        let mut archive = ZipArchive::new(Cursor::new(&mmap[zip_offset..]))?;
        let mut by_kind_id = HashMap::new();
        let mut by_kind_name = HashMap::new();

        // Zip-bomb guard: cap total decompressed bytes across all `.al` entries
        // so a malicious .app cannot expand to gigabytes during scan. 1 GiB is
        // far above any plausible legitimate package and well below typical
        // memory limits.
        const MAX_TOTAL_DECOMPRESSED_BYTES: u64 = 1_073_741_824; // 1 GiB
        let mut total_decompressed: u64 = 0;

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
            total_decompressed = total_decompressed.saturating_add(buf.len() as u64);
            if total_decompressed > MAX_TOTAL_DECOMPRESSED_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "decompressed .al header bytes exceed 1 GiB total — refusing potential zip bomb",
                ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // -- helpers --------------------------------------------------------------

    fn entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id,
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// Build a realistic `.app` byte blob: 40-byte NAVX header + ZIP archive
    /// whose entries are `(path, contents)`.
    fn build_app(files: &[(&str, &str)]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 28]); // pad to 40 bytes

        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (path, contents) in files {
                zip.start_file(*path, opts).unwrap();
                zip.write_all(contents.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);
        data
    }

    fn write_app(files: &[(&str, &str)]) -> tempfile::TempPath {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&build_app(files)).unwrap();
        tmp.flush().unwrap();
        tmp.into_temp_path()
    }

    // -- parse_object_header --------------------------------------------------

    #[test]
    fn parse_object_header_basic_unquoted() {
        let got = parse_object_header(b"codeunit 50100 MyCodeunit\n{\n}").unwrap();
        assert_eq!(got, (ObjectKind::Codeunit, 50100, "MyCodeunit".to_string()));
    }

    #[test]
    fn parse_object_header_quoted_name_with_spaces() {
        let got = parse_object_header(b"table 18 \"Customer Card\"\n{").unwrap();
        assert_eq!(got, (ObjectKind::Table, 18, "Customer Card".to_string()));
    }

    #[test]
    fn parse_object_header_skips_leading_line_and_block_comments() {
        let src = b"// license header\n/* block\n comment */\npage 42 MyPage\n{";
        let got = parse_object_header(src).unwrap();
        assert_eq!(got, (ObjectKind::Page, 42, "MyPage".to_string()));
    }

    #[test]
    fn parse_object_header_skips_pragma_before_object() {
        // A leading identifier that is not an object kind must be skipped,
        // and parsing must continue to the real object declaration.
        let src = b"namespace Foo.Bar;\ntableextension 50000 MyExt extends Customer\n{";
        let got = parse_object_header(src).unwrap();
        assert_eq!(
            got,
            (ObjectKind::TableExtension, 50000, "MyExt".to_string())
        );
    }

    #[test]
    fn parse_object_header_returns_none_when_no_object() {
        assert!(parse_object_header(b"// just a comment\nrandom text 123").is_none());
    }

    #[test]
    fn parse_object_header_returns_none_on_kind_without_id() {
        // "codeunit" with no integer id following must not match.
        assert!(parse_object_header(b"codeunit MyCodeunit\n{").is_none());
    }

    #[test]
    fn parse_object_header_ignores_keyword_inside_string_literal() {
        // The word "table" appears only inside a string literal; not an object.
        let src = b"'table 1 NotReal' enum 7 RealEnum\n{";
        let got = parse_object_header(src).unwrap();
        assert_eq!(got, (ObjectKind::Enum, 7, "RealEnum".to_string()));
    }

    // -- skip_ws_and_comments -------------------------------------------------

    #[test]
    fn skip_ws_and_comments_advances_past_mixed() {
        let b = b"  \t\n // line\n /* blk */ X";
        let mut i = 0;
        skip_ws_and_comments(b, &mut i);
        assert_eq!(b[i], b'X');
    }

    #[test]
    fn skip_ws_and_comments_stops_at_single_slash() {
        // A lone '/' (e.g. division) is not a comment and must not be skipped.
        let b = b"   /x";
        let mut i = 0;
        skip_ws_and_comments(b, &mut i);
        assert_eq!(b[i], b'/');
    }

    #[test]
    fn skip_ws_and_comments_handles_unterminated_block_comment() {
        let b = b"  /* never closed";
        let mut i = 0;
        skip_ws_and_comments(b, &mut i);
        // Must not panic and must consume to end.
        assert_eq!(i, b.len());
    }

    // -- parse_int ------------------------------------------------------------

    #[test]
    fn parse_int_reads_digits_and_stops() {
        assert_eq!(parse_int(b"12345abc", 0), Some((12345, 5)));
    }

    #[test]
    fn parse_int_none_when_no_digit() {
        assert_eq!(parse_int(b"abc", 0), None);
    }

    #[test]
    fn parse_int_offset_start() {
        assert_eq!(parse_int(b"xx99", 2), Some((99, 4)));
    }

    // -- parse_quoted_ident ---------------------------------------------------

    #[test]
    fn parse_quoted_ident_simple() {
        assert_eq!(
            parse_quoted_ident(b"\"Hello\" rest", 0),
            Some(("Hello".to_string(), 7))
        );
    }

    #[test]
    fn parse_quoted_ident_escaped_double_quote() {
        // "" inside the identifier collapses to a single ".
        let bytes = b"\"a\"\"b\"";
        assert_eq!(
            parse_quoted_ident(bytes, 0),
            Some(("a\"b".to_string(), bytes.len()))
        );
    }

    #[test]
    fn parse_quoted_ident_unterminated_is_none() {
        assert_eq!(parse_quoted_ident(b"\"unterminated", 0), None);
    }

    #[test]
    fn parse_quoted_ident_handles_multibyte_content() {
        let bytes = "\"Caf\u{e9}\"".as_bytes();
        let (s, end) = parse_quoted_ident(bytes, 0).unwrap();
        assert_eq!(s, "Caf\u{e9}");
        assert_eq!(end, bytes.len());
    }

    // -- parse_name -----------------------------------------------------------

    #[test]
    fn parse_name_unquoted_stops_at_brace() {
        let s = "MyObj{";
        assert_eq!(
            parse_name(s, s.as_bytes(), 0),
            Some(("MyObj".to_string(), 5))
        );
    }

    #[test]
    fn parse_name_quoted_delegates() {
        let s = "\"Sales Header\"";
        assert_eq!(
            parse_name(s, s.as_bytes(), 0),
            Some(("Sales Header".to_string(), s.len()))
        );
    }

    #[test]
    fn parse_name_none_at_end_or_empty() {
        let s = "abc";
        assert_eq!(parse_name(s, s.as_bytes(), 3), None); // i >= len
        let s2 = " ";
        assert_eq!(parse_name(s2, s2.as_bytes(), 0), None); // whitespace -> empty
    }

    // -- is_ident_start / is_ident_char --------------------------------------

    #[test]
    fn ident_classifiers() {
        assert!(is_ident_start(b'a'));
        assert!(is_ident_start(b'Z'));
        assert!(is_ident_start(b'_'));
        assert!(!is_ident_start(b'9'));
        assert!(!is_ident_start(b' '));

        assert!(is_ident_char(b'0'));
        assert!(is_ident_char(b'q'));
        assert!(is_ident_char(b'_'));
        assert!(!is_ident_char(b'-'));
    }

    // -- source_path_for_entry (lookup logic) ---------------------------------

    fn make_index_with(
        by_id: &[((ObjectKind, i32), &str)],
        by_name: &[((ObjectKind, &str), &str)],
    ) -> AppSourceIndex {
        let mut by_kind_id = HashMap::new();
        for ((k, id), p) in by_id {
            by_kind_id.insert((*k, *id), p.to_string());
        }
        let mut by_kind_name = HashMap::new();
        for ((k, n), p) in by_name {
            by_kind_name.insert((*k, n.to_string()), p.to_string());
        }
        // The mmap field is required but never read by the lookup-only tests;
        // back it with a tiny non-empty temp file (an empty file cannot be mapped).
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"x").unwrap();
        f.flush().unwrap();
        let mmap = Arc::new(unsafe { Mmap::map(f.as_file()) }.unwrap());

        AppSourceIndex {
            modified: SystemTime::UNIX_EPOCH,
            mmap,
            zip_offset: 0,
            by_kind_id,
            by_kind_name,
        }
    }

    #[test]
    fn source_path_prefers_id_when_id_nonzero() {
        let idx = make_index_with(
            &[((ObjectKind::Codeunit, 50100), "ById.al")],
            &[((ObjectKind::Codeunit, "mycu"), "ByName.al")],
        );
        let e = entry(ObjectKind::Codeunit, 50100, "MyCu");
        assert_eq!(idx.source_path_for_entry(&e), Some("ById.al"));
    }

    #[test]
    fn source_path_falls_back_to_name_when_id_missing() {
        // id != 0 but not present in by_kind_id -> name lookup (case-insensitive).
        let idx = make_index_with(&[], &[((ObjectKind::Table, "customer"), "Cust.al")]);
        let e = entry(ObjectKind::Table, 999, "CUSTOMER");
        assert_eq!(idx.source_path_for_entry(&e), Some("Cust.al"));
    }

    #[test]
    fn source_path_uses_name_when_id_zero() {
        // id == 0 must skip the id table entirely even if a (kind,0) entry existed.
        let idx = make_index_with(
            &[((ObjectKind::Page, 0), "ShouldNotUse.al")],
            &[((ObjectKind::Page, "mypage"), "ByName.al")],
        );
        let e = entry(ObjectKind::Page, 0, "MyPage");
        assert_eq!(idx.source_path_for_entry(&e), Some("ByName.al"));
    }

    #[test]
    fn source_path_none_when_unknown() {
        let idx = make_index_with(&[], &[]);
        let e = entry(ObjectKind::Report, 1, "Nope");
        assert_eq!(idx.source_path_for_entry(&e), None);
    }

    #[test]
    fn source_path_kind_must_match() {
        // Same id but different kind must not match.
        let idx = make_index_with(&[((ObjectKind::Table, 18), "Tab.al")], &[]);
        let e = entry(ObjectKind::Page, 18, "Whatever");
        assert_eq!(idx.source_path_for_entry(&e), None);
    }

    // -- from_app_path + extract_* (full pipeline) ----------------------------

    #[test]
    fn from_app_path_indexes_and_extracts_source() {
        let tab_src = "table 18 Customer\n{\n    fields { field(1; No; Code[20]) { } }\n}";
        let cod_src = "codeunit 50100 \"My Helper\"\n{\n    procedure Foo() begin end;\n}";
        let path = write_app(&[
            ("src/Tab18.Customer.al", tab_src),
            ("src/Cod50100.MyHelper.al", cod_src),
            ("NavxManifest.xml", "<Package/>"), // non-.al, must be ignored
        ]);

        let idx = AppSourceIndex::from_app_path(&path).unwrap();

        // Lookup by id.
        let cust = entry(ObjectKind::Table, 18, "Customer");
        assert_eq!(
            idx.source_path_for_entry(&cust),
            Some("src/Tab18.Customer.al")
        );
        assert_eq!(
            idx.extract_source_for_entry(&cust).as_deref(),
            Some(tab_src)
        );

        // Quoted name with id.
        let helper = entry(ObjectKind::Codeunit, 50100, "My Helper");
        assert_eq!(
            idx.extract_source_for_entry(&helper).as_deref(),
            Some(cod_src)
        );

        // Lookup by name (id 0) is case-insensitive.
        let by_name = entry(ObjectKind::Table, 0, "cUsToMeR");
        assert_eq!(
            idx.source_path_for_entry(&by_name),
            Some("src/Tab18.Customer.al")
        );
    }

    #[test]
    fn from_app_path_errors_when_not_a_zip() {
        // Valid-ish header but no ZIP signature anywhere -> InvalidData.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"NAVX not a zip at all, just plain bytes here ...........")
            .unwrap();
        tmp.flush().unwrap();
        let err = AppSourceIndex::from_app_path(tmp.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn from_app_path_errors_when_file_missing() {
        let err = AppSourceIndex::from_app_path(Path::new("/no/such/file.app")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn extract_source_by_path_none_for_unknown_path() {
        let path = write_app(&[("src/Tab18.Customer.al", "table 18 Customer { }")]);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();
        assert!(idx.extract_source_by_path("does/not/exist.al").is_none());
    }

    #[test]
    fn extract_source_for_entry_none_when_entry_unindexed() {
        let path = write_app(&[("src/Tab18.Customer.al", "table 18 Customer { }")]);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();
        let ghost = entry(ObjectKind::Report, 12345, "Ghost");
        assert!(idx.extract_source_for_entry(&ghost).is_none());
    }

    // -- get_or_build / cache -------------------------------------------------

    #[test]
    fn get_or_build_caches_same_arc_and_resolves_content() {
        let path = write_app(&[("src/Cod1.X.al", "codeunit 1 X { }")]);
        let p = path.to_path_buf();

        let a = get_or_build(&p).unwrap();
        let b = get_or_build(&p).unwrap();
        // Same path, unchanged mtime -> the second call must hit the cache and
        // return the very same Arc (double-checked-locking fast path).
        assert!(Arc::ptr_eq(&a, &b), "second build should return cached Arc");

        let x = entry(ObjectKind::Codeunit, 1, "X");
        assert_eq!(
            a.extract_source_for_entry(&x).as_deref(),
            Some("codeunit 1 X { }")
        );

        // After clearing the cache, a fresh build allocates a new Arc.
        clear_source_index_cache();
        let c = get_or_build(&p).unwrap();
        assert!(
            !Arc::ptr_eq(&a, &c),
            "cache clear should force a fresh build"
        );
        clear_source_index_cache();
    }

    #[test]
    fn get_or_build_errors_for_missing_file() {
        assert!(get_or_build(Path::new("/no/such/path/nope.app")).is_err());
    }
}
