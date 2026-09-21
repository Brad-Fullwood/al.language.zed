//! .app file reader.
//!
//! AL `.app` files have a NAVX header followed by a ZIP archive containing
//! `SymbolReference.json` (public API symbols) and `NavxManifest.xml` (metadata).

use std::io::{Cursor, Read, Seek};
use thiserror::Error;
use zip::ZipArchive;

use super::manifest::{self, NavxManifest};
use super::model::{SymbolPackage, SymbolReferenceJson};

const NAVX_MAGIC: &[u8; 4] = b"NAVX";

/// ZIP local file header magic (PK\x03\x04).
const ZIP_MAGIC: &[u8; 4] = &[0x50, 0x4B, 0x03, 0x04];

const MIN_HEADER_SIZE: usize = 4;

/// Maximum .app file size accepted (200 MB). Files larger than this are rejected
/// before reading to prevent excessive memory use or decompression bombs.
pub(crate) const MAX_APP_FILE_SIZE: u64 = 200 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_ARCHIVE_ENTRIES: usize = 200_000;

#[derive(Debug, Error)]
pub enum AppReaderError {
    #[error("Not a valid .app file: missing NAVX magic")]
    NotNavx,
    #[error("File too small ({0} bytes)")]
    TooSmall(usize),
    #[error(".app file too large ({0} bytes, limit is 200 MB)")]
    TooLarge(u64),
    #[error("Archive entry {name} is too large ({size} bytes, limit is {limit} bytes)")]
    EntryTooLarge { name: String, size: u64, limit: u64 },
    #[error("Archive contains too many entries ({0}, limit is 200000)")]
    TooManyEntries(usize),
    #[error("Archive expands to too much data ({size} bytes, limit is {limit} bytes)")]
    ArchiveExpandedTooLarge { size: u64, limit: u64 },
    #[error("ZIP signature not found after NAVX header")]
    NoZipSignature,
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("SymbolReference.json not found in archive")]
    NoSymbolReference,
    #[error("NavxManifest.xml not found in archive")]
    NoManifest,
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Manifest parse error: {0}")]
    Manifest(#[from] manifest::ManifestError),
}

pub fn read_app_bytes(data: &[u8]) -> Result<SymbolPackage, AppReaderError> {
    if data.len() as u64 > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::TooLarge(data.len() as u64));
    }
    if data.len() < MIN_HEADER_SIZE {
        return Err(AppReaderError::TooSmall(data.len()));
    }

    if &data[0..4] != NAVX_MAGIC {
        return Err(AppReaderError::NotNavx);
    }

    reject_oversized_directory(read_zip_trailer(data).as_ref())?;
    // The trailer says where the archive starts. When it does not add up, hand
    // the whole buffer to the zip reader, which infers a prefix the same way
    // `read_app_file` relies on, so both paths accept the same packages.
    let zip_offset = find_zip_offset(data).unwrap_or(0);

    let cursor = Cursor::new(&data[zip_offset..]);
    read_archive(ZipArchive::new(cursor)?)
}

/// Refuse a central directory larger than the entry limit before anything
/// parses it. `read_archive` re-checks the count the reader actually found.
fn reject_oversized_directory(trailer: Option<&ZipTrailer>) -> Result<(), AppReaderError> {
    match trailer {
        Some(trailer) if trailer.entries > MAX_ARCHIVE_ENTRIES as u64 => Err(
            AppReaderError::TooManyEntries(trailer.entries.min(usize::MAX as u64) as usize),
        ),
        _ => Ok(()),
    }
}

/// Read the tail of `file` and parse the archive trailer from it.
///
/// The end-of-central-directory record is within 64 KiB of the end, so this
/// costs one seek and one small read rather than a full parse.
fn read_trailer_from_file(file: &mut std::fs::File, size: u64) -> Option<ZipTrailer> {
    let tail_len = size.min((EOCD_MIN_LEN + MAX_EOCD_COMMENT + ZIP64_EOCD_MIN_LEN) as u64);
    file.seek(std::io::SeekFrom::End(-(tail_len as i64))).ok()?;
    let mut tail = vec![0u8; tail_len as usize];
    file.read_exact(&mut tail).ok()?;
    let trailer = read_zip_trailer(&tail);
    file.rewind().ok()?;
    trailer
}

