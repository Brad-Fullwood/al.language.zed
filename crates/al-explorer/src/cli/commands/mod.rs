pub mod build;
pub mod debug;
pub mod insight;
pub mod lsp;
mod response_contract;

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
/// as if it were a real object ID.
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

pub fn bc_server_params(
    cmd: &str,
    server: &str,
    company: &str,
    username: Option<&str>,
    password: Option<&str>,
    output_dir: Option<&str>,
) -> Result<serde_json::Value, String> {
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
        p["outputDir"] = serde_json::json!(absolutize_path(d)?);
    }
    Ok(p)
}

/// Make a user-supplied path absolute relative to the current working
/// directory, without requiring the path to exist yet.
///
/// Daemon endpoints (`newProject`, `profiling analyze`, snapshot/profile
/// `outputDir`, …) reject relative paths with `"… must be an absolute
/// path"`. The CLI accepts shell-style relative paths like `MyApp` or
/// `trace.alcpuprofile` because users naturally type them, so the CLI
/// must absolutize before forwarding. `canonicalize()` is
/// unsuitable here because it requires the target to exist; for `new`
/// the directory is being created on the daemon side.
pub fn absolutize_path(input: &str) -> Result<String, String> {
    let p = std::path::Path::new(input);
    if p.is_absolute() {
        return Ok(input.to_string());
    }
    let cwd = std::env::current_dir()
        .map_err(|error| format!("cannot resolve current directory: {error}"))?;
    Ok(cwd.join(p).to_string_lossy().into_owned())
}

pub fn project_root(project_arg: Option<&str>) -> Result<PathBuf, String> {
    match project_arg {
        Some(project) => Ok(PathBuf::from(project)),
        None => std::env::current_dir()
            .map_err(|error| format!("cannot resolve current project directory: {error}")),
    }
}

/// Convert a file path to a file:// URI string.
///
/// Returns `None` when the path cannot be canonicalized (i.e. the file does
/// not exist on disk). Previously this silently fell back to the
/// non-canonicalized path, which produced URIs the daemon could not match
/// against its open-document map and led to mysterious "no result" responses
/// for typo'd paths. Returning `None` lets callers surface a clear error.
pub fn file_to_uri(file: &str) -> Result<String, String> {
    let path = std::path::Path::new(file);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("cannot resolve current directory: {error}"))?
            .join(path)
    };
    let canon = abs
        .canonicalize()
        .map_err(|error| format!("cannot resolve '{}': {error}", abs.display()))?;
    url::Url::from_file_path(&canon)
        .map(|uri| uri.to_string())
        .map_err(|()| {
            format!(
                "path cannot be represented as a file URI: {}",
                canon.display()
            )
        })
}

pub fn connect(project_dir: Option<&str>) -> Result<DaemonClient, String> {
    let root = project_root(project_dir)?;
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

pub fn report_error(msg: &str, json: bool) -> ExitCode {
    if json {
        print_json(&serde_json::json!({ "error": msg }));
    } else {
        eprintln!("Error: {msg}");
    }
    ExitCode::FAILURE
}

pub fn collect_al_files(dir: &std::path::Path) -> Result<Vec<PathBuf>, String> {
    collect_al_files_for_extension(dir, "al")
}

fn collect_al_files_for_extension(
    dir: &std::path::Path,
    extension: &str,
) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current_dir) = stack.pop() {
        let canonical = current_dir.canonicalize().map_err(|error| {
            format!(
                "cannot resolve source directory '{}': {error}",
                current_dir.display()
            )
        })?;
        if !visited.insert(canonical) {
            continue;
        }
        let entries = std::fs::read_dir(&current_dir).map_err(|error| {
            format!(
                "cannot read source directory '{}': {error}",
                current_dir.display()
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot enumerate source directory '{}': {error}",
                    current_dir.display()
                )
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                format!("cannot inspect source path '{}': {error}", path.display())
            })?;
            let metadata = if file_type.is_symlink() {
                Some(std::fs::metadata(&path).map_err(|error| {
                    format!("cannot follow source path '{}': {error}", path.display())
                })?)
            } else {
                None
            };
            let is_dir =
                file_type.is_dir() || metadata.as_ref().is_some_and(std::fs::Metadata::is_dir);
            let is_file =
                file_type.is_file() || metadata.as_ref().is_some_and(std::fs::Metadata::is_file);
            if is_dir {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| {
                        format!(
                            "source directory has a non-Unicode name: {}",
                            path.display()
                        )
                    })?;
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                stack.push(path);
            } else if is_file
                && path
                    .extension()
                    .is_some_and(|value| value.eq_ignore_ascii_case(extension))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
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

        if let Some(availability) = e
            .get("source_availability")
            .and_then(|value| value.as_str())
        {
            println!("  source: {}", availability.replace('_', " "));
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
    run_command_with_exit(method, params, json, project_dir, format_fn, |_| {
        ExitCode::SUCCESS
    })
}

