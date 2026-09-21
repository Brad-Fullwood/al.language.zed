pub mod build;
pub mod debug;
pub mod insight;
pub mod lsp;
mod response_contract;

use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;

use al_protocol::DaemonClient;

/// Whether `--compact` was passed, set once by `cli::run`.
static COMPACT_JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Print JSON on one line from here on.
///
/// Indentation was 43% of the bytes of the largest measured answer:
/// `by-id codeunit 80` was 552,710 bytes pretty-printed and 315,393 compact,
/// for the same content.
pub fn set_compact_json(compact: bool) {
    COMPACT_JSON.store(compact, std::sync::atomic::Ordering::Relaxed);
}

pub fn print_json<T: Serialize>(value: &T) {
    let rendered = if COMPACT_JSON.load(std::sync::atomic::Ordering::Relaxed) {
        serde_json::to_string(value)
    } else {
        serde_json::to_string_pretty(value)
    };
    match rendered {
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

/// Build the JSON-RPC params shared by the `snapshot`/`profile` subcommands.
///
/// `company` is required (BC's dev endpoints reject an empty `?company=`, and
/// the daemon would otherwise fail after an already-established connection)
/// — validated here so the CLI fails fast with a clear message instead of
/// round-tripping to the daemon first.
///
/// `username`/`password` fall back to the `BC_USERNAME`/`BC_PASSWORD`
/// environment variables when the corresponding `--username`/`--password`
/// flag is omitted, which keeps the credential out of shell history and
/// `/proc/<pid>/cmdline`. There is no credentials-file reader.
pub fn bc_server_params(
    cmd: &str,
    server: &str,
    company: &str,
    username: Option<&str>,
    password: Option<&str>,
    output_dir: Option<&str>,
) -> Result<serde_json::Value, String> {
    if company.trim().is_empty() {
        return Err(
            "--company is required (Business Central rejects an empty ?company= parameter)"
                .to_string(),
        );
    }
    let username = username
        .map(str::to_string)
        .or_else(|| std::env::var("BC_USERNAME").ok());
    let password = password
        .map(str::to_string)
        .or_else(|| std::env::var("BC_PASSWORD").ok());
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

/// Resolve `lint`'s positional file arguments (`#[arg(num_args = 0..)]`) into
/// the list of files that should actually be linted.
///
/// The `Vec<String>` exists to work around Zed's `$ZED_FILE` expansion
/// splitting a single path containing spaces across multiple argv entries —
/// the original fix always joined every argument with `" "` to reconstruct
/// that one path. That silently broke the equally legitimate multi-file
/// invocation `al-explorer lint A.al B.al`, which resolved to the bogus
/// single path `"A.al B.al"` and failed instead of linting two files.
///
/// Disambiguate by checking the filesystem: a single argument is always one
/// file; for two or more, only treat them as fragments of one space-split
/// path when that reconstructed path actually exists on disk, otherwise
/// treat each argument as its own file.
pub fn resolve_lint_targets(file: &[String]) -> Vec<String> {
    match file.len() {
        0 => Vec::new(),
        1 => vec![file[0].clone()],
        _ => {
            let joined = file.join(" ");
            if std::path::Path::new(&joined).is_file() {
                vec![joined]
            } else {
                file.to_vec()
            }
        }
    }
}

/// Make a user-supplied path absolute relative to the current working
/// directory, without requiring the target to exist yet.
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

/// The `--timeout-ms` value for this process, set once by `cli::run`.
///
/// A global rather than a parameter because every one of the ~80 subcommand
/// functions calls [`connect`] and none of them should have to thread a
/// deadline through.
static REQUEST_TIMEOUT_OVERRIDE: std::sync::OnceLock<std::time::Duration> =
    std::sync::OnceLock::new();

/// Record the per-request deadline the caller asked for. Later calls are
/// ignored, so the first (the one `cli::run` makes) wins.
pub fn set_request_timeout_override(millis: u64) {
    if millis > 0 {
        let _ = REQUEST_TIMEOUT_OVERRIDE.set(std::time::Duration::from_millis(millis));
    }
}

pub fn connect(project_dir: Option<&str>) -> Result<DaemonClient, String> {
    let root = project_root(project_dir)?;
    DaemonClient::connect(&root)
        .map(|mut client| {
            if let Some(timeout) = REQUEST_TIMEOUT_OVERRIDE.get() {
                client.set_request_timeout(*timeout);
            }
            client
        })
        .map_err(|e| {
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
    let entries = match list_rows(result).as_array() {
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

/// The global `--limit`, `--offset`, `--fields` and `--scope` for this
/// process, set once by `cli::run`.
///
/// Held globally for the same reason as the request deadline: they apply to
/// every list-returning command and none of the ~80 command functions should
/// have to thread them through.
static PROJECTION_OVERRIDE: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();

/// Record the projection the caller asked for, as the params the daemon reads.
pub fn set_projection_override(
    limit: Option<usize>,
    offset: Option<usize>,
    fields: &[String],
    scope: Option<&str>,
) {
    let mut params = serde_json::Map::new();
    if let Some(limit) = limit {
        params.insert("limit".into(), serde_json::json!(limit));
    }
    if let Some(offset) = offset {
        params.insert("offset".into(), serde_json::json!(offset));
    }
    if !fields.is_empty() {
        params.insert("fields".into(), serde_json::json!(fields));
    }
    if let Some(scope) = scope {
        params.insert("scope".into(), serde_json::json!(scope));
    }
    if !params.is_empty() {
        let _ = PROJECTION_OVERRIDE.set(serde_json::Value::Object(params));
    }
}

/// Add the process-wide projection to one request's params.
///
/// A command that sets one of these itself keeps its own value: `search`
/// passes a `limit` that is the search bound, not a page size.
fn with_projection(params: Option<serde_json::Value>) -> Option<serde_json::Value> {
    let Some(serde_json::Value::Object(overrides)) = PROJECTION_OVERRIDE.get() else {
        return params;
    };
    let mut merged = match params {
        Some(serde_json::Value::Object(object)) => object,
        Some(other) => return Some(other),
        None => serde_json::Map::new(),
    };
    for (key, value) in overrides {
        merged.entry(key.clone()).or_insert_with(|| value.clone());
    }
    Some(serde_json::Value::Object(merged))
}

/// The rows of a list result, whether or not it came back projected.
///
/// A projected root-array method answers `{items, total, returned, offset,
/// truncated}` instead of a bare array, so the human formatters ask for the
/// rows through this rather than each knowing about the envelope. `--json`
/// prints the whole envelope, because `total` and `truncated` are the part an
/// agent needs.
pub fn list_rows(result: &serde_json::Value) -> &serde_json::Value {
    match result.get("items") {
        Some(items) if items.is_array() && result.get("total").is_some() => items,
        _ => result,
    }
}

/// Daemon methods that answer from one file and never write it.
///
/// The daemon refuses a path outside the project it has loaded, because the
/// same dispatchers are published over MCP, where the caller may be an agent
/// and the path may be anything it asks for. The person running the CLI can
/// already read their own files, so for these methods the CLI reads the file
/// and sends its text, and the daemon answers without opening the path.
/// Methods that rewrite the file are absent on purpose: content a caller
/// supplies can be analysed, never written back over a path the daemon was not
/// allowed to name.
const READ_ONLY_FILE_METHODS: &[&str] = &[
    "parse",
    "lint",
    "metrics",
    "hover",
    "definition",
    "references",
    "implementations",
    "completions",
    "signatureHelp",
    "documentSymbols",
    "foldingRanges",
    "semanticTokens",
    "inlayHints",
    "codeActions",
];

/// Whether a daemon error is the refusal to touch the path a request named.
///
/// `DaemonClient::request` flattens the JSON-RPC error to a string ending in
/// `(code N)`, so the code is matched in that form rather than re-parsed.
fn is_path_not_authorized(error: &str) -> bool {
    error.contains(&format!(
        "(code {})",
        al_protocol::jsonrpc::error_codes::PATH_NOT_AUTHORIZED
    ))
}

/// The path a request named, as `file` or as a `file://` URI.
fn requested_path(params: Option<&serde_json::Value>) -> Option<PathBuf> {
    let params = params?;
    if let Some(file) = params.get("file").and_then(|value| value.as_str()) {
        return Some(PathBuf::from(file));
    }
    let uri = params.get("uri").and_then(|value| value.as_str())?;
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

/// The same request with the file's text attached, for a read-only method the
/// daemon refused to open the path for.
fn params_with_text(
    method: &str,
    params: Option<&serde_json::Value>,
    error: &str,
) -> Option<serde_json::Value> {
    if !READ_ONLY_FILE_METHODS.contains(&method) || !is_path_not_authorized(error) {
        return None;
    }
    if params.is_some_and(|params| params.get("text").is_some()) {
        // The text was already sent and still refused: nothing left to try.
        return None;
    }
    let path = requested_path(params)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let mut retry = params?.clone();
    retry.as_object_mut()?.insert("text".into(), text.into());
    Some(retry)
}

/// Whether the request narrowed each row to a chosen set of keys.
fn asked_for_fields(params: Option<&serde_json::Value>) -> bool {
    params
        .and_then(|params| params.get("fields"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|fields| !fields.is_empty())
}

pub fn request_checked(
    client: &mut DaemonClient,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let mut sent = with_projection(params);
    // At most two passes: the second carries the text for a file the daemon
    // may not open, and `params_with_text` returns None once it is attached.
    loop {
        let result = client.request(method, sent.clone());
        let error = match result {
            Ok(result) => {
                // The contracts describe the method's own result shape, so
                // validate the rows rather than the projection envelope
                // wrapped around them. `--fields` removes the very keys they
                // check, and a caller who asked for a subset is not owed an
                // error for getting one.
                if !asked_for_fields(sent.as_ref()) {
                    let checked = list_rows(&result).clone();
                    if response_contract::handles(method) {
                        response_contract::validate(method, sent.as_ref(), &checked)?;
                    } else {
                        validate_run_command_result(method, &checked)?;
                    }
                }
                return Ok(result);
            }
            Err(error) => error,
        };
        match params_with_text(method, sent.as_ref(), &error) {
            Some(retry) => sent = Some(retry),
            None => return Err(explain_path_refusal(&error)),
        }
    }
}

/// Say why a path was refused when the CLI cannot work around it.
fn explain_path_refusal(error: &str) -> String {
    if is_path_not_authorized(error) {
        format!(
            "{error}\n\nHint: this command rewrites the file it is given, and the daemon changes \
             only files inside the project named above. Run it from that file's own project."
        )
    } else {
        error.to_string()
    }
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
        // `mode` selects which of the remaining fields are present, so it is
        // the one field the formatter cannot do without.
        "freeIds" => {
            require_object_field(result, "mode", serde_json::Value::is_string, "string")?;
            require_object_field(result, "usedCount", serde_json::Value::is_i64, "integer")?;
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
mod bc_server_params_tests {
    use super::bc_server_params;

    #[test]
    fn rejects_empty_company() {
        let err = bc_server_params("start", "http://localhost:7049/BC", "", None, None, None)
            .expect_err("empty --company must be rejected");
        assert!(err.contains("--company"), "got: {err}");
    }

    #[test]
    fn rejects_whitespace_only_company() {
        let err = bc_server_params("start", "http://localhost:7049/BC", "   ", None, None, None)
            .expect_err("whitespace-only --company must be rejected");
        assert!(err.contains("--company"), "got: {err}");
    }

    #[test]
    fn explicit_credentials_are_used_verbatim() {
        let params = bc_server_params(
            "start",
            "http://localhost:7049/BC",
            "CRONUS",
            Some("explicit-user"),
            Some("explicit-pass"),
            None,
        )
        .unwrap();
        assert_eq!(params["username"], "explicit-user");
        assert_eq!(params["password"], "explicit-pass");
    }

    #[test]
    #[serial_test::serial(bc_creds_env)]
    fn falls_back_to_env_vars_when_flags_are_absent() {
        // SAFETY: serialised via #[serial_test::serial] on this test's lock key.
        unsafe {
            std::env::set_var("BC_USERNAME", "env-user");
            std::env::set_var("BC_PASSWORD", "env-pass");
        }
        let result = bc_server_params(
            "start",
            "http://localhost:7049/BC",
            "CRONUS",
            None,
            None,
            None,
        );
        // SAFETY: serialised via #[serial_test::serial] on this test's lock key.
        unsafe {
            std::env::remove_var("BC_USERNAME");
            std::env::remove_var("BC_PASSWORD");
        }
        let params = result.unwrap();
        assert_eq!(params["username"], "env-user");
        assert_eq!(params["password"], "env-pass");
    }

    #[test]
    #[serial_test::serial(bc_creds_env)]
    fn omits_credentials_when_neither_flag_nor_env_present() {
        // SAFETY: serialised via #[serial_test::serial] on this test's lock key.
        unsafe {
            std::env::remove_var("BC_USERNAME");
            std::env::remove_var("BC_PASSWORD");
        }
        let params = bc_server_params(
            "start",
            "http://localhost:7049/BC",
            "CRONUS",
            None,
            None,
            None,
        )
        .unwrap();
        assert!(params.get("username").is_none());
        assert!(params.get("password").is_none());
    }
}

#[cfg(test)]
mod lint_target_tests {
    use super::resolve_lint_targets;

    #[test]
    fn no_files_resolves_to_empty() {
        assert!(resolve_lint_targets(&[]).is_empty());
    }

    #[test]
    fn single_file_passes_through_unchanged() {
        let targets = resolve_lint_targets(&["A.al".to_string()]);
        assert_eq!(targets, vec!["A.al".to_string()]);
    }

    #[test]
    fn multiple_nonexistent_paths_are_treated_as_separate_files() {
        // Neither "A.al" nor "A.al B.al" exists on disk, so two argv entries
        // must resolve to two separate lint targets — the regression this
        // guards: `al-explorer lint A.al B.al` used to silently become the
        // single bogus path "A.al B.al".
        let targets = resolve_lint_targets(&["A.al".to_string(), "B.al".to_string()]);
        assert_eq!(targets, vec!["A.al".to_string(), "B.al".to_string()]);
    }

    #[test]
    fn multiple_args_reconstruct_one_path_containing_a_space_when_it_exists() {
        // Zed's $ZED_FILE splitting produces multiple argv entries for a
        // single path containing a literal space; when the joined
        // reconstruction actually exists on disk, treat it as one file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("My File.al");
        std::fs::write(&path, "codeunit 1 X {}").unwrap();
        let parts: Vec<String> = path
            .to_string_lossy()
            .split(' ')
            .map(str::to_string)
            .collect();
        assert!(parts.len() >= 2, "fixture path must contain a space");

        let targets = resolve_lint_targets(&parts);
        assert_eq!(targets, vec![path.to_string_lossy().into_owned()]);
    }
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
            // `request_checked` consults `response_contract` first, so a method
            // it claims never reaches this fallback module.
            if super::response_contract::handles(&method) {
                continue;
            }
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
                "freeIds" => serde_json::json!({"mode": "summary", "usedCount": 0}),
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

#[cfg(test)]
mod subcommand_exit_code_tests {
    use std::process::ExitCode;

    use super::build::xlf_generated_path;
    use super::insight::dead_code_exit_code;
    use super::lsp::env::doctor_exit_code;
    use super::lsp::project::any_tenant_authenticated;
    use super::lsp::refactor::captured_test_failures;

    fn is_success(code: ExitCode) -> bool {
        format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
    }

    /// `Docs/reference/cli-commands.md` says exit 0 means the gate passed, and
    /// that a non-empty report is not silently treated as success. One row per
    /// command that can report a failed gate through a *valid* response, which
    /// is the case `report_error` does not cover.
    #[test]
    fn a_failed_gate_never_exits_zero() {
        // (command, the passing response reads as success, the failing one does)
        let rows: Vec<(&str, bool, bool)> = vec![
            (
                "dead-code",
                is_success(dead_code_exit_code(&serde_json::json!([]))),
                // Every finding is medium confidence, which is what
                // al-analysis emits for an unreferenced object.
                is_success(dead_code_exit_code(
                    &serde_json::json!([{ "n": "Unused", "confidence": "Medium" }]),
                )),
            ),
            (
                "setup",
                is_success(doctor_exit_code(&serde_json::json!({
                    "altoolInstalled": true,
                    "dotnetVersion": "8.0.100",
                    "project": {},
                    "indexedSymbols": 10,
                    "workspaceFiles": 3,
                }))),
                is_success(doctor_exit_code(&serde_json::json!({
                    "altoolInstalled": false,
                    "dotnetVersion": serde_json::Value::Null,
                    "project": {},
                }))),
            ),
            (
                "authenticate status",
                any_tenant_authenticated(&serde_json::json!({
                    "tenants": [{ "tenant": "contoso", "authenticated": true, "expired": false }]
                })),
                any_tenant_authenticated(&serde_json::json!({
                    "tenants": [
                        { "tenant": "contoso", "authenticated": false, "expired": true },
                        { "tenant": "fabrikam", "authenticated": true, "expired": true },
                    ]
                })),
            ),
            (
                "xlf generate",
                xlf_generated_path(&serde_json::json!({ "path": "Translations/App.g.xlf" }))
                    .is_some(),
                xlf_generated_path(&serde_json::json!({ "path": serde_json::Value::Null }))
                    .is_some(),
            ),
            (
                "test-snapshot capture",
                captured_test_failures(&serde_json::json!({ "testResult": { "failed": 0 } })) == 0,
                captured_test_failures(&serde_json::json!({ "testResult": { "failed": 2 } })) == 0,
            ),
        ];

        for (command, passing_is_zero, failing_is_zero) in rows {
            assert!(
                passing_is_zero,
                "{command} must exit 0 when the gate passes"
            );
            assert!(
                !failing_is_zero,
                "{command} must not exit 0 when the gate fails"
            );
        }
    }
}
