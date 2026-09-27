//! `pack-native --validate` on an untrusted project names the ignored settings
//! once per command.

use std::path::Path;
use std::process::{Command, Output};

const NOTICE: &str =
    "This project is not trusted, so these settings from its own files were ignored";

/// Run `al-explorer` with a scratch trust store, so no record from the
/// machine trusts the project, and a toolchain directory with no alc in it,
/// so `--validate` stops after reading the project's settings.
fn al_explorer(args: &[&str], config_home: &Path, tool_path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_al-explorer"))
        .args(args)
        .env("XDG_CONFIG_HOME", config_home)
        .env("AL_TOOL_PATH", tool_path)
        .output()
        .expect("al-explorer runs")
}

#[test]
fn validate_prints_the_untrusted_notice_once() {
    let config_home = tempfile::tempdir().unwrap();
    let tool_path = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let project = scratch.path().join("App");
    let project_arg = project.to_str().unwrap();

    let created = al_explorer(&["new", project_arg], config_home.path(), tool_path.path());
    assert!(created.status.success(), "al-explorer new: {created:?}");

    let packed = al_explorer(
        &["pack-native", "--validate", "--project", project_arg],
        config_home.path(),
        tool_path.path(),
    );
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&packed.stdout),
        String::from_utf8_lossy(&packed.stderr)
    );
    assert!(
        output.contains("--validate requires the Microsoft AL toolchain"),
        "the command must reach the alc step: {output}"
    );
    assert_eq!(output.matches(NOTICE).count(), 1, "{output}");
}