pub fn read_app_file(path: &std::path::Path) -> Result<SymbolPackage, AppReaderError> {
    let file_size = std::fs::metadata(path)?.len();
    if file_size > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::TooLarge(file_size));
    }
    if file_size < MIN_HEADER_SIZE as u64 {
        return Err(AppReaderError::TooSmall(file_size as usize));
    }
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0u8; MIN_HEADER_SIZE];
    file.read_exact(&mut magic)?;
    if &magic != NAVX_MAGIC {
        return Err(AppReaderError::NotNavx);
    }
    file.rewind()?;
    reject_oversized_directory(read_trailer_from_file(&mut file, file_size).as_ref())?;
    read_archive(ZipArchive::new(file)?)
}

/// Parse a standalone `SymbolReference.json` payload into the same normalized
/// symbols returned by [`read_app_file`].
///
/// This is used when a caller has generated the public surface in memory and
/// needs to compare it with a packaged baseline without writing a temporary
/// `.app`. The same size, BOM, padding, and JSON validation applies.
pub fn read_symbol_reference_bytes(
    json_bytes: &[u8],
    package_name: &str,
) -> Result<Vec<super::model::SymbolEntry>, AppReaderError> {
    if json_bytes.len() as u64 > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::TooLarge(json_bytes.len() as u64));
    }
    Ok(parse_symbol_reference_json(json_bytes)?.into_entries(package_name))
}

fn read_archive<R: Read + Seek>(
    mut archive: ZipArchive<R>,
) -> Result<SymbolPackage, AppReaderError> {
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(AppReaderError::TooManyEntries(archive.len()));
    }
    // Both wanted entries are resolved in the same pass over the name table.
    let mut names = find_files_in_archive(&archive, &["NavxManifest.xml", "SymbolReference.json"]);
    let symbol_reference_name = names.pop().flatten();
    let manifest_name = names.pop().flatten().ok_or(AppReaderError::NoManifest)?;
    let symbol_reference_name = symbol_reference_name.ok_or(AppReaderError::NoSymbolReference)?;
    let manifest = read_named_manifest(&mut archive, manifest_name)?;
    let objects = read_symbol_reference(&mut archive, symbol_reference_name, &manifest.name)?;
    Ok(SymbolPackage {
        app_id: manifest.app_id,
        name: manifest.name,
        publisher: manifest.publisher,
        version: manifest.version,
        object_count: objects.len(),
        objects,
    })
}

/// Read only package identity/version metadata without parsing
/// `SymbolReference.json`. Useful for dependency-satisfaction checks where
/// inflating a multi-megabyte symbol payload would be wasted work.
pub fn read_app_manifest_file(path: &std::path::Path) -> Result<NavxManifest, AppReaderError> {
    let file_size = std::fs::metadata(path)?.len();
    if file_size > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::TooLarge(file_size));
    }
    if file_size < MIN_HEADER_SIZE as u64 {
        return Err(AppReaderError::TooSmall(file_size as usize));
    }
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0u8; MIN_HEADER_SIZE];
    file.read_exact(&mut magic)?;
    if &magic != NAVX_MAGIC {
        return Err(AppReaderError::NotNavx);
    }
    file.rewind()?;
    reject_oversized_directory(read_trailer_from_file(&mut file, file_size).as_ref())?;
    // zip supports self-extracting/prefixed archives and infers the NAVX
    // prefix from the central directory, so dependency checks can read only
    // the small manifest entry instead of allocating the entire .app.
    let mut archive = ZipArchive::new(file)?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(AppReaderError::TooManyEntries(archive.len()));
    }
    read_manifest(&mut archive)
}

/// End of central directory record: 22 bytes plus a comment of up to 64 KiB.
const EOCD_SIGNATURE: &[u8; 4] = &[0x50, 0x4B, 0x05, 0x06];
const EOCD_MIN_LEN: usize = 22;
const MAX_EOCD_COMMENT: usize = u16::MAX as usize;
const ZIP64_EOCD_SIGNATURE: &[u8; 4] = &[0x50, 0x4B, 0x06, 0x06];
const ZIP64_EOCD_MIN_LEN: usize = 56;
const CENTRAL_DIRECTORY_SIGNATURE: &[u8; 4] = &[0x50, 0x4B, 0x01, 0x02];

