use std::fs;
use std::path::{Path, PathBuf};

use crate::model::SymbolEntry;
use crate::source_index;

/// Cache directory for extracted / generated virtual AL files.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("symbols")
}

/// Get or create a virtual AL file for a package symbol entry.
pub fn get_or_create(
    entry: &SymbolEntry,
    app_path: Option<&Path>,
    allow_outline_fallback: bool,
) -> std::io::Result<PathBuf> {
    let cache_root = cache_dir();
    let pkg_dir = cache_root.join(sanitize_filename(&entry.package));
    let filename = format!("{} {} {}.al", entry.kind, entry.id, entry.name);
    let file_path = pkg_dir.join(sanitize_filename(&filename));

    ensure_readonly_settings(&cache_root);

    if !file_path.exists() {
        let extracted = app_path.and_then(|path| extract_source_from_app(path, entry));

        let source = match extracted {
            Some(src) => src,
            None if allow_outline_fallback => generate_al(entry),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no source in .app for {} \"{}\"", entry.kind, entry.name),
                ));
            }
        };

        fs::create_dir_all(&pkg_dir)?;
        fs::write(&file_path, &source)?;
    }

    enforce_readonly(&file_path);
    Ok(file_path)
}

/// Kinds of members used for line matching.
#[derive(Debug, Clone)]
pub enum MemberKind {
    Field,
    Key,
    Control(String),
    EnumValue,
    Procedure,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub struct MemberRange {
    pub line: u32,
    pub col_start: u32,
    pub col_end: u32,
}

/// Scan a generated virtual AL file for the line that declares `member_name`.
pub fn find_member_line(path: &Path, member_name: &str) -> Option<u32> {
    find_member_range(path, member_name, MemberKind::Unknown).map(|r| r.line)
}

/// Scan a generated virtual AL file for the line that declares `member_name`, with a known kind.
pub fn find_member_line_with_kind(
    path: &Path,
    member_name: &str,
    kind: MemberKind,
) -> Option<u32> {
    find_member_range(path, member_name, kind).map(|r| r.line)
}

/// Find a precise member range (line/column) for deep-linking.
pub fn find_member_range(path: &Path, member_name: &str, kind: MemberKind) -> Option<MemberRange> {
    let content = fs::read_to_string(path).ok()?;
    find_member_range_in_text(&content, member_name, kind)
}

/// Check whether an `.app` file contains any `.al` source files.
pub fn app_has_source(app_path: &Path) -> bool {
    let data = match fs::read(app_path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    let zip_offset = match (4..data.len().saturating_sub(3))
        .find(|&i| &data[i..i + 4] == b"\x50\x4B\x03\x04") {
        Some(o) => o,
        None => return false,
    };

    let mut archive = match zip::ZipArchive::new(std::io::Cursor::new(&data[zip_offset..])) {
        Ok(a) => a,
        Err(_) => return false,
    };

    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            if file.name().to_lowercase().ends_with(".al") {
                return true;
            }
        }
    }

    false
}

fn extract_source_from_app(app_path: &Path, entry: &SymbolEntry) -> Option<String> {
    let index = source_index::get_or_build(app_path).ok()?;
    index.extract_source_for_entry(entry)
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn generate_al(entry: &SymbolEntry) -> String {
    let mut out = String::new();
    let name_quoted = if entry.name.contains(' ') { format!("\"{}\"", entry.name) } else { entry.name.clone() };
    let kind_lower = format!("{:?}", entry.kind).to_lowercase();

    out.push_str(&format!("{} {} {}\n{{\n", kind_lower, entry.id, name_quoted));
    for m in &entry.methods {
        let local = if m.is_local { "local " } else { "" };
        out.push_str(&format!("    {}procedure {}(", local, m.name));
        out.push_str(")\n    begin\n    end;\n\n");
    }
    out.push_str("}\n");
    out
}

fn enforce_readonly(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o444);
            let _ = fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        if let Ok(meta) = fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_readonly(true);
            let _ = fs::set_permissions(path, perms);
        }
    }
}

