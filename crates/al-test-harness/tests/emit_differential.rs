//! Differential test: the native pure-Rust `.app` emitter (`al-explorer
//! pack-native`) vs Microsoft's reference compiler `alc`.
//!
//! Both compile the SAME self-contained AL project (no BC/base-app symbols, so
//! `alc` needs no package cache) and we assert the produced `.app`s are
//! structurally identical:
//!   - the zip entry set matches,
//!   - `SymbolReference.json` is semantically identical (the symbol table alc
//!     publishes — methods, fields, ids, the FNV method-id hashes, …),
//!   - `NavxManifest.xml` matches except the unavoidable `<Build>` provenance
//!     line (timestamp + compiler name differ by construction).
//!
//! This is the live, end-to-end form of gap B3 ("differential-test against
//! alc"): instead of a checked-in golden it runs the real alc when present.
//!
//! Gated on `AL_TOOL_PATH` pointing at a dir containing `alc.dll` (e.g. an
//! installed `ms-dynamics-smb.al` extension's `bin/linux`), plus `dotnet` on
//! PATH. Skips cleanly otherwise (the common CI case). The `.app` format is a
//! 40-byte NAVX header followed by a zip payload.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use al_test_harness::al_explorer_binary;

/// One self-contained AL source file: (relative path under `src/`, contents).
/// Every object here compiles with no `.alpackages` — only primitive/System
/// types and references within the project.
const CORPUS: &[(&str, &str)] = &[
    (
        "Color.Enum.al",
        "enum 50100 Color { Extensible = true; value(0; Red){Caption='Red';} value(1; Green){Caption='Green';} }",
    ),
    (
        "MoreColor.EnumExt.al",
        "enumextension 50100 MoreColor extends Color { value(2; Blue){Caption='Blue';} }",
    ),
    (
        "IGreeter.Interface.al",
        "interface IGreeter { procedure Greet(Name: Text): Text; }",
    ),
    (
        "Greeter.Codeunit.al",
        "codeunit 50101 Greeter implements IGreeter\n{\n    procedure Greet(Name: Text): Text begin exit('Hi ' + Name); end;\n}",
    ),
    (
        "Widget.Table.al",
        "table 50100 Widget\n{\n    fields { field(1;\"No.\";Code[20]){} field(2;Name;Text[100]){} field(3;Qty;Decimal){} field(4;Kind;Enum Color){} }\n    keys { key(PK;\"No.\"){Clustered=true;} }\n}",
    ),
    (
        "WidgetCard.Page.al",
        "page 50100 WidgetCard\n{\n    PageType = Card; SourceTable = Widget; ApplicationArea = All;\n    layout { area(content) { group(General) { field(No; Rec.\"No.\"){} field(Name; Rec.Name){} field(Qty; Rec.Qty){} } } }\n    actions { area(processing) { action(DoIt) { ApplicationArea = All; trigger OnAction() begin Rec.Qty += 1; end; } } }\n}",
    ),
    (
        "WidgetQuery.Query.al",
        "query 50100 WidgetQuery\n{\n    QueryType = Normal;\n    elements { dataitem(W; Widget) { column(No; \"No.\"){} column(Qty; Qty){} } }\n}",
    ),
    (
        "WidgetAll.Perm.al",
        "permissionset 50100 WidgetAll { Assignable = true; Caption = 'Widget All'; Permissions = tabledata Widget = RIMD, table Widget = X; }",
    ),
    (
        "WidgetPort.Xmlport.al",
        "xmlport 50100 WidgetPort\n{\n    schema { textelement(Root) { tableelement(W; Widget) { fieldelement(No; W.\"No.\"){} fieldelement(Name; W.Name){} } } }\n}",
    ),
    (
        "WidgetReport.Report.al",
        "report 50100 WidgetReport\n{\n    dataset { dataitem(W; Widget) { column(No; \"No.\"){} column(Name; Name){} } }\n}",
    ),
];

const APP_JSON: &str = r#"{
  "id": "22222222-3333-4444-5555-666666666666",
  "name": "DiffCorpus",
  "publisher": "AL",
  "version": "1.0.0.0",
  "runtime": "14.0",
  "target": "Cloud",
  "idRanges": [{ "from": 50100, "to": 50199 }],
  "dependencies": []
}"#;

