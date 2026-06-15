use std::process::ExitCode;

use serde_json::Value;

use super::super::XlfCommands;

use super::{connect, print_json, project_root, report_error, run_command};

/// Print a build/package result in human-readable form and return the exit code.
///
/// Both `compile` and `package` return the same response shape:
/// `{ success, appPath?, diagnostics?, output? }`.
fn print_build_result(result: &Value, json: bool) -> ExitCode {
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if json {
        print_json(result);
    } else {
        if success {
            if let Some(path) = result.get("appPath").and_then(|v| v.as_str()) {
                println!("Compilation succeeded: {path}");
            } else {
                println!("Compilation succeeded");
            }
        } else {
            eprintln!("Compilation failed");
        }
        let diag_count = result
            .get("diagnostics")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        if let Some(diags) = result.get("diagnostics").and_then(|v| v.as_array()) {
            for d in diags {
                let severity = d
                    .get("severity")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error");
                let file = d.get("file").and_then(|v| v.as_str()).unwrap_or("?");
                let line = d.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                let col = d.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
                let code = d.get("code").and_then(|v| v.as_str()).unwrap_or("?");
                let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                eprintln!("{file}:{line}:{col}: {severity} {code}: {msg}");
            }
        }
        // ISSUE-077: when no structured diagnostics, show raw output
        if !success && diag_count == 0 {
            if let Some(output) = result.get("output").and_then(|v| v.as_str()) {
                if !output.trim().is_empty() {
                    eprintln!("{output}");
                }
            }
        }
    }
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Real-project compiles routinely exceed the default 30s request
/// deadline (alc on a large workspace, cold .NET start). 10 minutes.
const BUILD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

pub fn cmd_compile(project_dir: Option<&str>, json: bool) -> ExitCode {
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    client.set_request_timeout(BUILD_TIMEOUT);
    match client.request("compile", None) {
        Ok(result) => print_build_result(&result, json),
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_package(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    client.set_request_timeout(BUILD_TIMEOUT);
    match client.request("package", None) {
        Ok(result) => print_build_result(&result, json),
        Err(e) => report_error(&e, json),
    }
}

pub fn cmd_xlf(subcmd: &XlfCommands, json: bool) -> ExitCode {
    match subcmd {
        XlfCommands::Generate { project } => {
            let proj_root = project_root(project.as_deref());
            let params = serde_json::json!({ "project": proj_root.to_string_lossy().as_ref() });
            run_command(
                "xlf.generate",
                Some(params),
                json,
                project.as_deref(),
                |result| {
                    let path = result.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    let units = result.get("units").and_then(|v| v.as_u64()).unwrap_or(0);
                    if path.is_empty() || path == "null" {
                        eprintln!(
                            "No translatable texts found (check features.TranslationFile in app.json)"
                        );
                    } else {
                        println!("Generated: {path}  ({units} units)");
                    }
                },
            )
        }
        XlfCommands::Refresh { xlf, generated } => {
            let abs_xlf = canonicalize_xlf_path(xlf);
            let mut params = serde_json::json!({ "xlf": abs_xlf });
            if let Some(g) = generated {
                params["generated"] = serde_json::json!(canonicalize_xlf_path(g));
            }
            run_command("xlf.refresh", Some(params), json, None, |result| {
                let added = result
                    .get("added")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let changed = result
                    .get("changed")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let removed = result
                    .get("removed")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let preserved = result
                    .get("preserved")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                println!(
                    "Refresh complete: +{added} new, ~{changed} changed, -{removed} removed, {preserved} preserved"
                );
            })
        }
        XlfCommands::Untranslated { xlf } => {
            let abs_xlf = canonicalize_xlf_path(xlf);
            let params = serde_json::json!({ "xlf": abs_xlf });
            run_command("xlf.untranslated", Some(params), json, None, |result| {
                let count = result.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("{count} untranslated text(s):");
                if let Some(items) = result.get("untranslated").and_then(|v| v.as_array()) {
                    for item in items {
                        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let src = item.get("source").and_then(|v| v.as_str()).unwrap_or("");
                        println!("  [{id}] {src}");
                    }
                }
            })
        }
        XlfCommands::Suggest { xlf } => {
            let abs_xlf = canonicalize_xlf_path(xlf);
            let params = serde_json::json!({ "xlf": abs_xlf });
            run_command("xlf.suggest", Some(params), json, None, |result| {
                let count = result.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                println!("{count} suggestion(s):");
                if let Some(suggestions) = result.get("suggestions").and_then(|v| v.as_array()) {
                    for s in suggestions {
                        let unit_id = s.get("unit_id").and_then(|v| v.as_str()).unwrap_or("");
                        let src = s.get("source").and_then(|v| v.as_str()).unwrap_or("");
                        let translation = s
                            .get("suggested_translation")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let confidence =
                            s.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        println!(
                            "  [{unit_id}] {src} → {translation}  (confidence: {confidence:.2})"
                        );
                    }
                }
            })
        }
    }
}

fn canonicalize_xlf_path(path: &str) -> String {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        path.to_string()
    } else {
        std::env::current_dir()
            .ok()
            .map(|d| d.join(p).to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string())
    }
}
