use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use super::model::SymbolEntry;
use super::source_availability::{classify_metadata, SourceAvailability};
use super::source_index;
use super::source_index::{is_ident_char, is_ident_start, parse_quoted_ident};

const VIRTUAL_FILE_CACHE_VERSION: &[u8] = b"al-virtual-source-v2";

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
    get_or_create_with_availability(entry, app_path).map(|materialized| materialized.path)
}

/// A virtual source file plus the representation that was actually written.
#[derive(Debug, Clone)]
pub struct MaterializedSource {
    pub path: PathBuf,
    pub availability: SourceAvailability,
}

/// Materialize package navigation and report the representation actually
/// present in the returned file.
///
/// This differs from package-level availability hints: extraction can fail
/// after indexing because a package changed, disappeared, is unreadable, or
/// contains source that violates the extraction bounds. Every such failure is
/// downgraded to an outline or identity-only declaration here.
pub fn get_or_create_with_availability(
    entry: &SymbolEntry,
    app_path: Option<&Path>,
) -> std::io::Result<MaterializedSource> {
    let cache_root = cache_dir();
    let pkg_dir = cache_root.join(sanitize_filename(&entry.package));
    let file_path = pkg_dir.join(cache_filename(entry, app_path));

    ensure_readonly_settings(&cache_root);

    fs::create_dir_all(&pkg_dir)?;

    // Package identity and symbol metadata are part of the filename, so a
    // package replacement cannot reuse a stale object's virtual file even when
    // the replacement has an older timestamp. A binary update can still change
    // extraction or rendering logic without changing either input.
    if let Ok(cache_mtime) = fs::metadata(&file_path).and_then(|m| m.modified()) {
        if self_exe_mtime().is_some_and(|t| t > cache_mtime) {
            let _ = clear_readonly(&file_path);
            let _ = fs::remove_file(&file_path);
        }
    }

    if !file_path.is_file() {
        // Generate before publishing and rename into place. Creating the final
        // path first allowed a concurrent navigation request to observe a
        // partially-written AL file.
        let extracted = app_path.and_then(|path| extract_source_from_app(path, entry));
        let source = match extracted {
            Some(source) => source,
            None => match classify_metadata(entry) {
                SourceAvailability::GeneratedOutline => render_outline_with_note(entry),
                SourceAvailability::MetadataOnly => render_metadata_only_with_note(entry),
                SourceAvailability::WorkspaceSource | SourceAvailability::EmbeddedSource => {
                    render_outline_with_note(entry)
                }
            },
        };
        let tmp_path = virtual_file_temp_path(&file_path);
        let write_result = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp_path)?;
            file.write_all(source.as_bytes())?;
            file.flush()?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&tmp_path);
            return Err(error);
        }
        if let Err(error) = fs::rename(&tmp_path, &file_path) {
            // On Windows rename does not replace an existing destination. If a
            // racing writer won, its complete file is equally valid.
            if file_path.is_file() {
                let _ = fs::remove_file(&tmp_path);
            } else {
                let _ = fs::remove_file(&tmp_path);
                return Err(error);
            }
        }
    }

    enforce_readonly(&file_path);
    let availability = materialized_availability(&file_path)?;
    Ok(MaterializedSource {
        path: file_path,
        availability,
    })
}

fn materialized_availability(path: &Path) -> std::io::Result<SourceAvailability> {
    let mut file = fs::File::open(path)?;
    let prefix_len = OUTLINE_NOTE.len().max(METADATA_ONLY_NOTE.len());
    let mut prefix = vec![0; prefix_len];
    let read = file.read(&mut prefix)?;
    prefix.truncate(read);
    if prefix.starts_with(METADATA_ONLY_NOTE.as_bytes()) {
        Ok(SourceAvailability::MetadataOnly)
    } else if prefix.starts_with(OUTLINE_NOTE.as_bytes()) {
        Ok(SourceAvailability::GeneratedOutline)
    } else {
        Ok(SourceAvailability::EmbeddedSource)
    }
}

