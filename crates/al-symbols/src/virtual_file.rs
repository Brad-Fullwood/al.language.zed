//! Virtual AL file extraction / generation for package symbols.
//!
//! When the `.app` package contains actual `.al` source files, the matching
//! source is extracted and cached. Otherwise a stub is generated from the
//! `SymbolEntry` data. Files are cached at `~/.cache/al-lsp/symbols/{package}/`.

use std::io::{Cursor, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::{MethodSymbol, ObjectKind, SymbolEntry};

/// Cache directory for extracted / generated virtual AL files.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("symbols")
}

/// Get or create a virtual AL file for a package symbol entry.
///
/// When `app_path` is provided, the real `.al` source is extracted from the
/// `.app` ZIP archive if available. Falls back to generating a stub from
/// symbol data.
pub fn get_or_create(
    entry: &SymbolEntry,
    app_path: Option<&Path>,
    allow_outline_fallback: bool,
) -> std::io::Result<PathBuf> {
    let pkg_dir = cache_dir().join(sanitize_filename(&entry.package));
    let filename = format!("{} {} {}.al", entry.kind, entry.id, entry.name);
    let file_path = pkg_dir.join(sanitize_filename(&filename));

    if !file_path.exists() {
        // Try extracting real source from the .app first
        let extracted = app_path.and_then(|path| extract_source_from_app(path, entry));

        let source = match extracted {
            Some(src) => src,
            None if allow_outline_fallback => generate_al(entry),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no source in .app for {} \"{}\" and outline generation not approved", entry.kind, entry.name),
                ));
            }
        };

        std::fs::create_dir_all(&pkg_dir)?;
        std::fs::write(&file_path, &source)?;
        // Mark read-only so the editor prevents accidental edits
        std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o444))?;
    }

    Ok(file_path)
}

/// Try to extract the matching `.al` source file from an `.app` ZIP archive.
///
/// Returns `None` if the `.app` has no source files or the matching file isn't found.
fn extract_source_from_app(app_path: &Path, entry: &SymbolEntry) -> Option<String> {
    let data = std::fs::read(app_path).ok()?;

    // Find ZIP offset (skip NAVX header)
    let zip_offset = (4..data.len().saturating_sub(3))
        .find(|&i| &data[i..i + 4] == b"\x50\x4B\x03\x04")?;

    let cursor = Cursor::new(&data[zip_offset..]);
    let mut archive = zip::ZipArchive::new(cursor).ok()?;

    // Build match targets from the entry
    let kind_str = entry.kind.to_string();
    let name_nospace = entry.name.replace(' ', "").to_lowercase();
    let kind_lower = kind_str.to_lowercase();

    // Search for a matching .al file in the archive.
    // File naming: PascalCase with no spaces (e.g., "ItemJournal.Page.al")
    // Symbol names have spaces (e.g., "Item Journal")
    let mut best_match: Option<String> = None;
    for i in 0..archive.len() {
        let file = archive.by_index(i).ok()?;
        let file_name = file.name().to_string();
        if !file_name.to_lowercase().ends_with(".al") {
            continue;
        }

        let basename = file_name.rsplit('/').next().unwrap_or(&file_name);
        let basename_lower = basename.to_lowercase();
        let basename_nospace = basename_lower.replace(' ', "");

        // Match pattern: "{Name}.{Kind}.al" (e.g., "ItemJournal.Page.al")
        // Compare without spaces since filenames use PascalCase but symbols have spaces
        if basename_nospace.contains(&name_nospace) && basename_lower.contains(&kind_lower) {
            best_match = Some(file_name);
            break;
        }
    }

    let match_name = best_match?;
    let mut file = archive.by_name(&match_name).ok()?;
    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;

    tracing::debug!(
        app = %app_path.display(),
        zip_entry = %match_name,
        object = %entry.name,
        "extracted real source from .app"
    );

    Some(content)
}

/// Check whether an `.app` file contains any `.al` source files.
pub fn app_has_source(app_path: &Path) -> bool {
    let Ok(data) = std::fs::read(app_path) else {
        return false;
    };
    let Some(zip_offset) = (4..data.len().saturating_sub(3))
        .find(|&i| &data[i..i + 4] == b"\x50\x4B\x03\x04")
    else {
        return false;
    };
    let Ok(archive) = zip::ZipArchive::new(Cursor::new(&data[zip_offset..])) else {
        return false;
    };
    (0..archive.len()).any(|i| {
        archive
            .name_for_index(i)
            .is_some_and(|n| n.to_lowercase().ends_with(".al"))
    })
}