/// What the archive trailer says about the central directory.
struct ZipTrailer {
    /// Offset of the end-of-central-directory record in `data`.
    eocd_pos: usize,
    entries: u64,
    cd_size: u64,
    /// Offset of the central directory *within the archive*, so relative to
    /// the start of any prefix such as the NAVX header.
    cd_offset: u64,
}

fn read_u16(data: &[u8], at: usize) -> u64 {
    u16::from_le_bytes([data[at], data[at + 1]]) as u64
}

fn read_u32(data: &[u8], at: usize) -> u64 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as u64
}

fn read_u64(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().expect("8 bytes"))
}

/// Read the trailer of the last archive in `data`.
///
/// Every candidate prefix shares one trailer: the zip reader scans back from
/// the end of the buffer, which does not move when the prefix does. Reading it
/// once is what makes locating the archive O(1) instead of one full
/// central-directory parse per candidate.
fn read_zip_trailer(data: &[u8]) -> Option<ZipTrailer> {
    let scan_floor = data.len().saturating_sub(EOCD_MIN_LEN + MAX_EOCD_COMMENT);
    let mut pos = data.len().checked_sub(EOCD_MIN_LEN)?;
    let eocd_pos = loop {
        if &data[pos..pos + 4] == EOCD_SIGNATURE {
            break pos;
        }
        if pos == scan_floor {
            return None;
        }
        pos -= 1;
    };

    let entries = read_u16(data, eocd_pos + 10);
    let cd_size = read_u32(data, eocd_pos + 12);
    let cd_offset = read_u32(data, eocd_pos + 16);
    let is_zip64 =
        entries == u16::MAX as u64 || cd_size == u32::MAX as u64 || cd_offset == u32::MAX as u64;
    if !is_zip64 {
        return Some(ZipTrailer {
            eocd_pos,
            entries,
            cd_size,
            cd_offset,
        });
    }

    // Zip64: the real counts live in a record that sits just before the
    // locator, itself just before the EOCD. Both are near the end, so a short
    // backward scan finds the record without a full pass.
    let zip64_floor = eocd_pos.saturating_sub(MAX_EOCD_COMMENT + ZIP64_EOCD_MIN_LEN);
    let mut pos = eocd_pos.checked_sub(ZIP64_EOCD_MIN_LEN)?;
    loop {
        if &data[pos..pos + 4] == ZIP64_EOCD_SIGNATURE {
            return Some(ZipTrailer {
                eocd_pos,
                entries: read_u64(data, pos + 32),
                cd_size: read_u64(data, pos + 40),
                cd_offset: read_u64(data, pos + 48),
            });
        }
        if pos == zip64_floor {
            return None;
        }
        pos -= 1;
    }
}

fn is_valid_zip(data: &[u8], offset: usize) -> bool {
    ZipArchive::new(Cursor::new(&data[offset..])).is_ok()
}

pub(crate) fn find_zip_offset(data: &[u8]) -> Option<usize> {
    let trailer = read_zip_trailer(data)?;
    // Reject an oversized directory here rather than after parsing it: the
    // parse is the expensive part, and `read_archive` would reject it anyway.
    if trailer.entries > MAX_ARCHIVE_ENTRIES as u64 {
        tracing::warn!(
            entries = trailer.entries,
            "refusing .app whose central directory claims more entries than the limit"
        );
        return None;
    }

    // The central directory ends where the EOCD begins, and starts `cd_offset`
    // bytes into the archive, so the prefix length follows by arithmetic. No
    // scanning, and at most one validation.
    let cd_start = (trailer.eocd_pos as u64).checked_sub(trailer.cd_size);
    if let Some(prefix) = cd_start
        .and_then(|cd_start| cd_start.checked_sub(trailer.cd_offset))
        .and_then(|prefix| usize::try_from(prefix).ok())
        .filter(|prefix| *prefix < data.len())
    {
        let cd_pos = prefix as u64 + trailer.cd_offset;
        let directory_is_there = usize::try_from(cd_pos)
            .ok()
            .and_then(|cd_pos| data.get(cd_pos..cd_pos + 4))
            .is_some_and(|bytes| bytes == CENTRAL_DIRECTORY_SIGNATURE);
        if (directory_is_there || trailer.entries == 0) && is_valid_zip(data, prefix) {
            return Some(prefix);
        }
    }

    // Archives whose directory offsets already count the prefix land here.
    // Their payload starts at the first local header, so the standard 40-byte
    // NAVX header and then a short bounded scan cover them.
    const STANDARD_HEADER: usize = 40;
    const MAX_NAVX_HEADER_BYTES: usize = 1024 * 1024;
    const MAX_ZIP_VALIDATION_ATTEMPTS: usize = 8;
    if data.len() > STANDARD_HEADER + 3
        && &data[STANDARD_HEADER..STANDARD_HEADER + 4] == ZIP_MAGIC
        && is_valid_zip(data, STANDARD_HEADER)
    {
        return Some(STANDARD_HEADER);
    }
    let search_end = data.len().min(MAX_NAVX_HEADER_BYTES).saturating_sub(3);
    let mut attempts = 0usize;
    for i in MIN_HEADER_SIZE..search_end {
        if &data[i..i + 4] == ZIP_MAGIC {
            if is_valid_zip(data, i) {
                return Some(i);
            }
            attempts += 1;
            if attempts >= MAX_ZIP_VALIDATION_ATTEMPTS {
                tracing::warn!(
                    attempts,
                    "giving up on .app ZIP-offset probing after too many false PK signatures"
                );
                return None;
            }
        }
    }
    None
}

