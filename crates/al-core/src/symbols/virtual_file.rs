use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use memmap2::Mmap;

use super::model::SymbolEntry;
use super::source_index;
use super::source_index::{is_ident_char, is_ident_start, parse_quoted_ident};

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("symbols")
}

/// Tries to extract source from the .app ZIP archive first.
/// If no source is available, renders a complete outline from symbol metadata
/// with full procedure signatures, fields, keys, enum values, and attributes.
pub fn get_or_create(entry: &SymbolEntry, app_path: Option<&Path>) -> std::io::Result<PathBuf> {
    let cache_root = cache_dir();
    let pkg_dir = cache_root.join(sanitize_filename(&entry.package));
    let filename = format!("{} {} {}.al", entry.kind, entry.id, entry.name);
    let file_path = pkg_dir.join(sanitize_filename(&filename));

    ensure_readonly_settings(&cache_root);

    fs::create_dir_all(&pkg_dir)?;

    // F-041: invalidate the cache entry when the source `.app` package is
    // newer than the cached virtual file. Without this, replacing a package
    // with a newer version (same publisher + name + object id + name) would
    // serve stale generated source forever because `create_new` short-
    // circuited on AlreadyExists. Compare mtimes — if the .app post-dates
    // the cached file, drop the cached file (clearing its read-only bit
    // first so the remove succeeds on Windows + Unix).
    // Invalidate the cached virtual file when EITHER the source `.app` OR the
    // running al-lsp binary is newer than the cache. The binary check matters
    // because a rebuilt / upgraded al-lsp may extract or render symbol source
    // differently than the build that wrote the cache; without it, an older
    // build's output (e.g. an outline where the current build produces real
    // source) persists forever, since the `.app` mtime alone never changes
    // across an al-lsp upgrade.
    if let Ok(cache_mtime) = fs::metadata(&file_path).and_then(|m| m.modified()) {
        let app_newer = app_path
            .and_then(|app| fs::metadata(app).ok())
            .and_then(|m| m.modified().ok())
            .is_some_and(|t| t > cache_mtime);
        if app_newer || self_exe_mtime().is_some_and(|t| t > cache_mtime) {
            let _ = clear_readonly(&file_path);
            let _ = fs::remove_file(&file_path);
        }
    }

    // Use create_new to atomically create the file, avoiding a TOCTOU race.
    // If another thread/process already created it, AlreadyExists is fine.
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
    {
        Ok(mut f) => {
            let extracted = app_path.and_then(|path| extract_source_from_app(path, entry));
            let source = extracted.unwrap_or_else(|| render_outline(entry));
            f.write_all(source.as_bytes())?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another thread already wrote the file; use what's there.
        }
        Err(e) => return Err(e),
    }

    enforce_readonly(&file_path);
    Ok(file_path)
}

/// mtime of the running al-lsp executable, computed once. Cache entries older
/// than the binary are regenerated so a rebuilt server's symbol-source output
/// supersedes the previous build's (see `get_or_create`).
fn self_exe_mtime() -> Option<std::time::SystemTime> {
    use std::sync::OnceLock;
    static MTIME: OnceLock<Option<std::time::SystemTime>> = OnceLock::new();
    *MTIME.get_or_init(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
    })
}

/// Drop the read-only attribute on a cached virtual file so `remove_file`
/// can delete it. Best-effort: failures here are not fatal — the subsequent
/// `remove_file` will simply fail and the stale entry will linger.
fn clear_readonly(path: &Path) -> std::io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    // Clippy warns about the platform-portability footgun of calling
    // `set_readonly(false)` — on unix it sets mode 0o666 rather than
    // restoring the original mode. That is precisely the behaviour we
    // want here: a cached virtual file we are about to delete, where
    // any writable mode is fine and we don't care about preserving
    // umask-specific bits.
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    fs::set_permissions(path, perms)
}

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

pub fn find_member_range(path: &Path, member_name: &str, kind: MemberKind) -> Option<MemberRange> {
    let content = fs::read_to_string(path).ok()?;
    find_member_range_in_text(&content, member_name, kind)
}

/// Range of the object's own declaration name in the virtual file. Works for
/// both forms `get_or_create` produces: the synthetic outline (declaration on
/// line 0) and real embedded source (declaration after `namespace`/`using`
/// lines). Without this, navigating to an object passed `member_name=None` and
/// landed at `(0,0)` — the file start / outline — instead of the object.
pub fn find_object_range(path: &Path, entry: &SymbolEntry) -> Option<MemberRange> {
    let content = fs::read_to_string(path).ok()?;
    // Declaration line shape (both outline and alc source): `{kw} {id} {name}`.
    let prefix = format!(
        "{} {} ",
        entry.kind.al_keyword().to_ascii_lowercase(),
        entry.id
    );
    for (line_idx, line) in content.lines().enumerate() {
        let lead = line.len() - line.trim_start().len();
        if !line[lead..].to_ascii_lowercase().starts_with(&prefix) {
            continue;
        }
        let name_start = lead + prefix.len();
        let rest = &line[name_start..];
        // Name runs to an ` extends ` clause, the opening ` {`, or end of line;
        // quotes (for multi-word names) are kept so the whole name highlights.
        let name_len = rest
            .find(" extends ")
            .or_else(|| rest.find(" {"))
            .unwrap_or(rest.len());
        let name = rest[..name_len].trim_end();
        if name.is_empty() {
            continue;
        }
        return Some(MemberRange {
            line: line_idx as u32,
            col_start: crate::syntax::byte_col_to_utf16_col(line, name_start),
            col_end: crate::syntax::byte_col_to_utf16_col(line, name_start + name.len()),
        });
    }
    None
}

