pub mod build;
pub mod debug;
pub mod insight;
pub mod lsp;

use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;

use crate::client::DaemonClient;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

pub fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
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
        p["outputDir"] = serde_json::json!(d);
    }
    p
}

/// Get the project root directory.
pub fn project_root(project_arg: Option<&str>) -> PathBuf {
    project_arg
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// Convert a file path to a file:// URI string.
pub fn file_to_uri(file: &str) -> Option<String> {
    let path = std::path::Path::new(file);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let canon = abs.canonicalize().unwrap_or(abs);
    url::Url::from_file_path(canon).ok().map(|u| u.to_string())
}

/// Connect to the daemon, auto-starting if needed.
pub fn connect(project_dir: Option<&str>) -> Result<DaemonClient, String> {
    let root = project_root(project_dir);
    DaemonClient::connect(&root).map_err(|e| {
        if e.contains("No such file") || e.contains("Connection refused") {
            format!("{e}\n\nHint: Is the daemon running? Start it with: al-lsp daemon --project {}", root.display())
        } else if e.contains("app.json") {
            format!("{e}\n\nHint: No AL project found. Ensure app.json exists in {}", root.display())
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
    collect_al_files_recursive(dir, &mut files);
    files.sort();
    files
}

fn collect_al_files_recursive(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            collect_al_files_recursive(&path, files);
        } else if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("al")) {
            files.push(path);
        }
    }
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
        println!("{kind} {id} \"{name}\" (package: {pkg})");

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
                    let ret = m.get("return_type").and_then(|v| v.as_str()).unwrap_or("void");
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
                                    let ptype = p
                                        .get("type_name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
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
