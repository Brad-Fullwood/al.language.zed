use std::process::ExitCode;

use serde_json::Value;

use super::super::XlfCommands;

use super::{
    connect, print_json, project_root, report_error, request_checked, run_command,
    run_command_with_exit,
};

/// Print a build/package result in human-readable form and return the exit code.
///
/// Both `compile` and `package` return the same response shape:
/// `{ success, appPath?, diagnostics?, output?, backend?, validated? }`.
/// `backend`/`verificationLevel` distinguish verified-native builds from the
/// Microsoft `alc` path — see `al.useOfficialCompiler`.
fn print_build_result(result: &Value, json: bool) -> ExitCode {
    let success = result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let backend = result.get("backend").and_then(|v| v.as_str());
    if json {
        print_json(result);
    } else {
        if success {
            let label = match backend {
                Some("native") => "Native compilation succeeded (verified)",
                Some("alc") => "Compilation succeeded (Microsoft alc)",
                _ => "Compilation succeeded",
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
        // when no structured diagnostics, show raw output
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
    match request_checked(&mut client, "compile", None) {
        Ok(result) => print_build_result(&result, json),
        Err(e) => report_error(&e, json),
    }
}

/// Publish can compile a whole project and then upload it, so it needs both
/// the build deadline and the upload's.
const PUBLISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1200);

/// Compile the project and publish the `.app` to the BC dev API.
///
/// The daemon's `publish` method is the only publish path: it calls
/// `al_publish::publish`, which resolves the launch configuration, compiles,
/// and uploads (or RAD-deploys with `--incremental`).
pub fn cmd_publish(config: Option<&str>, incremental: bool, json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(client) => client,
        Err(error) => return report_error(&error, json),
    };
    client.set_request_timeout(PUBLISH_TIMEOUT);
    let mut params = serde_json::json!({ "incremental": incremental });
    if let Some(config) = config {
        params["config"] = serde_json::json!(config);
    }
    match request_checked(&mut client, "publish", Some(params)) {
        Ok(result) => {
            let success = result
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            if json {
                print_json(&result);
            } else {
                let server = result.get("server").and_then(|v| v.as_str()).unwrap_or("?");
                let method = result.get("method").and_then(|v| v.as_str()).unwrap_or("?");
                println!("Publish to {server} ({method}):");
                for step in result
                    .get("steps")
                    .and_then(|value| value.as_array())
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    let phase = step.get("phase").and_then(|v| v.as_str()).unwrap_or("?");
                    let ok = step
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let message = step.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    let mark = if ok { "[OK]" } else { "[!!]" };
                    println!("  {mark} {phase}: {message}");
                }
                for diagnostic in result
                    .get("diagnostics")
                    .and_then(|value| value.as_array())
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    let file = diagnostic
                        .get("file")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let line = diagnostic.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
                    let code = diagnostic
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let message = diagnostic
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    eprintln!("{file}:{line}: {code}: {message}");
                }
            }
            if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => report_error(&error, json),
    }
}

