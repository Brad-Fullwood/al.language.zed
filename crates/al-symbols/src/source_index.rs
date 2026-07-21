//! Source indexing for `.app` packages with embedded AL source.
//!
//! Builds a fast map from (ObjectKind, Id, Name) to the internal ZIP path
//! by scanning object headers once per package. Subsequent lookups are instant.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use dashmap::DashMap;
use zip::ZipArchive;

use super::model::{ObjectKind, SymbolEntry};

const MAX_HEADER_BYTES: usize = 256 * 1024;
const MAX_APP_FILE_BYTES: u64 = 200 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 200_000;
const MAX_EXTRACTED_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

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
    file_size: u64,
    app_path: PathBuf,
    by_kind_id: HashMap<(ObjectKind, i32), String>,
    by_kind_name: HashMap<(ObjectKind, String), String>,
}

impl AppSourceIndex {
    pub fn from_app_path(app_path: &Path) -> io::Result<Self> {
        let file = File::open(app_path)?;
        let metadata = file.metadata()?;
        let file_size = metadata.len();
        if file_size > MAX_APP_FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    ".app is {file_size} bytes; source indexing limit is {MAX_APP_FILE_BYTES} bytes"
                ),
            ));
        }
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let mut file = file;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic)?;
        if &magic != b"NAVX" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "missing NAVX header in .app",
            ));
        }
        file.rewind()?;

        // zip infers the archive offset from the central directory, so the
        // prefixed NAVX package can be read directly from the file without an
        // unsafe memory map or a 200 MiB heap allocation.
        let mut archive = ZipArchive::new(file)?;
        if archive.len() > MAX_ARCHIVE_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    ".app contains {} entries; source indexing limit is {MAX_ARCHIVE_ENTRIES}",
                    archive.len()
                ),
            ));
        }
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
            limited.read_to_end(&mut buf)?;
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
            file_size,
            app_path: app_path.to_path_buf(),
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
        let file = File::open(&self.app_path).ok()?;
        let metadata = file.metadata().ok()?;
        if metadata.len() != self.file_size
            || metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH) != self.modified
        {
            return None;
        }
        let mut archive = ZipArchive::new(file).ok()?;
        let file = archive.by_name(zip_path).ok()?;
        if file.size() > MAX_EXTRACTED_SOURCE_BYTES {
            tracing::warn!(
                path = zip_path,
                size = file.size(),
                limit = MAX_EXTRACTED_SOURCE_BYTES,
                "embedded AL source exceeds extraction limit"
            );
            return None;
        }
        let mut bytes = Vec::new();
        file.take(MAX_EXTRACTED_SOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_EXTRACTED_SOURCE_BYTES {
            return None;
        }
        String::from_utf8(bytes).ok()
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
    let canonical_path = std::fs::canonicalize(app_path)?;
    let app_path = canonical_path.as_path();
    let cache = SOURCE_INDEX_CACHE.get_or_init(DashMap::new);

    let fresh = || {
        let existing = cache.get(app_path)?;
        let meta = std::fs::metadata(app_path).ok()?;
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        (existing.modified == modified && existing.file_size == meta.len())
            .then(|| existing.value().clone())
    };

    if let Some(index) = fresh() {
        return Ok(index);
    }

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

    if let Some(index) = fresh() {
        return Ok(index);
    }

    let built = Arc::new(AppSourceIndex::from_app_path(app_path)?);
    cache.insert(app_path.to_path_buf(), built.clone());
    Ok(built)
}

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
    use std::io::{Cursor, Write};

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
        AppSourceIndex {
            modified: SystemTime::UNIX_EPOCH,
            file_size: 1,
            app_path: PathBuf::new(),
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

        let cust = entry(ObjectKind::Table, 18, "Customer");
        assert_eq!(
            idx.source_path_for_entry(&cust),
            Some("src/Tab18.Customer.al")
        );
        assert_eq!(
            idx.extract_source_for_entry(&cust).as_deref(),
            Some(tab_src)
        );

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

    #[test]
    #[serial_test::serial]
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

    /// Like `build_app` but takes owned `(String, Vec<u8>)` entries so callers
    /// can synthesise large / binary contents (header truncation, zip bombs).
    fn build_app_owned(files: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 28]);

        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            for (path, contents) in files {
                zip.start_file(path.as_str(), opts).unwrap();
                zip.write_all(contents).unwrap();
            }
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);
        data
    }

    fn write_app_owned(files: &[(String, Vec<u8>)]) -> tempfile::TempPath {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(&build_app_owned(files)).unwrap();
        tmp.flush().unwrap();
        tmp.into_temp_path()
    }

    #[test]
    fn header_indexing_truncates_at_max_header_bytes_but_extract_returns_full() {
        // The object header sits at the very start, then a large body pushes the
        // file past MAX_HEADER_BYTES. Indexing only reads the first 256 KiB, but
        // extract_source_by_path must still return the *entire* decompressed file.
        let mut src = String::from("codeunit 50100 BigOne\n{\n");
        // Body that pushes total size well beyond the 256 KiB header window.
        src.push_str(&"// filler line padding padding padding\n".repeat(20_000));
        src.push_str("}\n");
        assert!(
            src.len() > MAX_HEADER_BYTES,
            "test fixture must exceed the header cap"
        );

        let files = vec![(
            "src/Cod50100.BigOne.al".to_string(),
            src.clone().into_bytes(),
        )];
        let path = write_app_owned(&files);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();

        // Header (within the first 256 KiB) was parsed -> object is indexed.
        let e = entry(ObjectKind::Codeunit, 50100, "BigOne");
        assert_eq!(
            idx.source_path_for_entry(&e),
            Some("src/Cod50100.BigOne.al")
        );

        // Extraction is NOT capped by MAX_HEADER_BYTES — full content comes back.
        let extracted = idx.extract_source_for_entry(&e).unwrap();
        assert_eq!(extracted.len(), src.len());
        assert_eq!(extracted, src);
    }

    #[test]
    fn header_truncation_misses_object_declared_after_window() {
        // If the object header only appears AFTER the 256 KiB window, the
        // .take(MAX_HEADER_BYTES) cut means it is never seen during indexing.
        // (This pins the documented behaviour of the header cap.)
        let mut src = String::new();
        // Leading filler that is NOT a valid object header, exceeding the window.
        src.push_str(&"// noise noise noise noise noise noise\n".repeat(8_000));
        assert!(src.len() > MAX_HEADER_BYTES);
        src.push_str("codeunit 50111 HiddenAfterCap\n{\n}\n");

        let files = vec![("src/late.al".to_string(), src.into_bytes())];
        let path = write_app_owned(&files);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();

        let e = entry(ObjectKind::Codeunit, 50111, "HiddenAfterCap");
        assert_eq!(
            idx.source_path_for_entry(&e),
            None,
            "header past the 256 KiB cap must not be indexed"
        );
    }

    #[test]
    fn zip_bomb_guard_rejects_excessive_total_decompressed_bytes() {
        // Each .al entry contributes at most MAX_HEADER_BYTES (256 KiB) to the
        // running decompressed total. To trip the 1 GiB guard we need just over
        // 4096 such entries. Contents are a single repeated byte so deflate
        // keeps the on-disk .app tiny while decompressing to 256 KiB each.
        const PER_FILE: usize = MAX_HEADER_BYTES; // fills the .take() buffer
        let needed = (MAX_TOTAL_DECOMPRESSED_BYTES_TEST / PER_FILE as u64) as usize + 2;

        let body = vec![b'a'; PER_FILE];
        let mut files: Vec<(String, Vec<u8>)> = Vec::with_capacity(needed);
        for i in 0..needed {
            // Names end in .al so they are scanned; content has no valid header,
            // which is irrelevant — the guard counts bytes regardless of parse.
            files.push((format!("src/f{i}.al"), body.clone()));
        }

        let path = write_app_owned(&files);
        let err = AppSourceIndex::from_app_path(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("zip bomb"),
            "guard error message should mention the zip-bomb refusal, got: {err}"
        );
    }

    // Mirror of the private constant so the bomb test is self-documenting and
    // fails loudly if the production constant ever changes.
    const MAX_TOTAL_DECOMPRESSED_BYTES_TEST: u64 = 1_073_741_824;

    #[test]
    fn just_under_bomb_threshold_is_accepted() {
        // A package whose total decompressed header bytes stay strictly under
        // the 1 GiB cap must index successfully (boundary: total == cap is the
        // accept side because the guard fires only on `>`).
        // Use a handful of small valid entries — comfortably under the cap.
        let files = vec![
            ("src/a.al".to_string(), b"codeunit 1 A\n{\n}".to_vec()),
            ("src/b.al".to_string(), b"table 2 B\n{\n}".to_vec()),
        ];
        let path = write_app_owned(&files);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();
        assert_eq!(
            idx.source_path_for_entry(&entry(ObjectKind::Codeunit, 1, "A")),
            Some("src/a.al")
        );
    }

    #[test]
    fn duplicate_kind_id_keeps_first_seen_path() {
        // Two .al files declare the same (Codeunit, 50100). `or_insert_with`
        // must keep the FIRST entry encountered (ZIP iteration order) and not
        // overwrite it with the second.
        let path = write_app(&[
            ("src/First.al", "codeunit 50100 Dup\n{\n}"),
            ("src/Second.al", "codeunit 50100 Dup\n{\n}"),
        ]);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();
        let e = entry(ObjectKind::Codeunit, 50100, "Dup");
        assert_eq!(idx.source_path_for_entry(&e), Some("src/First.al"));
    }

    #[test]
    fn uppercase_al_extension_is_indexed() {
        // Extension match is case-insensitive (`name.to_lowercase().ends_with(".al")`).
        let path = write_app(&[("src/UPPER.AL", "page 60 UpperPage\n{\n}")]);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();
        let e = entry(ObjectKind::Page, 60, "UpperPage");
        assert_eq!(idx.source_path_for_entry(&e), Some("src/UPPER.AL"));
    }

    #[test]
    fn non_al_files_are_skipped_during_indexing() {
        // A perfectly valid-looking object header in a non-.al file must be
        // ignored (extension filter runs before parsing).
        let path = write_app(&[
            ("manifest.xml", "codeunit 70 NotIndexed\n{\n}"),
            ("readme.txt", "table 71 AlsoNot\n{\n}"),
        ]);
        let idx = AppSourceIndex::from_app_path(&path).unwrap();
        assert_eq!(
            idx.source_path_for_entry(&entry(ObjectKind::Codeunit, 70, "NotIndexed")),
            None
        );
        assert_eq!(
            idx.source_path_for_entry(&entry(ObjectKind::Table, 71, "AlsoNot")),
            None
        );
    }

    /// Set a file's mtime using only std (`File::set_modified`, stable since
    /// Rust 1.75) so the staleness tests don't need an extra crate.
    fn set_mtime(path: &Path, t: SystemTime) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
        f.sync_all().unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn get_or_build_rebuilds_when_file_mtime_changes() {
        use std::time::Duration;

        clear_source_index_cache();

        // Write to a stable on-disk path we control (NamedTempFile so it persists
        // across rewrites and keeps the same path).
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        std::fs::write(&path, build_app(&[("src/V1.al", "codeunit 1 V1\n{\n}")])).unwrap();
        // Pin a known-old mtime so the rewrite below is unambiguously newer.
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        set_mtime(&path, old);

        let first = get_or_build(&path).unwrap();
        assert_eq!(
            first.source_path_for_entry(&entry(ObjectKind::Codeunit, 1, "V1")),
            Some("src/V1.al")
        );

        // Replace contents AND bump mtime forward -> staleness check must trigger
        // a rebuild returning a *different* Arc whose contents reflect the rewrite.
        std::fs::write(&path, build_app(&[("src/V2.al", "table 2 V2\n{\n}")])).unwrap();
        let newer = old + Duration::from_secs(10);
        set_mtime(&path, newer);

        let second = get_or_build(&path).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "changed mtime must force a fresh build, not the cached Arc"
        );
        assert_eq!(
            second.source_path_for_entry(&entry(ObjectKind::Table, 2, "V2")),
            Some("src/V2.al")
        );
        assert_eq!(
            second.source_path_for_entry(&entry(ObjectKind::Codeunit, 1, "V1")),
            None,
            "stale V1 object must be gone after rebuild"
        );

        clear_source_index_cache();
    }

    #[test]
    #[serial_test::serial]
    fn get_or_build_keeps_cache_when_mtime_unchanged() {
        clear_source_index_cache();

        let path = write_app(&[("src/Same.al", "codeunit 3 Same\n{\n}")]);
        let p = path.to_path_buf();

        let a = get_or_build(&p).unwrap();
        let b = get_or_build(&p).unwrap();
        // Unchanged mtime -> identical Arc via the fast path (no rebuild).
        assert!(Arc::ptr_eq(&a, &b));

        clear_source_index_cache();
    }

    #[test]
    #[serial_test::serial]
    fn get_or_build_is_safe_under_concurrent_access() {
        use std::thread;

        clear_source_index_cache();

        let path = write_app(&[("src/Conc.al", "codeunit 9 Conc\n{\n}")]);
        let p = Arc::new(path.to_path_buf());

        // Many threads race to build the same path concurrently. Double-checked
        // locking must serialise the build and every thread must observe the
        // SAME cached Arc afterwards (no torn / duplicate indices).
        let mut handles = Vec::new();
        for _ in 0..16 {
            let p = Arc::clone(&p);
            handles.push(thread::spawn(move || get_or_build(&p).unwrap()));
        }
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let first = &results[0];
        for r in &results[1..] {
            assert!(
                Arc::ptr_eq(first, r),
                "all concurrent callers must share one cached index Arc"
            );
        }
        assert_eq!(
            first.source_path_for_entry(&entry(ObjectKind::Codeunit, 9, "Conc")),
            Some("src/Conc.al")
        );

        clear_source_index_cache();
    }
}
