pub mod build;
pub mod debug;
pub mod insight;
pub mod lsp;

use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;

use al_protocol::DaemonClient;

pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("{{\"error\":\"serialization failed: {e}\"}}"),
    }
}

/// Whether an object kind (as the daemon serializes it) carries a
/// developer-assigned numeric ID in AL syntax. Interfaces, profiles, page
/// customizations, control add-ins, entitlements, and .NET packages are
/// declared without one — for some of these the symbol packages store an
/// internal compiler hash in the `Id` slot, which must not be displayed
/// as if it were a real object ID (FB-3).
pub fn kind_has_numeric_id(kind: &str) -> bool {
    !matches!(
        kind,
        "Interface"
            | "Profile"
            | "ProfileExtension"
            | "PageCustomization"
            | "ControlAddIn"
            | "Entitlement"
            | "DotNet"
    )
}

/// Build JSON params for a BC server command with common connection fields.
pub fn bc_server_params(
    cmd: &str,
    server: &str,
    company: &str,
    username: Option<&str>,
    password: Option<&str>,
    output_dir: Option<&str>,
) -> serde_json::Value {
    let mut p = serde_json::json!({
        "cmd": cmd,
        "serverUrl": server,
        "company": company,
    });
    if let Some(u) = username {
        p["username"] = serde_json::json!(u);
    }
    if let Some(pw) = password {
        p["password"] = serde_json::json!(pw);
    }
    if let Some(d) = output_dir {
        p["outputDir"] = serde_json::json!(absolutize_path(d));
    }
    p
}

/// Make a user-supplied path absolute relative to the current working
/// directory, without requiring the path to exist yet.
///
/// Daemon endpoints (`newProject`, `profiling analyze`, snapshot/profile
/// `outputDir`, …) reject relative paths with `"… must be an absolute
/// path"`. The CLI accepts shell-style relative paths like `MyApp` or
/// `trace.alcpuprofile` because users naturally type them, so the CLI
/// must absolutize before forwarding (F-050). `canonicalize()` is
/// unsuitable here because it requires the target to exist; for `new`
/// the directory is being created on the daemon side.
pub fn absolutize_path(input: &str) -> String {
    let p = std::path::Path::new(input);
    if p.is_absolute() {
        return input.to_string();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p).to_string_lossy().into_owned(),
        Err(_) => input.to_string(),
    }
}

/// Get the project root directory.
pub fn project_root(project_arg: Option<&str>) -> PathBuf {
    project_arg
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Convert a file path to a file:// URI string.
///
/// Returns `None` when the path cannot be canonicalized (i.e. the file does
/// not exist on disk). Previously this silently fell back to the
/// non-canonicalized path, which produced URIs the daemon could not match
/// against its open-document map and led to mysterious "no result" responses
/// for typo'd paths. Returning `None` lets callers surface a clear error.
pub fn file_to_uri(file: &str) -> Option<String> {
    let path = std::path::Path::new(file);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let canon = match abs.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("error: file not found: {}", abs.display());
            return None;
        }
    };
    url::Url::from_file_path(canon).ok().map(|u| u.to_string())
}

/// Connect to the daemon, auto-starting if needed.
pub fn connect(project_dir: Option<&str>) -> Result<DaemonClient, String> {
    let root = project_root(project_dir);
    DaemonClient::connect(&root).map_err(|e| {
        if e.contains("No such file") || e.contains("Connection refused") {
            format!(
                "{e}\n\nHint: Is the daemon running? Start it with: al-lsp daemon --project {}",
                root.display()
            )
        } else if e.contains("app.json") {
            format!(
                "{e}\n\nHint: No AL project found. Ensure app.json exists in {}",
                root.display()
            )
        } else {
            e
        }
    })
}

/// Report an error in the appropriate format and return FAILURE.
pub fn report_error(msg: &str, json: bool) -> ExitCode {
    if json {
        print_json(&serde_json::json!({ "error": msg }));
    } else {
        eprintln!("Error: {msg}");
    }
    ExitCode::FAILURE
}

/// Collect all .al files under a directory.
pub fn collect_al_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        // Resolve symlinks to detect cycles
        if let Ok(canonical) = current_dir.canonicalize() {
            if !visited.insert(canonical) {
                continue; // Already visited this real path — skip to avoid cycle
            }
        }
        let entries = match std::fs::read_dir(&current_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("al"))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