pub fn cmd_package(json: bool) -> ExitCode {
    let mut client = match connect(None) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    client.set_request_timeout(BUILD_TIMEOUT);
    match request_checked(&mut client, "package", None) {
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
    analyzers: Option<&str>,
    json: bool,
) -> ExitCode {
    let dir = match project_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(error) => {
                return report_error(
                    &format!("Cannot determine the current project directory: {error}"),
                    json,
                );
            }
        },
    };

    // Our native compiler identifies itself in the manifest's <Build>.
    let compiler_version = concat!("al-explorer/", env!("CARGO_PKG_VERSION"));
    let timestamp = al_emit::now_timestamp();
    let config = match al_project::trust::evaluate(&dir) {
        Ok(evaluated) => {
            if let Some(advisory) = evaluated.decision.advisory() {
                eprintln!("{advisory}");
            }
            evaluated.config
        }
        Err(error) => {
            return report_error(&format!("cannot load AL project settings: {error}"), json);
        }
    };
    // Discover dependency packages independently of app.json parsing. The
    // native verifier owns the manifest contract and must be allowed to return
    // its structured ALN010x diagnostics for malformed manifests.
    let dependency_packages = match al_project::project::configured_symbol_packages(&dir, &config) {
        Ok(selection) => selection.packages,
        Err(error) => {
            return report_error(
                &format!("cannot scan configured symbol package folders: {error}"),
                json,
            );
        }
    };

    let verified = match al_emit::build_verified_app_from_project_with_packages(
        &dir,
        compiler_version,
        &timestamp,
        Some(dependency_packages.as_slice()),
    ) {
        Ok(result) => result,
        Err(e) => return report_error(&format!("native pack failed: {e}"), json),
    };
    let mut timings = verified.timings;
    let diagnostic_values = verified
        .diagnostics
        .iter()
        .map(|diagnostic| {
            serde_json::to_value(diagnostic).expect("verification diagnostic serializes")
        })
        .collect::<Vec<_>>();
    let Some(built) = verified.app else {
        if json {
            print_json(&serde_json::json!({
                "success": false,
                "backend": "native",
                "validated": true,
                "verificationLevel": "native-syntax-project-binding",
                "diagnostics": diagnostic_values,
                "timings": timings,
            }));
        } else {
            eprintln!("Native verification failed");
            for diagnostic in &verified.diagnostics {
                eprintln!(
                    "{}:{}:{}: {:?} {}: {}",
                    diagnostic.file,
                    diagnostic.line,
                    diagnostic.column,
                    diagnostic.severity,
                    diagnostic.code,
                    diagnostic.message
                );
            }
        }
        return ExitCode::FAILURE;
    };

    // Only start Microsoft's heavier compiler after the native gate passes.
    // This keeps syntax/project failures fast and makes `--validate` an
    // explicit compatibility oracle rather than the primary verifier.
    if validate {
        if let Some(code) = validate_with_alc(&dir, analyzers, json) {
            return code;
        }
    }

    let out_path = match out {
        Some(o) => std::path::PathBuf::from(o),
        None => dir.join("output").join(&built.file_name),
    };
    let parent = out_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    if let Err(e) = std::fs::create_dir_all(parent) {
        return report_error(&format!("creating {}: {e}", parent.display()), json);
    }
    let write_started = std::time::Instant::now();
    let write_result = al_emit::package::write_artifact_atomically(&out_path, &built.bytes);
    timings.output_write_ns = u64::try_from(write_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    timings.total_ns = timings.total_ns.saturating_add(timings.output_write_ns);
    if let Err(e) = write_result {
        return report_error(
            &format!("atomically writing {}: {e}", out_path.display()),
            json,
        );
    }

    if json {
        print_json(&serde_json::json!({
            "success": true,
            "appPath": out_path.to_string_lossy(),
            "bytes": built.bytes.len(),
            "backend": "native",
            "validated": true,
            "verificationLevel": "native-syntax-project-binding",
            "microsoftCompatibilityValidated": validate,
            "diagnostics": diagnostic_values,
            "timings": timings,
        }));
    } else {
        let compatibility = if validate {
            " + Microsoft compatibility check"
        } else {
            ""
        };
        println!(
            "Verified native package{compatibility} written: {} ({} bytes)",
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

/// Create a private, per-invocation temp dir for `--validate`'s alc copy.
///
/// `tempfile::tempdir()` creates the directory with owner-only permissions and
/// a random name, unlike the previous predictable `al-pack-validate-{pid}`
/// path under the shared, world-readable `std::env::temp_dir()` — which
/// another local user could pre-create/symlink (a race) or read proprietary
/// AL source from, and which two runs from a pid-reusing wrapper could
/// collide on. The returned `TempDir` guard removes the directory
/// automatically when it drops (every `validate_with_alc` return path,
/// including early errors and unwinding), so no manual cleanup is needed.
fn create_validation_tempdir() -> std::io::Result<tempfile::TempDir> {
    tempfile::tempdir()
}

/// The analyzers `--validate` asks alc to run: the `--analyzers` list when one
/// was given (empty entries dropped, so an empty value means none), otherwise
/// the project's `al.codeAnalyzers` as the trust gate left it.
fn validation_analyzers(requested: Option<&str>, project_setting: &[String]) -> Vec<String> {
    match requested {
        Some(list) => list
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
        None => project_setting.to_vec(),
    }
}

/// The requested analyzers that resolve through the project's own folders: a
/// custom name is looked up in `packages/` and `.netpackages/` before the
/// NuGet cache, and a relative path is joined to the project root. Built-in
/// cops come from the toolchain and an absolute path is the caller's choice.
fn project_local_analyzers(requested: &[String]) -> Vec<&str> {
    requested
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| {
            !entry.is_empty()
                && !al_project::analyzers::is_builtin_analyzer(entry)
                && !std::path::Path::new(entry).is_absolute()
        })
        .collect()
}

/// Compile `dir` with the Microsoft AL compiler (alc) and, if it reports
/// errors (or no toolchain is available), return an exit code so the caller
/// refuses to emit. Returns `None` when validation passes and the native emit
/// should proceed. Runs in a temp copy of the project so alc's output never
/// pollutes the user's tree.
///
/// alc runs with the project's own analyzers and compilation settings. It used
/// to get no analyzer list, which the build service reads as every installed
/// analyzer, so a project that plain alc compiles failed on cop errors from
/// analyzers it never enabled.
fn validate_with_alc(
    dir: &std::path::Path,
    analyzers: Option<&str>,
    json: bool,
) -> Option<ExitCode> {
    let settings = match al_project::trust::evaluate(dir) {
        Ok(settings) => settings,
        Err(error) => {
            return Some(report_error(
                &format!("reading the project's AL settings for --validate: {error}"),
                json,
            ));
        }
    };
    if !json {
        if let Some(advisory) = settings.decision.advisory() {
            eprintln!("{advisory}");
        }
    }
    let analyzers = validation_analyzers(analyzers, &settings.config.code_analyzers);
    if !settings.decision.is_trusted() {
        let project_local = project_local_analyzers(&analyzers);
        if !project_local.is_empty() {
            return Some(report_error(
                &format!(
                    "--analyzers {} would load an analyzer from this untrusted repository's own \
                     folders into alc. Run `{}` in the project to allow it, or pass the \
                     analyzer's absolute path.",
                    project_local.join(","),
                    al_project::trust::TRUST_COMMAND
                ),
                json,
            ));
        }
    }
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

    let tmp = match create_validation_tempdir() {
        Ok(t) => t,
        Err(e) => {
            return Some(report_error(
                &format!("creating validation temp dir: {e}"),
                json,
            ));
        }
    };
    let tmp_path = tmp.path();
    // Copy app.json + every source/.alpackages dir alc needs.
    if let Err(e) = (|| -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            // Skip prior build outputs to keep the copy lean.
            if name == "output" || name.to_string_lossy().starts_with(".al-build-tmp") {
                continue;
            }
            let from = entry.path();
            let to = tmp_path.join(&name);
            if entry.file_type()?.is_dir() {
                copy_dir(&from, &to)?;
            } else {
                std::fs::copy(&from, &to)?;
            }
        }
        Ok(())
    })() {
        return Some(report_error(
            &format!("preparing validation copy: {e}"),
            json,
        ));
    }

    let pkg_cache = tmp_path.join(".alpackages");
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            return Some(report_error(&format!("starting async runtime: {e}"), json));
        }
    };
    let result = runtime.block_on(al_compile::build(al_compile::BuildRequest {
        project_root: tmp_path,
        backend: al_compile::BuildBackend::Alc,
        toolchain: Some(&toolchain),
        dependency_packages: None,
        package_cache: Some(&pkg_cache),
        analyzers: Some(&analyzers),
        config: al_compile::CompilationConfigOptions::from(&settings.config),
    }));
    // `tmp` (the `TempDir` guard) is dropped — and the directory removed —
    // when this function returns, on every path below.

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

    if !json {
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
        if json {
            let diagnostics: Vec<_> = result
                .diagnostics
                .iter()
                .map(|diagnostic| {
                    serde_json::json!({
                        "file": diagnostic.file,
                        "line": diagnostic.line,
                        "column": diagnostic.column,
                        "severity": format!("{:?}", diagnostic.severity).to_lowercase(),
                        "code": diagnostic.code,
                        "message": diagnostic.message,
                    })
                })
                .collect();
            print_json(&serde_json::json!({
                "validated": true,
                "validationBackend": "alc",
                "success": false,
                "errorCount": errors,
                "diagnostics": diagnostics,
            }));
        } else {
            eprintln!("Validation failed: {errors} error(s) — .app not written.");
        }
        Some(ExitCode::FAILURE)
    }
}