/// Execute a daemon request whose valid result can still represent a failed
/// quality gate (for example lint findings, failed tests, or an incomplete
/// analysis). The same result-derived exit code is used for human and JSON
/// output so `--json` cannot accidentally turn a failing gate green.
pub fn run_command_with_exit<F, G>(
    method: &str,
    params: Option<serde_json::Value>,
    json: bool,
    project_dir: Option<&str>,
    format_fn: F,
    exit_fn: G,
) -> ExitCode
where
    F: FnOnce(&serde_json::Value),
    G: FnOnce(&serde_json::Value) -> ExitCode,
{
    let mut client = match connect(project_dir) {
        Ok(c) => c,
        Err(e) => return report_error(&e, json),
    };
    match request_checked(&mut client, method, params) {
        Ok(result) => {
            let exit_code = exit_fn(&result);
            if json {
                print_json(&result);
            } else {
                format_fn(&result);
            }
            exit_code
        }
        Err(e) => report_error(&e, json),
    }
}

pub fn request_checked(
    client: &mut DaemonClient,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let contract_params = params.clone();
    let result = client.request(method, params)?;
    if response_contract::handles(method) {
        response_contract::validate(method, contract_params.as_ref(), &result)?;
    } else {
        validate_run_command_result(method, &result)?;
    }
    Ok(result)
}

