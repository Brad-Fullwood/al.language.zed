//! Every built-in `al-explorer new` template must compile with Microsoft's alc
//! at 0 errors and 0 warnings (audit findings emitter-04 and generators-06).
//!
//! Runs when `AL_TOOL_PATH` (the dir holding `alc.dll`) and
//! `AL_PACKAGE_CACHE_PATH` (a dir with the Microsoft_*.app symbols) are set and
//! `dotnet` works. Otherwise it prints a skip message and passes.

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
fn every_template_compiles_with_alc_without_diagnostics() {
    let (Some(tool), Some(cache)) = (
        std::env::var_os("AL_TOOL_PATH"),
        std::env::var_os("AL_PACKAGE_CACHE_PATH"),
    ) else {
        eprintln!("skipped: set AL_TOOL_PATH (dir with alc.dll) and AL_PACKAGE_CACHE_PATH");
        return;
    };
    let alc = Path::new(&tool).join("alc.dll");
    let dotnet_ok = Command::new("dotnet")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !alc.is_file() || !dotnet_ok {
        eprintln!("skipped: alc.dll or dotnet not found");
        return;
    }

    let root = std::env::temp_dir().join(format!("al-scaffold-alc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mut failures = Vec::new();
    for template in TEMPLATES {
        let dir = root.join(template);
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
            .arg(format!(
                "/out:{}",
                root.join(format!("{template}.app")).display()
            ))
            .output()
            .expect("run alc");
        let text = String::from_utf8_lossy(&out.stdout);
        let diagnostics: Vec<&str> = text
            .lines()
            .filter(|l| l.contains(": error ") || l.contains(": warning "))
            .collect();
        if !out.status.success() || !diagnostics.is_empty() {
            failures.push(format!("{template}: {diagnostics:#?}"));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        failures.is_empty(),
        "alc diagnostics:\n{}",
        failures.join("\n")
    );
}