/// Sanitize a string for use as a filename.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

/// Generate AL source from a SymbolEntry for read-only navigation.
pub fn generate_al(entry: &SymbolEntry) -> String {
    let mut out = String::with_capacity(8192);

    // Header comment
    out.push_str(&format!(
        "// Generated from package: {}\n// This file is read-only \u{2014} it shows the public API surface.\n\n",
        entry.package
    ));

    let kind_lower = entry.kind.to_string().to_lowercase();
    let name_quoted = quote_name(&entry.name);

    match entry.kind {
        ObjectKind::Table => {
            out.push_str(&format!("table {} {}\n{{\n", entry.id, name_quoted));
            write_object_properties(&mut out, &entry.properties);
            write_fields(&mut out, &entry.fields);
            write_keys(&mut out, &entry.keys);
            write_variables(&mut out, &entry.variables);
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
        ObjectKind::Enum => {
            out.push_str(&format!("enum {} {}\n{{\n", entry.id, name_quoted));
            write_object_properties(&mut out, &entry.properties);
            for v in &entry.enum_values {
                out.push_str(&format!(
                    "    value({}; {}) {{ }}\n",
                    v.ordinal,
                    quote_name(&v.name)
                ));
            }
            out.push('\n');
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
        ObjectKind::Interface => {
            out.push_str(&format!("interface {}\n{{\n", name_quoted));
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
        ObjectKind::Page
        | ObjectKind::Codeunit
        | ObjectKind::Report
        | ObjectKind::Query
        | ObjectKind::XmlPort => {
            out.push_str(&format!("{} {} {}\n{{\n", kind_lower, entry.id, name_quoted));
            write_object_properties(&mut out, &entry.properties);
            write_variables(&mut out, &entry.variables);
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
        _ => {
            out.push_str(&format!("{} {} {}\n{{\n", kind_lower, entry.id, name_quoted));
            write_object_properties(&mut out, &entry.properties);
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
    }

    out
}

/// Write object-level properties (Caption, LookupPageID, etc.).
fn write_object_properties(out: &mut String, properties: &[crate::PropertyValue]) {
    if properties.is_empty() {
        return;
    }
    for p in properties {
        let val = escape_property_value(&p.value);
        out.push_str(&format!("    {} = {};\n", p.name, val));
    }
    out.push('\n');
}

/// Write fields with their properties.
fn write_fields(out: &mut String, fields: &[crate::FieldSymbol]) {
    if fields.is_empty() {
        return;
    }
    out.push_str("    fields\n    {\n");
    for f in fields {
        let fname = quote_name(&f.name);
        let ftype = if f.type_name.is_empty() { "Text" } else { &f.type_name };
        out.push_str(&format!("        field({}; {}; {})\n", f.id, fname, ftype));
        out.push_str("        {\n");
        for p in &f.properties {
            let val = escape_property_value(&p.value);
            out.push_str(&format!("            {} = {};\n", p.name, val));
        }
        out.push_str("        }\n");
    }
    out.push_str("    }\n\n");
}

/// Write keys section.
fn write_keys(out: &mut String, keys: &[crate::KeySymbol]) {
    if keys.is_empty() {
        return;
    }
    out.push_str("    keys\n    {\n");
    for k in keys {
        let fields: Vec<String> = k.field_names.iter().map(|f| quote_name(f)).collect();
        out.push_str(&format!("        key({}; {})\n", k.name, fields.join(", ")));
        out.push_str("        {\n");
        for p in &k.properties {
            let val = escape_property_value(&p.value);
            out.push_str(&format!("            {} = {};\n", p.name, val));
        }
        out.push_str("        }\n");
    }
    out.push_str("    }\n\n");
}

/// Write variable declarations.
fn write_variables(out: &mut String, variables: &[crate::VariableSymbol]) {
    let public_vars: Vec<_> = variables.iter().filter(|v| !v.is_protected).collect();
    if public_vars.is_empty() {
        return;
    }
    out.push_str("    var\n");
    for v in &public_vars {
        out.push_str(&format!("        {}: {};\n", v.name, v.type_name));
    }
    out.push('\n');
}

/// Escape a property value for AL syntax.
fn escape_property_value(val: &str) -> String {
    // If it looks like a boolean or number, don't quote
    if val == "0" || val == "1" || val.eq_ignore_ascii_case("true") || val.eq_ignore_ascii_case("false") {
        return val.to_string();
    }
    // If it already has quotes or is a complex expression, use as-is
    if val.contains('"') || val.contains('\'') || val.contains('\n') || val.contains('\r') {
        return format!("'{}'", val.replace('\'', "''"));
    }
    // Simple string values get single quotes
    format!("'{}'", val)
}

/// Write public method declarations.
fn write_methods(out: &mut String, methods: &[MethodSymbol]) {
    for m in methods.iter().filter(|m| !m.is_local) {
        for attr in &m.attributes {
            if attr.arguments.is_empty() {
                out.push_str(&format!("    [{}]\n", attr.name));
            } else {
                out.push_str(&format!("    [{}({})]\n", attr.name, attr.arguments.join(", ")));
            }
        }

        let params: Vec<String> = m
            .parameters
            .iter()
            .map(|p| {
                if p.is_var {
                    format!("var {}: {}", p.name, p.type_name)
                } else {
                    format!("{}: {}", p.name, p.type_name)
                }
            })
            .collect();

        let ret = m
            .return_type
            .as_deref()
            .map(|r| format!(": {}", r))
            .unwrap_or_default();
        out.push_str(&format!(
            "    procedure {}({}){}\n    begin\n    end;\n\n",
            m.name,
            params.join("; "),
            ret
        ));
    }
}

/// Quote a name if it contains spaces or special characters.
fn quote_name(name: &str) -> String {
    if name.contains(' ') || name.contains('.') || name.contains('/') {
        format!("\"{}\"", name)
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EnumValueSymbol, FieldSymbol, MethodSymbol, ParameterSymbol};

    #[test]
    fn generate_table_with_fields() {
        let entry = SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            package: "Microsoft.Application".to_string(),
            methods: vec![],
            fields: vec![
                FieldSymbol { id: 1, name: "No.".to_string(), type_name: "Code[20]".to_string(), properties: vec![] },
                FieldSymbol { id: 2, name: "Name".to_string(), type_name: "Text[100]".to_string(), properties: vec![] },
            ],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        let al = generate_al(&entry);
        assert!(al.contains("table 18 Customer"));
        assert!(al.contains("field(1; \"No.\"; Code[20])"));
        assert!(al.contains("field(2; Name; Text[100])"));
    }

    #[test]
    fn generate_enum_with_values() {
        let entry = SymbolEntry {
            kind: ObjectKind::Enum,
            id: 50100,
            name: "IJL Status".to_string(),
            extends: None,
            package: "test".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![
                EnumValueSymbol { ordinal: 0, name: "Pending".to_string() },
                EnumValueSymbol { ordinal: 1, name: "Posted".to_string() },
            ],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        let al = generate_al(&entry);
        assert!(al.contains("enum 50100 \"IJL Status\""));
        assert!(al.contains("value(0; Pending)"));
        assert!(al.contains("value(1; Posted)"));
    }

    #[test]
    fn generate_codeunit_with_methods() {
        let entry = SymbolEntry {
            kind: ObjectKind::Codeunit,
            id: 80,
            name: "Sales Post".to_string(),
            extends: None,
            package: "Microsoft.Application".to_string(),
            methods: vec![
                MethodSymbol {
                    name: "Run".to_string(),
                    parameters: vec![
                        ParameterSymbol { name: "SalesHeader".to_string(), type_name: "Record \"Sales Header\"".to_string(), is_var: true },
                    ],
                    return_type: None,
                    attributes: vec![],
                    is_local: false,
                },
                MethodSymbol {
                    name: "InternalHelper".to_string(),
                    parameters: vec![],
                    return_type: None,
                    attributes: vec![],
                    is_local: true, // should be excluded
                },
            ],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        let al = generate_al(&entry);
        assert!(al.contains("codeunit 80 \"Sales Post\""));
        assert!(al.contains("procedure Run(var SalesHeader: Record \"Sales Header\")"));
        assert!(!al.contains("InternalHelper")); // local methods excluded
    }

    #[test]
    fn virtual_file_cache_path() {
        let entry = SymbolEntry {
            kind: ObjectKind::Table,
            id: 18,
            name: "Customer".to_string(),
            extends: None,
            package: "Microsoft.Application".to_string(),
            methods: vec![],
            fields: vec![],
            controls: vec![],
            enum_values: vec![],
            keys: vec![],
            properties: vec![],
            variables: vec![],
        };
        let path = get_or_create(&entry, None, true).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("Microsoft.Application"));
        assert!(path.to_string_lossy().ends_with(".al"));
        // Verify the file is read-only
        let perms = std::fs::metadata(&path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o444, "virtual file should be read-only");
        // Cleanup — make writable first so remove succeeds
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_file(&path).unwrap();
    }
}