fn validate_run_command_result(method: &str, result: &serde_json::Value) -> Result<(), String> {
    let response_error = |expected: &str| {
        format!(
            "daemon returned an invalid response for '{method}': expected {expected}, got {}",
            json_type_name(result)
        )
    };
    match method {
        "entrypoints" => validate_array_object_fields(
            result,
            method,
            &[
                ("type", JsonFieldKind::String),
                ("object_name", JsonFieldKind::String),
                ("name", JsonFieldKind::String),
            ],
        )?,
        "obsolete" => validate_array_object_fields(
            result,
            method,
            &[
                ("kind", JsonFieldKind::String),
                ("object", JsonFieldKind::String),
                ("symbol", JsonFieldKind::String),
                ("state", JsonFieldKind::String),
                ("callerCount", JsonFieldKind::Unsigned),
            ],
        )?,
        "audit.dataClassification" => validate_array_object_fields(
            result,
            method,
            &[
                ("table", JsonFieldKind::String),
                ("field", JsonFieldKind::String),
                ("classification", JsonFieldKind::String),
                ("risk", JsonFieldKind::String),
            ],
        )?,
        "breaking" => validate_array_object_fields(
            result,
            method,
            &[
                ("kind", JsonFieldKind::String),
                ("object", JsonFieldKind::String),
                ("description", JsonFieldKind::String),
                ("isBreaking", JsonFieldKind::Boolean),
            ],
        )?,
        "arch.lint" => validate_array_object_fields(
            result,
            method,
            &[
                ("ruleId", JsonFieldKind::String),
                ("message", JsonFieldKind::String),
                ("object", JsonFieldKind::String),
            ],
        )?,
        "nativeCheck" => validate_array_object_fields(
            result,
            method,
            &[
                ("code", JsonFieldKind::String),
                ("severity", JsonFieldKind::String),
                ("objectType", JsonFieldKind::String),
                ("objectName", JsonFieldKind::String),
                ("message", JsonFieldKind::String),
            ],
        )?,
        "upgrade" => validate_array_object_fields(
            result,
            method,
            &[
                ("kind", JsonFieldKind::String),
                ("object", JsonFieldKind::String),
                ("description", JsonFieldKind::String),
                ("migrationHint", JsonFieldKind::String),
                ("severity", JsonFieldKind::String),
            ],
        )?,
        "object" | "byId" => validate_array_object_fields(
            result,
            method,
            &[
                ("kind", JsonFieldKind::String),
                ("id", JsonFieldKind::Integer),
                ("name", JsonFieldKind::String),
                ("package", JsonFieldKind::String),
            ],
        )?,
        "sqlPatterns" => validate_array_object_fields(
            result,
            method,
            &[
                ("kind", JsonFieldKind::String),
                ("message", JsonFieldKind::String),
                ("object", JsonFieldKind::String),
                ("procedure", JsonFieldKind::String),
                ("line", JsonFieldKind::Unsigned),
            ],
        )?,
        "rules" => validate_array_object_fields(
            result,
            method,
            &[
                ("code", JsonFieldKind::String),
                ("severity", JsonFieldKind::String),
                ("name", JsonFieldKind::String),
                ("description", JsonFieldKind::String),
            ],
        )?,
        "errorCodes" => validate_array_object_fields(
            result,
            method,
            &[
                ("code", JsonFieldKind::String),
                ("description", JsonFieldKind::String),
            ],
        )?,
        "builtinTypes" => validate_array_object_fields(
            result,
            method,
            &[
                ("name", JsonFieldKind::String),
                ("methods", JsonFieldKind::Array),
            ],
        )?,
        "tests.discover" => validate_array_object_fields(
            result,
            method,
            &[
                ("name", JsonFieldKind::String),
                ("id", JsonFieldKind::Integer),
                ("file", JsonFieldKind::String),
                ("tests", JsonFieldKind::Array),
                ("testInitializers", JsonFieldKind::Array),
                ("testCleanups", JsonFieldKind::Array),
            ],
        )
        .and_then(|()| validate_discovered_test_methods(result))?,
        "insightStats" => {
            require_object_field(result, "nodes", serde_json::Value::is_u64, "integer")?;
            require_object_field(result, "edges", serde_json::Value::is_u64, "integer")?;
        }
        "xlf.generate" => {
            require_object_field(result, "units", serde_json::Value::is_u64, "integer")?;
            let path = result
                .as_object()
                .and_then(|object| object.get("path"))
                .ok_or_else(|| response_error("an object with field 'path'"))?;
            if !path.is_null() && !path.is_string() {
                return Err(response_error("an object whose 'path' is a string or null"));
            }
        }
        "xlf.refresh" => {
            for field in ["added", "changed", "removed"] {
                require_object_field(result, field, serde_json::Value::is_array, "array")?;
            }
            require_object_field(result, "preserved", serde_json::Value::is_u64, "integer")?;
        }
        "xlf.untranslated" => {
            require_object_field(result, "count", serde_json::Value::is_u64, "integer")?;
            require_object_field(result, "untranslated", serde_json::Value::is_array, "array")?;
            validate_named_array_object_fields(
                result,
                "untranslated",
                &[
                    ("id", JsonFieldKind::String),
                    ("source", JsonFieldKind::String),
                ],
            )?;
        }
        "xlf.suggest" => {
            require_object_field(result, "count", serde_json::Value::is_u64, "integer")?;
            require_object_field(result, "suggestions", serde_json::Value::is_array, "array")?;
            validate_named_array_object_fields(
                result,
                "suggestions",
                &[
                    ("unit_id", JsonFieldKind::String),
                    ("source", JsonFieldKind::String),
                    ("suggested_translation", JsonFieldKind::String),
                    ("confidence", JsonFieldKind::Number),
                ],
            )?;
        }
        "diag" => {
            if !result.is_object() {
                return Err(response_error("a diagnostic summary object"));
            }
        }
        "tests.coverage" => {
            require_object_field(result, "coverage", serde_json::Value::is_array, "array")?;
            require_object_field(result, "untested", serde_json::Value::is_array, "array")?;
        }
        "tests.affected" => {
            require_object_field(result, "affected", serde_json::Value::is_array, "array")?;
            validate_named_array_object_fields(
                result,
                "affected",
                &[
                    ("codeunitName", JsonFieldKind::String),
                    ("codeunitId", JsonFieldKind::Integer),
                    ("methodName", JsonFieldKind::String),
                    ("line", JsonFieldKind::Integer),
                ],
            )?;
        }
        "tests.last_results" => {
            let Some(object) = result.as_object() else {
                return Err(response_error("a test-history object"));
            };
            match (object.get("lastResult"), object.get("results")) {
                (Some(last), _) if last.is_null() || last.is_object() => {}
                (_, Some(results)) if results.is_array() => {}
                _ => {
                    return Err(response_error(
                        "an object with object/null 'lastResult' or array 'results'",
                    ));
                }
            }
        }
        "tests.classify" => {
            require_object_field(
                result,
                "classifications",
                serde_json::Value::is_array,
                "array",
            )?;
            validate_named_array_object_fields(
                result,
                "classifications",
                &[
                    ("codeunitId", JsonFieldKind::Integer),
                    ("codeunitName", JsonFieldKind::String),
                    ("methodName", JsonFieldKind::String),
                    ("decision", JsonFieldKind::String),
                    ("runsLocally", JsonFieldKind::Boolean),
                    ("execution", JsonFieldKind::String),
                    ("reasons", JsonFieldKind::Array),
                ],
            )?;
        }
        "permissions.audit" => {
            for field in ["coverage", "overBroad", "overGrantedRights"] {
                require_object_field(result, field, serde_json::Value::is_array, "array")?;
            }
            validate_named_array_object_fields(
                result,
                "coverage",
                &[
                    ("kind", JsonFieldKind::String),
                    ("id", JsonFieldKind::Integer),
                    ("name", JsonFieldKind::String),
                    ("covered", JsonFieldKind::Boolean),
                    ("coveredBy", JsonFieldKind::Array),
                ],
            )?;
            validate_named_array_object_fields(
                result,
                "overBroad",
                &[
                    ("permissionSet", JsonFieldKind::String),
                    ("objectType", JsonFieldKind::String),
                    ("object", JsonFieldKind::String),
                    ("rights", JsonFieldKind::String),
                    ("reason", JsonFieldKind::String),
                ],
            )?;
            validate_named_array_object_fields(
                result,
                "overGrantedRights",
                &[
                    ("permissionSet", JsonFieldKind::String),
                    ("objectType", JsonFieldKind::String),
                    ("object", JsonFieldKind::String),
                    ("grantedRights", JsonFieldKind::String),
                    ("overGranted", JsonFieldKind::String),
                    ("observedRights", JsonFieldKind::String),
                    ("reason", JsonFieldKind::String),
                ],
            )?;
        }
        "deps" => {
            require_object_field(result, "project", serde_json::Value::is_object, "object")?;
            require_object_field(result, "explicit", serde_json::Value::is_array, "array")?;
            require_object_field(result, "all", serde_json::Value::is_array, "array")?;
        }
        _ => {
            return Err(format!(
                "CLI command '{method}' has no registered daemon response contract"
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum JsonFieldKind {
    String,
    Integer,
    Unsigned,
    Number,
    Boolean,
    Array,
}

impl JsonFieldKind {
    fn matches(self, value: &serde_json::Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Integer => value.is_i64(),
            Self::Unsigned => value.is_u64(),
            Self::Number => value.is_number(),
            Self::Boolean => value.is_boolean(),
            Self::Array => value.is_array(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::String => "a string",
            Self::Integer => "an integer",
            Self::Unsigned => "a non-negative integer",
            Self::Number => "a number",
            Self::Boolean => "a boolean",
            Self::Array => "an array",
        }
    }
}

fn validate_array_object_fields(
    value: &serde_json::Value,
    label: &str,
    fields: &[(&str, JsonFieldKind)],
) -> Result<(), String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("daemon response for '{label}' must be an array"))?;
    validate_object_items(items, label, fields)
}

fn validate_named_array_object_fields(
    value: &serde_json::Value,
    field: &str,
    fields: &[(&str, JsonFieldKind)],
) -> Result<(), String> {
    let items = value
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("daemon response field '{field}' must be an array"))?;
    validate_object_items(items, field, fields)
}

fn validate_discovered_test_methods(value: &serde_json::Value) -> Result<(), String> {
    let codeunits = value
        .as_array()
        .ok_or_else(|| "daemon response for 'tests.discover' must be an array".to_string())?;
    for (codeunit_index, codeunit) in codeunits.iter().enumerate() {
        for field in ["tests", "testInitializers", "testCleanups"] {
            let procedures = codeunit
                .get(field)
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    format!("tests.discover[{codeunit_index}].{field} must be an array")
                })?;
            validate_object_items(
                procedures,
                &format!("tests.discover[{codeunit_index}].{field}"),
                &[
                    ("name", JsonFieldKind::String),
                    ("line", JsonFieldKind::Unsigned),
                    ("handlerFunctions", JsonFieldKind::Array),
                ],
            )?;
        }
    }
    Ok(())
}