fn ensure_readonly_settings(cache_root: &Path) {
    let settings_dir = cache_root.join(".zed");
    let settings_path = settings_dir.join("settings.json");
    let _ = fs::create_dir_all(&settings_dir);

    let mut settings: serde_json::Value = if let Ok(text) = fs::read_to_string(&settings_path) {
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let list = settings
        .get_mut("read_only_files")
        .and_then(|v| v.as_array_mut());

    let pattern = "symbols/**/*.al";
    match list {
        Some(arr) => {
            let exists = arr.iter().any(|v| v.as_str() == Some(pattern));
            if !exists {
                arr.push(serde_json::Value::String(pattern.to_string()));
            }
        }
        None => {
            settings["read_only_files"] =
                serde_json::Value::Array(vec![serde_json::Value::String(pattern.to_string())]);
        }
    }

    if let Ok(text) = serde_json::to_string_pretty(&settings) {
        let _ = fs::write(&settings_path, text);
    }
}

fn find_member_range_in_text(
    content: &str,
    member_name: &str,
    kind: MemberKind,
) -> Option<MemberRange> {
    let needle = member_name.to_lowercase();

    for (line_idx, line) in content.lines().enumerate() {
        let range = match kind {
            MemberKind::Procedure => find_procedure_range(line, &needle),
            MemberKind::Field => find_call_range(line, "field", 1, &needle),
            MemberKind::Key => find_call_range(line, "key", 0, &needle),
            MemberKind::EnumValue => find_call_range(line, "value", 1, &needle),
            MemberKind::Control(ref k) => find_call_range(line, k, 0, &needle),
            MemberKind::Unknown => {
                find_procedure_range(line, &needle)
                    .or_else(|| find_call_range(line, "field", 1, &needle))
                    .or_else(|| find_call_range(line, "value", 1, &needle))
            }
        };

        if let Some((col_start, col_end)) = range {
            return Some(MemberRange {
                line: line_idx as u32,
                col_start: col_start as u32,
                col_end: col_end as u32,
            });
        }
    }
    None
}

fn find_procedure_range(line: &str, needle: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_char(bytes[i]) {
                i += 1;
            }
            let word = &line[start..i];
            if word.eq_ignore_ascii_case("procedure") || word.eq_ignore_ascii_case("trigger") {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if let Some((name, col_start, col_end)) = parse_name_token(line, bytes, j) {
                    if name.to_lowercase() == needle {
                        return Some((col_start, col_end));
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn find_call_range(line: &str, keyword: &str, arg_index: usize, needle: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if is_ident_start(bytes[i]) {
            let start = i;
            i += 1;
            while i < bytes.len() && is_ident_char(bytes[i]) {
                i += 1;
            }
            let word = &line[start..i];
            if word.eq_ignore_ascii_case(keyword) {
                let mut j = i;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'(' {
                    if let Some((name, col_start, col_end)) = parse_call_arg(line, bytes, j + 1, arg_index) {
                        if name.to_lowercase() == needle {
                            return Some((col_start, col_end));
                        }
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn parse_call_arg(
    line: &str,
    bytes: &[u8],
    mut i: usize,
    target_index: usize,
) -> Option<(String, usize, usize)> {
    let mut arg_idx = 0usize;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b')' {
            return None;
        }
        let (name, col_start, col_end) = parse_name_token(line, bytes, i)?;
        let mut token_end = col_end;
        if col_start > 0 && bytes[col_start.saturating_sub(1)] == b'"' {
            token_end = col_end.saturating_add(1);
        }
        if arg_idx == target_index {
            return Some((name, col_start, col_end));
        }
        i = token_end;
        // Move to next separator at depth 0.
        let mut depth = 0i32;
        let mut in_string = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_string {
                if b == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    in_string = false;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' => {
                    in_string = true;
                    i += 1;
                }
                b'(' => {
                    depth += 1;
                    i += 1;
                }
                b')' => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                    i += 1;
                }
                b';' | b',' => {
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        arg_idx += 1;
    }
}

fn parse_name_token(
    line: &str,
    bytes: &[u8],
    mut i: usize,
) -> Option<(String, usize, usize)> {
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'"' {
        let start_col = i + 1;
        i += 1;
        let mut out = String::new();
        while i < bytes.len() {
            if bytes[i] == b'"' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                    out.push('"');
                    i += 2;
                    continue;
                }
                let end_col = i;
                return Some((out, start_col, end_col));
            }
            let ch = line[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
        }
        return None;
    }

    let start_col = i;
    while i < bytes.len()
        && !bytes[i].is_ascii_whitespace()
        && bytes[i] != b';'
        && bytes[i] != b','
        && bytes[i] != b')'
        && bytes[i] != b'('
    {
        i += 1;
    }
    if i == start_col {
        None
    } else {
        let name = line[start_col..i].trim().to_string();
        Some((name, start_col, i))
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