pub fn print_symbol_entries(result: &serde_json::Value) {
    let entries = match result.as_array() {
        Some(arr) => arr.clone(),
        None => vec![result.clone()],
    };
    for e in &entries {
        let kind = e.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        let id = e.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let name = e.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        let pkg = e.get("package").and_then(|v| v.as_str()).unwrap_or("?");
        if id > 0 && kind_has_numeric_id(kind) {
            println!("{kind} {id} \"{name}\" (package: {pkg})");
        } else {
            println!("{kind} \"{name}\" (package: {pkg})");
        }

        if let Some(extends) = e.get("extends").and_then(|v| v.as_str()) {
            println!("  extends: {extends}");
        }
        if let Some(fields) = e.get("fields").and_then(|v| v.as_array()) {
            if !fields.is_empty() {
                println!("  fields:");
                for f in fields {
                    let fname = f.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let ftype = f.get("type_name").and_then(|v| v.as_str()).unwrap_or("?");
                    let fid = f.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                    println!("    {fname}: {ftype} (id {fid})");
                }
            }
        }
        if let Some(methods) = e.get("methods").and_then(|v| v.as_array()) {
            if !methods.is_empty() {
                println!("  methods:");
                for m in methods {
                    let mname = m.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let ret = m
                        .get("return_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("void");
                    let is_local = m.get("is_local").and_then(|v| v.as_bool()).unwrap_or(false);
                    let scope = if is_local { " [local]" } else { "" };
                    let params = m
                        .get("parameters")
                        .and_then(|v| v.as_array())
                        .map(|ps| {
                            ps.iter()
                                .map(|p| {
                                    let pname =
                                        p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let ptype =
                                        p.get("type_name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let is_var =
                                        p.get("is_var").and_then(|v| v.as_bool()).unwrap_or(false);
                                    if is_var {
                                        format!("var {pname}: {ptype}")
                                    } else {
                                        format!("{pname}: {ptype}")
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join("; ")
                        })
                        .unwrap_or_default();
                    println!("    {mname}({params}): {ret}{scope}");
                }
            }
        }
        if let Some(values) = e.get("enum_values").and_then(|v| v.as_array()) {
            if !values.is_empty() {
                println!("  values:");
                for v in values {
                    let ordinal = v.get("ordinal").and_then(|v| v.as_i64()).unwrap_or(0);
                    let vname = v.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("    {ordinal} = {vname}");
                }
            }
        }
        println!();
    }
}

/// Execute a daemon JSON-RPC request with the common connect/request/format lifecycle.
///
/// Connects to the daemon, sends `method` with `params`, then calls `format_fn` for
/// human-readable output (skipped when `json` is true). Returns `ExitCode::SUCCESS`
/// on success or `ExitCode::FAILURE` on connect/request error.
///
/// Commands with conditional exit codes, multi-step logic, early returns inside the
/// format branch, or non-standard timeouts should stay manual rather than use this helper.
pub fn run_command<F>(
    method: &str,
    params: Option<serde_json::Value>,
    json: bool,
    project_dir: Option<&str>,
    format_fn: F,
) -> ExitCode
where
    F: FnOnce(&serde_json::Value),
{
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match client.request(method, params) {
        Ok(result) => {
            if json {
                print_json(&result);
            } else {
                format_fn(&result);
            }
            ExitCode::SUCCESS
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn print_lint_diag(file: Option<&str>, d: &serde_json::Value) {
    let code = d.get("code").and_then(|v| v.as_str()).unwrap_or("?");
    let msg = d.get("message").and_then(|v| v.as_str()).unwrap_or("?");
    let sev = d.get("severity").and_then(|v| v.as_str()).unwrap_or("?");
    let line = d.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
    let col = d.get("column").and_then(|v| v.as_u64()).unwrap_or(0);
    let prefix = file.unwrap_or("?");
    eprintln!("{prefix}:{line}:{col}: {sev} [{code}] {msg}");
}

#[cfg(test)]
mod path_tests {
    use super::absolutize_path;

    #[test]
    fn absolutize_path_passes_through_absolute_paths() {
        // Positive: already-absolute paths must round-trip unchanged.
        let abs = "/tmp/foo/bar";
        assert_eq!(absolutize_path(abs), abs);
    }

    #[test]
    fn absolutize_path_resolves_relative_paths_against_cwd() {
        // Positive: relative paths must come out absolute (F-050).
        let cwd = std::env::current_dir().expect("current_dir is required for this test");
        let resolved = absolutize_path("MyApp");
        assert!(
            std::path::Path::new(&resolved).is_absolute(),
            "expected absolute, got {resolved}"
        );
        assert!(resolved.starts_with(&cwd.to_string_lossy().to_string()));
        assert!(resolved.ends_with("MyApp"));
    }

    #[test]
    fn absolutize_path_does_not_require_target_to_exist() {
        // Negative: must NOT depend on filesystem state (canonicalize would
        // fail on `trace.alcpuprofile` before the file is captured).
        let resolved = absolutize_path("nonexistent.alcpuprofile");
        assert!(std::path::Path::new(&resolved).is_absolute());
        assert!(!std::path::Path::new(&resolved).exists());
    }
}
