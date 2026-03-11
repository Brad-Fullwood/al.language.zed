//! Virtual AL file generation from package symbols.
//!
//! Generates read-only `.al` source files from `SymbolEntry` data so that
//! go-to-definition can navigate into package objects (`.app` files).
//! Files are cached at `~/.cache/al-lsp/symbols/{package}/`.

use std::path::PathBuf;

use crate::{MethodSymbol, ObjectKind, SymbolEntry};

/// Cache directory for generated virtual AL files.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("symbols")
}

/// Get or create a virtual AL file for a package symbol entry.
/// Returns the file path on disk.
pub fn get_or_create(entry: &SymbolEntry) -> std::io::Result<PathBuf> {
    let pkg_dir = cache_dir().join(sanitize_filename(&entry.package));
    let filename = format!("{} {} {}.al", entry.kind, entry.id, entry.name);
    let file_path = pkg_dir.join(sanitize_filename(&filename));

    if !file_path.exists() {
        let source = generate_al(entry);
        std::fs::create_dir_all(&pkg_dir)?;
        std::fs::write(&file_path, &source)?;
    }

    Ok(file_path)
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
    let mut out = String::with_capacity(4096);

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
            if !entry.fields.is_empty() {
                out.push_str("    fields\n    {\n");
                for f in &entry.fields {
                    let fname = quote_name(&f.name);
                    let ftype = if f.type_name.is_empty() { "Text" } else { &f.type_name };
                    out.push_str(&format!(
                        "        field({}; {}; {})\n        {{\n        }}\n",
                        f.id, fname, ftype
                    ));
                }
                out.push_str("    }\n\n");
            }
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
        ObjectKind::Enum => {
            out.push_str(&format!("enum {} {}\n{{\n", entry.id, name_quoted));
            for v in &entry.enum_values {
                out.push_str(&format!(
                    "    value({}; {}) {{ }}\n",
                    v.ordinal,
                    quote_name(&v.name)
                ));
            }
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
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
        _ => {
            out.push_str(&format!("{} {} {}\n{{\n", kind_lower, entry.id, name_quoted));
            write_methods(&mut out, &entry.methods);
            out.push_str("}\n");
        }
    }

    out
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
                FieldSymbol { id: 1, name: "No.".to_string(), type_name: "Code[20]".to_string() },
                FieldSymbol { id: 2, name: "Name".to_string(), type_name: "Text[100]".to_string() },
            ],
            controls: vec![],
            enum_values: vec![],
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
        };
        let path = get_or_create(&entry).unwrap();
        assert!(path.exists());
        assert!(path.to_string_lossy().contains("Microsoft.Application"));
        assert!(path.to_string_lossy().ends_with(".al"));
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }
}