fn read_manifest<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<NavxManifest, AppReaderError> {
    let manifest_name =
        find_file_in_archive(archive, "NavxManifest.xml").ok_or(AppReaderError::NoManifest)?;
    read_named_manifest(archive, manifest_name)
}

fn read_named_manifest<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest_name: String,
) -> Result<NavxManifest, AppReaderError> {
    let file = archive.by_name(&manifest_name)?;
    if file.size() > MAX_MANIFEST_BYTES {
        return Err(AppReaderError::EntryTooLarge {
            name: manifest_name,
            size: file.size(),
            limit: MAX_MANIFEST_BYTES,
        });
    }
    let mut xml_bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut xml_bytes)?;
    if xml_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(AppReaderError::EntryTooLarge {
            name: manifest_name,
            size: xml_bytes.len() as u64,
            limit: MAX_MANIFEST_BYTES,
        });
    }

    // BC's `.app` toolchain inconsistently emits a UTF-8 BOM in the
    // manifest XML (consistent only in SymbolReference.json). When present,
    // the BOM appears as bytes before `<?xml ?>` and quick-xml rejects the
    // file with a "characters before XML declaration" error. Strip it so
    // Both BC versions must parse identically.
    let xml_slice = strip_utf8_bom(&xml_bytes);

    Ok(manifest::parse_manifest(xml_slice)?)
}

fn read_symbol_reference(
    archive: &mut ZipArchive<impl Read + Seek>,
    sr_name: String,
    package_name: &str,
) -> Result<Vec<super::model::SymbolEntry>, AppReaderError> {
    let file = archive.by_name(&sr_name)?;
    if file.size() > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::EntryTooLarge {
            name: sr_name,
            size: file.size(),
            limit: MAX_APP_FILE_SIZE,
        });
    }
    let mut json_bytes = Vec::new();
    // Cap matches the outer .app file limit; protects against decompression
    // bombs even when callers feed read_app_bytes directly with un-capped input.
    file.take(MAX_APP_FILE_SIZE + 1)
        .read_to_end(&mut json_bytes)?;
    if json_bytes.len() as u64 > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::EntryTooLarge {
            name: sr_name,
            size: json_bytes.len() as u64,
            limit: MAX_APP_FILE_SIZE,
        });
    }

    let sr = parse_symbol_reference_json(&json_bytes)?;
    Ok(sr.into_entries(package_name))
}

fn parse_symbol_reference_json(json_bytes: &[u8]) -> Result<SymbolReferenceJson, AppReaderError> {
    let json_slice = strip_utf8_bom(json_bytes);
    let mut values =
        serde_json::Deserializer::from_slice(json_slice).into_iter::<SymbolReferenceJson>();

    match values.next().transpose()? {
        Some(sr) if has_only_json_padding(&json_slice[values.byte_offset()..]) => Ok(sr),
        _ => Ok(serde_json::from_slice(json_slice)?),
    }
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    }
}

fn has_only_json_padding(bytes: &[u8]) -> bool {
    // Real BC packages may leave NUL padding or a DOS EOF marker after the JSON payload.
    bytes
        .iter()
        .all(|byte| byte.is_ascii_whitespace() || matches!(byte, 0x00 | 0x1A))
}