/// The `.g.xlf` path an `xlf.generate` response reports, if it wrote one.
pub(crate) fn xlf_generated_path(result: &serde_json::Value) -> Option<&str> {
    result
        .get("path")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|path| !path.is_empty())
}

pub fn cmd_xlf(subcmd: &XlfCommands, json: bool) -> ExitCode {
    match subcmd {
        XlfCommands::Generate { project } => {
            let proj_root = match project_root(project.as_deref()) {
                Ok(root) => root,
                Err(error) => return report_error(&error, json),
            };
            let params = serde_json::json!({ "project": proj_root.to_string_lossy().as_ref() });
            // Writing no `.g.xlf` is a failed gate, not a success: the project
            // asked for a translation file and did not get one. `path` is null
            // when the daemon found nothing translatable.
            run_command_with_exit(
                "xlf.generate",
                Some(params),
                json,
                project.as_deref(),
                |result| {
                    let path = xlf_generated_path(result);
                    let units = result.get("units").and_then(|v| v.as_u64()).unwrap_or(0);
                    match path {
                        Some(path) => println!("Generated: {path}  ({units} units)"),
                        None => eprintln!(
                            "No translatable texts found (check features.TranslationFile in app.json)"
                        ),
                    }
                },
                |result| {
                    if xlf_generated_path(result).is_some() {
                        ExitCode::SUCCESS
                    } else {
                        ExitCode::FAILURE
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

#[cfg(test)]
mod validation_tempdir_tests {
    use super::create_validation_tempdir;

    /// The old implementation derived the validation copy's path
    /// deterministically from the process id
    /// (`al-pack-validate-{pid}` under the shared `std::env::temp_dir()`),
    /// so two validations from the same process — or from a pid-reusing
    /// wrapper — landed on the exact same path. `tempfile::tempdir()` must
    /// produce a fresh, unpredictable directory on every call.
    #[test]
    fn two_calls_never_collide_on_the_same_process_id() {
        let a = create_validation_tempdir().expect("first tempdir must be created");
        let b = create_validation_tempdir().expect("second tempdir must be created");
        assert_ne!(
            a.path(),
            b.path(),
            "two validation temp dirs from the same process must not collide"
        );
        assert!(a.path().is_dir());
        assert!(b.path().is_dir());
    }

    #[test]
    fn path_does_not_match_the_old_predictable_pid_scheme() {
        let dir = create_validation_tempdir().expect("tempdir must be created");
        let pid_name = format!("al-pack-validate-{}", std::process::id());
        assert_ne!(
            dir.path().file_name().and_then(|n| n.to_str()),
            Some(pid_name.as_str()),
            "must not reproduce the old predictable pid-based directory name"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{project_local_analyzers, validation_analyzers};

    fn project_setting() -> Vec<String> {
        vec!["CodeCop".to_string(), "UICop".to_string()]
    }

    #[test]
    fn validation_uses_the_project_setting_without_a_flag() {
        assert_eq!(
            validation_analyzers(None, &project_setting()),
            project_setting()
        );
    }

    #[test]
    fn an_explicit_list_replaces_the_project_setting() {
        assert_eq!(
            validation_analyzers(Some(" AppSourceCop , PerTenantCop,"), &project_setting()),
            vec!["AppSourceCop".to_string(), "PerTenantCop".to_string()]
        );
    }

    /// An untrusted repository could ship `packages/LinterCop.dll`; only the
    /// toolchain's own cops and absolute paths skip the project's folders.
    #[test]
    fn custom_names_and_relative_paths_resolve_through_the_project() {
        let absolute = std::env::temp_dir()
            .join("Custom.dll")
            .to_string_lossy()
            .into_owned();
        let requested = vec![
            "CodeCop".to_string(),
            "UICop.dll".to_string(),
            "LinterCop".to_string(),
            "tools/Mine.dll".to_string(),
            absolute,
        ];
        assert_eq!(
            project_local_analyzers(&requested),
            vec!["LinterCop", "tools/Mine.dll"]
        );
    }

    #[test]
    fn an_empty_value_runs_no_analyzer() {
        assert!(validation_analyzers(Some(""), &project_setting()).is_empty());
        assert!(validation_analyzers(Some(" , "), &project_setting()).is_empty());
    }
}
