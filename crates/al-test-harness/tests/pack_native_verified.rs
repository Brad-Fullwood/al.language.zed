//! Direct CLI regression coverage for the always-on native verification gate.
//! These tests need no ALTool, `dotnet`, `alc`, service, or Business Central.

use std::path::Path;
use std::process::Command;

use al_test_harness::al_explorer_binary;

const APP_JSON: &str = r#"{
  "id":"33333333-4444-5555-6666-777777777777",
  "name":"Verified CLI", "publisher":"AL", "version":"1.0.0.0",
  "runtime":"14.0", "target":"Cloud",
  "idRanges":[{"from":50100,"to":50149}], "dependencies":[]
}"#;

fn test_dir(suffix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "al-pack-native-{suffix}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ))
}

fn make_project(root: &Path, manifest: &str, source: &str) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("app.json"), manifest).unwrap();
    std::fs::write(root.join("src/C.Codeunit.al"), source).unwrap();
}

fn pack_json(project: &Path, out: &Path) -> std::process::Output {
    Command::new(al_explorer_binary())
        .args(["pack-native", "--project"])
        .arg(project)
        .arg("--out")
        .arg(out)
        .arg("--json")
        .output()
        .expect("run al-explorer pack-native")
}

#[test]
fn valid_project_emits_a_verified_navx_app_without_microsoft_stack() {
    let dir = test_dir("valid");
    make_project(
        &dir,
        APP_JSON,
        "codeunit 50100 C { procedure P() begin end; }",
    );
    let app = dir.join("verified.app");
    let output = pack_json(&dir, &app);
    assert!(
        output.status.success(),
        "pack-native failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["success"], true);
    assert_eq!(response["backend"], "native");
    assert_eq!(response["validated"], true);
    assert_eq!(response["microsoftCompatibilityValidated"], false);
    assert_eq!(&std::fs::read(&app).unwrap()[..4], b"NAVX");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn syntax_failure_has_a_structured_range_and_writes_no_app() {
    let dir = test_dir("syntax");
    make_project(
        &dir,
        APP_JSON,
        "codeunit 50100 C { procedure P() begin if then",
    );
    let app = dir.join("rejected.app");
    let output = pack_json(&dir, &app);
    assert!(!output.status.success());
    assert!(!app.exists());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let diagnostic = response["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|diagnostic| diagnostic["code"] == "ALN0001")
        .expect("native syntax diagnostic");
    assert!(diagnostic["line"].is_number());
    assert!(diagnostic["column"].is_number());
    assert!(diagnostic["endLine"].is_number());
    assert!(diagnostic["endColumn"].is_number());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn malformed_manifest_is_a_verification_failure_and_writes_no_app() {
    let dir = test_dir("manifest");
    make_project(
        &dir,
        r#"{"id":"bad","publisher":"AL","version":"1.0"}"#,
        "codeunit 50100 C { }",
    );
    let app = dir.join("rejected.app");
    let output = pack_json(&dir, &app);
    assert!(!output.status.success());
    assert!(!app.exists());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let codes: Vec<&str> = response["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"ALN0101"),
        "missing required field: {codes:?}"
    );
    assert!(codes.contains(&"ALN0102"), "invalid GUID: {codes:?}");
    assert!(codes.contains(&"ALN0103"), "invalid version: {codes:?}");
    std::fs::remove_dir_all(dir).unwrap();
}