/// Locate `alc.dll` from `AL_TOOL_PATH`, returning the dll path if a live run is
/// possible (the dll exists and `dotnet` is on PATH).
fn alc_dll() -> Option<PathBuf> {
    let dir = std::env::var_os("AL_TOOL_PATH")?;
    let dll = PathBuf::from(dir).join("alc.dll");
    if !dll.is_file() {
        return None;
    }
    // `dotnet --version` must succeed (alc is run as `dotnet alc.dll`).
    Command::new("dotnet")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(dll)
}

fn write_project(root: &Path) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(root.join(".alpackages")).unwrap();
    std::fs::write(root.join("app.json"), APP_JSON).unwrap();
    for (name, body) in CORPUS {
        std::fs::write(src.join(name), body).unwrap();
    }
}

/// Read a single entry out of a `.app` (skip the 40-byte NAVX header, then unzip).
fn read_app_entry(app: &Path, entry_name: &str) -> Vec<u8> {
    let bytes = std::fs::read(app).unwrap_or_else(|e| panic!("read {}: {e}", app.display()));
    assert!(bytes.len() > 40 && &bytes[0..4] == b"NAVX", "not a NAVX .app: {}", app.display());
    let zip_bytes = bytes[40..].to_vec();
    let reader = std::io::Cursor::new(zip_bytes);
    let mut zip = zip::ZipArchive::new(reader).expect("parse zip payload");
    let mut f = zip
        .by_name(entry_name)
        .unwrap_or_else(|_| panic!("entry {entry_name} missing in {}", app.display()));
    let mut out = Vec::new();
    f.read_to_end(&mut out).unwrap();
    out
}

fn entry_names(app: &Path) -> Vec<String> {
    let bytes = std::fs::read(app).unwrap();
    let reader = std::io::Cursor::new(bytes[40..].to_vec());
    let mut zip = zip::ZipArchive::new(reader).unwrap();
    let mut names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect();
    names.sort();
    names
}

/// Parse a (possibly BOM-prefixed) JSON document into a normalized value.
fn parse_json(bytes: &[u8]) -> serde_json::Value {
    let s = std::str::from_utf8(bytes).unwrap().trim_start_matches('\u{feff}');
    serde_json::from_str(s).expect("valid JSON")
}

/// Drop the single `<Build .../>` line (timestamp + compiler version differ by
/// construction between alc and the native emitter).
fn manifest_without_build(xml: &[u8]) -> String {
    std::str::from_utf8(xml)
        .unwrap()
        .lines()
        .filter(|l| !l.trim_start().starts_with("<Build "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn native_emit_matches_alc() {
    let Some(dll) = alc_dll() else {
        eprintln!("SKIP: AL_TOOL_PATH/alc.dll + dotnet not available — alc differential skipped");
        return;
    };
    let tmp = std::env::temp_dir().join(format!("al-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    write_project(&tmp);

    // 1) Reference: alc.
    let alc_app = tmp.join("alc.app");
    let out = Command::new("dotnet")
        .arg(&dll)
        .arg(format!("/project:{}", tmp.display()))
        .arg(format!("/out:{}", alc_app.display()))
        .arg(format!("/packagecachepath:{}", tmp.join(".alpackages").display()))
        .output()
        .expect("run alc");
    assert!(
        alc_app.is_file(),
        "alc did not produce an .app:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // 2) Native emitter.
    let native_app = tmp.join("native.app");
    let out = Command::new(al_explorer_binary())
        .arg("pack-native")
        .arg("--project")
        .arg(&tmp)
        .arg("--out")
        .arg(&native_app)
        .output()
        .expect("run al-explorer pack-native");
    assert!(
        native_app.is_file(),
        "pack-native failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // 3a) Same entry set.
    assert_eq!(entry_names(&alc_app), entry_names(&native_app), "zip entry sets differ");

    // 3b) SymbolReference.json semantically identical.
    let alc_sym = parse_json(&read_app_entry(&alc_app, "SymbolReference.json"));
    let native_sym = parse_json(&read_app_entry(&native_app, "SymbolReference.json"));
    assert_eq!(
        alc_sym, native_sym,
        "SymbolReference.json differs between alc and native emitter"
    );

    // 3c) NavxManifest.xml identical except the <Build> provenance line.
    let alc_manifest = manifest_without_build(&read_app_entry(&alc_app, "NavxManifest.xml"));
    let native_manifest = manifest_without_build(&read_app_entry(&native_app, "NavxManifest.xml"));
    assert_eq!(alc_manifest, native_manifest, "NavxManifest.xml differs (ignoring <Build>)");

    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!("OK: native .app matches alc {} for {} object kinds", dll.display(), CORPUS.len());
}
