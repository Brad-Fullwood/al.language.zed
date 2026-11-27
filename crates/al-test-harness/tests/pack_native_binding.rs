//! `al-explorer pack-native` refuses a project whose bodies name something
//! that does not exist, with the codes and messages `alc` prints (journey-02,
//! journey-03). The fixture sources and the expected messages come from a
//! recorded alc run, see crates/al-emit/tests/fixtures/alc-binding.

use std::path::{Path, PathBuf};
use std::process::Command;

use al_test_harness::al_explorer_binary;

const APP_JSON: &str = r#"{ "id": "33333333-4444-5555-6666-777777777778", "name": "BindProj", "publisher": "AL", "version": "1.0.0.0", "runtime": "14.0", "target": "Cloud", "idRanges": [{ "from": 50100, "to": 50149 }], "dependencies": [] }"#;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../al-emit/tests/fixtures/alc-binding")
}

fn project(name: &str, codeunit: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("al-bind-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join(".alpackages")).unwrap();
    std::fs::write(dir.join("app.json"), APP_JSON).unwrap();
    std::fs::copy(
        fixtures().join("JourneyItem.Table.al"),
        dir.join("src/JourneyItem.Table.al"),
    )
    .unwrap();
    std::fs::write(dir.join("src/C.Codeunit.al"), codeunit).unwrap();
    dir
}

fn pack(dir: &Path, out: &Path) -> std::process::Output {
    let config = dir.join("empty-config");
    std::fs::create_dir_all(&config).unwrap();
    Command::new(al_explorer_binary())
        .env("XDG_CONFIG_HOME", &config)
        .env("HOME", &config)
        .args(["pack-native", "--json", "--project"])
        .arg(dir)
        .arg("--out")
        .arg(out)
        .output()
        .expect("run al-explorer pack-native")
}

fn refused(name: &str, body: &str, code: &str, message: &str) {
    let source = format!(
        "codeunit 50100 C\n{{\n    procedure P()\n    var\n        Item: Record \"AZJ Journey Item\";\n    begin\n{body}\n    end;\n}}\n"
    );
    let dir = project(name, &source);
    let out = dir.join("out.app");
    let res = pack(&dir, &out);
    let stdout = String::from_utf8_lossy(&res.stdout);
    assert!(!res.status.success(), "build must fail:\n{stdout}");
    assert!(!out.exists(), "no .app may be written");
    assert!(
        stdout.contains(&format!("\"code\": \"{code}\"")),
        "{stdout}"
    );
    assert!(stdout.contains(message), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unquoted_missing_field_fails_the_build_with_al0132() {
    refused(
        "member",
        "        Item.NoSuchField := 'x';",
        "AL0132",
        "'Record \\\"AZJ Journey Item\\\"' does not contain a definition for 'NoSuchField'",
    );
}

#[test]
fn an_undeclared_name_fails_the_build_with_al0118() {
    refused(
        "name",
        "        Undeclared := 1;",
        "AL0118",
        "The name 'Undeclared' does not exist in the current context.",
    );
}

#[test]
fn a_call_to_an_undeclared_procedure_fails_the_build_with_al0118() {
    refused(
        "call",
        "        NoSuchProc();",
        "AL0118",
        "The name 'NoSuchProc' does not exist in the current context.",
    );
    refused(
        "callexpr",
        "        Item.Quantity := NoSuchFunc(1) + 1;",
        "AL0118",
        "The name 'NoSuchFunc' does not exist in the current context.",
    );
}