/// Resolve archive entries by file name, case-insensitively and ignoring
/// directory prefixes, in one pass over the name table.
///
/// `by_index` seeks to and parses an entry's local file header, so resolving a
/// name that way costs a seek per entry; a cloud-targeted Base Application
/// carries one entry per source file. `file_names` reads the central directory
/// already in memory.
///
/// The entry cap (T-sec-008) is a zip-bomb guard: real BC packages stay well
/// under 100k entries, and the 200 MB limit on file size says nothing about
/// entry count.
fn find_files_in_archive<R: Read + Seek>(
    archive: &ZipArchive<R>,
    targets: &[&str],
) -> Vec<Option<String>> {
    let entries = archive.len();
    if entries > MAX_ARCHIVE_ENTRIES {
        tracing::warn!(
            entries,
            limit = MAX_ARCHIVE_ENTRIES,
            ".app archive entry count exceeds safety cap — aborting search"
        );
        return vec![None; targets.len()];
    }
    let wanted: Vec<String> = targets.iter().map(|target| target.to_lowercase()).collect();
    let mut found: Vec<Option<String>> = vec![None; targets.len()];
    for name in archive.file_names() {
        let filename = name.rsplit('/').next().unwrap_or(name).to_lowercase();
        for (slot, want) in found.iter_mut().zip(&wanted) {
            if slot.is_none() && filename == *want {
                *slot = Some(name.to_string());
            }
        }
        if found.iter().all(Option::is_some) {
            break;
        }
    }
    found
}

