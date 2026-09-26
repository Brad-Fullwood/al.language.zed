//! Fixtures shared by the `index` submodule tests.

use crate::model::{ObjectKind, SymbolEntry};

pub(super) fn make_entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
    SymbolEntry {
        kind,
        id,
        name: name.to_string(),
        package: "TestPkg".to_string(),
        ..Default::default()
    }
}

pub(super) fn make_extension(kind: ObjectKind, id: i32, name: &str, extends: &str) -> SymbolEntry {
    SymbolEntry {
        kind,
        id,
        name: name.to_string(),
        extends: Some(extends.to_string()),
        package: "TestPkg".to_string(),
        ..Default::default()
    }
}

pub(super) fn build_app(name: &str, table_id: i32, table_name: &str) -> Vec<u8> {
    build_app_with_id(
        "00000000-0000-0000-0000-000000000001",
        name,
        table_id,
        table_name,
    )
}

pub(super) fn build_app_with_id(
    app_id: &str,
    name: &str,
    table_id: i32,
    table_name: &str,
) -> Vec<u8> {
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    let manifest = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Package><App Id="{app_id}" Name="{name}" Publisher="Contoso" Version="1.0.0.0" /></Package>"#
    );
    let symbols = format!(
        r#"{{ "Tables": [ {{ "Id": {table_id}, "Name": "{table_name}", "Fields": [], "Methods": [] }} ] }}"#
    );

    let mut data = Vec::new();
    data.extend_from_slice(b"NAVX");
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&[0u8; 32]);

    let mut zip_buf = Vec::new();
    {
        let mut zip = zip::ZipWriter::new(Cursor::new(&mut zip_buf));
        let opts = SimpleFileOptions::default();
        zip.start_file("NavxManifest.xml", opts).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("SymbolReference.json", opts).unwrap();
        zip.write_all(symbols.as_bytes()).unwrap();
        zip.finish().unwrap();
    }
    data.extend_from_slice(&zip_buf);
    data
}
