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
//! Instead of relying on a checked-in golden file, this runs the real compiler
//! when it is available.
//!
//! Gated on `AL_TOOL_PATH` pointing at a dir containing `alc.dll` (e.g. an
//! installed `ms-dynamics-smb.al` extension's `bin/linux`), plus `dotnet` on
//! PATH. These external-contract tests are ignored by default and must be run
//! explicitly with `--ignored`; missing prerequisites are failures in that
//! profile. The `.app` format is a 40-byte NAVX header followed by a zip payload.

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
        "codeunit 50101 Greeter implements IGreeter\n{\n    procedure Greet(Name: Text): Text begin exit('Hi'); end;\n}",
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
        "report 50100 WidgetReport\n{\n    Caption = 'Widget Report';\n    DefaultRenderingLayout = WidgetLayout;\n    dataset { dataitem(W; Widget) { column(No; \"No.\"){} column(Name; Name){} } }\n    rendering { layout(WidgetLayout) { Type = RDLC; LayoutFile = 'layout/Widget.rdl'; Caption = 'Widget layout'; } }\n}",
    ),
    (
        "WidgetExt.TableExt.al",
        "tableextension 50101 WidgetExt extends Widget { fields { field(50100; Note; Text[50]) { Caption = 'Note'; } } }",
    ),
    (
        "WidgetCardExt.PageExt.al",
        "pageextension 50101 WidgetCardExt extends WidgetCard { layout { addlast(content) { field(Note; Rec.Note) { ApplicationArea = All; Caption = 'Note'; } } } }",
    ),
    (
        "WidgetColorExt.EnumExt.al",
        "enumextension 50101 WidgetColorExt extends Color { value(3; Cyan) { Caption = 'Cyan'; } }",
    ),
    (
        "WidgetPermsExt.PermExt.al",
        "permissionsetextension 50101 WidgetPermsExt extends WidgetAll { Permissions = tabledata Widget = D; }",
    ),
    (
        "WidgetReportExt.ReportExt.al",
        "reportextension 50101 WidgetReportExt extends WidgetReport { dataset { add(W) { column(Quantity; Qty) { } } } }",
    ),
    (
        "WidgetRoleCenter.Page.al",
        "page 50101 WidgetRoleCenter { PageType = RoleCenter; Caption = 'Widget Role Center'; }",
    ),
    (
        "Widget.Profile.al",
        "profile WidgetProfile { Caption = 'Widget Profile'; RoleCenter = WidgetRoleCenter; }",
    ),
    (
        "Widget.ProfileExt.al",
        "profileextension WidgetProfileExt extends WidgetProfile { Caption = 'Widget Profile Extension'; }",
    ),
    (
        "Widget.ControlAddIn.al",
        "controladdin WidgetAddIn { RequestedHeight = 100; MinimumHeight = 50; VerticalStretch = true; }",
    ),
];

const APP_JSON: &str = r#"{
  "id": "22222222-3333-4444-5555-666666666666",
  "name": "DiffCorpus",
  "publisher": "AL",
  "version": "1.0.0.0",
  "runtime": "15.0",
  "target": "Cloud",
  "logo": "res/logo.png",
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
    std::fs::create_dir_all(root.join("layout")).unwrap();
    std::fs::create_dir_all(root.join("res")).unwrap();
    std::fs::write(root.join("layout/Widget.rdl"), b"<Report />").unwrap();
    std::fs::write(root.join("res/logo.png"), b"PNG").unwrap();
}

/// Read a single entry out of a `.app` (skip the 40-byte NAVX header, then unzip).
fn read_app_entry(app: &Path, entry_name: &str) -> Vec<u8> {
    let bytes = std::fs::read(app).unwrap_or_else(|e| panic!("read {}: {e}", app.display()));
    assert!(
        bytes.len() > 40 && &bytes[0..4] == b"NAVX",
        "not a NAVX .app: {}",
        app.display()
    );
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
    let mut names: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect();
    names.sort();
    names
}

