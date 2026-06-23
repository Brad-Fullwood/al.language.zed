//! `.app` package writer — the inverse of [`al_symbols::app_inspect`].
//!
//! A `.app` is a 40-byte `NAVX` header followed by a Deflated OPC/ZIP archive.
//! The header layout (decoded from alc 17.0.34 output):
//!
//! ```text
//! offset  size  field
//!   0       4   "NAVX" magic
//!   4       4   u32 LE  header length (always 40 — offset to the ZIP)
//!   8       4   u32 LE  format version (2)
//!  12      16   package GUID (random per build — alc is NOT byte-deterministic)
//!  28       8   u64 LE  ZIP payload length
//!  36       4   "NAVX" magic (trailing sentinel)
//!  40      ..   ZIP payload (Deflated)
//! ```
//!
//! Because alc randomises the GUID, byte-identity with alc is neither possible
//! nor required; this writer targets a functionally valid package.

use std::io::{Cursor, Write};

use thiserror::Error;

const NAVX_MAGIC: &[u8; 4] = b"NAVX";
/// Fixed header length / offset to the ZIP payload.
const NAVX_HEADER_LEN: u32 = 40;
/// NAVX container format version alc 17.x writes.
const NAVX_FORMAT_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum EmitError {
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("random source failed: {0}")]
    Random(String),
    #[error("project error: {0}")]
    Project(String),
}

pub fn random_package_guid() -> Result<[u8; 16], EmitError> {
    let mut guid = [0u8; 16];
    getrandom::getrandom(&mut guid).map_err(|e| EmitError::Random(e.to_string()))?;
    Ok(guid)
}

/// A random v4 GUID in .NET `Guid.ToString("B")` form (`{xxxxxxxx-…}`, braces,
/// lowercase) — alc stamps navigation `ControlGUID`s with `Guid.NewGuid()`, so
/// these are inherently non-deterministic (like the package GUID).
pub fn random_guid_braced() -> Result<String, EmitError> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).map_err(|e| EmitError::Random(e.to_string()))?;
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    Ok(format!(
        "{{{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}}}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

/// Write a plain Deflated ZIP of `entries` (name → bytes), in order. Used both
/// for the `.app` payload and for nested OPC bundles (e.g. a control add-in's
/// `addin/<name>.zip`).
pub fn write_zip(entries: &[(String, Vec<u8>)]) -> Result<Vec<u8>, EmitError> {
    let mut zip_buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(Cursor::new(&mut zip_buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in entries {
            zw.start_file(name, opts)?;
            zw.write_all(bytes)?;
        }
        zw.finish()?;
    }
    Ok(zip_buf)
}

/// Write a `.app` package: a Deflated ZIP of `entries` (name → bytes) wrapped in
/// the NAVX header. Entries are written in the order given.
pub fn write_app_package(
    entries: &[(String, Vec<u8>)],
    package_guid: [u8; 16],
) -> Result<Vec<u8>, EmitError> {
    let zip_buf = write_zip(entries)?;

    let zip_len = zip_buf.len() as u64;
    let mut out = Vec::with_capacity(NAVX_HEADER_LEN as usize + zip_buf.len());
    out.extend_from_slice(NAVX_MAGIC);
    out.extend_from_slice(&NAVX_HEADER_LEN.to_le_bytes());
    out.extend_from_slice(&NAVX_FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&package_guid);
    out.extend_from_slice(&zip_len.to_le_bytes());
    out.extend_from_slice(NAVX_MAGIC);
    debug_assert_eq!(out.len(), NAVX_HEADER_LEN as usize);
    out.extend_from_slice(&zip_buf);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_symbols::app_inspect::{list_app_entries, AppEntryKind};

    #[test]
    fn writes_navx_header_then_zip() {
        let entries = vec![
            ("NavxManifest.xml".to_string(), b"<Package/>".to_vec()),
            (
                "src/src/Hello.al".to_string(),
                b"codeunit 50100 X {}".to_vec(),
            ),
        ];
        let guid = [7u8; 16];
        let app = write_app_package(&entries, guid).unwrap();

        assert_eq!(&app[0..4], b"NAVX");
        assert_eq!(u32::from_le_bytes(app[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(app[8..12].try_into().unwrap()), 2);
        assert_eq!(&app[12..28], &guid);
        let zip_len = u64::from_le_bytes(app[28..36].try_into().unwrap());
        assert_eq!(&app[36..40], b"NAVX");
        assert_eq!(zip_len as usize, app.len() - 40);
    }

    #[test]
    fn round_trips_through_the_unpacker() {
        // The package we write must be readable by our own `.app` inspector,
        // with the same entries and classifications.
        let entries = vec![
            (
                "NavxManifest.xml".to_string(),
                b"<?xml version=\"1.0\"?><Package/>".to_vec(),
            ),
            (
                "src/src/Hello.al".to_string(),
                b"codeunit 50100 X {}".to_vec(),
            ),
            (
                "SymbolReference.json".to_string(),
                b"{\"Codeunits\":[]}".to_vec(),
            ),
        ];
        let app = write_app_package(&entries, random_package_guid().unwrap()).unwrap();

        let contents = list_app_entries(&app).unwrap();
        assert_eq!(contents.navx_header_len, 40);
        let names: Vec<&str> = contents.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "NavxManifest.xml",
                "src/src/Hello.al",
                "SymbolReference.json"
            ]
        );
        assert!(contents.has_source());
        assert!(!contents.has_compiled_code());
        let al = contents
            .entries
            .iter()
            .find(|e| e.name.ends_with(".al"))
            .unwrap();
        assert_eq!(al.kind, AppEntryKind::AlSource);
    }
}
