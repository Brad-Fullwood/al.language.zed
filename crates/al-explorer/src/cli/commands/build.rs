use std::process::ExitCode;

use serde_json::Value;

use super::super::XlfCommands;

use super::{connect, print_json, project_root, report_error, run_command};

/// Print a build/package result in human-readable form and return the exit code.
///
/// Both `compile` and `package` return the same response shape:
/// `{ success, appPath?, diagnostics?, output?, backend?, validated? }`.
/// `backend`/`validated` distinguish the native emitter (parses + packages,
/// no semantic analysis) from the Microsoft `alc` path (full compiler
/// validation) — see `al.useOfficialCompiler` — so a successful native emit
/// is never printed as if it were a validated compile.
fn print_build_result(result: &Value, json: bool) -> ExitCode {
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let validated = result.get("validated").and_then(|v| v.as_bool());
    if json {
        print_json(result);
    } else {
        if success {
            let label = match validated {
                Some(true) => "Compilation succeeded (Microsoft alc, validated)",
                Some(false) => "Native emit succeeded (no compiler validation)",
                None => "Compilation succeeded",
            };
            if let Some(path) = result.get("appPath").and_then(|v| v.as_str()) {
                println!("{label}: {path}");
            } else {
                println!("{label}");
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

/// Real-project compiles can exceed the default 30s request deadline,
/// especially when the user opts into Microsoft's compiler. 10 minutes.
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

/// Build a deployable `.app` natively (pure Rust, no Microsoft `alc`): extract
/// symbols, emit `SymbolReference.json`, and package the NAVX/ZIP. Writes to
/// `--out`, or `<project>/output/<publisher>_<name>_<version>.app`.
pub fn cmd_pack_native(
    project_dir: Option<&str>,
    out: Option<&str>,
    validate: bool,
    json: bool,
) -> ExitCode {
    let dir = match project_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => std::env::current_dir().unwrap_or_default(),
    };

    // B1: optional semantic validation gate. The native emitter is structural
    // only — a parseable-but-invalid program would otherwise be packed into an
    // .app the BC server then rejects. With --validate, run the Microsoft AL
    // compiler (alc) as the diagnostic oracle and refuse to emit on errors.
    if validate {
        if let Some(code) = validate_with_alc(&dir, json) {
            return code;
        }
    }

    // Our native compiler identifies itself in the manifest's <Build>.
    let compiler_version = concat!("al-explorer/", env!("CARGO_PKG_VERSION"));
    let timestamp = al_emit::now_timestamp();

    let built = match al_emit::build_app_from_project(&dir, compiler_version, &timestamp) {
        Ok(b) => b,
        Err(e) => return report_error(&format!("native pack failed: {e}"), json),
    };

    let out_path = match out {
        Some(o) => std::path::PathBuf::from(o),
        None => dir.join("output").join(&built.file_name),
    };
    if let Some(parent) = out_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return report_error(&format!("creating {}: {e}", parent.display()), json);
        }
    }
    if let Err(e) = std::fs::write(&out_path, &built.bytes) {
        return report_error(&format!("writing {}: {e}", out_path.display()), json);
    }

    if json {
        print_json(&serde_json::json!({
            "success": true,
            "appPath": out_path.to_string_lossy(),
            "bytes": built.bytes.len(),
        }));
    } else {
        println!(
            "Native package written: {} ({} bytes)",
            out_path.display(),
            built.bytes.len()
        );
    }
    ExitCode::SUCCESS
}

/// Recursively copy a directory tree (used to validate in a throwaway copy so
/// alc's own `.app`/temp output never lands in the user's project).
fn copy_dir(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// B1 validation gate: compile `dir` with the Microsoft AL compiler (alc) and,
/// if it reports errors (or no toolchain is available), return an exit code so
/// the caller refuses to emit. Returns `None` when validation passes and the
/// native emit should proceed. Runs in a temp copy of the project so alc's
/// output never pollutes the user's tree.
fn validate_with_alc(dir: &std::path::Path, json: bool) -> Option<ExitCode> {
    let toolchain = match al_project::toolchain::find_toolchain() {
        Ok(t) => t,
        Err(e) => {
            return Some(report_error(
                &format!(
                    "--validate requires the Microsoft AL toolchain (alc), which was not found: {e}. \
                     Set AL_TOOL_PATH to an extension's bin/<platform> dir or install ALTool."
                ),
                json,
            ));
        }
    };

    let tmp = std::env::temp_dir().join(format!("al-pack-validate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    // Copy app.json + every source/.alpackages dir alc needs.
    if let Err(e) = std::fs::create_dir_all(&tmp).and_then(|()| {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            // Skip prior build outputs to keep the copy lean.
            if name == "output" || name.to_string_lossy().starts_with(".al-build-tmp") {
                continue;
            }
            let from = entry.path();
            let to = tmp.join(&name);
            if entry.file_type()?.is_dir() {
                copy_dir(&from, &to)?;
            } else {
                std::fs::copy(&from, &to)?;
            }
        }
        Ok(())
    }) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Some(report_error(
            &format!("preparing validation copy: {e}"),
            json,
        ));
    }

    let pkg_cache = tmp.join(".alpackages");
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Some(report_error(&format!("starting async runtime: {e}"), json));
        }
    };
    let result = runtime.block_on(al_compile::compile_project(
        &toolchain,
        &tmp,
        Some(&pkg_cache),
    ));
    let _ = std::fs::remove_dir_all(&tmp);

    let result = match result {
        Ok(r) => r,
        Err(e) => {
            return Some(report_error(
                &format!("alc validation failed to run: {e}"),
                json,
            ));
        }
    };

    let errors = result
        .diagnostics
        .iter()
        .filter(|d| d.severity == al_compile::DiagnosticSeverity::Error)
        .count();

    if json {
        // Map the temp paths back so the user sees their own filenames.
        let diags: Vec<_> = result
            .diagnostics
            .iter()
            .map(|d| {
                serde_json::json!({
                    "file": d.file, "line": d.line, "column": d.column,
                    "severity": format!("{:?}", d.severity).to_lowercase(),
                    "code": d.code, "message": d.message,
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "validated": true,
            "success": result.success && errors == 0,
            "errorCount": errors,
            "diagnostics": diags,
        }));
    } else {
        for d in &result.diagnostics {
            let sev = format!("{:?}", d.severity).to_lowercase();
            let file = std::path::Path::new(&d.file)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.file.clone());
            eprintln!(
                "{file}:{}:{}: {sev} {}: {}",
                d.line, d.column, d.code, d.message
            );
        }
    }

    if result.success && errors == 0 {
        if !json {
            eprintln!("Validation passed (alc {}): no errors.", toolchain.version);
        }
        None
    } else {
        if !json {
            eprintln!("Validation failed: {errors} error(s) — .app not written.");
        }
        Some(ExitCode::FAILURE)
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