fn validate_object_items(
    items: &[serde_json::Value],
    label: &str,
    fields: &[(&str, JsonFieldKind)],
) -> Result<(), String> {
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .ok_or_else(|| format!("{label}[{index}] must be an object"))?;
        for (field, kind) in fields {
            let value = object
                .get(*field)
                .ok_or_else(|| format!("{label}[{index}] is missing required field '{field}'"))?;
            if !kind.matches(value) {
                return Err(format!(
                    "{label}[{index}].{field} must be {}, got {}",
                    kind.label(),
                    json_type_name(value)
                ));
            }
        }
    }
    Ok(())
}

fn require_object_field(
    value: &serde_json::Value,
    field: &str,
    predicate: fn(&serde_json::Value) -> bool,
    expected_type: &str,
) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("daemon response must be an object containing '{field}'"))?;
    let field_value = object
        .get(field)
        .ok_or_else(|| format!("daemon response is missing required field '{field}'"))?;
    if !predicate(field_value) {
        return Err(format!(
            "daemon response field '{field}' must be {expected_type}, got {}",
            json_type_name(field_value)
        ));
    }
    Ok(())
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
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
    use super::{absolutize_path, validate_run_command_result};

    #[test]
    fn absolutize_path_passes_through_absolute_paths() {
        let abs = std::env::current_dir().expect("current_dir is required for this test");
        let abs = abs.to_string_lossy();
        assert_eq!(absolutize_path(&abs).unwrap(), abs);
    }

    #[test]
    fn absolutize_path_resolves_relative_paths_against_cwd() {
        // Positive: relative paths must come out absolute.
        let cwd = std::env::current_dir().expect("current_dir is required for this test");
        let resolved = absolutize_path("MyApp").unwrap();
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
        let resolved = absolutize_path("nonexistent.alcpuprofile").unwrap();
        assert!(std::path::Path::new(&resolved).is_absolute());
        assert!(!std::path::Path::new(&resolved).exists());
    }

    #[test]
    fn command_response_contract_rejects_clean_looking_wrong_shapes() {
        for (method, value) in [
            ("obsolete", serde_json::json!({})),
            ("insightStats", serde_json::json!({})),
            (
                "tests.coverage",
                serde_json::json!({ "coverage": [], "untested": null }),
            ),
            (
                "permissions.audit",
                serde_json::json!({
                    "coverage": [],
                    "overBroad": [],
                    "overGrantedRights": null
                }),
            ),
        ] {
            assert!(
                validate_run_command_result(method, &value).is_err(),
                "{method} must reject {value}"
            );
        }
    }

    #[test]
    fn every_run_command_method_has_a_response_contract() {
        let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/commands");
        let mut methods = std::collections::BTreeSet::new();
        for path in super::collect_al_files_for_extension(&source_root, "rs").unwrap() {
            let source = std::fs::read_to_string(&path).unwrap();
            for (offset, _) in source.match_indices("run_command") {
                let after_name = &source[offset + "run_command".len()..];
                let Some(after_open) = after_name.trim_start().strip_prefix('(') else {
                    continue;
                };
                let Some(after_quote) = after_open.trim_start().strip_prefix('"') else {
                    continue;
                };
                if let Some((method, _)) = after_quote.split_once('"') {
                    methods.insert(method.to_string());
                }
            }
        }
        assert!(!methods.is_empty());
        for method in methods {
            let probe = match method.as_str() {
                "entrypoints"
                | "obsolete"
                | "audit.dataClassification"
                | "breaking"
                | "arch.lint"
                | "nativeCheck"
                | "upgrade"
                | "object"
                | "byId"
                | "sqlPatterns"
                | "rules"
                | "errorCodes"
                | "builtinTypes"
                | "tests.discover" => serde_json::json!([]),
                "insightStats" => serde_json::json!({"nodes": 0, "edges": 0}),
                "xlf.refresh" => {
                    serde_json::json!({
                        "added": [],
                        "changed": [],
                        "removed": [],
                        "preserved": 0
                    })
                }
                "xlf.untranslated" => serde_json::json!({"count": 0, "untranslated": []}),
                "xlf.suggest" => serde_json::json!({"count": 0, "suggestions": []}),
                "tests.affected" => serde_json::json!({"affected": []}),
                "tests.last_results" => serde_json::json!({"results": []}),
                "tests.classify" => serde_json::json!({"classifications": []}),
                "tests.coverage" => serde_json::json!({"coverage": [], "untested": []}),
                "permissions.audit" => serde_json::json!({
                    "coverage": [],
                    "overBroad": [],
                    "overGrantedRights": []
                }),
                "deps" => serde_json::json!({"project": {}, "explicit": [], "all": []}),
                "diag" => serde_json::json!({}),
                "xlf.generate" => serde_json::json!({"path": null, "units": 0}),
                other => panic!("run_command method has no test probe: {other}"),
            };
            validate_run_command_result(&method, &probe)
                .unwrap_or_else(|error| panic!("{method}: {error}"));
        }
    }

    #[test]
    fn daemon_requests_cannot_bypass_the_checked_boundary() {
        // This used to scan only `src/cli/commands`, which allowed the TUI to
        // call DaemonClient directly and silently interpret malformed payloads
        // as empty results. Enforce one checked JSON-RPC boundary for the whole
        // al-explorer crate.
        let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut direct_requests = Vec::new();
        let request_needle = ["client", ".request("].concat();
        for path in super::collect_al_files_for_extension(&source_root, "rs").unwrap() {
            let source = std::fs::read_to_string(&path).unwrap();
            for (line_index, line) in source.lines().enumerate() {
                if line.contains(&request_needle) {
                    direct_requests.push((path.clone(), line_index + 1, line.to_string()));
                }
            }
        }
        assert_eq!(
            direct_requests.len(),
            1,
            "every daemon request must pass through request_checked: {direct_requests:#?}"
        );
        let (path, _, line) = &direct_requests[0];
        assert!(path.ends_with("mod.rs"), "{path:?}");
        assert!(
            line.contains(&format!("let result = {request_needle}")),
            "{line}"
        );
    }

    #[test]
    fn every_literal_checked_request_has_a_registered_contract() {
        let source_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cli/commands");
        let mut methods = std::collections::BTreeSet::new();
        for path in super::collect_al_files_for_extension(&source_root, "rs").unwrap() {
            let source = std::fs::read_to_string(&path).unwrap();
            for (offset, _) in source.match_indices("request_checked") {
                let after_name = &source[offset + "request_checked".len()..];
                let Some(after_open) = after_name.trim_start().strip_prefix('(') else {
                    continue;
                };
                let Some(after_client) = after_open.trim_start().strip_prefix("&mut client") else {
                    continue;
                };
                let Some(after_comma) = after_client.trim_start().strip_prefix(',') else {
                    continue;
                };
                let Some(after_quote) = after_comma.trim_start().strip_prefix('"') else {
                    continue;
                };
                if let Some((method, _)) = after_quote.split_once('"') {
                    methods.insert(method.to_string());
                }
            }
        }
        assert!(methods.len() > 40, "unexpectedly few methods: {methods:#?}");
        for method in methods {
            if super::response_contract::handles(&method) {
                continue;
            }
            let error = validate_run_command_result(&method, &serde_json::Value::Null)
                .expect_err("a null probe must not satisfy a registered response contract");
            assert!(
                !error.contains("has no registered daemon response contract"),
                "{method} bypasses response validation"
            );
        }

        // The two generic query helpers receive these method names dynamically.
        for method in [
            "hover",
            "definition",
            "typeDefinition",
            "declaration",
            "implementation",
            "references",
            "signatureHelp",
            "completions",
            "documentSymbols",
            "foldingRanges",
            "semanticTokens",
            "search",
            "object",
            "location",
            "events",
            "trace",
            "impact",
            "tests.discover",
            "tests.last_results",
            "tests.run_batch",
            "tests.run_auto",
        ] {
            if super::response_contract::handles(method) {
                continue;
            }
            let error = validate_run_command_result(method, &serde_json::Value::Null)
                .expect_err("a null probe must not satisfy a registered response contract");
            assert!(
                !error.contains("has no registered daemon response contract"),
                "{method} has no response contract"
            );
        }
    }
}
