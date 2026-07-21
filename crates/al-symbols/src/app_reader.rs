//! .app file reader.
//!
//! AL `.app` files have a NAVX header followed by a ZIP archive containing
//! `SymbolReference.json` (public API symbols) and `NavxManifest.xml` (metadata).

use std::io::{Cursor, Read};
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

    let zip_offset = find_zip_offset(data).ok_or(AppReaderError::NoZipSignature)?;

    let zip_data = &data[zip_offset..];

    let cursor = Cursor::new(zip_data);
    let mut archive = ZipArchive::new(cursor)?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(AppReaderError::TooManyEntries(archive.len()));
    }

    let manifest = read_manifest(&mut archive)?;

    let objects = read_symbol_reference(&mut archive, &manifest.name)?;

    Ok(SymbolPackage {
        app_id: manifest.app_id,
        name: manifest.name,
        publisher: manifest.publisher,
        version: manifest.version,
        object_count: objects.len(),
        objects,
    })
}

pub fn read_app_file(path: &std::path::Path) -> Result<SymbolPackage, AppReaderError> {
    let file_size = std::fs::metadata(path)?.len();
    if file_size > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::TooLarge(file_size));
    }
    let data = std::fs::read(path)?;
    read_app_bytes(&data)
}

/// Read only package identity/version metadata without parsing
/// `SymbolReference.json`. Useful for dependency-satisfaction checks where
/// inflating a multi-megabyte symbol payload would be wasted work.
pub fn read_app_manifest_file(path: &std::path::Path) -> Result<NavxManifest, AppReaderError> {
    let file_size = std::fs::metadata(path)?.len();
    if file_size > MAX_APP_FILE_SIZE {
        return Err(AppReaderError::TooLarge(file_size));
    }
    let data = std::fs::read(path)?;
    if data.len() < MIN_HEADER_SIZE {
        return Err(AppReaderError::TooSmall(data.len()));
    }
    if &data[0..4] != NAVX_MAGIC {
        return Err(AppReaderError::NotNavx);
    }
    let zip_offset = find_zip_offset(&data).ok_or(AppReaderError::NoZipSignature)?;
    let mut archive = ZipArchive::new(Cursor::new(&data[zip_offset..]))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(AppReaderError::TooManyEntries(archive.len()));
    }
    read_manifest(&mut archive)
}

pub(crate) fn find_zip_offset(data: &[u8]) -> Option<usize> {
    fn is_valid_zip(data: &[u8], offset: usize) -> bool {
        ZipArchive::new(Cursor::new(&data[offset..])).is_ok()
    }

    // The standard NAVX header is 40 bytes. Check there first (common case O(1)).
    const STANDARD_HEADER: usize = 40;
    if data.len() > STANDARD_HEADER + 3
        && &data[STANDARD_HEADER..STANDARD_HEADER + 4] == ZIP_MAGIC
        && is_valid_zip(data, STANDARD_HEADER)
    {
        return Some(STANDARD_HEADER);
    }
    // NAVX headers are tiny (40 bytes in current packages). A bounded fallback
    // supports historical/variable headers without scanning an entire 200 MB
    // package or accepting a coincidental PK signature inside header data.
    const MAX_NAVX_HEADER_BYTES: usize = 1024 * 1024;
    let search_end = data.len().min(MAX_NAVX_HEADER_BYTES).saturating_sub(3);
    for i in MIN_HEADER_SIZE..search_end {
        if &data[i..i + 4] == ZIP_MAGIC && is_valid_zip(data, i) {
            return Some(i);
        }
    }
    None
}

fn read_manifest(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<NavxManifest, AppReaderError> {
    let manifest_name =
        find_file_in_archive(archive, "NavxManifest.xml").ok_or(AppReaderError::NoManifest)?;

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
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    package_name: &str,
) -> Result<Vec<super::model::SymbolEntry>, AppReaderError> {
    let sr_name = find_file_in_archive(archive, "SymbolReference.json")
        .ok_or(AppReaderError::NoSymbolReference)?;

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

/// Find a file in the archive by name (case-insensitive, ignoring path prefixes).
/// Maximum number of entries we will inspect when locating a single file
/// inside a `.app` ZIP archive (T-sec-008). Real BC packages have well
/// under 100k entries; refusing to walk a many-million-entry archive
/// caps zip-bomb amplification — a malicious .app could otherwise force
/// `archive.by_index(i)` calls in a tight loop until they exceed the
/// 200 MB outer cap on file *size* (which says nothing about entry
/// count). Higher than realistic BC packages by ~10x.
fn find_file_in_archive(archive: &mut ZipArchive<Cursor<&[u8]>>, target: &str) -> Option<String> {
    let target_lower = target.to_lowercase();
    let entries = archive.len();
    if entries > MAX_ARCHIVE_ENTRIES {
        tracing::warn!(
            entries,
            limit = MAX_ARCHIVE_ENTRIES,
            target = target,
            ".app archive entry count exceeds safety cap — aborting search"
        );
        return None;
    }
    for i in 0..entries {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name().to_string();
            let filename = name.rsplit('/').next().unwrap_or(&name);
            if filename.to_lowercase() == target_lower {
                return Some(name);
            }
        }
    }
    None
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

    #[test]
    fn reject_navx_without_zip() {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&[0u8; 100]);
        let err = read_app_bytes(&data).unwrap_err();
        assert!(matches!(err, AppReaderError::NoZipSignature));
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