/// Parse a (possibly BOM-prefixed) JSON document into a normalized value.
fn parse_json(bytes: &[u8]) -> serde_json::Value {
    let s = std::str::from_utf8(bytes)
        .unwrap()
        .trim_start_matches('\u{feff}');
    serde_json::from_str(s).expect("valid JSON")
}

/// `alc` does not promise a stable file-discovery order for sibling object
/// files. Object-group member order is not a symbol-reference semantic, while
/// member/property order inside each object is. Normalize only those top-level
/// groups before comparing their object payloads.
fn normalize_symbol_reference(mut value: serde_json::Value) -> serde_json::Value {
    let Some(root) = value.as_object_mut() else {
        return value;
    };
    for group in root.values_mut() {
        let Some(items) = group.as_array_mut() else {
            continue;
        };
        if !items.iter().all(serde_json::Value::is_object) {
            continue;
        }
        items.sort_by(|left, right| {
            let identity = |item: &serde_json::Value| {
                (
                    item.get("Id").and_then(serde_json::Value::as_i64),
                    item.get("Name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                )
            };
            identity(left).cmp(&identity(right))
        });
    }
    value
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            std::fs::copy(&source_path, &destination_path).unwrap();
        }
    }
}

fn copy_package_cache(source: &Path, destination: &Path) -> usize {
    std::fs::create_dir_all(destination).unwrap();
    let mut copied = 0;
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let source_path = entry.path();
        if source_path.extension().and_then(|ext| ext.to_str()) != Some("app") {
            continue;
        }
        std::fs::copy(&source_path, destination.join(entry.file_name())).unwrap();
        copied += 1;
    }
    copied
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
#[ignore = "requires AL_TOOL_PATH/alc.dll and dotnet; run with --ignored"]
fn native_emit_matches_alc() {
    let dll = alc_dll().expect("AL_TOOL_PATH/alc.dll and a working dotnet host are required");
    let tmp = std::env::temp_dir().join(format!("al-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    write_project(&tmp);

    // 1) Reference: alc.
    let alc_app = tmp.join("alc.app");
    let out = Command::new("dotnet")
        .arg(&dll)
        .arg(format!("/project:{}", tmp.display()))
        .arg(format!("/out:{}", alc_app.display()))
        .arg(format!(
            "/packagecachepath:{}",
            tmp.join(".alpackages").display()
        ))
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
    assert_eq!(
        entry_names(&alc_app),
        entry_names(&native_app),
        "zip entry sets differ"
    );

    // 3b) SymbolReference.json semantically identical.
    let alc_sym = normalize_symbol_reference(parse_json(&read_app_entry(
        &alc_app,
        "SymbolReference.json",
    )));
    let native_sym = normalize_symbol_reference(parse_json(&read_app_entry(
        &native_app,
        "SymbolReference.json",
    )));
    assert_eq!(
        alc_sym, native_sym,
        "SymbolReference.json differs between alc and native emitter"
    );

    // 3c) NavxManifest.xml identical except the <Build> provenance line.
    let alc_manifest = manifest_without_build(&read_app_entry(&alc_app, "NavxManifest.xml"));
    let native_manifest = manifest_without_build(&read_app_entry(&native_app, "NavxManifest.xml"));
    assert_eq!(
        alc_manifest, native_manifest,
        "NavxManifest.xml differs (ignoring <Build>)"
    );

    // The supported generated assets are deterministic content, not merely
    // present archive entries. (The control-addin ZIP itself has ZIP metadata,
    // so its nested content is covered by the emitter unit tests.)
    for entry in [
        "layout/layout/Widget.rdl",
        "logo/logo.png",
        "ProfileSymbolReferences/WidgetProfile.json",
        "ProfileSymbolReferences/WidgetProfileExt.json",
        "addin/controladdins.dock",
        "navigation.xml",
    ] {
        assert_eq!(
            read_app_entry(&alc_app, entry),
            read_app_entry(&native_app, entry),
            "generated asset differs: {entry}"
        );
    }
    assert_eq!(
        read_app_entry(&alc_app, "TextData/DiffCorpus.TextData.en-US.xliff"),
        read_app_entry(&native_app, "TextData/DiffCorpus.TextData.en-US.xliff"),
        "XLIFF sources, trans-unit IDs/order, or metadata differ"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!(
        "OK: native .app matches alc {} for {} object kinds",
        dll.display(),
        CORPUS.len()
    );
}

#[test]
#[ignore = "requires AL_TOOL_PATH/alc.dll and dotnet; run with --ignored"]
fn native_emit_resolves_declared_alc_dependency_like_alc() {
    let dll = alc_dll().expect("AL_TOOL_PATH/alc.dll and a working dotnet host are required");
    let tmp = std::env::temp_dir().join(format!("al-dependency-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let dependency = tmp.join("dependency");
    let dependent = tmp.join("dependent");
    std::fs::create_dir_all(dependency.join("src")).unwrap();
    std::fs::create_dir_all(dependency.join(".alpackages")).unwrap();
    std::fs::write(
        dependency.join("app.json"),
        r#"{"id":"11111111-2222-3333-4444-555555555555","name":"Dependency","publisher":"AL","version":"1.0.0.0","runtime":"15.0","target":"Cloud","idRanges":[{"from":50100,"to":50149}],"dependencies":[]}"#,
    )
    .unwrap();
    std::fs::write(
        dependency.join("src/Dependency.al"),
        "table 50100 \"Dependency Widget\" { fields { field(1; No; Code[20]) {} } keys { key(PK; No) { Clustered = true; } } }",
    )
    .unwrap();
    let dependency_app = dependency.join("Dependency.app");
    let output = Command::new("dotnet")
        .arg(&dll)
        .arg(format!("/project:{}", dependency.display()))
        .arg(format!("/out:{}", dependency_app.display()))
        .arg(format!(
            "/packagecachepath:{}",
            dependency.join(".alpackages").display()
        ))
        .output()
        .expect("run alc for dependency");
    assert!(
        dependency_app.is_file(),
        "dependency alc failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    std::fs::create_dir_all(dependent.join("src")).unwrap();
    std::fs::create_dir_all(dependent.join(".alpackages")).unwrap();
    std::fs::copy(
        &dependency_app,
        dependent.join(".alpackages/Dependency.app"),
    )
    .unwrap();
    std::fs::write(
        dependent.join("app.json"),
        r#"{"id":"99999999-2222-3333-4444-555555555555","name":"Dependent","publisher":"AL","version":"1.0.0.0","runtime":"15.0","target":"Cloud","idRanges":[{"from":50100,"to":50149}],"dependencies":[{"id":"11111111-2222-3333-4444-555555555555","name":"Dependency","publisher":"AL","version":"1.0.0.0"}]}"#,
    )
    .unwrap();
    std::fs::write(
        dependent.join("src/UsesDependency.al"),
        "codeunit 50100 UsesDependency { procedure Accept(var Value: Record \"Dependency Widget\") begin end; }",
    )
    .unwrap();
    let alc_app = dependent.join("alc.app");
    let output = Command::new("dotnet")
        .arg(&dll)
        .arg(format!("/project:{}", dependent.display()))
        .arg(format!("/out:{}", alc_app.display()))
        .arg(format!(
            "/packagecachepath:{}",
            dependent.join(".alpackages").display()
        ))
        .output()
        .expect("run alc for dependent");
    assert!(
        alc_app.is_file(),
        "dependent alc failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let native_app = dependent.join("native.app");
    let output = Command::new(al_explorer_binary())
        .args(["pack-native", "--project"])
        .arg(&dependent)
        .arg("--out")
        .arg(&native_app)
        .output()
        .expect("run native dependent build");
    assert!(
        native_app.is_file(),
        "native dependent build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(entry_names(&alc_app), entry_names(&native_app));
    assert_eq!(
        normalize_symbol_reference(parse_json(&read_app_entry(
            &alc_app,
            "SymbolReference.json"
        ))),
        normalize_symbol_reference(parse_json(&read_app_entry(
            &native_app,
            "SymbolReference.json"
        ))),
        "dependency-qualified SymbolReference.json differs"
    );
    assert_eq!(
        manifest_without_build(&read_app_entry(&alc_app, "NavxManifest.xml")),
        manifest_without_build(&read_app_entry(&native_app, "NavxManifest.xml")),
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore = "requires AL_TOOL_PATH, AL_PACKAGE_CACHE_PATH, and dotnet; run with --ignored"]
fn native_emit_matches_alc_for_base_app_bindings_and_resources() {
    let dll = alc_dll().expect("AL_TOOL_PATH/alc.dll and a working dotnet host are required");
    let package_cache = std::env::var_os("AL_PACKAGE_CACHE_PATH")
        .map(PathBuf::from)
        .expect("AL_PACKAGE_CACHE_PATH is required for the Base Application differential");
    assert!(
        package_cache.is_dir(),
        "AL_PACKAGE_CACHE_PATH is not a directory: {}",
        package_cache.display()
    );

    let tmp = std::env::temp_dir().join(format!("al-base-app-diff-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("emit_base_app_project");
    copy_tree(&fixture, &tmp);
    let copied = copy_package_cache(&package_cache, &tmp.join(".alpackages"));
    assert!(
        copied > 0,
        "AL_PACKAGE_CACHE_PATH contains no top-level .app packages"
    );

    let alc_app = tmp.join("alc.app");
    let output = Command::new("dotnet")
        .arg(&dll)
        .arg(format!("/project:{}", tmp.display()))
        .arg(format!("/out:{}", alc_app.display()))
        .arg(format!(
            "/packagecachepath:{}",
            tmp.join(".alpackages").display()
        ))
        .output()
        .expect("run alc for Base Application fixture");
    assert!(
        alc_app.is_file(),
        "Base Application fixture alc failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let native_app = tmp.join("native.app");
    let output = Command::new(al_explorer_binary())
        .args(["pack-native", "--project"])
        .arg(&tmp)
        .arg("--out")
        .arg(&native_app)
        .output()
        .expect("run native Base Application fixture build");
    assert!(
        native_app.is_file(),
        "native Base Application fixture build failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        entry_names(&alc_app),
        entry_names(&native_app),
        "Base Application fixture archive entry sets differ"
    );
    assert_eq!(
        normalize_symbol_reference(parse_json(&read_app_entry(
            &alc_app,
            "SymbolReference.json"
        ))),
        normalize_symbol_reference(parse_json(&read_app_entry(
            &native_app,
            "SymbolReference.json"
        ))),
        "Base Application fixture SymbolReference.json differs"
    );
    assert_eq!(
        manifest_without_build(&read_app_entry(&alc_app, "NavxManifest.xml")),
        manifest_without_build(&read_app_entry(&native_app, "NavxManifest.xml")),
        "Base Application fixture NavxManifest.xml differs (ignoring <Build>)"
    );
    for entry in ["layout/layout/EmitterCustomer.rdl", "logo/logo.png"] {
        assert_eq!(
            read_app_entry(&alc_app, entry),
            read_app_entry(&native_app, entry),
            "Base Application fixture resource differs: {entry}"
        );
    }
    let xliff_entry = "TextData/Emitter%20Base%20App%20Differential.TextData.en-US.xliff";
    assert_eq!(
        read_app_entry(&alc_app, xliff_entry),
        read_app_entry(&native_app, xliff_entry),
        "Base Application XLIFF sources, trans-unit IDs/order, or metadata differ"
    );

    let _ = std::fs::remove_dir_all(&tmp);
    eprintln!(
        "OK: Base Application control/property/page-customization bindings and resources match alc {}",
        dll.display()
    );
}