fn find_file_in_archive<R: Read + Seek>(archive: &ZipArchive<R>, target: &str) -> Option<String> {
    find_files_in_archive(archive, &[target])
        .pop()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn make_test_app(manifest_xml: &str, symbol_json: &str) -> Vec<u8> {
        let mut data = Vec::new();

        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]);

        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(manifest_xml.as_bytes()).unwrap();

            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(symbol_json.as_bytes()).unwrap();

            zip.finish().unwrap();
        }

        data.extend_from_slice(&zip_buf);
        data
    }

    fn test_manifest() -> String {
        r#"<?xml version="1.0" encoding="utf-8"?>
<Package>
  <App Id="test-app-id-1234"
       Name="Test App"
       Publisher="Test Publisher"
       Version="1.0.0.0" />
</Package>"#
            .to_string()
    }

    fn test_symbols() -> String {
        r#"{
    "Tables": [
        {
            "Id": 50100,
            "Name": "Test Table",
            "Fields": [
                { "Id": 1, "Name": "Entry No.", "TypeDefinition": { "Name": "Integer" } }
            ],
            "Methods": []
        }
    ],
    "Codeunits": [
        {
            "Id": 50101,
            "Name": "Test Codeunit",
            "Methods": [
                {
                    "Name": "Run",
                    "Parameters": [],
                    "Attributes": [],
                    "IsLocal": false
                }
            ]
        }
    ]
}"#
        .to_string()
    }

    #[test]
    fn read_valid_app() {
        let data = make_test_app(&test_manifest(), &test_symbols());
        let pkg = read_app_bytes(&data).unwrap();

        assert_eq!(pkg.app_id, "test-app-id-1234");
        assert_eq!(pkg.name, "Test App");
        assert_eq!(pkg.publisher, "Test Publisher");
        assert_eq!(pkg.version, "1.0.0.0");
        assert_eq!(pkg.objects.len(), 2); // 1 table + 1 codeunit
    }

    #[test]
    fn manifest_only_read_does_not_parse_symbol_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ManifestOnly.app");
        std::fs::write(&path, make_test_app(&test_manifest(), "not-json")).unwrap();

        let manifest = read_app_manifest_file(&path).expect("manifest should still be readable");
        assert_eq!(manifest.app_id, "test-app-id-1234");
        assert_eq!(manifest.version, "1.0.0.0");
        assert!(matches!(read_app_file(&path), Err(AppReaderError::Json(_))));
    }

    #[test]
    fn detect_navx_magic() {
        let data = make_test_app(&test_manifest(), &test_symbols());
        assert_eq!(&data[0..4], b"NAVX");
    }

    #[test]
    fn reject_non_navx() {
        let data = b"NOT_NAVX_FILE_CONTENT";
        let err = read_app_bytes(data).unwrap_err();
        assert!(matches!(err, AppReaderError::NotNavx));
    }

    #[test]
    fn reject_too_small() {
        let data = b"NA";
        let err = read_app_bytes(data).unwrap_err();
        assert!(matches!(err, AppReaderError::TooSmall(2)));
    }

    fn make_app_with_entries(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]);

        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (name, contents) in entries {
                zip.start_file(*name, options).unwrap();
                zip.write_all(contents.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);
        data
    }

    #[test]
    fn missing_navx_manifest() {
        let data = make_app_with_entries(&[("SymbolReference.json", &test_symbols())]);
        let err = read_app_bytes(&data).unwrap_err();
        assert!(matches!(err, AppReaderError::NoManifest), "got {err:?}");
    }

    #[test]
    fn missing_symbol_reference() {
        let data = make_app_with_entries(&[("NavxManifest.xml", &test_manifest())]);
        let err = read_app_bytes(&data).unwrap_err();
        assert!(
            matches!(err, AppReaderError::NoSymbolReference),
            "got {err:?}"
        );
    }

    /// A NAVX file with no archive in it is refused, and the bytes and file
    /// paths refuse it the same way: they now locate the payload by the same
    /// rule instead of one scanning for `PK\x03\x04` and the other trusting
    /// the zip reader.
    #[test]
    fn reject_navx_without_zip() {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&[0u8; 100]);

        let from_bytes = read_app_bytes(&data).unwrap_err();
        assert!(
            matches!(from_bytes, AppReaderError::Zip(_)),
            "got {from_bytes:?}"
        );

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&data).unwrap();
        file.flush().unwrap();
        let from_file = read_app_file(file.path()).unwrap_err();
        assert_eq!(
            std::mem::discriminant(&from_bytes),
            std::mem::discriminant(&from_file),
            "the two paths must agree: {from_bytes:?} vs {from_file:?}"
        );
    }

    #[test]
    fn variable_header_size() {
        let manifest = test_manifest();
        let symbols = test_symbols();

        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&[0u8; 46]);

        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(symbols.as_bytes()).unwrap();
            zip.finish().unwrap();
        }

        data.extend_from_slice(&zip_buf);

        let pkg = read_app_bytes(&data).unwrap();
        assert_eq!(pkg.name, "Test App");
    }

    /// The archive is located from the trailer, so planted `PK\x03\x04`
    /// signatures cost nothing: neither a validation attempt each nor, past
    /// the old attempt cap, a refusal to read a valid package.
    #[test]
    fn planted_signatures_before_the_archive_cost_no_validation() {
        let valid = make_test_app(&test_manifest(), &test_symbols());
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        for _ in 0..500 {
            data.extend_from_slice(b"PK\x03\x04junk");
        }
        // `make_test_app` writes its own 40-byte NAVX header before the zip.
        let offset = data.len() + 40;
        data.extend_from_slice(&valid);

        assert_eq!(find_zip_offset(&data), Some(offset));
        assert_eq!(read_app_bytes(&data).unwrap().name, "Test App");
    }

    /// A central directory bigger than the entry limit is refused from the
    /// trailer, before anything parses it.
    #[test]
    fn an_oversized_central_directory_is_refused_without_parsing() {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&[0u8; 36]);
        data.extend_from_slice(b"PK\x03\x04");
        data.extend_from_slice(&[0u8; 64]);
        // An EOCD claiming three million entries.
        data.extend_from_slice(EOCD_SIGNATURE);
        data.extend_from_slice(&[0u8; 6]);
        data.extend_from_slice(&u16::MAX.to_le_bytes());
        data.extend_from_slice(&64u32.to_le_bytes());
        data.extend_from_slice(&40u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 2]);
        // The zip64 record the sentinel points at.
        let mut zip64 = Vec::new();
        zip64.extend_from_slice(ZIP64_EOCD_SIGNATURE);
        zip64.extend_from_slice(&44u64.to_le_bytes());
        zip64.extend_from_slice(&[0u8; 20]);
        zip64.extend_from_slice(&3_000_000u64.to_le_bytes());
        zip64.extend_from_slice(&3_000_000u64.to_le_bytes());
        zip64.extend_from_slice(&64u64.to_le_bytes());
        zip64.extend_from_slice(&40u64.to_le_bytes());
        let eocd_at = data.len() - EOCD_MIN_LEN;
        data.splice(eocd_at..eocd_at, zip64);

        assert!(find_zip_offset(&data).is_none());
    }

    /// A package built from many source files carries one archive entry per
    /// file, with the wanted entries at either end of the name table.
    #[test]
    fn both_wanted_entries_are_found_in_a_thirty_thousand_entry_archive() {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]);

        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(test_manifest().as_bytes()).unwrap();
            for i in 0..30_000 {
                zip.start_file(format!("src/Object{i}.al"), options)
                    .unwrap();
                zip.write_all(b"codeunit 1 X { }").unwrap();
            }
            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(test_symbols().as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);

        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&data).unwrap();
        file.flush().unwrap();

        let archive =
            ZipArchive::new(std::fs::File::open(file.path()).unwrap()).expect("archive opens");
        assert_eq!(archive.len(), 30_002);
        let names = find_files_in_archive(&archive, &["NavxManifest.xml", "SymbolReference.json"]);
        assert_eq!(
            names,
            vec![
                Some("NavxManifest.xml".to_string()),
                Some("SymbolReference.json".to_string())
            ]
        );
        drop(archive);

        let package = read_app_file(file.path()).unwrap();
        assert_eq!(package.name, "Test App");
    }

    #[test]
    fn zip_offset_probing_is_bounded_against_planted_signatures() {
        // A hostile file packed with false PK signatures must be rejected
        // after a bounded number of expensive ZIP-validation attempts instead
        // of forcing an EOCD scan at every one of them.
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        for _ in 0..1000 {
            data.extend_from_slice(b"PK\x03\x04junk");
        }
        let started = std::time::Instant::now();
        assert!(find_zip_offset(&data).is_none());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "bounded probing must reject quickly"
        );
    }

    #[test]
    fn false_zip_signature_inside_navx_header_is_ignored() {
        let valid = make_test_app(&test_manifest(), &test_symbols());
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(b"junkPK\x03\x04not-a-zip");
        data.resize(64, 0);
        data.extend_from_slice(&valid[40..]);

        let pkg = read_app_bytes(&data).expect("reader must continue past false PK signature");
        assert_eq!(pkg.name, "Test App");
    }

    #[test]
    fn oversized_manifest_is_rejected_before_parsing() {
        let oversized = " ".repeat((MAX_MANIFEST_BYTES + 1) as usize);
        let data = make_test_app(&oversized, &test_symbols());

        let err = read_app_bytes(&data).expect_err("oversized manifest must be refused");
        assert!(
            matches!(err, AppReaderError::EntryTooLarge { ref name, .. } if name == "NavxManifest.xml"),
            "got {err:?}"
        );
    }

    #[test]
    fn read_symbol_reference_with_bom_and_trailing_padding() {
        let mut symbols = String::from("\u{feff}");
        symbols.push_str(&test_symbols());
        symbols.push('\0');
        symbols.push('\0');
        symbols.push('\u{001a}');
        symbols.push('\n');

        let data = make_test_app(&test_manifest(), &symbols);
        let pkg = read_app_bytes(&data).unwrap();

        assert_eq!(pkg.name, "Test App");
        assert_eq!(pkg.objects.len(), 2);
    }

    #[test]
    fn reject_symbol_reference_with_non_padding_trailing_bytes() {
        let mut symbols = test_symbols();
        symbols.push_str("oops");

        let data = make_test_app(&test_manifest(), &symbols);
        let err = read_app_bytes(&data).unwrap_err();

        assert!(matches!(err, AppReaderError::Json(_)));
    }

    #[test]
    fn manifest_with_utf8_bom_is_parsed() {
        // BC's .app toolchain
        // sometimes emits a UTF-8 BOM in NavxManifest.xml (consistent only
        // in SymbolReference.json). Without stripping, quick-xml rejects
        // the file. Verify both BOM-prefixed and BOM-less manifests parse
        // to the same metadata.
        let mut bommed = vec![0xEF, 0xBB, 0xBF];
        bommed.extend_from_slice(test_manifest().as_bytes());
        let bommed_xml = String::from_utf8(bommed).unwrap();

        let data = make_test_app(&bommed_xml, &test_symbols());
        let pkg = read_app_bytes(&data).expect("BOM-prefixed manifest must parse");
        assert_eq!(pkg.name, "Test App");
        assert_eq!(pkg.publisher, "Test Publisher");
    }
}
