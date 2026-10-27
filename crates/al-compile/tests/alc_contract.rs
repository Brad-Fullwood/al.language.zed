//! Live contract with the real `alc`: every switch `CompilationConfigOptions`
//! can emit must be accepted. Run with
//! `AL_TOOL_PATH=<dir with alc.dll> cargo test -p al-compile --test alc_contract -- --ignored`.
//! Missing prerequisites fail the test.

use std::path::PathBuf;

use al_compile::{compile_project_with_analyzers, CompilationConfigOptions};

#[tokio::test]
#[ignore = "requires AL_TOOL_PATH/alc.dll and dotnet; run with --ignored"]
async fn alc_accepts_every_switch_the_builder_emits() {
    let toolchain = al_project::toolchain::find_toolchain()
        .expect("alc not found: set AL_TOOL_PATH to the directory holding alc.dll");

    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alc_contract");
    let work = tempfile::tempdir().unwrap();
    let project = work.path().join("proj");
    copy_dir(&fixture, &project);
    let probing = work.path().join("probe");
    std::fs::create_dir_all(&probing).unwrap();
    let ruleset = work.path().join("rules.ruleset.json");
    std::fs::write(&ruleset, r#"{"name":"r","rules":[]}"#).unwrap();

    // Every field on, so a new emitted switch is covered the day it is added.
    let options = CompilationConfigOptions {
        compilation_options: vec!["/parallel+".to_string(), "/nowarn:AL0432".to_string()],
        incremental_build: true,
        enable_external_rulesets: true,
        rule_set_path: Some(ruleset),
        assembly_probing_paths: vec![probing],
        output_analyzer_statistics: true,
    };

    // Some(&[]): no analyzers, so only the option switches are under test.
    let result = compile_project_with_analyzers(&toolchain, &project, None, Some(&[]), &options)
        .await
        .expect("alc ran");
    assert!(
        !result.output.contains("AL1009"),
        "alc rejected a switch: {}",
        result.output
    );
    assert!(result.success, "alc failed: {}", result.output);
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}