/// Uses memory-mapped I/O to avoid reading the entire file into memory.
/// Only examines zip entry names — no file content is read.
pub fn app_has_source(app_path: &Path) -> bool {
    let file = match fs::File::open(app_path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    // SAFETY: .app files are opened read-only. Concurrent modification is
    // prevented by the staleness check at the call site (package version
    // comparison via `modified` timestamp). On Linux, MAP_PRIVATE means a
    // concurrent file replacement serves stale data rather than UB. On
    // Windows, the file cannot be replaced while it is mapped.
    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(m) => m,
        Err(_) => return false,
    };

    let zip_offset = match super::app_reader::find_zip_offset(&mmap) {
        Some(o) => o,
        None => return false,
    };

    let mut archive = match zip::ZipArchive::new(std::io::Cursor::new(&mmap[zip_offset..])) {
        Ok(a) => a,
        Err(_) => return false,
    };

    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index_raw(i) {
            if entry.name().to_ascii_lowercase().ends_with(".al") {
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
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Produces valid AL syntax with full procedure signatures (parameters + types + return type),
/// field declarations (id + name + type), key declarations, enum values, event declarations
/// with attributes, and global variables. This is the standard output for packages without
/// embedded source — not a degraded mode.
pub fn render_outline(entry: &SymbolEntry) -> String {
    use super::model::{FieldSymbol, MethodSymbol};

    fn format_name(name: &str) -> String {
        let needs_quoting = name.contains(' ')
            || name.contains('.')
            || name.contains('/')
            || name.contains('-')
            || name.contains('&')
            || name.contains('(')
            || name.contains(')');
        if needs_quoting {
            format!("\"{}\"", name)
        } else {
            name.to_string()
        }
    }

    fn render_field(out: &mut String, f: &FieldSymbol) {
        let n = format_name(&f.name);
        if f.type_name.is_empty() {
            out.push_str(&format!("        field({}; {}) {{ }}\n", f.id, n));
        } else {
            out.push_str(&format!(
                "        field({}; {}; {}) {{ }}\n",
                f.id, n, f.type_name
            ));
        }
    }

    fn render_method(out: &mut String, m: &MethodSymbol) {
        for attr in &m.attributes {
            out.push_str(&format!("    [{}", attr.name));
            if !attr.arguments.is_empty() {
                out.push_str(&format!("({})", attr.arguments.join(", ")));
            }
            out.push_str("]\n");
        }

        let params: Vec<String> = m
            .parameters
            .iter()
            .map(|p| {
                let var_prefix = if p.is_var { "var " } else { "" };
                format!("{}{}: {}", var_prefix, p.name, p.type_name)
            })
            .collect();

        let local = if m.is_local { "    local " } else { "    " };
        out.push_str(&format!(
            "{}procedure {}({})",
            local,
            m.name,
            params.join("; ")
        ));
        if let Some(ref ret) = m.return_type {
            out.push_str(&format!(": {}", ret));
        }
        out.push_str(";\n");
    }

    let mut out = String::new();
    let name_str = format_name(&entry.name);
    let kw = entry.kind.al_keyword();

    if let Some(ref extends) = entry.extends {
        let ext = format_name(extends);
        out.push_str(&format!(
            "{} {} {} extends {}\n",
            kw, entry.id, name_str, ext
        ));
    } else {
        out.push_str(&format!("{} {} {}\n", kw, entry.id, name_str));
    }
    out.push_str("{\n");

    if !entry.fields.is_empty() {
        out.push_str("    fields\n    {\n");
        for f in &entry.fields {
            render_field(&mut out, f);
        }
        out.push_str("    }\n\n");
    }

    if !entry.keys.is_empty() {
        out.push_str("    keys\n    {\n");
        for k in &entry.keys {
            let fields = k.field_names.join(", ");
            out.push_str(&format!("        key({}; {})\n", k.name, fields));
        }
        out.push_str("    }\n\n");
    }

    if !entry.enum_values.is_empty() {
        for v in &entry.enum_values {
            let v_name = format_name(&v.name);
            out.push_str(&format!("    value({}; {}) {{ }}\n", v.ordinal, v_name));
        }
        out.push('\n');
    }

    if !entry.variables.is_empty() {
        out.push_str("    var\n");
        for v in &entry.variables {
            let prot = if v.is_protected { "protected " } else { "" };
            out.push_str(&format!("        {}{}: {};\n", prot, v.name, v.type_name));
        }
        out.push('\n');
    }

    for m in &entry.methods {
        render_method(&mut out, m);
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
            if let Err(e) = fs::set_permissions(path, perms) {
                tracing::debug!(path = %path.display(), error = %e, "enforce_readonly: chmod 0o444 failed (virtual file may stay writable)");
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Ok(meta) = fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_readonly(true);
            if let Err(e) = fs::set_permissions(path, perms) {
                tracing::debug!(path = %path.display(), error = %e, "enforce_readonly: set_readonly failed (virtual file may stay writable)");
            }
        }
    }
}

fn ensure_readonly_settings(cache_root: &Path) {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
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
            if let Err(e) = fs::write(&settings_path, text) {
                tracing::warn!(
                    path = %settings_path.display(),
                    error = %e,
                    "failed to write virtual AL settings file"
                );
            }
        }
    });
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
            MemberKind::Unknown => find_procedure_range(line, &needle)
                .or_else(|| find_call_range(line, "field", 1, &needle))
                .or_else(|| find_call_range(line, "value", 1, &needle)),
        };

        if let Some((col_start, col_end)) = range {
            // The inner parsers (`find_procedure_range` / `find_call_range`)
            // operate on the line's bytes, so `col_start`/`col_end` are byte
            // offsets. LSP `Position.character` is a UTF-16 code-unit offset,
            // so convert before reporting — otherwise deep-linking into a
            // virtual `.app` member whose line contains non-ASCII characters
            // (e.g. an accented field name) lands at the wrong column.
            return Some(MemberRange {
                line: line_idx as u32,
                col_start: crate::syntax::byte_col_to_utf16_col(line, col_start),
                col_end: crate::syntax::byte_col_to_utf16_col(line, col_end),
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

fn find_call_range(
    line: &str,
    keyword: &str,
    arg_index: usize,
    needle: &str,
) -> Option<(usize, usize)> {
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
                    if let Some((name, col_start, col_end)) =
                        parse_call_arg(line, bytes, j + 1, arg_index)
                    {
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

fn parse_name_token(line: &str, bytes: &[u8], mut i: usize) -> Option<(String, usize, usize)> {
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'"' {
        let start_col = i + 1;
        let (name, end) = parse_quoted_ident(bytes, i)?;
        // end points to the byte after the closing `"`, so col_end = end - 1
        return Some((name, start_col, end - 1));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_member_range_ascii_field_columns() {
        // ASCII line: byte offsets and UTF-16 columns coincide.
        let text = "        field(1; \"Test\"; Code) { }";
        let r = find_member_range_in_text(text, "Test", MemberKind::Field).unwrap();
        assert_eq!(r.line, 0);
        // The opening quote is at byte 17, name "Test" starts at col 18.
        let expected_start = text.find("Test").unwrap() as u32;
        assert_eq!(r.col_start, expected_start);
        assert_eq!(r.col_end, expected_start + 4);
    }

    #[test]
    fn find_member_range_reports_utf16_columns_for_non_ascii_name() {
        // The field name contains a 2-byte-UTF-8 / 1-UTF-16-unit character
        // ("ë"). col_end must be the UTF-16 offset, not the byte offset, so
        // an editor deep-link lands on the right column.
        let text = "        field(1; \"Tëst\"; Code) { }";
        let r = find_member_range_in_text(text, "Tëst", MemberKind::Field).unwrap();
        assert_eq!(r.line, 0);

        // Everything before the name is ASCII, so col_start is unaffected.
        let name_byte_start = text.find("Tëst").unwrap();
        let prefix = &text[..name_byte_start];
        let expected_start = crate::syntax::byte_col_to_utf16_col(text, name_byte_start);
        assert_eq!(r.col_start, expected_start);
        assert_eq!(r.col_start, prefix.chars().count() as u32);

        // "Tëst" is 4 chars / 4 UTF-16 units but 5 UTF-8 bytes, so a byte-
        // based col_end would be one too large.
        assert_eq!(r.col_end, r.col_start + 4);
        let byte_end = name_byte_start + "Tëst".len();
        assert!(
            (byte_end as u32) > r.col_end,
            "byte offset ({byte_end}) must exceed the UTF-16 col_end ({}) for a non-ASCII name",
            r.col_end
        );
    }

    #[test]
    fn find_member_range_procedure_after_non_ascii_is_utf16() {
        // Non-ASCII *before* the matched name shifts both columns; verify
        // col_start is the UTF-16 offset rather than the byte offset.
        let text = "    // café\n    procedure Foo()";
        let r = find_member_range_in_text(text, "Foo", MemberKind::Procedure).unwrap();
        // The procedure is on line index 1; the prefix on that line is ASCII.
        assert_eq!(r.line, 1);
        let line1 = text.lines().nth(1).unwrap();
        let expected_start =
            crate::syntax::byte_col_to_utf16_col(line1, line1.find("Foo").unwrap());
        assert_eq!(r.col_start, expected_start);
        assert_eq!(r.col_end, r.col_start + 3);
    }
}
