//! `al-explorer pack-native --validate` runs the Microsoft
//! AL compiler (alc) as an additional semantic compatibility oracle after the
//! always-on native syntax/project/binding verifier, and refuses to emit a .app
//! when either gate reports errors.
//!
//! Gated on `AL_TOOL_PATH` pointing at a dir with `alc.dll` + `dotnet` on PATH;
//! skips cleanly otherwise (the common CI case).

use std::path::Path;
use std::process::Command;

use al_test_harness::al_explorer_binary;

fn alc_available() -> bool {
    let Some(dir) = std::env::var_os("AL_TOOL_PATH") else {
        return false;
    };
    if !Path::new(&dir).join("alc.dll").is_file() {
        return false;
    }
    Command::new("dotnet")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const APP_JSON: &str = r#"{ "id": "33333333-4444-5555-6666-777777777777", "name": "ValidateProj", "publisher": "AL", "version": "1.0.0.0", "runtime": "14.0", "target": "Cloud", "idRanges": [{ "from": 50100, "to": 50149 }], "dependencies": [] }"#;

fn make_project(root: &Path, codeunit: &str) {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(root.join(".alpackages")).unwrap();
    std::fs::write(root.join("app.json"), APP_JSON).unwrap();
    std::fs::write(src.join("C.Codeunit.al"), codeunit).unwrap();
}

/// Run `al-explorer pack-native --project <dir> --out <out> --validate`.
fn pack_validate(dir: &Path, out: &Path) -> std::process::Output {
    Command::new(al_explorer_binary())
        .args(["pack-native", "--project"])
        .arg(dir)
        .arg("--out")
        .arg(out)
        .arg("--validate")
        .output()
        .expect("run al-explorer pack-native --validate")
}

#[test]
fn validate_passes_valid_project_and_emits() {
    if !alc_available() {
        eprintln!("SKIP: AL_TOOL_PATH/alc.dll + dotnet not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!("al-b1-ok-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    make_project(
        &dir,
        "codeunit 50100 C\n{\n    procedure Add(A: Integer; B: Integer): Integer begin exit(A + B); end;\n}",
    );
    let out = dir.join("ok.app");
    let res = pack_validate(&dir, &out);
    assert!(
        res.status.success() && out.is_file(),
        "valid project should validate and emit:\n{}\n{}",
        String::from_utf8_lossy(&res.stdout),
        String::from_utf8_lossy(&res.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn validate_refuses_parseable_but_invalid_project() {
    if !alc_available() {
        eprintln!("SKIP: AL_TOOL_PATH/alc.dll + dotnet not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!("al-b1-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // Parses fine, but `Undeclared` does not exist → alc AL0118.
    make_project(
        &dir,
        "codeunit 50100 C\n{\n    procedure Bad(): Integer begin exit(Undeclared + 1); end;\n}",
    );
    let out = dir.join("bad.app");
    let res = pack_validate(&dir, &out);
    assert!(
        !res.status.success(),
        "invalid project must fail validation:\n{}\n{}",
        String::from_utf8_lossy(&res.stdout),
        String::from_utf8_lossy(&res.stderr)
    );
    assert!(
        !out.is_file(),
        "no .app must be written when validation fails (got one at {})",
        out.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
