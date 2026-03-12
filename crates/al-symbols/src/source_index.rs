//! Source indexing for `.app` packages with embedded AL source.
//!
//! Builds a fast map from (ObjectKind, Id, Name) to the internal ZIP path
//! by scanning object headers once per package. Subsequent lookups are instant.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::SystemTime;

use dashmap::DashMap;
use memmap2::Mmap;
use zip::ZipArchive;

use crate::model::{ObjectKind, SymbolEntry};

const ZIP_MAGIC: &[u8; 4] = &[0x50, 0x4B, 0x03, 0x04];
const MAX_HEADER_BYTES: usize = 256 * 1024;

/// Cached source index per `.app` file.
static SOURCE_INDEX_CACHE: OnceLock<DashMap<PathBuf, Arc<AppSourceIndex>>> = OnceLock::new();

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
        let modified = file.metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let mmap = unsafe { Mmap::map(&file)? };
        let zip_offset = find_zip_offset(&mmap).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "ZIP signature not found in .app")
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
                by_kind_id
                    .entry((kind, id))
                    .or_insert_with(|| name.clone());
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
pub fn get_or_build(app_path: &Path) -> io::Result<Arc<AppSourceIndex>> {
    let cache = SOURCE_INDEX_CACHE.get_or_init(DashMap::new);
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

fn find_zip_offset(data: &[u8]) -> Option<usize> {
    // Scan for ZIP local file header after NAVX header.
    if data.len() > 44 && &data[40..44] == ZIP_MAGIC {
        return Some(40);
    }
    for i in 4..data.len().saturating_sub(3) {
        if &data[i..i + 4] == ZIP_MAGIC {
            return Some(i);
        }
    }
    None
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
            if let Some(kind) = object_kind_from_keyword(ident) {
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

fn object_kind_from_keyword(s: &str) -> Option<ObjectKind> {
    match s.to_ascii_lowercase().as_str() {
        "table" => Some(ObjectKind::Table),
        "tableextension" => Some(ObjectKind::TableExtension),
        "page" => Some(ObjectKind::Page),
        "pageextension" => Some(ObjectKind::PageExtension),
        "codeunit" => Some(ObjectKind::Codeunit),
        "report" => Some(ObjectKind::Report),
        "reportextension" => Some(ObjectKind::ReportExtension),
        "xmlport" => Some(ObjectKind::XmlPort),
        "query" => Some(ObjectKind::Query),
        "enum" => Some(ObjectKind::Enum),
        "enumextension" => Some(ObjectKind::EnumExtension),
        "interface" => Some(ObjectKind::Interface),
        "permissionset" => Some(ObjectKind::PermissionSet),
        "permissionsetextension" => Some(ObjectKind::PermissionSetExtension),
        "profile" => Some(ObjectKind::Profile),
        "pagecustomization" => Some(ObjectKind::PageCustomization),
        "controladdin" => Some(ObjectKind::ControlAddIn),
        "entitlement" => Some(ObjectKind::Entitlement),
        _ => None,
    }
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

fn parse_name(s: &str, bytes: &[u8], mut i: usize) -> Option<(String, usize)> {
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'"' {
        i += 1;
        let mut out = String::new();
        while i < bytes.len() {
            if bytes[i] == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    out.push('"');
                    i += 2;
                    continue;
                }
                i += 1;
                break;
            }
            let ch = s[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
        }
        return Some((out, i));
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

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
