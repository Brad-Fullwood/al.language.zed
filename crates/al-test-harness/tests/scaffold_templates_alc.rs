//! Every built-in `al-explorer new` template must compile with Microsoft's alc
//! at 0 errors and 0 warnings (audit findings emitter-04 and generators-06).
//!
//! Ignored by default. It needs `AL_TOOL_PATH` (the dir holding `alc.dll`),
//! `AL_PACKAGE_CACHE_PATH` (a dir with the Microsoft_*.app symbols) and a
//! working `dotnet`. A missing input fails the test. `make microsoft-contracts`
//! runs it with `--ignored`.

use std::path::Path;
use std::process::Command;

use al_project::scaffold::{create_project, ScaffoldConfig};

const TEMPLATES: &[&str] = &[
    "default",
    "pte",
    "appsource",
    "library",
    "test",
    "copilot",
    "agent",
    "api",
];

#[test]
#[ignore = "Microsoft toolchain profile"]
fn every_template_compiles_with_alc_without_diagnostics() {
    let tool =
        std::env::var_os("AL_TOOL_PATH").expect("AL_TOOL_PATH must name the dir holding alc.dll");
    let cache = std::env::var_os("AL_PACKAGE_CACHE_PATH")
        .expect("AL_PACKAGE_CACHE_PATH must name a dir with the Microsoft_*.app symbols");
    let alc = Path::new(&tool).join("alc.dll");
    assert!(alc.is_file(), "alc.dll not found at {}", alc.display());
    let dotnet_ok = Command::new("dotnet")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    assert!(dotnet_ok, "dotnet --version failed");

    let root = std::env::temp_dir().join(format!("al-scaffold-alc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut failures = Vec::new();
    for template in TEMPLATES {
        let dir = root.join(template);
        let app = root.join(format!("{template}.app"));
        // The library call is what `al-explorer new` runs.
        let config = ScaffoldConfig {
            name: format!("A {template}"),
            publisher: "Audit".to_string(),
            template: template.parse().expect("built-in template name"),
            ..Default::default()
        };
        create_project(&dir, &config).unwrap_or_else(|e| panic!("new {template}: {e}"));
        let out = Command::new("dotnet")
            .env("DOTNET_ROLL_FORWARD", "Major")
            .arg(&alc)
            .arg(format!("/project:{}", dir.display()))
            .arg(format!("/packagecachepath:{}", Path::new(&cache).display()))
            .arg(format!("/out:{}", app.display()))
            .output()
            .expect("run alc");
        // Scan both streams. A diagnostic on stderr would otherwise pass.
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let diagnostics: Vec<&str> = text
            .lines()
            .filter(|l| l.contains(": error ") || l.contains(": warning "))
            .collect();
        if !out.status.success() || !diagnostics.is_empty() {
            failures.push(format!("{template}: {diagnostics:#?}"));
        }
        if !app.is_file() {
            failures.push(format!("{template}: no .app at {}", app.display()));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "alc diagnostics:\n{}",
        failures.join("\n")
    );
}
