//! `.app` package inspection and extraction.
//!
//! Unlike `app_reader`, this module exposes every archive entry for inspection.
//!
//! A `.app` is a `NAVX` header followed by an OPC/ZIP archive. Cloud-targeted
//! packages contain AL source, symbols, and metadata rather than compiled IL.

use std::io::{Cursor, Read};
use std::path::Path;

use zip::ZipArchive;

use super::app_reader::{find_zip_offset, AppReaderError};

const NAVX_MAGIC: &[u8; 4] = b"NAVX";

/// Max bytes peeked from each entry to classify its content.
const PEEK_LEN: usize = 8;

/// Best-effort classification of a `.app` archive entry from its name + leading
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AppEntryKind {
    AlSource,
    /// JSON (e.g. `SymbolReference.json`).
    Json,
    /// XML (manifest, entitlements, media listing, doc comments).
    Xml,
    /// A .NET assembly (PE/`MZ`) - indicates compiled code in the package.
    DotNetAssembly,
    Other,
}

/// One entry inside a `.app` archive.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppEntry {
    /// Full path of the entry within the archive.
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Compressed size in bytes.
    pub compressed_size: u64,
    pub kind: AppEntryKind,
}

impl AppEntry {
    fn classify(name: &str, head: &[u8]) -> AppEntryKind {
        if head.starts_with(b"MZ") {
            return AppEntryKind::DotNetAssembly;
        }
        let lower = name.to_ascii_lowercase();
        if lower.ends_with(".al") {
            AppEntryKind::AlSource
        } else if lower.ends_with(".json") {
            AppEntryKind::Json
        } else if lower.ends_with(".xml") {
            AppEntryKind::Xml
        } else {
            // Fall back to a content sniff for extension-less entries.
            let trimmed = strip_bom(head);
            match trimmed.first() {
                Some(b'{') | Some(b'[') => AppEntryKind::Json,
                Some(b'<') => AppEntryKind::Xml,
                _ => AppEntryKind::Other,
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppContents {
    /// Byte offset where the ZIP payload starts (size of the NAVX header).
    pub navx_header_len: usize,
    pub entries: Vec<AppEntry>,
}

impl AppContents {
    /// True when the package carries compiled .NET code (a runtime/server
    /// package), as opposed to an `alc` source package (source + symbols).
    pub fn has_compiled_code(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.kind == AppEntryKind::DotNetAssembly)
    }

    pub fn has_source(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.kind == AppEntryKind::AlSource)
    }
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// ZIP archive borrowing the `.app` byte buffer (after the NAVX header).
type AppArchive<'a> = ZipArchive<Cursor<&'a [u8]>>;

fn open_archive(data: &[u8]) -> Result<(usize, AppArchive<'_>), AppReaderError> {
    if data.len() < 4 {
        return Err(AppReaderError::TooSmall(data.len()));
    }
    if &data[0..4] != NAVX_MAGIC {
        return Err(AppReaderError::NotNavx);
    }
    let offset = find_zip_offset(data).ok_or(AppReaderError::NoZipSignature)?;
    let archive = ZipArchive::new(Cursor::new(&data[offset..]))?;
    Ok((offset, archive))
}

pub fn list_app_entries(data: &[u8]) -> Result<AppContents, AppReaderError> {
    let (offset, mut archive) = open_archive(data)?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        let size = file.size();
        let compressed_size = file.compressed_size();
        let mut head = [0u8; PEEK_LEN];
        let n = file.read(&mut head).unwrap_or(0);
        entries.push(AppEntry {
            name: name.clone(),
            size,
            compressed_size,
            kind: AppEntry::classify(&name, &head[..n]),
        });
    }
    Ok(AppContents {
        navx_header_len: offset,
        entries,
    })
}

pub fn list_app_file(path: &Path) -> Result<AppContents, AppReaderError> {
    let data = std::fs::read(path)?;
    list_app_entries(&data)
}

/// Extract every entry of a `.app` into `dest_dir`, returning the contents
/// listing. Entry paths are sanitised against directory traversal (a malicious
/// `.app` cannot write outside `dest_dir`).
pub fn extract_app(data: &[u8], dest_dir: &Path) -> Result<AppContents, AppReaderError> {
    let (offset, mut archive) = open_archive(data)?;
    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();
        let Some(rel) = safe_join(dest_dir, &name) else {
            tracing::warn!(entry = %name, "skipping .app entry with unsafe path");
            continue;
        };
        let size = file.size();
        let compressed_size = file.compressed_size();
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)?;
        if let Some(parent) = rel.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&rel, &bytes)?;
        entries.push(AppEntry {
            name: name.clone(),
            size,
            compressed_size,
            kind: AppEntry::classify(&name, &bytes[..bytes.len().min(PEEK_LEN)]),
        });
    }
    Ok(AppContents {
        navx_header_len: offset,
        entries,
    })
}

