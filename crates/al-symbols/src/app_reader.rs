//! .app file reader.
//!
//! AL `.app` files have a NAVX header followed by a ZIP archive containing
//! `SymbolReference.json` (public API symbols) and `NavxManifest.xml` (metadata).

use std::io::{Cursor, Read};
use thiserror::Error;
use zip::ZipArchive;

use crate::manifest::{self, NavxManifest};
use crate::model::{SymbolPackage, SymbolReferenceJson};

/// NAVX magic bytes.
const NAVX_MAGIC: &[u8; 4] = b"NAVX";

/// ZIP local file header magic (PK\x03\x04).
const ZIP_MAGIC: &[u8; 4] = &[0x50, 0x4B, 0x03, 0x04];

/// Minimum header size before scanning for ZIP.
const MIN_HEADER_SIZE: usize = 4;

#[derive(Debug, Error)]
pub enum AppReaderError {
    #[error("Not a valid .app file: missing NAVX magic")]
    NotNavx,
    #[error("File too small ({0} bytes)")]
    TooSmall(usize),
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

/// Read and parse a `.app` file from raw bytes.
///
/// Steps:
/// 1. Verify NAVX magic bytes at offset 0
/// 2. Scan for ZIP PK signature (handles variable header sizes)
/// 3. Open ZIP archive
/// 4. Parse NavxManifest.xml for package metadata
/// 5. Parse SymbolReference.json for symbols
/// 6. Return SymbolPackage
pub fn read_app_bytes(data: &[u8]) -> Result<SymbolPackage, AppReaderError> {
    if data.len() < MIN_HEADER_SIZE {
        return Err(AppReaderError::TooSmall(data.len()));
    }

    // 1. Verify NAVX magic
    if &data[0..4] != NAVX_MAGIC {
        return Err(AppReaderError::NotNavx);
    }

    // 2. Scan for ZIP PK signature after magic bytes
    let zip_offset = find_zip_offset(data)
        .ok_or(AppReaderError::NoZipSignature)?;

    let zip_data = &data[zip_offset..];

    // 3. Open ZIP
    let cursor = Cursor::new(zip_data);
    let mut archive = ZipArchive::new(cursor)?;

    // 4. Parse NavxManifest.xml
    let manifest = read_manifest(&mut archive)?;

    // 5. Parse SymbolReference.json
    let objects = read_symbol_reference(&mut archive, &manifest.name)?;

    // 6. Build SymbolPackage
    Ok(SymbolPackage {
        app_id: manifest.app_id,
        name: manifest.name,
        publisher: manifest.publisher,
        version: manifest.version,
        objects,
    })
}

/// Read and parse a `.app` file from a file path.
pub fn read_app_file(path: &std::path::Path) -> Result<SymbolPackage, AppReaderError> {
    let data = std::fs::read(path)?;
    read_app_bytes(&data)
}

/// Scan for the ZIP PK\x03\x04 signature starting from byte 4.
fn find_zip_offset(data: &[u8]) -> Option<usize> {
    // The standard NAVX header is 40 bytes. Check there first (common case O(1)).
    const STANDARD_HEADER: usize = 40;
    if data.len() > STANDARD_HEADER + 3 && &data[STANDARD_HEADER..STANDARD_HEADER + 4] == ZIP_MAGIC {
        return Some(STANDARD_HEADER);
    }
    // Fall back to scanning from byte 4 for non-standard headers.
    for i in MIN_HEADER_SIZE..data.len().saturating_sub(3) {
        if &data[i..i + 4] == ZIP_MAGIC {
            return Some(i);
        }
    }
    None
}

/// Extract and parse NavxManifest.xml from the ZIP archive.
fn read_manifest(archive: &mut ZipArchive<Cursor<&[u8]>>) -> Result<NavxManifest, AppReaderError> {
    let manifest_name = find_file_in_archive(archive, "NavxManifest.xml")
        .ok_or(AppReaderError::NoManifest)?;

    let mut file = archive.by_name(&manifest_name)?;
    let mut xml_bytes = Vec::new();
    file.read_to_end(&mut xml_bytes)?;

    Ok(manifest::parse_manifest(&xml_bytes)?)
}

/// Extract and parse SymbolReference.json from the ZIP archive.
fn read_symbol_reference(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    package_name: &str,
) -> Result<Vec<crate::model::SymbolEntry>, AppReaderError> {
    let sr_name = find_file_in_archive(archive, "SymbolReference.json")
        .ok_or(AppReaderError::NoSymbolReference)?;

    let mut file = archive.by_name(&sr_name)?;
    let mut json_bytes = Vec::new();
    file.read_to_end(&mut json_bytes)?;

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
fn find_file_in_archive(
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    target: &str,
) -> Option<String> {
    let target_lower = target.to_lowercase();
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name().to_string();
            // Match the filename part (after last /)
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

    /// Create a synthetic .app file for testing.
    fn make_test_app(
        manifest_xml: &str,
        symbol_json: &str,
    ) -> Vec<u8> {
        let mut data = Vec::new();

        // NAVX header (40 bytes)
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes()); // version
        data.extend_from_slice(&[0u8; 32]); // padding to 40 bytes

        // Create ZIP in memory
        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);

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
</Package>"#.to_string()
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
}"#.to_string()
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
        // Test with a non-standard header size (e.g., 50 bytes instead of 40)
        let manifest = test_manifest();
        let symbols = test_symbols();

        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&[0u8; 46]); // 50 byte header total

        // Create ZIP
        let mut zip_buf = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
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
}