fn virtual_file_temp_path(target: &Path) -> PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let filename = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("symbol.al");
    target.with_file_name(format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        sequence
    ))
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
/// can delete it.
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
            col_start: al_syntax::byte_col_to_utf16_col(line, name_start),
            col_end: al_syntax::byte_col_to_utf16_col(line, name_start + name.len()),
        });
    }
    None
}

/// Examines ZIP entry metadata directly from the file; no package payload or
/// source content is buffered.
pub fn app_has_source(app_path: &Path) -> bool {
    let mut file = match fs::File::open(app_path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    if file.metadata().map_or(true, |metadata| {
        metadata.len() > super::app_reader::MAX_APP_FILE_SIZE
    }) {
        return false;
    }
    let mut magic = [0u8; 4];
    if std::io::Read::read_exact(&mut file, &mut magic).is_err() || &magic != b"NAVX" {
        return false;
    }
    if std::io::Seek::rewind(&mut file).is_err() {
        return false;
    }
    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(_) => return false,
    };
    if archive.len() > super::app_reader::MAX_ARCHIVE_ENTRIES {
        return false;
    }

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

/// Stable cache filename for one exact source representation input.
///
/// Including the canonical package path, size, timestamp, and serialized
/// symbol metadata prevents a same-named package upgrade from serving an old
/// embedded file or outline. The human-readable prefix is bounded so long AL
/// identifiers cannot exceed common 255-byte filesystem component limits.
fn cache_filename(entry: &SymbolEntry, app_path: Option<&Path>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(VIRTUAL_FILE_CACHE_VERSION);
    if let Ok(metadata) = serde_json::to_vec(entry) {
        hasher.update(metadata);
    }
    match app_path {
        Some(path) => {
            hasher.update(b"package:");
            let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            hasher.update(canonical.to_string_lossy().as_bytes());
            match fs::metadata(path) {
                Ok(metadata) => {
                    hasher.update(metadata.len().to_le_bytes());
                    if let Ok(modified) = metadata.modified() {
                        match modified.duration_since(std::time::UNIX_EPOCH) {
                            Ok(duration) => {
                                hasher.update(duration.as_secs().to_le_bytes());
                                hasher.update(duration.subsec_nanos().to_le_bytes());
                            }
                            Err(error) => {
                                hasher.update(b"pre-epoch:");
                                hasher.update(error.duration().as_secs().to_le_bytes());
                                hasher.update(error.duration().subsec_nanos().to_le_bytes());
                            }
                        }
                    }
                }
                Err(error) => {
                    hasher.update(b"unavailable:");
                    hasher.update(error.kind().to_string().as_bytes());
                }
            }
        }
        None => hasher.update(b"metadata-only"),
    }
    let digest = format!("{:x}", hasher.finalize());

    let readable = sanitize_filename(&format!("{} {} {}", entry.kind, entry.id, entry.name));
    let mut bounded = String::new();
    for character in readable.chars() {
        if bounded.len() + character.len_utf8() > 180 {
            break;
        }
        bounded.push(character);
    }
    format!("{bounded}-{}.al", &digest[..24])
}

/// Notice prefixed to outlines reconstructed from package metadata.
///
/// The wording avoids AL member keywords because [`find_member_range`] scans
/// the rendered file as text.
pub const OUTLINE_NOTE: &str = "\
// Reconstructed public API from SymbolReference.json.\n\
// Implementation bodies are not included in AL symbol packages.\n\n";

/// Notice for an object whose package exposes identity metadata but no source
/// or member declarations from which to reconstruct a useful API outline.
pub const METADATA_ONLY_NOTE: &str = "\
// Package metadata only: original AL source and API member metadata are unavailable.\n\
// The declaration below exists solely to provide a stable navigation target.\n\n";

/// Render a package outline with [`OUTLINE_NOTE`].
pub fn render_outline_with_note(entry: &SymbolEntry) -> String {
    let body = render_outline(entry);
    let mut out = String::with_capacity(OUTLINE_NOTE.len() + body.len());
    out.push_str(OUTLINE_NOTE);
    out.push_str(&body);
    out
}

pub fn render_metadata_only_with_note(entry: &SymbolEntry) -> String {
    let body = render_outline(entry);
    let mut out = String::with_capacity(METADATA_ONLY_NOTE.len() + body.len());
    out.push_str(METADATA_ONLY_NOTE);
    out.push_str(&body);
    out
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
                col_start: al_syntax::byte_col_to_utf16_col(line, col_start),
                col_end: al_syntax::byte_col_to_utf16_col(line, col_end),
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
    use std::io::Cursor;

    fn write_app_with_raw_source(source: &[u8]) -> tempfile::NamedTempFile {
        let mut zip_bytes = Vec::new();
        {
            let cursor = Cursor::new(&mut zip_bytes);
            let mut zip = zip::ZipWriter::new(cursor);
            zip.start_file(
                "src/SalesPost.Codeunit.al",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(source).unwrap();
            zip.finish().unwrap();
        }

        let mut app = tempfile::NamedTempFile::new().unwrap();
        app.write_all(b"NAVX").unwrap();
        app.write_all(&1u32.to_le_bytes()).unwrap();
        app.write_all(&40u32.to_le_bytes()).unwrap();
        app.write_all(&[0u8; 28]).unwrap();
        app.write_all(&zip_bytes).unwrap();
        app.flush().unwrap();
        app
    }

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
        let expected_start = al_syntax::byte_col_to_utf16_col(text, name_byte_start);
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
        let expected_start = al_syntax::byte_col_to_utf16_col(line1, line1.find("Foo").unwrap());
        assert_eq!(r.col_start, expected_start);
        assert_eq!(r.col_end, r.col_start + 3);
    }

    use crate::model::{MethodSymbol, ObjectKind, ParameterSymbol};

    fn package_codeunit() -> SymbolEntry {
        SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales-Post".to_string(),
            package: "Base Application".to_string(),
            methods: vec![MethodSymbol {
                name: "PostDocument".to_string(),
                parameters: vec![ParameterSymbol {
                    name: "Preview".to_string(),
                    type_name: "Boolean".to_string(),
                    is_var: false,
                }],
                return_type: Some("Boolean".to_string()),
                attributes: Vec::new(),
                is_local: false,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn render_outline_carries_no_implementation_body() {
        // The core limitation: a package outline renders the *declaration* of a
        // method (signature, terminated by `;`) but never an implementation —
        // there is no `begin`/`end` body because `.app` packages don't ship one.
        let outline = render_outline(&package_codeunit());
        assert!(
            outline.contains("procedure PostDocument(Preview: Boolean): Boolean;"),
            "outline should render the public signature; got:\n{outline}"
        );
        assert!(
            !outline.to_ascii_lowercase().contains("begin"),
            "a package outline must not contain an implementation body; got:\n{outline}"
        );
    }

    #[test]
    fn render_outline_with_note_documents_the_limitation() {
        let entry = package_codeunit();
        let noted = render_outline_with_note(&entry);

        assert!(noted.starts_with(OUTLINE_NOTE));
        assert!(noted.contains("Reconstructed public API"));
        assert!(noted.contains("Implementation bodies are not included"));
        assert!(noted.contains("codeunit 80 \"Sales-Post\""));
        assert!(noted.contains("procedure PostDocument(Preview: Boolean): Boolean;"));

        assert!(!render_outline(&entry).starts_with(OUTLINE_NOTE));
    }

    #[test]
    fn outline_note_does_not_shadow_member_navigation() {
        let noted = render_outline_with_note(&package_codeunit());
        let r = find_member_range_in_text(&noted, "PostDocument", MemberKind::Procedure)
            .expect("member must still be locatable past the prepended note");
        let line = noted.lines().nth(r.line as usize).unwrap();
        assert!(line.contains("procedure PostDocument"));
    }

    #[test]
    #[serial_test::serial]
    fn get_or_create_writes_the_note_for_a_sourceless_package() {
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", tmp.path());

        let entry = package_codeunit();
        let path = get_or_create(&entry, None).expect("virtual file should be written");
        let content = fs::read_to_string(&path).expect("virtual file should be readable");

        match prev {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }

        assert!(
            content.contains("Implementation bodies are not included"),
            "virtual file must document the no-body limitation; got:\n{content}"
        );
        assert!(content.contains("codeunit 80 \"Sales-Post\""));
        assert!(!content.to_ascii_lowercase().contains("begin"));
    }

    #[test]
    #[serial_test::serial]
    fn materialization_downgrades_non_utf8_embedded_source_to_outline() {
        let cache = tempfile::tempdir().unwrap();
        let previous_cache = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", cache.path());

        let mut source = b"codeunit 80 \"Sales-Post\"\n{\n".to_vec();
        source.extend_from_slice(&[0xff, b'\n', b'}']);
        let app = write_app_with_raw_source(&source);
        let entry = package_codeunit();
        let materialized =
            get_or_create_with_availability(&entry, Some(app.path())).expect("outline fallback");
        let content = fs::read_to_string(&materialized.path).unwrap();

        match previous_cache {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }

        assert_eq!(
            materialized.availability,
            SourceAvailability::GeneratedOutline
        );
        assert!(content.starts_with(OUTLINE_NOTE));
        assert!(content.contains("procedure PostDocument"));
    }

    #[test]
    #[serial_test::serial]
    fn materialization_labels_identity_only_fallback() {
        let cache = tempfile::tempdir().unwrap();
        let previous_cache = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", cache.path());

        let entry = SymbolEntry {
            kind: crate::ObjectKind::Page,
            id: 50_100,
            name: "Identity Only".to_string(),
            package: "Metadata App".to_string(),
            ..Default::default()
        };
        let materialized =
            get_or_create_with_availability(&entry, None).expect("metadata fallback");
        let content = fs::read_to_string(&materialized.path).unwrap();

        match previous_cache {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }

        assert_eq!(materialized.availability, SourceAvailability::MetadataOnly);
        assert!(content.starts_with(METADATA_ONLY_NOTE));
    }

    #[test]
    #[serial_test::serial]
    fn virtual_cache_key_changes_with_symbol_metadata() {
        let cache = tempfile::tempdir().unwrap();
        let previous_cache = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", cache.path());

        let first = package_codeunit();
        let mut second = first.clone();
        second.methods[0].name = "PostReplacement".to_string();

        let first_path = get_or_create(&first, None).expect("first outline");
        let second_path = get_or_create(&second, None).expect("replacement outline");
        let second_content = fs::read_to_string(&second_path).unwrap();

        match previous_cache {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }

        assert_ne!(first_path, second_path);
        assert!(second_content.contains("procedure PostReplacement"));
        assert!(!second_content.contains("procedure PostDocument"));
    }

    #[test]
    #[serial_test::serial]
    fn virtual_cache_key_changes_with_package_path() {
        let cache = tempfile::tempdir().unwrap();
        let previous_cache = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", cache.path());

        let first_app = write_app_with_raw_source(
            b"codeunit 80 \"Sales-Post\" { procedure First() begin end; }",
        );
        let second_app = write_app_with_raw_source(
            b"codeunit 80 \"Sales-Post\" { procedure Second() begin end; }",
        );
        let entry = package_codeunit();

        let first =
            get_or_create_with_availability(&entry, Some(first_app.path())).expect("first source");
        let second = get_or_create_with_availability(&entry, Some(second_app.path()))
            .expect("replacement source");
        let second_content = fs::read_to_string(&second.path).unwrap();

        match previous_cache {
            Some(value) => std::env::set_var("XDG_CACHE_HOME", value),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }

        assert_ne!(first.path, second.path);
        assert!(second_content.contains("procedure Second"));
        assert!(!second_content.contains("procedure First"));
    }
}