/// Join an archive entry name under `base`, rejecting absolute paths and any
/// component that would escape `base` (`..`). Returns `None` for unsafe names.
fn safe_join(base: &Path, entry: &str) -> Option<std::path::PathBuf> {
    use std::path::Component;
    let candidate = Path::new(entry);
    let mut out = base.to_path_buf();
    for comp in candidate.components() {
        match comp {
            Component::Normal(part) => out.push(part),
            // Strip leading slashes / drive prefixes; reject traversal.
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn make_app(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]); // 40-byte header

        let mut zip_buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut zip_buf));
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, bytes) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(bytes).unwrap();
            }
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);
        data
    }

    #[test]
    fn lists_all_entries_with_classification() {
        let data = make_app(&[
            ("NavxManifest.xml", b"<?xml version=\"1.0\"?><Package/>"),
            ("src/Hello.al", b"codeunit 50100 X { }"),
            ("SymbolReference.json", b"{\"Tables\":[]}"),
            ("logo.png", b"\x89PNG\r\n\x1a\n"),
        ]);
        let contents = list_app_entries(&data).unwrap();
        assert_eq!(contents.navx_header_len, 40);
        assert_eq!(contents.entries.len(), 4);

        let by_name = |n: &str| contents.entries.iter().find(|e| e.name == n).unwrap().kind;
        assert_eq!(by_name("NavxManifest.xml"), AppEntryKind::Xml);
        assert_eq!(by_name("src/Hello.al"), AppEntryKind::AlSource);
        assert_eq!(by_name("SymbolReference.json"), AppEntryKind::Json);
        assert_eq!(by_name("logo.png"), AppEntryKind::Other);

        assert!(contents.has_source());
        assert!(!contents.has_compiled_code());
    }

    #[test]
    fn detects_compiled_assembly() {
        let data = make_app(&[("CompiledLogic.dll", b"MZ\x90\x00\x03\x00\x00\x00")]);
        let contents = list_app_entries(&data).unwrap();
        assert_eq!(contents.entries[0].kind, AppEntryKind::DotNetAssembly);
        assert!(contents.has_compiled_code());
        assert!(!contents.has_source());
    }

    #[test]
    fn extract_writes_files_and_rejects_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let data = make_app(&[
            ("src/Hello.al", b"codeunit 50100 X { }"),
            ("../escape.txt", b"nope"),
        ]);
        let contents = extract_app(&data, dir.path()).unwrap();
        // The traversal entry is skipped, the safe one is written.
        assert!(dir.path().join("src/Hello.al").is_file());
        assert!(!dir.path().parent().unwrap().join("escape.txt").exists());
        assert!(contents.entries.iter().any(|e| e.name == "src/Hello.al"));
    }

    #[test]
    fn rejects_non_navx() {
        let err = list_app_entries(b"PK\x03\x04not-navx").unwrap_err();
        assert!(matches!(err, AppReaderError::NotNavx));
    }
}
