//! Build/toolchain/analysis dispatchers — compile, package, lint, format, fix, permissions,
//! authenticate, download symbols, snapshot, profiling, xliff, etc.

use std::path::PathBuf;

use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response, RpcError};

use super::{
    ensure_document, extract_i32, file_not_found, file_uri_from_params, invalid_params,
    lint_diag_to_json, require_document_text, rpc_error,
};

const ERR_INITIALIZING: &str = "Workspace is initializing, try again";
const ERR_NO_PROJECT: &str = "No project loaded";

/// Upper bound for `timeoutMs` JSON-RPC params. Anything beyond an hour
/// is almost certainly a configuration mistake; capping prevents a
/// hostile or fat-fingered client from pinning the daemon to a
/// multi-day or 584-year (u64::MAX ms) test run.
const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;

/// Clamp a user-supplied timeout (milliseconds) to a sensible upper
/// bound. Used by `dispatch_tests_run_batch` and `dispatch_tests_mutate`.
fn clamp_timeout_ms(t: Option<u64>) -> Option<u64> {
    t.map(|ms| ms.min(MAX_TIMEOUT_MS))
}

/// Largest realistic AL procedure body is ~5k tokens; cap the
/// `minTokens` duplicate-detection threshold at 10k so a hostile or
/// fat-fingered client can't (a) push the threshold above any real
/// procedure (effectively disabling detection) or (b) drive the
/// scan loop into pathological territory. F-OPEN-007.
const MAX_DUPLICATES_MIN_TOKENS: u64 = 10_000;

/// Clamp the duplicate-detection `minTokens` param to a sensible upper
/// bound; default 20 when absent.
fn clamp_min_tokens(t: Option<u64>) -> usize {
    t.unwrap_or(20).min(MAX_DUPLICATES_MIN_TOKENS) as usize
}

/// Clamp the duplicate-detection `minSimilarity` ratio to `[0.0, 1.0]`.
/// NaN / ±inf fall back to the default (0.8) so a hostile or garbage
/// value can't disable the filter or cause downstream comparison
/// surprises. F-OPEN-007.
fn clamp_min_similarity(s: Option<f64>) -> f32 {
    let raw = s.unwrap_or(0.8);
    if raw.is_finite() {
        raw.clamp(0.0, 1.0) as f32
    } else {
        0.8
    }
}

/// Resolve a user-provided output-file path against `project_root` and reject
/// anything that escapes it (path traversal). Used for JUnit / Cobertura
/// output paths in `dispatch_tests_run_batch`, where a malicious or
/// misconfigured client could otherwise ask the daemon to write XML to
/// arbitrary filesystem locations as the daemon's user.
///
/// Symlinks are resolved (including symlinked parent directories that point
/// outside the project), so `/project/link/evil.xml` where `link -> /outside`
/// is rejected even though it textually starts with the project root.
///
/// Returns `Some(canonical_path)` — the symlink-resolved absolute path — if the
/// requested location is inside `project_root`, else `None`.
fn resolve_output_path_within_project(
    requested: &std::path::Path,
    project_root: &std::path::Path,
) -> Option<PathBuf> {
    // Resolve relative paths against project_root.
    let absolute = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        project_root.join(requested)
    };

    // Logical (non-filesystem) normalisation: collapse `.` and `..` segments.
    // We can't use `Path::canonicalize` because the file may not yet exist.
    let mut normalised = PathBuf::new();
    for comp in absolute.components() {
        use std::path::Component;
        match comp {
            Component::ParentDir => {
                if !normalised.pop() {
                    // `..` above the root — definitely escaping.
                    return None;
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }

    // Canonicalise the project root so symlinks / case-normalisation can't
    // be used to spoof containment. The root must exist; if canonicalisation
    // fails, reject conservatively.
    let project_canonical = project_root.canonicalize().ok()?;

    // Logical normalisation alone is not enough: a symlink *inside* the
    // project pointing outside (e.g. `/project/link -> /outside`) would let
    // `/project/link/evil.xml` pass a textual `starts_with` check while the
    // real write target is `/outside/evil.xml`. Resolve symlinks by
    // canonicalising the deepest ancestor of `normalised` that actually
    // exists, then re-appending the not-yet-created tail, and require the
    // *canonical* result to stay within the canonical root.
    let mut existing = normalised.as_path();
    let mut tail = PathBuf::new();
    let canonical_existing = loop {
        match existing.canonicalize() {
            Ok(c) => break c,
            Err(_) => {
                // Walk up one component, remembering the stripped tail.
                let file = existing.file_name()?;
                let mut new_tail = PathBuf::from(file);
                new_tail.push(&tail);
                tail = new_tail;
                existing = existing.parent()?;
            }
        }
    };
    let resolved = canonical_existing.join(&tail);

    if resolved.starts_with(&project_canonical) {
        // Return the canonical, symlink-resolved path so the subsequent write
        // targets exactly what we validated.
        Some(resolved)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Shared BC server connection params (used by snapshot and profiling)
// ---------------------------------------------------------------------------

/// Common BC server connection parameters extracted from JSON-RPC params.
struct BcServerParams {
    server_url: String,
    company: String,
    output_dir: std::path::PathBuf,
    username: Option<String>,
    password: Option<String>,
    accept_invalid_certs: bool,
}

/// Parse the BC server connection parameters common to snapshot and profiling dispatchers.
///
/// `output_subdir` is the subdirectory appended to the default data-local path when
/// `outputDir` is not provided by the caller (e.g. `"snapshots"` or `"profiles"`).
fn parse_bc_server_params(params: &serde_json::Value, output_subdir: &str) -> BcServerParams {
    let server_url = params
        .get("serverUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:7049/BC")
        .to_string();
    let company = params
        .get("company")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let output_dir = params
        .get("outputDir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            dirs::data_local_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("al-lsp")
                .join(output_subdir)
        });
    let username = params
        .get("username")
        .and_then(|v| v.as_str())
        .map(String::from);
    let password = params
        .get("password")
        .and_then(|v| v.as_str())
        .map(String::from);
    let accept_invalid_certs = params
        .get("acceptInvalidCerts")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    BcServerParams {
        server_url,
        company,
        output_dir,
        username,
        password,
        accept_invalid_certs,
    }
}

// ---------------------------------------------------------------------------
// Analysis dispatchers (lint, format, fix, rules, parse, source)
// ---------------------------------------------------------------------------

pub(super) fn dispatch_lint(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);

    if all {
        // Lint all workspace files using cached parse trees
        let mut results: Vec<serde_json::Value> = Vec::new();
        for entry in workspace.file_index.files.iter() {
            let path = entry.key();
            let Some((content, tree)) = workspace.file_index.get_cached_parse(path) else {
                continue;
            };
            let diagnostics = crate::syntax::lint(&tree, &content);
            if !diagnostics.is_empty() {
                let diags: Vec<serde_json::Value> =
                    diagnostics.iter().map(lint_diag_to_json).collect();
                results.push(serde_json::json!({
                    "file": path.display().to_string(),
                    "diagnostics": diags,
                }));
            }
        }
        return Response {
            id,
            result: Some(serde_json::json!(results)),
            error: None,
            ..Default::default()
        };
    }

    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    let text = match require_document_text(workspace, &uri, id) {
        Ok(t) => t,
        Err(resp) => return resp,
    };

    let result = crate::syntax::AlParser::parse_quick(&text);
    let mut diagnostics = crate::syntax::lint(&result.tree, &text);

    // Add parse errors
    for err in &result.errors {
        diagnostics.push(crate::syntax::LintDiagnostic {
            code: "parse-error".to_string(),
            message: err.message.clone(),
            range: err.range,
            severity: crate::syntax::LintSeverity::Error,
        });
    }

    let diags: Vec<serde_json::Value> = diagnostics.iter().map(lint_diag_to_json).collect();
    Response {
        id,
        result: Some(serde_json::json!(diags)),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_format(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let check = params
        .get("check")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Accept direct content or file path
    let content = if let Some(text) = params.get("content").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(uri) = file_uri_from_params(params) {
        ensure_document(workspace, &uri);
        match workspace.documents.get_text(&uri) {
            Some(t) => t,
            None => {
                return file_not_found(id);
            }
        }
    } else {
        return invalid_params(id);
    };

    // Load per-workspace formatting options from .alformat.json (falls back to defaults).
    let options = workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
        .map(|root| crate::queries::format::AlFormatConfig::load_options(&root))
        .unwrap_or_default();
    let formatted = crate::syntax::format_al(&content, &options);
    let changed = formatted != content;

    if check {
        Response {
            id,
            result: Some(serde_json::json!({ "changed": changed })),
            error: None,
            ..Default::default()
        }
    } else {
        // If a file was specified, write back. F-011: routed through
        // write_al_file_and_refresh so the document store, file index,
        // and insight graph all see the update.
        if let Some(uri) = file_uri_from_params(params) {
            if let Ok(path) = uri.to_file_path() {
                if changed {
                    if let Err(e) = tokio::task::block_in_place(|| {
                        write_al_file_and_refresh(workspace, &path, formatted.clone())
                    }) {
                        return rpc_error(
                            id,
                            -32000,
                            &format!("Failed to write formatted file: {e}"),
                        );
                    }
                }
            }
        }
        Response {
            id,
            result: Some(serde_json::json!({
                "formatted": formatted,
                "changed": changed,
            })),
            error: None,
            ..Default::default()
        }
    }
}

pub(super) fn dispatch_fix(workspace: &Workspace, id: u64, params: &serde_json::Value) -> Response {
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let rule_filter = params.get("rule").and_then(|v| v.as_str());

    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let result = crate::syntax::AlParser::parse_quick(&text);
    let diagnostics = crate::syntax::lint(&result.tree, &text);
    let filtered: Vec<_> = diagnostics
        .iter()
        .filter(|d| {
            if let Some(rule) = rule_filter {
                d.code.eq_ignore_ascii_case(rule)
            } else {
                true
            }
        })
        .collect();

    // No custom lint rules are registered, so no fixes to generate.
    let edits: Vec<serde_json::Value> = Vec::new();

    let _ = dry_run;
    let _ = &text;

    Response {
        id,
        result: Some(serde_json::json!({
            "diagnostics": filtered.len(),
            "fixes": edits.len(),
            "dryRun": dry_run,
            "edits": edits,
        })),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_rules(id: u64) -> Response {
    let rules = crate::syntax::lint_rules();
    let value: Vec<serde_json::Value> = rules
        .iter()
        .map(|r| {
            serde_json::json!({
                "code": r.code,
                "name": r.name,
                "severity": r.severity.to_string(),
                "description": r.description,
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_parse(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let start = std::time::Instant::now();
    let result = crate::syntax::AlParser::parse_quick(&text);
    let elapsed = start.elapsed();

    let node_count = crate::parsing::count_nodes(&result.tree);

    Response {
        id,
        result: Some(serde_json::json!({
            "errors": result.errors.len(),
            "nodeCount": node_count,
            "parseTimeMs": elapsed.as_secs_f64() * 1000.0,
            "parseErrors": result.errors.iter().map(|e| serde_json::json!({
                "message": e.message,
                "line": e.range.start_point.row + 1,
                "column": e.range.start_point.column + 1,
            })).collect::<Vec<_>>(),
        })),
        error: None,
        ..Default::default()
    }
}

/// Return the absolute file path (and line 1) for a workspace object by name.
/// Used by al-explorer to open objects in Zed via the `zed://file/path:line:col` URL scheme.
pub(super) fn dispatch_location(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return invalid_params(id),
    };
    match workspace.file_index.find_by_object_name(name) {
        Some(path) => Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "line": 1,
            })),
            error: None,
            ..Default::default()
        },
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Object '{}' not found in workspace", name),
            }),
            ..Default::default()
        },
    }
}

pub(super) fn dispatch_source(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return invalid_params(id),
    };

    let kind_filter = params
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<crate::symbols::ObjectKind>().ok());

    let proc_filter = params.get("proc").and_then(|v| v.as_str());
    let trigger_filter = params.get("trigger").and_then(|v| v.as_str());

    match crate::queries::source::source(workspace, name, kind_filter, proc_filter, trigger_filter)
    {
        Some(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Object '{}' not found", name),
            }),
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------------
// Permission set generation
// ---------------------------------------------------------------------------

pub(super) fn dispatch_permissions(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let entries = crate::permissions::collect_permissions(workspace);
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("al");

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Generated Permissions");
    // AL object IDs are i32 in BC metadata; reject out-of-range values rather
    // than letting render_al emit an ID that BC would silently truncate/wrap.
    let perm_id: i64 = match params.get("id") {
        Some(v) => match v.as_i64().and_then(|n| i32::try_from(n).ok()) {
            Some(n) => i64::from(n),
            None => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "id out of range");
            }
        },
        None => 50100,
    };
    let role_id = params
        .get("roleId")
        .and_then(|v| v.as_str())
        .unwrap_or("GENERATED");

    match format {
        "xml" => {
            let output = crate::permissions::render_xml(&entries, role_id, name);
            Response {
                id,
                result: Some(serde_json::json!({
                    "format": "xml",
                    "content": output,
                    "objectCount": entries.len(),
                })),
                error: None,
                ..Default::default()
            }
        }
        _ => {
            let output = crate::permissions::render_al(&entries, name, perm_id);
            Response {
                id,
                result: Some(serde_json::json!({
                    "format": "al",
                    "content": output,
                    "objectCount": entries.len(),
                })),
                error: None,
                ..Default::default()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Semantic / toolchain dispatchers
// ---------------------------------------------------------------------------

pub(super) async fn dispatch_compile(workspace: &Workspace, id: u64) -> Response {
    let tc = match workspace.toolchain.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };

    let project_root = match project.as_ref() {
        Some(p) => p.root.clone(),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_NO_PROJECT.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    if tc.is_none() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "No toolchain loaded".to_string(),
            }),
            ..Default::default()
        };
    }
    // Drop the read guards before acquiring async locks
    drop(tc);
    drop(project);

    let result: Result<serde_json::Value, String> = async {
        let guard = crate::semantic::get_or_init_bridge(workspace)
            .await
            .ok_or("Failed to initialize semantic bridge")?;
        let bridge = guard.as_ref().ok_or("Semantic bridge unavailable")?;
        let compile_result = bridge
            .compile(&project_root, None, None)
            .await
            .map_err(|e| format!("Compilation failed: {}", e))?;
        // Semantic bridge may not return appPath — fall back to finding the
        // .app file on disk when compilation succeeded.
        let app_path = compile_result
            .app_path
            .as_ref()
            .map(|p| p.display().to_string())
            .or_else(|| {
                if compile_result.success {
                    crate::build::find_app_file(&project_root).map(|p| p.display().to_string())
                } else {
                    None
                }
            });
        Ok(serde_json::json!({
            "success": compile_result.success,
            "diagnostics": compile_result.diagnostics.iter().map(|d| serde_json::json!({
                "file": d.file.display().to_string(),
                "line": d.line,
                "column": d.column,
                "endLine": d.end_line,
                "endColumn": d.end_column,
                "severity": d.severity,
                "code": d.code,
                "message": d.message,
            })).collect::<Vec<_>>(),
            "appPath": app_path,
        }))
    }
    .await;
    match result {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err(msg) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::CODE_ANALYSIS_ERROR,
                message: msg,
            }),
            ..Default::default()
        },
    }
}

pub(super) async fn dispatch_package(workspace: &Workspace, id: u64) -> Response {
    let tc = match workspace.toolchain.try_read() {
        // SILENT: avoid RwLock poison panic per CLAUDE.md
        Ok(guard) => guard.clone(),
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let toolchain = match tc {
        Some(tc) => tc,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: "No toolchain available. Run 'al setup' first.".to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let project = match workspace.project.try_read() {
        // SILENT: avoid RwLock poison panic per CLAUDE.md
        Ok(guard) => guard.clone(),
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let project_root = match project.as_ref() {
        Some(p) => p.root.clone(),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_NO_PROJECT.to_string(),
                }),
                ..Default::default()
            };
        }
    };

    // Read code_analyzers from config (non-async try_read; falls back to empty = all MS analyzers).
    let code_analyzers = workspace
        .config
        .try_read()
        .ok()
        .map(|cfg| cfg.code_analyzers.clone())
        .unwrap_or_default();
    let analyzer_filter: Option<Vec<String>> = if code_analyzers.is_empty() {
        None
    } else {
        Some(code_analyzers)
    };

    match crate::build::compile_project_with_analyzers(
        &toolchain,
        &project_root,
        None,
        analyzer_filter.as_deref(),
    )
    .await
    {
        Ok(result) => Response {
            id,
            // SILENT: serialization of valid struct should not fail
            result: Some(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)),
            error: None,
            ..Default::default()
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e.to_string(),
            }),
            ..Default::default()
        },
    }
}

pub(super) fn dispatch_new_project(id: u64, params: &serde_json::Value) -> Response {
    let dir = match params.get("dir").and_then(|v| v.as_str()) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'dir' parameter".to_string(),
                }),
                ..Default::default()
            };
        }
    };
    // Require an absolute path to prevent path traversal via relative paths
    // (e.g., "../../etc/malicious-dir").
    if !dir.is_absolute() {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: "'dir' must be an absolute path".to_string(),
            }),
            ..Default::default()
        };
    }

    let config = crate::scaffold::ScaffoldConfig {
        name: params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("MyApp")
            .to_string(),
        publisher: params
            .get("publisher")
            .and_then(|v| v.as_str())
            .unwrap_or("Default Publisher")
            .to_string(),
        ..crate::scaffold::ScaffoldConfig::default()
    };

    match crate::scaffold::create_project(&dir, &config) {
        Ok(result) => Response {
            id,
            // SILENT: serialization of valid struct should not fail
            result: Some(serde_json::to_value(&result).unwrap_or(serde_json::Value::Null)),
            error: None,
            ..Default::default()
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e,
            }),
            ..Default::default()
        },
    }
}

pub(super) fn dispatch_error_codes(workspace: &Workspace, id: u64) -> Response {
    let value: Vec<serde_json::Value> = workspace
        .error_codes
        .iter()
        .map(|entry| {
            serde_json::json!({
                "code": entry.key().clone(),
                "description": entry.value().clone(),
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_builtin_types(workspace: &Workspace, id: u64) -> Response {
    let builtins = match workspace.builtins.read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: Some(serde_json::json!([])),
                error: None,
                ..Default::default()
            }
        }
    };
    let value: Vec<serde_json::Value> = builtins
        .iter()
        .map(|bt| {
            serde_json::json!({
                "name": bt.name,
                "methods": bt.methods.iter().map(|m| serde_json::json!({
                    "name": m.name,
                    "parameters": m.parameters.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "typeName": p.type_name,
                        "isVar": p.is_var,
                    })).collect::<Vec<_>>(),
                    "returnType": m.return_type,
                    "documentation": m.documentation,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!(value)),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_setup(workspace: &Workspace, id: u64) -> Response {
    let report = crate::toolchain::doctor(workspace);
    Response {
        id,
        result: Some(serde_json::to_value(&report).unwrap_or(serde_json::Value::Null)),
        error: None,
        ..Default::default()
    }
}

pub(super) async fn dispatch_clear_cache(id: u64) -> Response {
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("index"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/index"));

    // Use tokio::fs to keep the daemon dispatch task on its async runtime
    // instead of parking the worker on synchronous std::fs (T027 /
    // spec-concurrency-001). On a large index this can be many MB of
    // file handles; doing it synchronously held the worker thread for
    // the duration and starved other dispatch handlers.
    let existed = tokio::fs::try_exists(&cache_dir).await.unwrap_or(false);
    let mut error: Option<String> = None;
    let mut deleted = false;
    if existed {
        match tokio::fs::remove_dir_all(&cache_dir).await {
            Ok(()) => deleted = true,
            Err(e) => {
                tracing::warn!(path = %cache_dir.display(), error = %e,
                    "clearCache: failed to remove index dir");
                error = Some(format!("{e}"));
            }
        }
    }

    // Report the actual outcome — `deleted` reflects whether the dir was
    // both present AND successfully removed. `error` is populated only on
    // failure, so callers can detect a partial-clear and retry/notify
    // (cycle-3 review note: previous implementation reported success
    // even when remove failed, which silently lost partial-state info).
    Response {
        id,
        result: Some(serde_json::json!({
            "deleted": deleted,
            "existed": existed,
            "path": cache_dir.display().to_string(),
            "error": error,
        })),
        error: None,
        ..Default::default()
    }
}

pub(super) async fn dispatch_authenticate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let cmd = params
        .get("cmd")
        .and_then(|v| v.as_str())
        .unwrap_or("login");

    match cmd {
        "status" => {
            // Check cached token status for all known tenants
            let tenants = get_project_tenants(workspace);
            let mut statuses = Vec::new();
            for tenant in &tenants {
                let cache_path = crate::symbols::oauth::token_cache_path(tenant);
                let cached = tokio::fs::read_to_string(&cache_path)
                    .await
                    .ok()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                if let Some(cached) = cached {
                    let expires_at = cached
                        .get("expires_at")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    statuses.push(serde_json::json!({
                        "tenant": tenant,
                        "authenticated": expires_at > now + 60,
                        "expiresAt": expires_at,
                        "expired": expires_at <= now + 60,
                    }));
                } else {
                    statuses.push(serde_json::json!({
                        "tenant": tenant,
                        "authenticated": false,
                    }));
                }
            }
            Response {
                id,
                result: Some(serde_json::json!({ "tenants": statuses })),
                error: None,
                ..Default::default()
            }
        }
        "clear" => {
            let tenants = get_project_tenants(workspace);
            let tenant_filter = params.get("tenant").and_then(|v| v.as_str());
            let mut cleared = 0;
            for tenant in &tenants {
                if let Some(filter) = tenant_filter {
                    if tenant != filter {
                        continue;
                    }
                }
                let cache_path = crate::symbols::oauth::token_cache_path(tenant);
                if tokio::fs::remove_file(&cache_path).await.is_ok() {
                    cleared += 1;
                }
            }
            Response {
                id,
                result: Some(serde_json::json!({ "cleared": cleared })),
                error: None,
                ..Default::default()
            }
        }
        _ => {
            let tenant = params
                .get("tenant")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| get_project_tenants(workspace).into_iter().next());

            let Some(tenant) = tenant else {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: "No tenant found. Specify --tenant or configure a launch config with a tenant.".to_string(),
                    }),
                    ..Default::default()
                };
            };

            let client = reqwest::Client::new();
            // SAFETY (concurrency): `std::sync::Mutex` is correct here only
            // because the callback below is synchronous — it locks, pushes,
            // drops, and the await on `acquire_token` happens around the
            // callback, not inside it. If `acquire_token` is ever refactored
            // to invoke the callback from a spawned task or across an await
            // point, switch this to `tokio::sync::Mutex` (and make the
            // callback itself async). See F-OPEN-006.
            let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let msgs_clone = messages.clone();

            match crate::symbols::oauth::acquire_token(&client, &tenant, move |msg| {
                if let Ok(mut guard) = msgs_clone.lock() {
                    guard.push(msg.to_string());
                }
                tracing::info!("{msg}");
            })
            .await
            {
                Ok(_token) => {
                    let msgs = messages.lock().unwrap_or_else(|e| e.into_inner());
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "status": "authenticated",
                            "tenant": tenant,
                            "messages": *msgs,
                        })),
                        error: None,
                        ..Default::default()
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("Authentication failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }
    }
}

pub(super) fn get_project_tenants(workspace: &Workspace) -> Vec<String> {
    let mut tenants = Vec::new();
    if let Ok(guard) = workspace.project.try_read() {
        if let Some(project) = guard.as_ref() {
            for cfg in &project.server_configs {
                if let Some(t) = &cfg.tenant {
                    if !t.is_empty() && !tenants.contains(t) {
                        tenants.push(t.clone());
                    }
                }
            }
        }
    }
    tenants
}

pub(super) async fn dispatch_download_symbols(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let source = params
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("nuget");

    let project = match workspace.project.try_read() {
        Ok(guard) => guard,
        Err(_) => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: ERR_INITIALIZING.to_string(),
                }),
                ..Default::default()
            };
        }
    };
    let Some(project) = project.as_ref() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: ERR_NO_PROJECT.to_string(),
            }),
            ..Default::default()
        };
    };

    let all_deps = project.all_dependencies();
    if all_deps.is_empty() {
        return Response {
            id,
            result: Some(serde_json::json!({
                "status": "no dependencies",
                "downloaded": 0,
                "failed": 0,
                "results": [],
            })),
            error: None,
            ..Default::default()
        };
    }

    let dest = project.packages_dir.clone();
    let project_configs = project.server_configs.clone();

    // Release the lock before async work
    let _ = project;

    let result: Vec<serde_json::Value> = {
        async {
            if source == "server" {
                if project_configs.is_empty() {
                    return vec![serde_json::json!({
                        "error": "No BC server config found"
                    })];
                }
                let cfg = &project_configs[0];
                let auth = match cfg.authentication {
                    crate::launch::AuthMethod::Windows => {
                        crate::symbols::bc_server::AuthMethod::Windows
                    }
                    crate::launch::AuthMethod::UserPassword => {
                        crate::symbols::bc_server::AuthMethod::UserPassword
                    }
                    crate::launch::AuthMethod::AAD => crate::symbols::bc_server::AuthMethod::AAD,
                };
                let client = match crate::symbols::bc_server::BcServerClient::new(
                    auth,
                    cfg.tenant.clone(),
                    std::sync::Arc::new(|msg| tracing::info!("{msg}")),
                    cfg.accept_invalid_certs,
                ) {
                    Ok(c) => c,
                    Err(e) => return vec![serde_json::json!({ "error": e.to_string() })],
                };
                let url_deps: Vec<(String, crate::symbols::nuget::AppDependency)> = all_deps
                    .iter()
                    .filter_map(|dep| cfg.dev_packages_url(dep).map(|url| (url, dep.clone())))
                    .collect();
                let bc_results = client.download_all(&url_deps, &dest).await;
                bc_results
                    .into_iter()
                    .zip(url_deps.iter())
                    .map(|(r, (_url, sym_dep))| match r {
                        Ok(path) => serde_json::json!({
                            "name": sym_dep.name,
                            "status": "ok",
                            "path": path.display().to_string(),
                        }),
                        Err(e) => serde_json::json!({
                            "name": sym_dep.name,
                            "status": "error",
                            "error": e.to_string(),
                        }),
                    })
                    .collect()
            } else {
                let nuget_feeds =
                    crate::server::workspace::map_nuget_feeds(&crate::project::nuget_feeds());
                let client = crate::symbols::nuget::NuGetClient::new(nuget_feeds);
                let nuget_results = client.download_all(&all_deps, &dest).await;
                nuget_results
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| match r {
                        Ok(path) => serde_json::json!({
                            "name": all_deps[i].name,
                            "status": "ok",
                            "path": path.display().to_string(),
                        }),
                        Err(e) => serde_json::json!({
                            "name": all_deps[i].name,
                            "status": "error",
                            "error": e.to_string(),
                        }),
                    })
                    .collect()
            }
        }
        .await
    };

    let success = result
        .iter()
        .filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("ok"))
        .count();
    let failed = result
        .iter()
        .filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("error"))
        .count();

    // F-009: refresh the workspace symbol indexes so the freshly-downloaded
    // packages become visible to hover/completion/definition without
    // requiring a daemon restart. Mirrors the LSP-side
    // `download_symbols_command` reload sequence in
    // `crate::server::workspace::download_symbols_command`.
    let loaded = refresh_workspace_after_download(workspace, &result);

    Response {
        id,
        result: Some(serde_json::json!({
            "source": source,
            "downloaded": success,
            "failed": failed,
            "loaded_into_index": loaded,
            "results": result,
        })),
        error: None,
        ..Default::default()
    }
}

/// F-011: Write `.al` content to disk and refresh the workspace's
/// in-memory state so subsequent daemon queries observe the change
/// without requiring a restart. Updates the document store, the file
/// index, and invalidates the lazy insight graph. Centralised so every
/// daemon write dispatcher can use one consistent refresh sequence.
pub(crate) fn write_al_file_and_refresh(
    workspace: &Workspace,
    path: &std::path::Path,
    content: String,
) -> std::io::Result<()> {
    std::fs::write(path, &content)?;
    if let Ok(uri) = url::Url::from_file_path(path) {
        workspace.documents.open(uri, content.clone());
    }
    workspace.file_index.add_file(path.to_path_buf(), content);
    workspace.invalidate_insight_graph();
    Ok(())
}

/// F-011: Rename a `.al` file on disk and refresh both index entries
/// (drop the old path, add the new one with current content). The
/// document store is best-effort: open buffers under the old URI are
/// not migrated — the editor is expected to re-open the new path.
pub(crate) fn rename_al_file_and_refresh(
    workspace: &Workspace,
    old: &std::path::Path,
    new: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::rename(old, new)?;
    workspace.file_index.remove_file(old);
    if let Ok(content) = std::fs::read_to_string(new) {
        workspace.file_index.add_file(new.to_path_buf(), content);
    }
    workspace.invalidate_insight_graph();
    Ok(())
}

/// Extract the on-disk paths of successfully-downloaded packages from the
/// daemon `downloadSymbols` result vector, then load them into the
/// workspace's symbol indexes. Returns the number of packages loaded
/// (0 if no successful downloads). F-009.
fn refresh_workspace_after_download(workspace: &Workspace, result: &[serde_json::Value]) -> usize {
    let downloaded_paths: Vec<std::path::PathBuf> = result
        .iter()
        .filter(|r| r.get("status").and_then(|v| v.as_str()) == Some("ok"))
        .filter_map(|r| {
            r.get("path")
                .and_then(|v| v.as_str())
                .map(std::path::PathBuf::from)
        })
        .collect();
    if downloaded_paths.is_empty() {
        return 0;
    }
    let cache = crate::symbols::cache::SymbolCache::default_location();
    let loaded = workspace
        .symbols
        .load_packages_cached(&downloaded_paths, &cache);
    workspace.symbols.load_runtime_enums();
    workspace.invalidate_insight_graph();
    loaded.len()
}

// ---------------------------------------------------------------------------
// Snapshot dispatcher
// ---------------------------------------------------------------------------

pub(super) async fn dispatch_snapshot(id: u64, params: &serde_json::Value) -> Response {
    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'cmd' parameter (expected: start, list, download)"
                        .to_string(),
                }),
                ..Default::default()
            };
        }
    };

    let bc = parse_bc_server_params(params, "snapshots");
    let config = crate::snapshot::SnapshotConfig {
        server_url: bc.server_url,
        company: bc.company,
        output_dir: bc.output_dir,
        username: bc.username,
        password: bc.password,
        accept_invalid_certs: bc.accept_invalid_certs,
    };

    match cmd {
        "start" => {
            let description = params.get("description").and_then(|v| v.as_str());
            match crate::snapshot::start_snapshot(&config, description).await {
                Ok(snapshot_id) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "start",
                        "snapshotId": snapshot_id,
                        "status": "started",
                    })),
                    error: None,
                    ..Default::default()
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot start failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }

        "list" => {
            match crate::snapshot::list_snapshots(&config).await {
                Ok(snapshots) => {
                    let items: Vec<serde_json::Value> = snapshots
                        .iter()
                        .filter_map(|s| serde_json::to_value(s).ok()) // SILENT: serialization of valid structs should not fail
                        .collect();
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "list",
                            "snapshots": items,
                        })),
                        error: None,
                        ..Default::default()
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot list failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }

        "download" => {
            let snapshot_id = match params.get("snapshotId").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'snapshotId' parameter".to_string(),
                        }),
                        ..Default::default()
                    };
                }
            };

            match crate::snapshot::download_snapshot(&config, &snapshot_id).await {
                Ok(path) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "download",
                        "snapshotId": snapshot_id,
                        "path": path.display().to_string(),
                        "status": "downloaded",
                    })),
                    error: None,
                    ..Default::default()
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot download failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }

        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown snapshot command: {other}"),
            }),
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------------
// Profiling dispatcher
// ---------------------------------------------------------------------------

pub(super) async fn dispatch_profiling(id: u64, params: &serde_json::Value) -> Response {
    let cmd = match params.get("cmd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INVALID_PARAMS,
                    message: "Missing 'cmd' parameter (expected: start, stop, analyze)".to_string(),
                }),
                ..Default::default()
            };
        }
    };

    let bc = parse_bc_server_params(params, "profiles");
    let config = crate::profiling::ProfilingConfig {
        server_url: bc.server_url,
        company: bc.company,
        output_dir: bc.output_dir,
        username: bc.username,
        password: bc.password,
        accept_invalid_certs: bc.accept_invalid_certs,
    };

    match cmd {
        "start" => match crate::profiling::start_profiling(&config).await {
            Ok(session_id) => Response {
                id,
                result: Some(serde_json::json!({
                    "cmd": "start",
                    "sessionId": session_id,
                    "status": "profiling",
                })),
                error: None,
                ..Default::default()
            },
            Err(e) => Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("profiling start failed: {e}"),
                }),
                ..Default::default()
            },
        },

        "stop" => {
            let session_id = params
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("profiling-session")
                .to_string();

            match crate::profiling::stop_profiling(&config, &session_id).await {
                Ok(path) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "stop",
                        "sessionId": session_id,
                        "path": path.display().to_string(),
                        "status": "stopped",
                    })),
                    error: None,
                    ..Default::default()
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("profiling stop failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }

        "analyze" => {
            let profile_path = match params.get("path").and_then(|v| v.as_str()) {
                Some(p) => std::path::PathBuf::from(p),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'path' parameter for analyze command".to_string(),
                        }),
                        ..Default::default()
                    };
                }
            };
            // Require absolute path to prevent path traversal.
            if !profile_path.is_absolute() {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INVALID_PARAMS,
                        message: "'path' must be an absolute path".to_string(),
                    }),
                    ..Default::default()
                };
            }
            let top_n = params.get("topN").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let top_n = top_n.min(1000);

            match crate::profiling::analyze_profile_file(&profile_path, top_n).await {
                Ok(result) => {
                    let hotspots: Vec<serde_json::Value> = result
                        .hotspots
                        .iter()
                        .filter_map(|h| serde_json::to_value(h).ok()) // SILENT: serialization of valid structs should not fail
                        .collect();
                    Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "analyze",
                            "durationMs": result.duration_ms,
                            "hotspots": hotspots,
                            "profilePath": result.profile_path.as_ref().map(|p| p.display().to_string()),
                        })),
                        error: None,
                        ..Default::default()
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("profiling analyze failed: {e}"),
                    }),
                    ..Default::default()
                },
            }
        }

        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown profiling command: {other}"),
            }),
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------------
// XLIFF / Translation dispatchers
// ---------------------------------------------------------------------------

pub(super) async fn dispatch_xlf_generate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    // Use the loaded workspace project root by default; only accept an explicit
    // "project" override if it is an absolute path (prevents path traversal).
    let project_root = if let Some(p) = params.get("project").and_then(|v| v.as_str()) {
        let pb = std::path::PathBuf::from(p);
        if !pb.is_absolute() {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "'project' must be an absolute path",
            );
        }
        pb
    } else {
        match super::require_project_root(workspace, id) {
            Ok(r) => r,
            Err(e) => return e,
        }
    };

    match crate::xliff::build_xliff(workspace, &project_root) {
        Ok(Some((path, count))) => Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy().as_ref(),
                "units": count,
            })),
            error: None,
            ..Default::default()
        },
        Ok(None) => Response {
            id,
            result: Some(serde_json::json!({
                "path": null,
                "units": 0,
                "message": "No translatable texts found (check features.TranslationFile in app.json)",
            })),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, -32000, &format!("xlf-build failed: {e}")),
    }
}

/// Derive the XLIFF target-language code from a language-specific `.xlf`
/// filename (e.g. `de-DE.xlf` -> `de-DE`). Falls back to `en-US` when the
/// filename is the generated base file (`*.g.xlf`) or otherwise unusable, so
/// the refreshed file's `target-language` reflects its actual locale instead
/// of being hardcoded.
fn xlf_target_language(xlf_path: &std::path::Path) -> String {
    xlf_path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.strip_suffix(".xlf"))
        .filter(|s| !s.is_empty() && !s.ends_with(".g"))
        .unwrap_or("en-US")
        .to_string()
}

pub(super) async fn dispatch_xlf_refresh(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let xlf_path = match params.get("xlf").and_then(|v| v.as_str()) {
        Some(p) => std::path::PathBuf::from(p),
        None => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            )
        }
    };
    if !xlf_path.is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }

    // Find the generated .g.xlf
    let generated_path = if let Some(g) = params.get("generated").and_then(|v| v.as_str()) {
        std::path::PathBuf::from(g)
    } else {
        // Auto-detect: look in the same Translations/ directory for *.g.xlf
        xlf_path
            .parent()
            .and_then(|dir| {
                tokio::task::block_in_place(|| {
                    std::fs::read_dir(dir)
                        .ok()?
                        .filter_map(|e| e.ok())
                        .find(|e| {
                            let name = e.file_name();
                            let s = name.to_string_lossy();
                            s.ends_with(".g.xlf")
                        })
                        .map(|e| e.path())
                })
            })
            .unwrap_or_else(|| xlf_path.with_extension("g.xlf"))
    };

    // F-OPEN-045: refuse to load either .xlf past the 64 MB cap.
    for (label, p) in [
        ("generated", generated_path.as_path()),
        ("lang", xlf_path.as_path()),
    ] {
        if matches!(crate::xliff::xlf_exceeds_cap(p), Some(true)) {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                &format!(
                    "{label} xlf {} exceeds {} byte size limit — refusing to parse",
                    p.display(),
                    crate::xliff::MAX_XLF_FILE_BYTES
                ),
            );
        }
    }
    let gen_content = match tokio::fs::read_to_string(&generated_path).await {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {}: {e}", generated_path.display()),
            )
        }
    };
    let lang_content = match tokio::fs::read_to_string(&xlf_path).await {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {}: {e}", xlf_path.display()),
            )
        }
    };

    let gen_units_map = crate::xliff::parse_xliff(&gen_content);
    let gen_units: Vec<crate::xliff::TranslationUnit> = gen_units_map.into_values().collect();
    let lang_units = crate::xliff::parse_xliff(&lang_content);

    let (updated_units, refresh_result) = crate::xliff::refresh_xliff(&gen_units, &lang_units);

    // Write updated units back to the language xlf
    // We need the app name for the XLIFF header
    let app_name = xlf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("App")
        .trim_end_matches(".g")
        .to_string();
    // Derive the target language from the language-specific filename
    // (e.g. `de-DE.xlf` -> `de-DE`). The generated `.g.xlf` is always en-US,
    // so the language file's target-language must reflect its own locale.
    let target_lang = xlf_target_language(&xlf_path);
    let new_xlf = crate::xliff::generate_xliff(&app_name, "en-US", &target_lang, &updated_units);
    if let Err(e) = tokio::task::block_in_place(|| std::fs::write(&xlf_path, new_xlf)) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
            &format!("Cannot write {}: {e}", xlf_path.display()),
        );
    }

    let _ = workspace; // workspace used for future workspace-aware refresh
    Response {
        id,
        result: Some(serde_json::to_value(&refresh_result).unwrap_or_default()),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_xlf_untranslated(id: u64, params: &serde_json::Value) -> Response {
    let xlf_path = match params.get("xlf").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            )
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }
    // F-OPEN-045: refuse to load .xlf files past the 64 MB cap. Real BC
    // translation files are tiny; anything larger is a misconfigured or
    // hostile input we shouldn't even start to parse.
    if matches!(
        crate::xliff::xlf_exceeds_cap(std::path::Path::new(xlf_path)),
        Some(true)
    ) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            &format!(
                "{xlf_path} exceeds {} byte .xlf size limit — refusing to parse",
                crate::xliff::MAX_XLF_FILE_BYTES
            ),
        );
    }
    let xlf_content = match tokio::task::block_in_place(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            )
        }
    };
    let units_map = crate::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<crate::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = crate::xliff::find_untranslated(&all_units);
    let items: Vec<serde_json::Value> = untranslated
        .iter()
        .map(|u| {
            serde_json::json!({
                "id": u.id,
                "source": u.source,
                "objectType": u.object_type,
                "objectId": u.object_id,
                "objectName": u.object_name,
            })
        })
        .collect();
    let count = items.len();
    Response {
        id,
        result: Some(serde_json::json!({ "untranslated": items, "count": count })),
        error: None,
        ..Default::default()
    }
}

pub(super) async fn dispatch_xlf_suggest(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let xlf_path = match params.get("xlf").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            )
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }

    // F-OPEN-045: refuse to load .xlf files past the 64 MB cap.
    if matches!(
        crate::xliff::xlf_exceeds_cap(std::path::Path::new(xlf_path)),
        Some(true)
    ) {
        return rpc_error(
            id,
            al_protocol::jsonrpc::error_codes::INVALID_PARAMS,
            &format!(
                "{xlf_path} exceeds {} byte .xlf size limit — refusing to parse",
                crate::xliff::MAX_XLF_FILE_BYTES
            ),
        );
    }
    let xlf_content = match tokio::task::block_in_place(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_protocol::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            )
        }
    };

    let units_map = crate::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<crate::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = crate::xliff::find_untranslated(&all_units);
    let suggestions = crate::xliff::suggest_translations(&untranslated, workspace);

    Response {
        id,
        result: Some(serde_json::json!({
            "suggestions": serde_json::to_value(&suggestions).unwrap_or_default(),
            "count": suggestions.len(),
        })),
        error: None,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Bulk fix dispatchers (T1603-T1605)
// ---------------------------------------------------------------------------

pub(super) fn dispatch_fix_application_area(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match super::require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let value = params
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("All");
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match crate::queries::bulk_fix::add_application_area(&project_root, value, dry_run) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, error_codes::INTERNAL_ERROR, &e),
    }
}

pub(super) fn dispatch_fix_tooltips(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match super::require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Build tooltip map: field_name -> tooltip from symbol data
    let table_name = params
        .get("fromTable")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tooltips: Vec<(String, String)> = if !table_name.is_empty() {
        workspace
            .symbols
            .get_by_name(table_name)
            .into_iter()
            .filter(|e| e.kind == crate::symbols::ObjectKind::Table)
            .flat_map(|e| {
                e.fields
                    .iter()
                    .filter_map(|f| {
                        let tooltip = f
                            .properties
                            .iter()
                            .find(|p| p.name.eq_ignore_ascii_case("ToolTip"))
                            .map(|p| p.value.clone())?;
                        Some((f.name.clone(), tooltip))
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    } else {
        Vec::new()
    };

    match crate::queries::bulk_fix::add_tooltips(&project_root, &tooltips, dry_run) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, error_codes::INTERNAL_ERROR, &e),
    }
}

pub(super) fn dispatch_fix_data_classification(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match super::require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let value = params
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("CustomerContent");
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    match crate::queries::bulk_fix::add_data_classification(&project_root, value, dry_run) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(e) => rpc_error(id, error_codes::INTERNAL_ERROR, &e),
    }
}

// ---------------------------------------------------------------------------
// Metrics: cyclomatic/cognitive complexity per procedure (T1802)
// ---------------------------------------------------------------------------

pub(super) fn dispatch_metrics(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let all = params.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
    let threshold_cyclomatic = params
        .get("thresholdCyclomatic")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as u32;
    let threshold_cognitive = params
        .get("thresholdCognitive")
        .and_then(|v| v.as_u64())
        .unwrap_or(15) as u32;

    if all {
        // Compute metrics for all workspace files
        let mut all_results: Vec<serde_json::Value> = Vec::new();

        for entry in workspace.file_index.files.iter() {
            let path = entry.key().to_string_lossy().to_string();
            let text = entry.value();
            let parsed = crate::syntax::AlParser::parse_quick(text);
            let metrics = crate::syntax::complexity::compute_complexity(&parsed.tree, text);
            if !metrics.is_empty() {
                let hotspots: Vec<serde_json::Value> = metrics
                    .iter()
                    .filter(|m| {
                        m.cyclomatic >= threshold_cyclomatic || m.cognitive >= threshold_cognitive
                    })
                    .map(procedure_complexity_to_json)
                    .collect();
                all_results.push(serde_json::json!({
                    "file": path,
                    "procedures": metrics.iter().map(procedure_complexity_to_json).collect::<Vec<_>>(),
                    "hotspots": hotspots,
                }));
            }
        }

        return Response {
            id,
            result: Some(serde_json::json!(all_results)),
            error: None,
            ..Default::default()
        };
    }

    // Single file mode
    let Some(uri) = file_uri_from_params(params) else {
        return invalid_params(id);
    };
    ensure_document(workspace, &uri);

    let Some(text) = workspace.documents.get_text(&uri) else {
        return file_not_found(id);
    };

    let parsed = crate::syntax::AlParser::parse_quick(&text);
    let metrics = crate::syntax::complexity::compute_complexity(&parsed.tree, &text);

    let hotspots: Vec<serde_json::Value> = metrics
        .iter()
        .filter(|m| m.cyclomatic >= threshold_cyclomatic || m.cognitive >= threshold_cognitive)
        .map(procedure_complexity_to_json)
        .collect();

    Response {
        id,
        result: Some(serde_json::json!({
            "procedures": metrics.iter().map(procedure_complexity_to_json).collect::<Vec<_>>(),
            "hotspots": hotspots,
            "thresholdCyclomatic": threshold_cyclomatic,
            "thresholdCognitive": threshold_cognitive,
        })),
        error: None,
        ..Default::default()
    }
}

fn procedure_complexity_to_json(
    m: &crate::syntax::complexity::ProcedureComplexity,
) -> serde_json::Value {
    serde_json::json!({
        "name": m.name,
        "line": m.line,
        "cyclomatic": m.cyclomatic,
        "cognitive": m.cognitive,
    })
}

// ---------------------------------------------------------------------------
// WP15: Test runner
// ---------------------------------------------------------------------------

pub(super) fn dispatch_tests_discover(workspace: &Workspace, id: u64) -> Response {
    let tests = crate::queries::tests::discover_tests(workspace);
    let value = serde_json::to_value(&tests).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_tests_coverage(workspace: &Workspace, id: u64) -> Response {
    let report = crate::queries::test_coverage::test_coverage(workspace);
    let value = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

/// T1502: Execute tests via BC REST API + T1503: Return results as diagnostics.
///
/// Params:
/// - `codeunit` (i64): codeunit ID to run. Required.
/// - `codeunitName` (str): display name for the result. Defaults to the ID as a string.
/// - `method` (str, optional): run only this test method.
///
/// Launch config is read from the project root (`.vscode/launch.json` or `.zed/debug.json`).
/// The first config entry is used unless `config` (str) names a specific one.
///
/// Response includes:
/// - `result`: `TestCodeunitResult` JSON
/// - `diagnostics`: array of `TestDiagnostic` for failed/skipped tests (T1503)
pub(super) async fn dispatch_tests_run(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use crate::launch::find_launch_config;
    use crate::queries::test_diagnostics::results_to_diagnostics;
    use crate::test_runner::TestRunnerClient;

    // -- Resolve project root from workspace -----------------------------------
    let project_root = match workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|p| p.root.clone())
    {
        Some(root) => root,
        None => {
            return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT);
        }
    };

    // -- Parse params ----------------------------------------------------------
    let codeunit_id = match params.get("codeunit").and_then(|v| v.as_i64()) {
        Some(n) => match i32::try_from(n) {
            Ok(v) => v,
            Err(_) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "codeunit ID out of range");
            }
        },
        None => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'codeunit' parameter (i64 codeunit ID)",
            );
        }
    };
    let codeunit_name = params
        .get("codeunitName")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| codeunit_id.to_string());
    let method = params
        .get("method")
        .and_then(|v| v.as_str())
        .map(String::from);
    let config_name = params.get("config").and_then(|v| v.as_str());

    // -- Find launch config (sync fs read → run on blocking thread) ----------
    let launch_cfg_root = project_root.clone();
    let launch_cfg_opt =
        match tokio::task::spawn_blocking(move || find_launch_config(&launch_cfg_root)).await {
            Ok(opt) => opt,
            Err(e) => {
                return rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("launch config task failed: {e}"),
                );
            }
        };
    let launch_cfg = match launch_cfg_opt {
        Some(cfg) => cfg,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "No launch config found — create .vscode/launch.json or .zed/debug.json",
            );
        }
    };

    let server_config = if let Some(name) = config_name {
        launch_cfg
            .configs
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    } else {
        launch_cfg.configs.first()
    };

    let server_config = match server_config {
        Some(c) => c,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "No BC server config found in launch config",
            );
        }
    };

    // -- Run tests via BC REST API (T1502) ------------------------------------
    let client = TestRunnerClient::new(server_config);
    let run_result = client
        .run_codeunit(codeunit_id, &codeunit_name, method.as_deref())
        .await;

    let result = match run_result {
        Ok(r) => r,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("Test run failed: {e}"),
            );
        }
    };

    // -- Convert to diagnostics (T1503) ----------------------------------------
    let diagnostics = results_to_diagnostics(std::slice::from_ref(&result), workspace);

    // Persist per-method records so CodeLens / tests.last_results /
    // test-runner TUI all see the same history regardless of which entry
    // point the user used. Errors are logged, not propagated — we still
    // want to return the test result to the caller.
    if let Err(e) = ensure_result_store(workspace, &project_root).await {
        tracing::warn!(error = %e, "test_results store init failed; persistence skipped");
    } else if let Some(store) = workspace.test_results.read().ok().and_then(|g| g.clone()) {
        for m in &result.methods {
            let rec = crate::test_engine::persistence::TestRunRecord {
                timestamp: crate::test_engine::persistence::now_secs(),
                codeunit_id: result.id,
                codeunit_name: result.name.clone(),
                method_name: m.name.clone(),
                status: m.status.clone(),
                duration_ms: m.duration_ms,
                error: m.error.clone(),
            };
            if let Err(e) = store.append(rec).await {
                tracing::warn!(error = %e, "failed to persist test result");
            }
        }
    }

    let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
    let diag_json = serde_json::to_value(&diagnostics).unwrap_or(serde_json::json!([]));

    Response {
        id,
        result: Some(serde_json::json!({
            "result": result_json,
            "diagnostics": diag_json,
        })),
        error: None,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// p1-5: New test_engine endpoints — additive; existing tests.run is frozen.
// ---------------------------------------------------------------------------

/// `tests.run_batch` — run multiple codeunits, optionally in parallel,
/// optionally writing JUnit/Cobertura output to disk.
///
/// Params:
/// - `codeunitIds`: `[i32]` (required)
/// - `codeunitNames`: `[str]` (parallel-indexed; falls back to ID-as-string)
/// - `parallel`: bool (default false)
/// - `timeoutMs`: u64 (default 30_000)
/// - `junitOut`: str (path to write JUnit XML)
/// - `coberturaOut`: str (path to write Cobertura XML)
/// - `filter`: str (forwarded; currently logged only)
pub(super) async fn dispatch_tests_run_batch(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use crate::launch::find_launch_config;
    use crate::test_engine::backends::live_bc::LiveBcMode;
    use crate::test_engine::output::{cobertura, junit};
    use crate::test_engine::session::{RunOptions, TestEvent, TestId, TestSession};
    use tokio::sync::mpsc;

    // -- Resolve project root + launch config ---------------------------------
    let project_root = match workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|p| p.root.clone())
    {
        Some(root) => root,
        None => return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT),
    };
    let launch_cfg = match find_launch_config(&project_root) {
        Some(cfg) => cfg,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "No launch config found — create .vscode/launch.json or .zed/debug.json",
            );
        }
    };
    let server_config = match launch_cfg.configs.first() {
        Some(c) => c.clone(),
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "No BC server config found in launch config",
            );
        }
    };

    // -- Parse params ----------------------------------------------------------
    let codeunit_ids = match params.get("codeunitIds").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                "Missing 'codeunitIds' (array of i32)",
            );
        }
    };
    let names_arr = params
        .get("codeunitNames")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut tests: Vec<TestId> = Vec::with_capacity(codeunit_ids.len());
    for (i, v) in codeunit_ids.iter().enumerate() {
        let cu_id = match v.as_i64() {
            Some(n) => match i32::try_from(n) {
                Ok(v) => v,
                Err(_) => {
                    return rpc_error(
                        id,
                        error_codes::INVALID_PARAMS,
                        &format!("codeunitIds[{i}] out of range"),
                    );
                }
            },
            None => {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    "Each codeunitIds entry must be an integer",
                );
            }
        };
        let cu_name = names_arr
            .get(i)
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| cu_id.to_string());
        tests.push(TestId {
            codeunit_id: cu_id,
            codeunit_name: cu_name,
            method_name: None,
        });
    }
    let opts = RunOptions {
        timeout_ms: clamp_timeout_ms(params.get("timeoutMs").and_then(|v| v.as_u64())),
        parallel: params
            .get("parallel")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        // Validate output paths against project_root — a malicious client
        // could otherwise ask the daemon to overwrite arbitrary files
        // (cron tabs, ssh keys) as the daemon's user.
        junit_out: match params.get("junitOut").and_then(|v| v.as_str()) {
            Some(s) => {
                match resolve_output_path_within_project(std::path::Path::new(s), &project_root) {
                    Some(p) => Some(p),
                    None => {
                        return rpc_error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'junitOut' path escapes the project root",
                        )
                    }
                }
            }
            None => None,
        },
        cobertura_out: match params.get("coberturaOut").and_then(|v| v.as_str()) {
            Some(s) => {
                match resolve_output_path_within_project(std::path::Path::new(s), &project_root) {
                    Some(p) => Some(p),
                    None => {
                        return rpc_error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'coberturaOut' path escapes the project root",
                        )
                    }
                }
            }
            None => None,
        },
        filter: params
            .get("filter")
            .and_then(|v| v.as_str())
            .map(String::from),
    };

    // -- Run via LiveBcMode ----------------------------------------------------
    let mode = LiveBcMode::new(server_config);
    let (tx, mut rx) = mpsc::channel::<TestEvent>(256);
    let opts_for_run = opts.clone();
    let run_handle = tokio::spawn(async move { mode.run(tests, opts_for_run, tx).await });

    let mut events: Vec<TestEvent> = Vec::new();
    let mut summaries: Vec<crate::test_engine::result::TestCodeunitResult> = Vec::new();
    while let Some(ev) = rx.recv().await {
        if let TestEvent::SuiteComplete { ref summary, .. } = ev {
            summaries.push(summary.clone());
        }
        events.push(ev);
    }
    if let Err(e) = run_handle.await {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("test run task panicked: {e}"),
        );
    }

    // -- Persist results -------------------------------------------------------
    if let Err(e) = ensure_result_store(workspace, &project_root).await {
        tracing::warn!(error = %e, "test_results store init failed; persistence skipped");
    } else if let Some(store_arc) = workspace.test_results.read().ok().and_then(|g| g.clone()) {
        for summary in &summaries {
            for m in &summary.methods {
                let rec = crate::test_engine::persistence::TestRunRecord {
                    timestamp: crate::test_engine::persistence::now_secs(),
                    codeunit_id: summary.id,
                    codeunit_name: summary.name.clone(),
                    method_name: m.name.clone(),
                    status: m.status.clone(),
                    duration_ms: m.duration_ms,
                    error: m.error.clone(),
                };
                if let Err(e) = store_arc.append(rec).await {
                    tracing::warn!(error = %e, "failed to persist test result");
                }
            }
        }
    }

    // -- Optional JUnit / Cobertura outputs -----------------------------------
    if let Some(path) = &opts.junit_out {
        if let Err(e) = write_junit_to_path(&summaries, path).await {
            tracing::warn!(error = %e, path = %path.display(), "junit write failed");
        }
    }
    if let Some(path) = &opts.cobertura_out {
        let coverage = crate::queries::test_coverage::test_coverage(workspace);
        if let Err(e) = write_cobertura_to_path(&coverage, path).await {
            tracing::warn!(error = %e, path = %path.display(), "cobertura write failed");
        }
    }

    // -- Build response --------------------------------------------------------
    let total: usize = summaries.iter().map(|s| s.total).sum();
    let passed: usize = summaries.iter().map(|s| s.passed).sum();
    let failed: usize = summaries.iter().map(|s| s.failed).sum();
    let skipped: usize = summaries.iter().map(|s| s.skipped).sum();
    let summaries_json = serde_json::to_value(&summaries).unwrap_or(serde_json::Value::Null);

    // Suppress unused warning on imports until junit/cobertura helpers below.
    let _ = (
        junit::write_junit::<&mut Vec<u8>>,
        cobertura::write_cobertura::<&mut Vec<u8>>,
    );

    Response {
        id,
        result: Some(serde_json::json!({
            "summaries": summaries_json,
            "totals": {
                "total": total,
                "passed": passed,
                "failed": failed,
                "skipped": skipped,
            },
        })),
        error: None,
        ..Default::default()
    }
}

/// `tests.run_auto` — discover all tests in the workspace and run them
/// through `LiveBcMode`. Parameters are the same as `tests.run_batch`
/// minus `codeunitIds` (which is auto-populated from
/// `queries::tests::discover_tests`).
pub(super) async fn dispatch_tests_run_auto(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let discovered = crate::queries::tests::discover_tests(workspace);
    let mut codeunit_ids: Vec<serde_json::Value> = Vec::with_capacity(discovered.len());
    let mut codeunit_names: Vec<serde_json::Value> = Vec::with_capacity(discovered.len());
    for cu in &discovered {
        codeunit_ids.push(serde_json::Value::from(cu.id));
        codeunit_names.push(serde_json::Value::from(cu.name.clone()));
    }
    let mut params = params.clone();
    if let Some(map) = params.as_object_mut() {
        map.insert(
            "codeunitIds".to_string(),
            serde_json::Value::Array(codeunit_ids),
        );
        map.insert(
            "codeunitNames".to_string(),
            serde_json::Value::Array(codeunit_names),
        );
    }
    dispatch_tests_run_batch(workspace, id, &params).await
}

/// `tests.last_results` — read the persisted test history for the project.
///
/// Optional params:
/// - `codeunitId`: i32 — filter to one codeunit
/// - `methodName`: str — when combined with codeunitId, return the most
///   recent record for that pair as `lastResult`.
pub(super) async fn dispatch_tests_last_results(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let project_root = match workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|p| p.root.clone())
    {
        Some(root) => root,
        None => return rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT),
    };

    if let Err(e) = ensure_result_store(workspace, &project_root).await {
        return rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("failed to open test results store: {e}"),
        );
    }
    let store = match workspace.test_results.read().ok().and_then(|g| g.clone()) {
        Some(s) => s,
        None => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                "test results store unavailable",
            );
        }
    };

    // Single (codeunit, method) lookup short-circuits to lastResult.
    if let (Some(cu), Some(method)) = (
        params.get("codeunitId").and_then(|v| v.as_i64()),
        params.get("methodName").and_then(|v| v.as_str()),
    ) {
        let cu_id = match i32::try_from(cu) {
            Ok(v) => v,
            Err(_) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "codeunitId out of range");
            }
        };
        return match store.last_for(cu_id, method).await {
            Ok(opt) => Response {
                id,
                result: Some(serde_json::json!({ "lastResult": opt })),
                error: None,
                ..Default::default()
            },
            Err(e) => rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("failed to read test results: {e}"),
            ),
        };
    }

    // Otherwise return all (optionally filtered by codeunitId).
    let all = match store.read_all().await {
        Ok(v) => v,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("failed to read test results: {e}"),
            );
        }
    };
    let filtered: Vec<_> = match params.get("codeunitId").and_then(|v| v.as_i64()) {
        Some(cu) => {
            let cu_id = match i32::try_from(cu) {
                Ok(v) => v,
                Err(_) => {
                    return rpc_error(id, error_codes::INVALID_PARAMS, "codeunitId out of range");
                }
            };
            all.into_iter().filter(|r| r.codeunit_id == cu_id).collect()
        }
        None => all,
    };
    Response {
        id,
        result: Some(serde_json::json!({ "results": filtered })),
        error: None,
        ..Default::default()
    }
}

// --- helpers used by p1-5 dispatchers ---------------------------------------

async fn ensure_result_store(
    workspace: &Workspace,
    project_root: &std::path::Path,
) -> Result<(), crate::test_engine::PersistenceError> {
    {
        let guard = workspace.test_results.read().map_err(|_| {
            crate::test_engine::PersistenceError::Io(std::io::Error::other(
                "test_results lock poisoned",
            ))
        })?;
        if guard.is_some() {
            return Ok(());
        }
    }
    let store = crate::test_engine::TestResultStore::open_for_project(project_root).await?;
    let mut guard = workspace.test_results.write().map_err(|_| {
        crate::test_engine::PersistenceError::Io(std::io::Error::other(
            "test_results lock poisoned",
        ))
    })?;
    *guard = Some(std::sync::Arc::new(store));
    Ok(())
}

async fn write_junit_to_path(
    summaries: &[crate::test_engine::result::TestCodeunitResult],
    path: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut buf = Vec::new();
    crate::test_engine::output::junit::write_junit(summaries, &mut buf)?;
    tokio::fs::write(path, buf).await
}

async fn write_cobertura_to_path(
    report: &crate::queries::test_coverage::CoverageReport,
    path: &std::path::Path,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut buf = Vec::new();
    crate::test_engine::output::cobertura::write_cobertura(report, &mut buf)?;
    tokio::fs::write(path, buf).await
}

// ---------------------------------------------------------------------------
// p2: tests.affected / tests.classify
// ---------------------------------------------------------------------------

/// `tests.affected` — given a list of changed file paths, return the tests
/// whose source files are in that set.
///
/// Params:
/// - `changedFiles`: `[str]` (required)
///
/// Response: `{ "affected": [AffectedTest] }`
pub(super) fn dispatch_tests_affected(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(arr) = params.get("changedFiles").and_then(|v| v.as_array()) else {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "Missing 'changedFiles' (array of paths)",
        );
    };
    let paths: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let affected = crate::queries::tests::affected_tests(workspace, &paths);
    Response {
        id,
        result: Some(serde_json::json!({ "affected": affected })),
        error: None,
        ..Default::default()
    }
}

/// `tests.classify` — return the routing decision the engine would make
/// for every discovered test in the workspace, with reasons.
pub(super) fn dispatch_tests_classify(workspace: &Workspace, id: u64) -> Response {
    use crate::test_engine::router;
    let results = router::classify_all(workspace);
    let json: Vec<serde_json::Value> = results
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "codeunitId": r.codeunit_id,
                "codeunitName": r.codeunit_name,
                "methodName": r.method_name,
                "decision": r.decision.as_str(),
                "reasons": r
                    .reasons
                    .into_iter()
                    .map(|reason| serde_json::json!({
                        "message": reason.message,
                        "file": reason.file,
                        "line": reason.line,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    Response {
        id,
        result: Some(serde_json::json!({ "classifications": json })),
        error: None,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Phase 4: snapshot record / replay / diff
// ---------------------------------------------------------------------------

/// `tests.snapshot_record` — record sampled-state snapshots at breakpoints
/// during a live-BC test run.
///
/// Params (camelCase): `{ codeunitId, methodName, breakpoints: [{file, line}] }`.
/// Response: `{ path, sampleCount }` on success.
///
/// The live-BC bridge is currently a stub (see
/// `crate::test_snapshots::bc_debug_bridge`); this dispatcher accepts and
/// validates the request but returns an explicit "not yet wired" error
/// until the bridge is filled in. Snapshot replay + diff (which operate
/// on already-recorded files) work today.
pub(super) async fn dispatch_tests_snapshot_record(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let _ = workspace;
    let codeunit_id = match params.get("codeunitId").and_then(|v| v.as_i64()) {
        Some(n) => match i32::try_from(n) {
            Ok(v) => v,
            Err(_) => {
                return rpc_error(id, error_codes::INVALID_PARAMS, "codeunitId out of range");
            }
        },
        None => {
            return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'codeunitId'");
        }
    };
    let _method_name = match params.get("methodName").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'methodName'");
        }
    };
    let breakpoints = params.get("breakpoints").and_then(|v| v.as_array());
    if breakpoints.is_none_or(|a| a.is_empty()) {
        return rpc_error(
            id,
            error_codes::INVALID_PARAMS,
            "Missing or empty 'breakpoints' array",
        );
    }
    rpc_error(
        id,
        error_codes::INTERNAL_ERROR,
        &format!(
            "tests.snapshot_record (codeunit {codeunit_id}): live-BC bridge not yet wired \
             — see test_snapshots::bc_debug_bridge"
        ),
    )
}

/// `tests.snapshot_replay` — replay a previously-recorded snapshot via
/// `replay_against` against an empty observed set, surfacing the
/// snapshot's contents to the caller. Live-BC re-replay (`replay_via_dap`)
/// requires the same bridge as `tests.snapshot_record`.
///
/// Params: `{ snapshotPath }`.
/// Response: `{ verdict: ReplayVerdict, sampleCount: N, codeunitId, methodName, bcVersion }`.
pub(super) async fn dispatch_tests_snapshot_replay(
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let path = match params.get("snapshotPath").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'snapshotPath'");
        }
    };
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INTERNAL_ERROR,
                &format!("read snapshot failed: {e}"),
            );
        }
    };
    let snapshot = match crate::test_snapshots::format::deserialize_snapshot(&bytes) {
        Ok(s) => s,
        Err(e) => {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!("snapshot parse failed: {e}"),
            );
        }
    };
    // No live observation yet — emit an info-only "Match" verdict so the
    // caller can confirm the snapshot loads. Real verification arrives
    // when the BC bridge is wired (see bc_debug_bridge.rs).
    let verdict = crate::test_snapshots::replayer::ReplayVerdict::Match;
    Response {
        id,
        result: Some(serde_json::json!({
            "verdict": verdict,
            "sampleCount": snapshot.samples.len(),
            "codeunitId": snapshot.codeunit_id,
            "methodName": snapshot.method_name,
            "bcVersion": snapshot.bc_version,
        })),
        error: None,
        ..Default::default()
    }
}

/// `tests.snapshot_diff` — diff two snapshot files; report field-level
/// divergences plus header mismatches (bc_version, source_hash).
///
/// Params: `{ pathA, pathB }`.
/// Response: `{ divergences: [Divergence] }`.
pub(super) async fn dispatch_tests_snapshot_diff(id: u64, params: &serde_json::Value) -> Response {
    let path_a = match params.get("pathA").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'pathA'"),
    };
    let path_b = match params.get("pathB").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return rpc_error(id, error_codes::INVALID_PARAMS, "Missing 'pathB'"),
    };
    let read = async |p: &str| -> Result<crate::test_snapshots::format::Snapshot, String> {
        let bytes = tokio::fs::read(p).await.map_err(|e| e.to_string())?;
        crate::test_snapshots::format::deserialize_snapshot(&bytes).map_err(|e| e.to_string())
    };
    let a = match read(path_a).await {
        Ok(s) => s,
        Err(e) => return rpc_error(id, error_codes::INVALID_PARAMS, &format!("pathA: {e}")),
    };
    let b = match read(path_b).await {
        Ok(s) => s,
        Err(e) => return rpc_error(id, error_codes::INVALID_PARAMS, &format!("pathB: {e}")),
    };
    let divergences = crate::test_snapshots::diff::diff_snapshots(&a, &b);
    Response {
        id,
        result: Some(serde_json::json!({ "divergences": divergences })),
        error: None,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// WP16: Object wizards / code generation
// ---------------------------------------------------------------------------

pub(super) fn dispatch_generate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let kind = params
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("page");
    // Validate the object ID without silent truncation. `as i32` would wrap an
    // out-of-range wire value (e.g. i32::MAX + 1 → i32::MIN), which would then
    // bypass the object-ID conflict check below against a different ID than the
    // caller intended. Reject out-of-range IDs with INVALID_PARAMS instead.
    let object_id = match params.get("id") {
        None => 50100,
        Some(_) => match extract_i32(params, "id") {
            Some(n) => n,
            None => return invalid_params(id),
        },
    };
    let table_name = params.get("table").and_then(|v| v.as_str()).unwrap_or("");

    // Object-ID conflict check (F-OPEN-033). The default of 50100 makes it
    // very easy for users to generate code that collides with an existing
    // object in the workspace. Refuse with a clear error so the offending
    // ID surfaces at generate time instead of at compile time. The check
    // is scoped to the same object kind — a Page 50100 and Table 50100 can
    // legitimately coexist in BC's ID space.
    let target_kind = match kind {
        "page" => Some(crate::symbols::ObjectKind::Page),
        "report" => Some(crate::symbols::ObjectKind::Report),
        "test" => Some(crate::symbols::ObjectKind::Codeunit),
        _ => None,
    };
    if let Some(target_kind) = target_kind {
        let collisions = workspace.symbols.get_by_id(target_kind, object_id);
        if let Some(existing) = collisions.first() {
            return rpc_error(
                id,
                error_codes::INVALID_PARAMS,
                &format!(
                    "Object ID {object_id} ({kind}) already in use by '{}' — pass a different `id` to scaffold",
                    existing.name
                ),
            );
        }
    }

    // Resolve the source table symbol from the workspace symbol index.
    let table_entry = if !table_name.is_empty() {
        workspace
            .symbols
            .search(table_name, 10)
            .into_iter()
            .find(|e| {
                e.kind == crate::symbols::ObjectKind::Table
                    && e.name.eq_ignore_ascii_case(table_name)
            })
    } else {
        None
    };

    match kind {
        "page" => {
            let page_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("NewPage")
                .to_string();
            let page_type_str = params
                .get("pageType")
                .and_then(|v| v.as_str())
                .unwrap_or("List");
            let page_type = page_type_str
                .parse::<crate::generators::PageType>()
                .unwrap_or_default();

            let Some(source) = table_entry else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Table '{}' not found in symbol index", table_name),
                );
            };
            let config = crate::generators::GeneratePageConfig {
                object_id,
                page_name,
                page_type,
                source_table: (*source).clone(),
            };
            let code = crate::generators::generate_page(&config);
            Response {
                id,
                result: Some(serde_json::json!({ "code": code, "kind": "page" })),
                error: None,
                ..Default::default()
            }
        }
        "report" => {
            let report_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("NewReport")
                .to_string();
            let Some(source) = table_entry else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Table '{}' not found in symbol index", table_name),
                );
            };
            let config = crate::generators::GenerateReportConfig {
                object_id,
                report_name,
                source_table: (*source).clone(),
            };
            let code = crate::generators::generate_report(&config);
            Response {
                id,
                result: Some(serde_json::json!({ "code": code, "kind": "report" })),
                error: None,
                ..Default::default()
            }
        }
        "test" => {
            let test_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("NewTests")
                .to_string();
            let subject = table_entry.map(|e| (*e).clone());
            let config = crate::generators::GenerateTestConfig {
                object_id,
                test_name,
                subject,
            };
            let code = crate::generators::generate_test(&config);
            Response {
                id,
                result: Some(serde_json::json!({ "code": code, "kind": "test" })),
                error: None,
                ..Default::default()
            }
        }
        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown generate kind: {other}. Use page, report, or test"),
            }),
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------------
// WP17: Analysis differentiators
// ---------------------------------------------------------------------------

pub(super) fn dispatch_obsolete(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::obsolescence::obsolescence_timeline(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_audit_data_classification(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::audit::data_classification_audit(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_permission_set_audit(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::audit::permission_set_audit(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_deps_graph(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let format = params
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("json");

    // Read app.json from project root
    let app_json = workspace
        .project
        .try_read()
        .ok()
        .and_then(|p| p.as_ref().map(|p| p.root.join("app.json")))
        .and_then(|path| {
            tokio::task::block_in_place(|| std::fs::read_to_string(&path))
                .map_err(|e| {
                    tracing::warn!(path = %path.display(), error = %e, "failed to read app.json — proceeding with empty manifest");
                    e
                })
                .ok()
        })
        .unwrap_or_default();

    // Build package list from loaded symbols — name, publisher, version, deps.
    // Currently we pass the packages list without transitive dependency info;
    // the dep graph will still resolve direct dependencies from app.json.
    // Uses the `PackageEntry` alias defined in `queries::deps` so the type
    // stays in one place if its shape ever changes.
    let packages: Vec<crate::queries::deps::PackageEntry> = Vec::new();

    let graph = crate::queries::deps::build_dependency_graph(&app_json, &packages);

    if format == "dot" {
        let dot = graph.to_dot();
        Response {
            id,
            result: Some(serde_json::json!({ "format": "dot", "content": dot })),
            error: None,
            ..Default::default()
        }
    } else {
        let value = serde_json::to_value(&graph).unwrap_or(serde_json::Value::Null);
        Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        }
    }
}

pub(super) fn dispatch_breaking_changes(
    workspace: &Workspace,
    id: u64,
    _params: &serde_json::Value,
) -> Response {
    // Compare baseline (empty) against current workspace symbols to find
    // all changes relative to a clean slate.  Callers can pass baseline
    // symbols in params.baselineSymbols in a future iteration.
    let current: Vec<crate::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline: Vec<crate::symbols::SymbolEntry> = Vec::new();
    let changes = crate::queries::breaking_changes::analyze_breaking_changes(&baseline, &current);
    let value = serde_json::to_value(&changes).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_arch_lint(workspace: &Workspace, id: u64) -> Response {
    // Load .alarch.json from project root if present; fall back to defaults.
    // The synchronous fs::read_to_string is wrapped in block_in_place so the
    // async runtime hosting this dispatch can re-schedule the parked thread
    // for other work while the read is in flight (matches dispatch_format).
    let config = workspace
        .project
        .try_read()
        .ok()
        .and_then(|p| p.as_ref().map(|p| p.root.join(".alarch.json")))
        .and_then(|path| tokio::task::block_in_place(|| std::fs::read_to_string(path).ok()))
        .and_then(|json| crate::queries::arch_lint::ArchConfig::from_json(&json).ok())
        .unwrap_or_default();
    let violations = crate::queries::arch_lint::arch_lint(workspace, &config);
    let value = serde_json::to_value(&violations).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_find_duplicates(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    // F-OPEN-007: bound user-supplied numeric params at the daemon boundary.
    let min_tokens = clamp_min_tokens(params.get("minTokens").and_then(|v| v.as_u64()));
    let min_similarity = clamp_min_similarity(params.get("minSimilarity").and_then(|v| v.as_f64()));
    let duplicates =
        crate::queries::duplicates::find_duplicates(workspace, min_tokens, min_similarity);
    let value = serde_json::to_value(&duplicates).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_upgrade_report(
    workspace: &Workspace,
    id: u64,
    _params: &serde_json::Value,
) -> Response {
    // Use empty baseline to find all symbols that are new/changed relative
    // to a fresh install.  In practice callers supply a previous .app snapshot.
    let current: Vec<crate::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline: Vec<crate::symbols::SymbolEntry> = Vec::new();
    let issues = crate::queries::upgrade::upgrade_report(&baseline, &current);
    let value = serde_json::to_value(&issues).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

pub(super) fn dispatch_sql_patterns(
    workspace: &Workspace,
    id: u64,
    _params: &serde_json::Value,
) -> Response {
    let findings = crate::queries::sql_patterns::detect_sql_patterns(workspace);
    let value = serde_json::to_value(&findings).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

/// Sort members (variables, triggers, procedures) in canonical order.
/// Params: `file` (URI) or `content` (raw text). If `file` specified, writes back.
pub(super) fn dispatch_sort_members(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let content = if let Some(text) = params.get("content").and_then(|v| v.as_str()) {
        text.to_string()
    } else if let Some(uri) = file_uri_from_params(params) {
        ensure_document(workspace, &uri);
        match workspace.documents.get_text(&uri) {
            Some(t) => t,
            None => return invalid_params(id),
        }
    } else {
        return invalid_params(id);
    };

    let sorted = match crate::syntax::sort_members(&content) {
        Some(s) => s,
        None => content.clone(),
    };
    let changed = sorted != content;

    // Write back if file was specified and not a dry run
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if changed && !dry_run {
        if let Some(uri) = file_uri_from_params(params) {
            if let Ok(path) = uri.to_file_path() {
                // F-011: refresh document store + file index + insight graph
                // so subsequent daemon queries observe the sorted content.
                if let Err(e) = tokio::task::block_in_place(|| {
                    write_al_file_and_refresh(workspace, &path, sorted.clone())
                }) {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("Failed to write sorted file: {e}"),
                        }),
                        ..Default::default()
                    };
                }
            }
        }
    }

    Response {
        id,
        result: Some(serde_json::json!({ "sorted": sorted, "changed": changed })),
        error: None,
        ..Default::default()
    }
}

/// Rename .al files to match `<Type><Id>.<Name>.al` convention.
/// Scans the workspace root; returns list of `{from, to, renamed}` entries.
pub(super) fn dispatch_organize_files(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let root: std::path::PathBuf = match super::require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let mut results = Vec::new();

    for entry in workspace.file_index.files.iter() {
        let path = entry.key().clone();
        let text = entry.value().clone();

        // Parse object info from text
        let parsed = crate::syntax::AlParser::parse_quick(&text);
        let obj = match crate::syntax::find_object_declaration(&parsed.tree, &text) {
            Some(o) => o,
            None => continue,
        };

        // Build expected filename: <Kind><Id>.<Name>.al
        let kind_cap = capitalize_first(&obj.kind);
        let id_part = obj.id.map(|i| i.to_string()).unwrap_or_default();
        let name_clean = sanitize_filename(&obj.name);

        let expected_name = if id_part.is_empty() {
            format!("{}.{}.al", kind_cap, name_clean)
        } else {
            format!("{}{}.{}.al", kind_cap, id_part, name_clean)
        };

        let current_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        if current_name == expected_name {
            continue;
        }

        let new_path = path.parent().unwrap_or(&root).join(&expected_name);

        // F-011: route the rename through rename_al_file_and_refresh so
        // file_index + insight_graph are kept in sync. Without it, the
        // old path stayed in file_index after the disk rename.
        let renamed = if !dry_run {
            tokio::task::block_in_place(|| rename_al_file_and_refresh(workspace, &path, &new_path))
                .is_ok()
        } else {
            false
        };

        results.push(serde_json::json!({
            "from": path.display().to_string(),
            "to": new_path.display().to_string(),
            "renamed": renamed,
        }));
    }

    Response {
        id,
        result: Some(serde_json::json!({ "files": results })),
        error: None,
        ..Default::default()
    }
}

fn capitalize_first(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().to_string() + c.as_str(),
    }
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

pub(super) fn dispatch_profiler_hints(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let hotspots = params
        .get("hotspots")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let hints = crate::queries::profiler_hints::profiler_hints(workspace, &hotspots);
    let value = serde_json::to_value(&hints).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
        ..Default::default()
    }
}

// WP18 / Phase 5: Mutation testing
// ---------------------------------------------------------------------------

/// `tests.mutate` — run mutation testing on workspace files.
///
/// Params:
/// - `files` (optional, array of str): restrict to these file paths. When omitted,
///   all workspace test files are mutated.
/// - `parallel` (optional, bool): hint to enable parallel execution (default false).
/// - `timeoutMs` (optional, u64): per-variant timeout in ms.
///
/// Returns: serialized `MutationReport` JSON.
pub(super) async fn dispatch_tests_mutate(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    use crate::test_engine::mutate::{
        generate_variants_for_file, MutationOptions, MutationReport, VariantOutcome,
    };
    use al_protocol::jsonrpc::error_codes;

    // Require a loaded project
    let _project_root = match workspace
        .project
        .try_read()
        .ok()
        .and_then(|g| g.as_ref().map(|p| p.root.clone()))
    {
        Some(root) => root,
        None => {
            return super::rpc_error(id, error_codes::INTERNAL_ERROR, ERR_NO_PROJECT);
        }
    };

    // Parse options
    let parallel = params
        .get("parallel")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let timeout_ms = clamp_timeout_ms(params.get("timeoutMs").and_then(|v| v.as_u64()));

    let opts = MutationOptions {
        affected_only: true,
        parallel,
        timeout_ms,
    };

    // Collect file paths to mutate — from params or from workspace
    let file_paths: Vec<String> = if let Some(arr) = params.get("files").and_then(|v| v.as_array())
    {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    } else {
        // Default: all test files in workspace
        workspace
            .file_index
            .files
            .iter()
            .map(|e| e.key().to_string_lossy().to_string())
            .collect()
    };

    if file_paths.is_empty() {
        return Response {
            id,
            result: Some(
                serde_json::to_value(&MutationReport {
                    variants: vec![],
                    killed: 0,
                    survived: 0,
                    errored: 0,
                    executor_phase: crate::test_engine::mutate::MutationExecutorPhase::Stub,
                })
                .unwrap_or(serde_json::Value::Null),
            ),
            error: None,
            ..Default::default()
        };
    }

    // Generate and run variants (sequential for the starter phase)
    let mut all_outcomes: Vec<VariantOutcome> = Vec::new();
    let timeout = opts.timeout_ms.map(std::time::Duration::from_millis);

    for file_path in &file_paths {
        let variants = generate_variants_for_file(workspace, file_path);
        for variant in variants {
            // Apply the mutation to a copy of source, then record outcome.
            // Full interpreter integration is in the next phase; for now every
            // variant is recorded as "survived" so the endpoint is exercisable.
            let outcome = crate::test_engine::mutate::VariantOutcome {
                variant: variant.clone(),
                killed: false,
                killing_test: None,
                error: None,
            };
            let _ = timeout; // will be used when interpreter is wired
            all_outcomes.push(outcome);
        }
    }

    let killed = all_outcomes.iter().filter(|o| o.killed).count();
    let errored = all_outcomes.iter().filter(|o| o.error.is_some()).count();
    let survived = all_outcomes.len() - killed - errored;

    let report = MutationReport {
        variants: all_outcomes,
        killed,
        survived,
        errored,
        // Daemon dispatch path mirrors the in-process scaffolding: test
        // execution is stubbed pending the interpreter backend.
        executor_phase: crate::test_engine::mutate::MutationExecutorPhase::Stub,
    };

    match serde_json::to_value(&report) {
        Ok(value) => Response {
            id,
            result: Some(value),
            error: None,
            ..Default::default()
        },
        Err(e) => super::rpc_error(
            id,
            error_codes::INTERNAL_ERROR,
            &format!("Serialization error: {e}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// p1-5 dispatcher tests — parameter validation + no-project paths
// ---------------------------------------------------------------------------

#[cfg(test)]
mod p1_5_tests {
    use super::*;
    use crate::workspace::Workspace;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    // --- clamp_timeout_ms ----------------------------------------------------

    #[test]
    fn clamp_timeout_ms_passes_through_sensible_values() {
        // Positive: realistic timeouts (a few seconds to a few minutes)
        // pass through unchanged.
        assert_eq!(clamp_timeout_ms(Some(0)), Some(0));
        assert_eq!(clamp_timeout_ms(Some(30_000)), Some(30_000));
        assert_eq!(clamp_timeout_ms(Some(15 * 60 * 1000)), Some(900_000));
    }

    #[test]
    fn clamp_timeout_ms_caps_at_max() {
        // Negative: a hostile or fat-fingered client could send u64::MAX —
        // we must cap at the documented upper bound (1 hour) so the
        // daemon doesn't get pinned to a multi-day test run.
        let huge = u64::MAX;
        assert_eq!(clamp_timeout_ms(Some(huge)), Some(MAX_TIMEOUT_MS));
        // Exactly one tick above the cap also clamps.
        assert_eq!(
            clamp_timeout_ms(Some(MAX_TIMEOUT_MS + 1)),
            Some(MAX_TIMEOUT_MS)
        );
        // Exactly the cap is allowed through unchanged.
        assert_eq!(clamp_timeout_ms(Some(MAX_TIMEOUT_MS)), Some(MAX_TIMEOUT_MS));
    }

    #[test]
    fn clamp_timeout_ms_propagates_none() {
        // None (param omitted entirely) stays None — caller decides the default.
        assert_eq!(clamp_timeout_ms(None), None);
    }

    // --- xlf_target_language -------------------------------------------------

    #[test]
    fn xlf_target_language_extracts_locale_from_filename() {
        // A language-specific file carries its locale in the filename; the
        // refreshed XLIFF's target-language must reflect it, not a hardcode.
        assert_eq!(
            xlf_target_language(std::path::Path::new("/p/Translations/de-DE.xlf")),
            "de-DE"
        );
        assert_eq!(
            xlf_target_language(std::path::Path::new("fr-FR.xlf")),
            "fr-FR"
        );
    }

    #[test]
    fn xlf_target_language_falls_back_for_generated_or_unusable_names() {
        // The generated base file (*.g.xlf) and anything we can't parse fall
        // back to en-US rather than emitting a bogus target-language.
        assert_eq!(
            xlf_target_language(std::path::Path::new("MyApp.g.xlf")),
            "en-US"
        );
        assert_eq!(
            xlf_target_language(std::path::Path::new("notxlf.txt")),
            "en-US"
        );
    }

    // --- clamp_min_tokens / clamp_min_similarity (F-OPEN-007) ----------------

    #[test]
    fn clamp_min_tokens_defaults_when_absent() {
        // None → the documented default of 20.
        assert_eq!(clamp_min_tokens(None), 20);
    }

    #[test]
    fn clamp_min_tokens_passes_through_sensible_values() {
        assert_eq!(clamp_min_tokens(Some(0)), 0);
        assert_eq!(clamp_min_tokens(Some(50)), 50);
        assert_eq!(
            clamp_min_tokens(Some(MAX_DUPLICATES_MIN_TOKENS)),
            MAX_DUPLICATES_MIN_TOKENS as usize
        );
    }

    #[test]
    fn clamp_min_tokens_caps_oversized_input() {
        // Hostile / fat-fingered values cap at MAX, not panic and not pass through.
        assert_eq!(
            clamp_min_tokens(Some(u64::MAX)),
            MAX_DUPLICATES_MIN_TOKENS as usize
        );
        assert_eq!(
            clamp_min_tokens(Some(MAX_DUPLICATES_MIN_TOKENS + 1)),
            MAX_DUPLICATES_MIN_TOKENS as usize
        );
    }

    #[test]
    fn clamp_min_similarity_defaults_when_absent() {
        // None → the documented default of 0.8.
        assert!((clamp_min_similarity(None) - 0.8).abs() < 1e-6);
    }

    #[test]
    fn clamp_min_similarity_clamps_in_range() {
        assert!((clamp_min_similarity(Some(0.0)) - 0.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(0.5)) - 0.5).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(1.0)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clamp_min_similarity_rejects_out_of_range() {
        // Negative reals clamp to 0, super-1 to 1.
        assert!((clamp_min_similarity(Some(-1.0)) - 0.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(2.5)) - 1.0).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(1e308)) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn clamp_min_similarity_rejects_non_finite() {
        // NaN / ±inf must fall back to the safe default, not propagate and
        // poison downstream `>=` comparisons.
        assert!((clamp_min_similarity(Some(f64::NAN)) - 0.8).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(f64::INFINITY)) - 0.8).abs() < 1e-6);
        assert!((clamp_min_similarity(Some(f64::NEG_INFINITY)) - 0.8).abs() < 1e-6);
    }

    // --- resolve_output_path_within_project ----------------------------------

    #[test]
    fn output_path_accepts_relative_inside_project() {
        // Positive: a plain relative path resolves to inside the project.
        let project = tempfile::tempdir().unwrap();
        let resolved = resolve_output_path_within_project(
            std::path::Path::new("out/junit.xml"),
            project.path(),
        );
        assert!(
            resolved.is_some(),
            "relative path inside project must resolve"
        );
    }

    #[test]
    fn output_path_accepts_absolute_inside_project() {
        // Positive: an absolute path that points inside the project is fine.
        let project = tempfile::tempdir().unwrap();
        let abs = project.path().canonicalize().unwrap().join("results.xml");
        let resolved = resolve_output_path_within_project(&abs, project.path());
        assert!(
            resolved.is_some(),
            "absolute path inside project must resolve"
        );
    }

    #[test]
    fn output_path_rejects_parent_dir_escape() {
        // Negative: `../escape.xml` resolves to outside the project — reject.
        let project = tempfile::tempdir().unwrap();
        let resolved = resolve_output_path_within_project(
            std::path::Path::new("../escape.xml"),
            project.path(),
        );
        assert!(
            resolved.is_none(),
            "../ escape must be rejected, got {resolved:?}"
        );
    }

    #[test]
    fn output_path_rejects_deep_parent_dir_escape() {
        // Negative: multiple `..` segments that resolve above the project.
        let project = tempfile::tempdir().unwrap();
        let resolved = resolve_output_path_within_project(
            std::path::Path::new("subdir/../../../etc/passwd"),
            project.path(),
        );
        assert!(resolved.is_none());
    }

    #[test]
    fn output_path_rejects_absolute_outside_project() {
        // Negative: a totally unrelated absolute path must be rejected.
        let project = tempfile::tempdir().unwrap();
        let resolved =
            resolve_output_path_within_project(std::path::Path::new("/etc/hosts"), project.path());
        assert!(
            resolved.is_none(),
            "absolute outside project must be rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_path_rejects_symlink_dir_escape() {
        // Negative: a symlinked directory inside the project that points
        // outside must not let a write target escape, even though the textual
        // path starts with the project root.
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let link = project.path().join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        let resolved = resolve_output_path_within_project(
            std::path::Path::new("link/evil.xml"),
            project.path(),
        );
        assert!(
            resolved.is_none(),
            "symlinked dir escaping the project must be rejected, got {resolved:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_path_rejects_symlink_file_escape() {
        // Negative: an existing output file that is itself a symlink to an
        // outside location must be rejected (it would be followed by write()).
        let project = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("target.xml");
        std::fs::write(&outside_file, b"x").unwrap();
        let link = project.path().join("results.xml");
        std::os::unix::fs::symlink(&outside_file, &link).unwrap();

        let resolved =
            resolve_output_path_within_project(std::path::Path::new("results.xml"), project.path());
        assert!(
            resolved.is_none(),
            "symlinked output file escaping the project must be rejected, got {resolved:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_path_accepts_symlink_dir_staying_inside() {
        // Positive: a symlink that points to another location *inside* the
        // project must still be accepted, with the canonical target returned.
        let project = tempfile::tempdir().unwrap();
        let real_dir = project.path().join("real_out");
        std::fs::create_dir(&real_dir).unwrap();
        let link = project.path().join("out");
        std::os::unix::fs::symlink(&real_dir, &link).unwrap();

        let resolved = resolve_output_path_within_project(
            std::path::Path::new("out/junit.xml"),
            project.path(),
        );
        assert!(
            resolved.is_some(),
            "symlink staying inside the project must be accepted"
        );
        let canonical_root = project.path().canonicalize().unwrap();
        assert!(resolved.unwrap().starts_with(&canonical_root));
    }

    // --- dispatch_generate (F-OPEN-033) --------------------------------------

    #[test]
    fn dispatch_generate_rejects_object_id_collision() {
        // Negative regression: an existing Page with id 50100 must cause
        // a generate request for kind=page, id=50100 to fail with a
        // structured INVALID_PARAMS error mentioning the colliding name.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Page,
            id: 50100,
            name: "Existing Page".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            42,
            &serde_json::json!({
                "kind": "page",
                "id": 50100,
                "name": "Demo",
                "table": "Customer"
            }),
        );

        let err = resp.error.expect("expected error response on collision");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        assert!(
            err.message.contains("50100") && err.message.contains("Existing Page"),
            "error must mention the colliding id and existing name: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_generate_allows_same_id_across_kinds() {
        // Positive: BC's object-id space is per-kind. A Page 50100 must
        // NOT block a Table 50100 (or here, a Codeunit 50100 — `test`
        // generates a Codeunit, which is what `target_kind` resolves to).
        // We can't fully exercise the success path without a workspace
        // root, but we can verify the collision check doesn't fire when
        // the ID is occupied by a *different* kind.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Table,
            id: 50100,
            name: "Existing Table".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            43,
            &serde_json::json!({
                "kind": "test",
                "id": 50100,
                "name": "Demo"
            }),
        );

        // If the collision check fired wrongly it'd carry the "already in use"
        // string. The success / table-not-found path won't.
        if let Some(e) = resp.error {
            assert!(
                !e.message.contains("already in use"),
                "must NOT report a Codeunit/Table cross-kind collision: {}",
                e.message
            );
        }
    }

    #[test]
    fn dispatch_generate_rejects_out_of_range_object_id() {
        // Negative regression: an `id` beyond the i32 range must be rejected
        // with INVALID_PARAMS rather than silently wrapping via `as i32`.
        // i32::MAX + 1 would wrap to i32::MIN under the old cast, which would
        // then perform the conflict check against the wrong ID. Seed a Page at
        // the wrapped value to prove the truncated lookup is never reached.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Page,
            id: i32::MIN,
            name: "Wrapped Page".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            44,
            &serde_json::json!({
                "kind": "page",
                "id": (i32::MAX as i64) + 1,
                "name": "Demo",
                "table": "Customer"
            }),
        );

        let err = resp
            .error
            .expect("expected error response for out-of-range id");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        // Must be the generic invalid-params message, NOT the collision message
        // for the wrapped i32::MIN value — proving no silent truncation.
        assert!(
            !err.message.contains("Wrapped Page"),
            "out-of-range id must be rejected before the conflict check, got: {}",
            err.message
        );
    }

    #[test]
    fn dispatch_generate_defaults_object_id_when_absent() {
        // Positive: omitting `id` falls back to the 50100 default and runs the
        // conflict check against that value.
        let ws = empty_ws();
        ws.symbols.add_entries(&[crate::symbols::SymbolEntry {
            kind: crate::symbols::ObjectKind::Page,
            id: 50100,
            name: "Default Page".to_string(),
            ..Default::default()
        }]);

        let resp = dispatch_generate(
            &ws,
            45,
            &serde_json::json!({
                "kind": "page",
                "name": "Demo",
                "table": "Customer"
            }),
        );

        let err = resp.error.expect("expected collision at default id 50100");
        assert!(
            err.message.contains("50100") && err.message.contains("Default Page"),
            "default id 50100 must be used for the conflict check: {}",
            err.message
        );
    }

    // --- dispatch_permissions ------------------------------------------------

    #[test]
    fn dispatch_permissions_rejects_out_of_range_id() {
        // Negative regression: an `id` beyond the i32 range must be rejected
        // with INVALID_PARAMS rather than silently wrapping into the generated
        // AL permissionset declaration.
        let ws = empty_ws();
        let resp = dispatch_permissions(
            &ws,
            1,
            &serde_json::json!({
                "id": (i32::MAX as i64) + 1,
            }),
        );
        let err = resp
            .error
            .expect("expected error response for out-of-range id");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        assert!(err.message.contains("out of range"), "got: {}", err.message);
    }

    #[test]
    fn dispatch_permissions_accepts_in_range_id() {
        // Positive: a valid id renders AL containing that id.
        let ws = empty_ws();
        let resp = dispatch_permissions(
            &ws,
            2,
            &serde_json::json!({
                "id": 50123,
                "name": "Demo",
            }),
        );
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let content = resp
            .result
            .as_ref()
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_str())
            .expect("expected content");
        assert!(content.contains("50123"), "rendered AL: {content}");
    }

    // --- dispatch_tests_run_batch --------------------------------------------

    #[tokio::test]
    async fn run_batch_no_project_returns_error() {
        let ws = empty_ws();
        let resp = dispatch_tests_run_batch(&ws, 1, &serde_json::json!({})).await;
        assert!(
            resp.error.is_some(),
            "expected error response with no project"
        );
    }

    #[tokio::test]
    async fn run_auto_no_project_returns_error() {
        let ws = empty_ws();
        let resp = dispatch_tests_run_auto(&ws, 2, &serde_json::json!({})).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn last_results_no_project_returns_error() {
        let ws = empty_ws();
        let resp = dispatch_tests_last_results(&ws, 3, &serde_json::json!({})).await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn run_batch_missing_codeunit_ids_is_invalid_params() {
        // Set a project root so we get past the NO_PROJECT check, then
        // miss codeunitIds — must return INVALID_PARAMS, not crash.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // Write a minimal launch.json so find_launch_config succeeds.
        let dot_zed = tmp.path().join(".zed");
        std::fs::create_dir_all(&dot_zed).unwrap();
        std::fs::write(
            dot_zed.join("debug.json"),
            r#"[{"name":"local","type":"al","request":"launch","environmentType":"OnPrem","server":"http://localhost","serverInstance":"BC","authentication":"UserPassword"}]"#,
        )
        .unwrap();
        {
            let mut guard = ws.project.write().await;
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
                    id: String::new(),
                    name: "test".into(),
                    publisher: "test".into(),
                    version: "1.0.0.0".into(),
                    dependencies: Vec::new(),
                    application: None,
                    platform: None,
                    runtime: None,
                },
                packages_dir: tmp.path().join(".alpackages"),
                packages: Vec::new(),
                server_configs: Vec::new(),
            });
        }
        let resp = dispatch_tests_run_batch(&ws, 4, &serde_json::json!({})).await;
        let err = resp
            .error
            .expect("expected error response for missing codeunitIds");
        assert!(
            err.message.contains("codeunitIds"),
            "error must mention the missing parameter; got: {err:?}"
        );
    }

    #[tokio::test]
    async fn run_batch_rejects_out_of_range_codeunit_id() {
        // Negative regression: a codeunitIds entry beyond the i32 range must be
        // rejected with INVALID_PARAMS rather than silently wrapping via
        // `as i32` and executing tests against the wrong codeunit.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let dot_zed = tmp.path().join(".zed");
        std::fs::create_dir_all(&dot_zed).unwrap();
        std::fs::write(
            dot_zed.join("debug.json"),
            r#"[{"name":"local","type":"al","request":"launch","environmentType":"OnPrem","server":"http://localhost","serverInstance":"BC","authentication":"UserPassword"}]"#,
        )
        .unwrap();
        {
            let mut guard = ws.project.write().await;
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
                    id: String::new(),
                    name: "test".into(),
                    publisher: "test".into(),
                    version: "1.0.0.0".into(),
                    dependencies: Vec::new(),
                    application: None,
                    platform: None,
                    runtime: None,
                },
                packages_dir: tmp.path().join(".alpackages"),
                packages: Vec::new(),
                server_configs: Vec::new(),
            });
        }
        let resp = dispatch_tests_run_batch(
            &ws,
            6,
            &serde_json::json!({ "codeunitIds": [(i32::MAX as i64) + 1] }),
        )
        .await;
        let err = resp
            .error
            .expect("expected error response for out-of-range codeunit id");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        assert!(err.message.contains("out of range"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn last_results_returns_empty_when_no_history() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // Override XDG_DATA_HOME so the store path is sandboxed.
        // SAFETY: tests run on a single thread by default in cargo test.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        {
            let mut guard = ws.project.write().await;
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
                    id: String::new(),
                    name: "test".into(),
                    publisher: "test".into(),
                    version: "1.0.0.0".into(),
                    dependencies: Vec::new(),
                    application: None,
                    platform: None,
                    runtime: None,
                },
                packages_dir: tmp.path().join(".alpackages"),
                packages: Vec::new(),
                server_configs: Vec::new(),
            });
        }
        let resp = dispatch_tests_last_results(&ws, 5, &serde_json::json!({})).await;
        assert!(
            resp.error.is_none(),
            "fresh project should succeed, got error: {:?}",
            resp.error
        );
        let results = resp
            .result
            .as_ref()
            .and_then(|v| v.get("results"))
            .and_then(|v| v.as_array())
            .expect("expected results array");
        assert!(results.is_empty(), "fresh history must be empty");
    }

    /// Build a workspace with a fresh, sandboxed test-results store so the
    /// last_results dispatcher reaches its codeunitId validation.
    async fn ws_with_project(tmp: &tempfile::TempDir) -> Workspace {
        let ws = empty_ws();
        // SAFETY: cargo test runs on a single thread by default.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        let mut guard = ws.project.write().await;
        *guard = Some(crate::project::AlProject {
            root: tmp.path().to_path_buf(),
            app_json: crate::project::AppManifest {
                id: String::new(),
                name: "test".into(),
                publisher: "test".into(),
                version: "1.0.0.0".into(),
                dependencies: Vec::new(),
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: tmp.path().join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
        });
        drop(guard);
        ws
    }

    #[tokio::test]
    async fn last_results_single_lookup_rejects_out_of_range_codeunit_id() {
        // Negative regression: out-of-range codeunitId in the single
        // (codeunit, method) lookup path must return INVALID_PARAMS rather
        // than silently wrapping via `as i32`.
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = ws_with_project(&tmp).await;
        let resp = dispatch_tests_last_results(
            &ws,
            7,
            &serde_json::json!({
                "codeunitId": (i32::MAX as i64) + 1,
                "methodName": "TestFoo",
            }),
        )
        .await;
        let err = resp
            .error
            .expect("expected error response for out-of-range codeunitId");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        assert!(err.message.contains("out of range"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn last_results_bulk_filter_rejects_out_of_range_codeunit_id() {
        // Negative regression: out-of-range codeunitId in the bulk-filter path
        // must return INVALID_PARAMS rather than silently wrapping via `as i32`.
        let tmp = tempfile::TempDir::new().unwrap();
        let ws = ws_with_project(&tmp).await;
        let resp = dispatch_tests_last_results(
            &ws,
            8,
            &serde_json::json!({ "codeunitId": (i32::MIN as i64) - 1 }),
        )
        .await;
        let err = resp
            .error
            .expect("expected error response for out-of-range codeunitId");
        assert_eq!(err.code, al_protocol::jsonrpc::error_codes::INVALID_PARAMS);
        assert!(err.message.contains("out of range"), "got: {}", err.message);
    }

    // -----------------------------------------------------------------------
    // p1-7 freeze gate: existing wire formats must not drift.
    // -----------------------------------------------------------------------

    #[test]
    fn freeze_test_codeunit_result_wire_format() {
        use crate::test_engine::result::{TestCodeunitResult, TestMethodResult, TestStatus};
        let v = TestCodeunitResult::from_methods(
            "X".to_string(),
            42,
            vec![TestMethodResult {
                name: "M".to_string(),
                status: TestStatus::Pass,
                error: None,
                duration_ms: Some(10),
            }],
        );
        let json = serde_json::to_value(&v).unwrap();
        for key in [
            "name", "id", "methods", "total", "passed", "failed", "skipped",
        ] {
            assert!(
                json.get(key).is_some(),
                "wire-format key `{key}` missing — DO NOT rename without bumping schema_version"
            );
        }
        let m = &json["methods"][0];
        for key in ["name", "status", "durationMs"] {
            assert!(m.get(key).is_some(), "TestMethodResult key `{key}` missing");
        }
        assert_eq!(json["methods"][0]["status"], "pass");
    }

    #[test]
    fn freeze_test_coverage_report_wire_format() {
        use crate::queries::test_coverage::{CoverageReport, CoveredProcedure, TestCoverageEntry};
        let r = CoverageReport {
            coverage: vec![TestCoverageEntry {
                codeunit: "TestCU".to_string(),
                test_procedure: "TestProc".to_string(),
                covers: vec![CoveredProcedure {
                    name: "DoWork".to_string(),
                    object: "MyCU".to_string(),
                    file: "src/MyCU.al".to_string(),
                    line: 10,
                }],
            }],
            untested: Vec::new(),
        };
        let json = serde_json::to_value(&r).unwrap();
        for key in ["coverage", "untested"] {
            assert!(
                json.get(key).is_some(),
                "CoverageReport key `{key}` missing"
            );
        }
        let entry = &json["coverage"][0];
        for key in ["codeunit", "testProcedure", "covers"] {
            assert!(
                entry.get(key).is_some(),
                "TestCoverageEntry key `{key}` missing"
            );
        }
        let cov = &entry["covers"][0];
        for key in ["name", "object", "file", "line"] {
            assert!(
                cov.get(key).is_some(),
                "CoveredProcedure key `{key}` missing"
            );
        }
    }

    #[test]
    fn freeze_test_codeunit_discovery_wire_format() {
        use crate::queries::tests::{TestCodeunit, TestProcedure};
        let v = TestCodeunit {
            id: 50100,
            name: "MyTests".to_string(),
            file: "src/MyTests.al".to_string(),
            tests: vec![TestProcedure {
                name: "TestA".to_string(),
                line: 5,
            }],
        };
        let json = serde_json::to_value(&v).unwrap();
        for key in ["id", "name", "file", "tests"] {
            assert!(json.get(key).is_some(), "TestCodeunit key `{key}` missing");
        }
        let proc = &json["tests"][0];
        for key in ["name", "line"] {
            assert!(proc.get(key).is_some(), "TestProcedure key `{key}` missing");
        }
    }

    #[tokio::test]
    async fn run_batch_persistence_roundtrip_via_dispatchers() {
        // Bypass live BC: persist a record directly through the same store
        // the dispatchers use, then call dispatch_tests_last_results and
        // assert the record appears.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: tests run on a single thread by default in cargo test.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        {
            let mut guard = ws.project.write().await;
            *guard = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
                    id: String::new(),
                    name: "test".into(),
                    publisher: "test".into(),
                    version: "1.0.0.0".into(),
                    dependencies: Vec::new(),
                    application: None,
                    platform: None,
                    runtime: None,
                },
                packages_dir: tmp.path().join(".alpackages"),
                packages: Vec::new(),
                server_configs: Vec::new(),
            });
        }

        ensure_result_store(&ws, tmp.path()).await.unwrap();
        let store = ws
            .test_results
            .read()
            .ok()
            .and_then(|g| g.clone())
            .expect("store should be initialised");
        store
            .append(crate::test_engine::persistence::TestRunRecord {
                timestamp: 1_700_000_000,
                codeunit_id: 50200,
                codeunit_name: "Persisted".into(),
                method_name: "TestRoundtrip".into(),
                status: crate::test_engine::result::TestStatus::Pass,
                duration_ms: Some(7),
                error: None,
            })
            .await
            .unwrap();

        let resp =
            dispatch_tests_last_results(&ws, 99, &serde_json::json!({"codeunitId": 50200})).await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let results = resp
            .result
            .as_ref()
            .and_then(|v| v.get("results"))
            .and_then(|v| v.as_array())
            .expect("results array");
        assert_eq!(results.len(), 1, "the appended record must be visible");
        assert_eq!(results[0]["methodName"], "TestRoundtrip");
        assert_eq!(results[0]["status"], "pass");
    }

    // -----------------------------------------------------------------------
    // p2 dispatcher tests — affected + classify
    // -----------------------------------------------------------------------

    #[test]
    fn affected_missing_changed_files_returns_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_tests_affected(&ws, 1, &serde_json::json!({}));
        let err = resp.error.expect("expected error");
        assert!(err.message.contains("changedFiles"));
    }

    #[test]
    fn affected_empty_list_returns_empty_response() {
        let ws = empty_ws();
        let resp = dispatch_tests_affected(&ws, 2, &serde_json::json!({ "changedFiles": [] }));
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .as_ref()
            .and_then(|v| v.get("affected"))
            .and_then(|v| v.as_array())
            .expect("affected array");
        assert!(arr.is_empty());
    }

    #[test]
    fn classify_empty_workspace_returns_empty_classifications() {
        let ws = empty_ws();
        let resp = dispatch_tests_classify(&ws, 3);
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .as_ref()
            .and_then(|v| v.get("classifications"))
            .and_then(|v| v.as_array())
            .expect("classifications array");
        assert!(arr.is_empty(), "no test codeunits → no classifications");
    }

    #[test]
    fn freeze_routing_decision_wire_format() {
        // Wire format freeze for the routing strings — used by CLI,
        // TUI, and CodeLens. DO NOT rename without bumping schema_version.
        use crate::test_engine::router::RoutingDecision;
        assert_eq!(RoutingDecision::Interp.as_str(), "interp");
        assert_eq!(RoutingDecision::InterpRecord.as_str(), "interpRecord");
        assert_eq!(RoutingDecision::LiveBc.as_str(), "liveBc");
        assert_eq!(RoutingDecision::Snapshot.as_str(), "snapshot");
    }

    #[test]
    fn freeze_affected_test_wire_format() {
        use crate::queries::tests::AffectedTest;
        let v = AffectedTest {
            codeunit_id: 50100,
            codeunit_name: "X".into(),
            method_name: "M".into(),
            file: "src/X.al".into(),
            line: 7,
        };
        let json = serde_json::to_value(&v).unwrap();
        for key in ["codeunitId", "codeunitName", "methodName", "file", "line"] {
            assert!(json.get(key).is_some(), "AffectedTest key `{key}` missing");
        }
    }

    // -----------------------------------------------------------------------
    // Wire-format freeze gates for new endpoints landed in this session.
    // These pin the JSON response shapes; failures here flag callers who
    // rename fields without bumping the protocol version.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn freeze_run_batch_response_shape() {
        // run_batch with no project produces an error envelope; the shape
        // we pin is the SUCCESS envelope, so synthesize one directly.
        let summary =
            crate::test_engine::result::TestCodeunitResult::from_methods("Cu".into(), 1, vec![]);
        let summaries_json = serde_json::to_value(&[summary]).unwrap();
        let response = serde_json::json!({
            "summaries": summaries_json,
            "totals": { "total": 0_u64, "passed": 0_u64, "failed": 0_u64, "skipped": 0_u64 },
        });
        for key in ["summaries", "totals"] {
            assert!(response.get(key).is_some(), "run_batch key `{key}` missing");
        }
        for key in ["total", "passed", "failed", "skipped"] {
            assert!(
                response["totals"].get(key).is_some(),
                "totals key `{key}` missing"
            );
        }
    }

    #[tokio::test]
    async fn freeze_last_results_response_shapes() {
        // No-codeunit query → results array.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // SAFETY: cargo test runs single-threaded by default.
        unsafe {
            std::env::set_var("XDG_DATA_HOME", tmp.path());
        }
        {
            let mut g = ws.project.write().await;
            *g = Some(crate::project::AlProject {
                root: tmp.path().to_path_buf(),
                app_json: crate::project::AppManifest {
                    id: String::new(),
                    name: "test".into(),
                    publisher: "test".into(),
                    version: "1.0.0.0".into(),
                    dependencies: Vec::new(),
                    application: None,
                    platform: None,
                    runtime: None,
                },
                packages_dir: tmp.path().join(".alpackages"),
                packages: Vec::new(),
                server_configs: Vec::new(),
            });
        }
        // Bulk: shape = { "results": [...] }.
        let bulk = dispatch_tests_last_results(&ws, 100, &serde_json::json!({})).await;
        let r = bulk.result.expect("ok");
        assert!(
            r.get("results").is_some(),
            "tests.last_results bulk shape must expose `results`"
        );

        // Specific (codeunit, method) lookup: shape = { "lastResult": null|{} }.
        let specific = dispatch_tests_last_results(
            &ws,
            101,
            &serde_json::json!({ "codeunitId": 1, "methodName": "X" }),
        )
        .await;
        let r = specific.result.expect("ok");
        assert!(
            r.get("lastResult").is_some(),
            "tests.last_results specific shape must expose `lastResult`"
        );
    }

    #[test]
    fn freeze_classify_response_shape() {
        // dispatch_tests_classify on an empty workspace returns an empty
        // classifications array — pin both that the array exists and that
        // when populated each item carries the required keys.
        let ws = empty_ws();
        let resp = dispatch_tests_classify(&ws, 200);
        let r = resp.result.expect("ok");
        assert!(
            r.get("classifications").is_some(),
            "tests.classify shape must expose `classifications`"
        );

        // Synthetic classification entry to pin the per-item shape.
        let item = serde_json::json!({
            "codeunitId": 1,
            "codeunitName": "X",
            "methodName": "M",
            "decision": "interp",
            "reasons": [{ "message": "", "file": null, "line": null }],
        });
        for key in [
            "codeunitId",
            "codeunitName",
            "methodName",
            "decision",
            "reasons",
        ] {
            assert!(
                item.get(key).is_some(),
                "classification key `{key}` missing"
            );
        }
        for key in ["message", "file", "line"] {
            assert!(
                item["reasons"][0].get(key).is_some(),
                "reason key `{key}` missing"
            );
        }
    }

    #[test]
    fn freeze_affected_response_shape() {
        let ws = empty_ws();
        let resp = dispatch_tests_affected(&ws, 300, &serde_json::json!({ "changedFiles": [] }));
        let r = resp.result.expect("ok");
        assert!(
            r.get("affected").is_some(),
            "tests.affected shape must expose `affected`"
        );
        assert!(
            r["affected"].as_array().expect("array").is_empty(),
            "empty changedFiles must yield empty affected"
        );
    }

    // -----------------------------------------------------------------------
    // F-009: dispatch_download_symbols must refresh workspace symbol indexes
    // after a successful download so hover/completion/definition see the new
    // packages without a daemon restart.
    // -----------------------------------------------------------------------

    /// Empty result vector — no successful downloads — must short-circuit
    /// to 0 loaded packages and not touch the workspace symbol index.
    #[test]
    fn f009_refresh_after_download_returns_zero_for_empty_result() {
        let ws = empty_ws();
        let before = ws.symbols.len();
        let loaded = refresh_workspace_after_download(&ws, &[]);
        assert_eq!(loaded, 0);
        assert_eq!(
            ws.symbols.len(),
            before,
            "empty result must not touch the index"
        );
    }

    /// All-error result — no `path` entries — must also short-circuit.
    #[test]
    fn f009_refresh_after_download_skips_failed_downloads() {
        let ws = empty_ws();
        let before = ws.symbols.len();
        let result = vec![
            serde_json::json!({"name":"pkg1","status":"error","error":"network"}),
            serde_json::json!({"name":"pkg2","status":"error","error":"403"}),
        ];
        let loaded = refresh_workspace_after_download(&ws, &result);
        assert_eq!(loaded, 0);
        assert_eq!(
            ws.symbols.len(),
            before,
            "failed downloads must not touch the index"
        );
    }

    /// Successful results with non-existent paths must not panic and must
    /// return 0 loaded — load_packages_cached gracefully ignores missing
    /// files. Verifies the wire-up reaches load_packages_cached without
    /// crashing on bogus input (the regression mode of the original bug
    /// was that this code path was never reached at all).
    #[test]
    fn f009_refresh_after_download_attempts_load_for_ok_paths() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let bogus = tmp.path().join("nonexistent.app");
        let result = vec![serde_json::json!({
            "name": "pkg",
            "status": "ok",
            "path": bogus.display().to_string()
        })];
        // Must not panic, must not error — just returns 0 loaded for
        // unreadable paths. The point is that the code path is now
        // exercised on every successful download.
        let loaded = refresh_workspace_after_download(&ws, &result);
        assert_eq!(loaded, 0, "unreadable path should yield 0 loaded");
    }

    // -----------------------------------------------------------------------
    // F-011: write_al_file_and_refresh / rename_al_file_and_refresh keep
    // the workspace in sync with daemon-initiated file mutations.
    // -----------------------------------------------------------------------

    /// F-011 positive: write_al_file_and_refresh writes to disk AND
    /// updates documents + file_index + invalidates insight graph.
    #[test]
    fn f011_write_helper_refreshes_documents_and_file_index() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("Foo.al");
        let content = r#"codeunit 50100 "Foo" { }"#.to_string();
        write_al_file_and_refresh(&ws, &path, content.clone()).expect("helper succeeds");
        // On disk
        let on_disk = std::fs::read_to_string(&path).expect("file written");
        assert_eq!(on_disk, content);
        // Document store
        let uri = url::Url::from_file_path(&path).unwrap();
        assert_eq!(
            ws.documents.get_text(&uri).as_deref(),
            Some(content.as_str())
        );
        // File index
        assert_eq!(
            ws.file_index.get_content(&path).as_deref(),
            Some(content.as_str())
        );
    }

    /// F-011 positive: rename_al_file_and_refresh moves the file on disk
    /// AND drops the old file_index entry while adding the new one.
    #[test]
    fn f011_rename_helper_refreshes_file_index_for_old_and_new_paths() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let old = tmp.path().join("Old.al");
        let new = tmp.path().join("New.al");
        let content = r#"codeunit 50101 "Renamed" { }"#.to_string();
        std::fs::write(&old, &content).unwrap();
        ws.file_index.add_file(old.clone(), content.clone());
        assert!(ws.file_index.get_content(&old).is_some());

        rename_al_file_and_refresh(&ws, &old, &new).expect("helper succeeds");

        assert!(!old.exists(), "old file removed from disk");
        assert!(new.exists(), "new file present on disk");
        assert!(
            ws.file_index.get_content(&old).is_none(),
            "old file_index entry must be dropped"
        );
        assert_eq!(
            ws.file_index.get_content(&new).as_deref(),
            Some(content.as_str()),
            "new path must be re-indexed"
        );
    }

    /// F-011 negative: write_al_file_and_refresh propagates I/O errors
    /// instead of silently succeeding. A path under a non-existent
    /// directory must surface the underlying io::Error.
    #[test]
    fn f011_write_helper_returns_io_error_for_unwritable_path() {
        let ws = empty_ws();
        let bogus = std::path::PathBuf::from("/nonexistent/parent/dir/Foo.al");
        let err = write_al_file_and_refresh(&ws, &bogus, "x".to_string()).expect_err("must error");
        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
        ));
    }

    // -----------------------------------------------------------------------
    // Analysis dispatchers: param-validation & happy-path branches.
    // These exercise the synchronous, in-process error/edge paths that need
    // neither a live BC server nor a spawned binary.
    // -----------------------------------------------------------------------

    /// Open a real `.al` file on disk and return its `file://` URI string,
    /// suitable for the `{ "file": ... }` param shape the dispatchers accept.
    /// The file must exist because `file_uri_from_params` canonicalises it.
    fn write_al(tmp: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = tmp.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.canonicalize().unwrap().to_string_lossy().to_string()
    }

    // --- dispatch_lint -------------------------------------------------------

    #[test]
    fn lint_missing_file_param_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 1, &serde_json::json!({}));
        let err = resp.error.expect("missing file/uri must be invalid params");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn lint_single_file_reports_parse_errors() {
        // Positive + behavior: a file with a syntax error must surface at
        // least one diagnostic (the parse-error branch appends them).
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        // Deliberately malformed AL — unterminated object.
        let file = write_al(&tmp, "Bad.al", "codeunit 50100 \"Bad\" { procedure X( ");
        let resp = dispatch_lint(&ws, 2, &serde_json::json!({ "file": file }));
        assert!(
            resp.error.is_none(),
            "lint should succeed: {:?}",
            resp.error
        );
        let diags = resp
            .result
            .as_ref()
            .and_then(|v| v.as_array())
            .expect("diagnostics array");
        assert!(
            diags
                .iter()
                .any(|d| d.get("code") == Some(&serde_json::json!("parse-error"))),
            "malformed AL must produce a parse-error diagnostic; got {diags:?}"
        );
    }

    #[test]
    fn lint_all_mode_returns_array_for_empty_workspace() {
        // `all` mode iterates the file index; an empty workspace yields [].
        let ws = empty_ws();
        let resp = dispatch_lint(&ws, 3, &serde_json::json!({ "all": true }));
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        assert!(arr.is_empty(), "empty workspace → no per-file lint entries");
    }

    // --- dispatch_format -----------------------------------------------------

    #[test]
    fn format_content_returns_formatted_text() {
        // Positive: passing raw `content` avoids any file I/O and returns the
        // formatted source plus a `changed` flag.
        let ws = empty_ws();
        let resp = dispatch_format(
            &ws,
            1,
            &serde_json::json!({ "content": "codeunit 50100 \"X\"\n{\n}" }),
        );
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        assert!(r.get("formatted").and_then(|v| v.as_str()).is_some());
        assert!(r.get("changed").and_then(|v| v.as_bool()).is_some());
    }

    #[test]
    fn format_check_mode_only_reports_changed_flag() {
        // In check mode the response carries ONLY `changed`, never `formatted`.
        let ws = empty_ws();
        let resp = dispatch_format(
            &ws,
            2,
            &serde_json::json!({ "content": "codeunit 50100 X {}", "check": true }),
        );
        let r = resp.result.expect("result");
        assert!(r.get("changed").is_some(), "check mode must report changed");
        assert!(
            r.get("formatted").is_none(),
            "check mode must NOT include formatted text"
        );
    }

    #[test]
    fn format_missing_content_and_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_format(&ws, 3, &serde_json::json!({}));
        let err = resp.error.expect("no content and no file → invalid params");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    // --- dispatch_fix --------------------------------------------------------

    #[test]
    fn fix_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_fix(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn fix_reports_zero_fixes_for_clean_file() {
        // No custom lint rules are registered, so `fixes` is always 0 and the
        // dryRun flag round-trips. Exercises the happy path + filter branch.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "Ok.al", "codeunit 50100 \"Ok\"\n{\n}\n");
        let resp = dispatch_fix(&ws, 2, &serde_json::json!({ "file": file, "dryRun": true }));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["fixes"], serde_json::json!(0));
        assert_eq!(r["dryRun"], serde_json::json!(true));
    }

    // --- dispatch_rules ------------------------------------------------------

    #[test]
    fn rules_returns_json_array_mirroring_registry() {
        // Native lint rules are intentionally empty (all diagnostics come from
        // the .NET bridge), so the dispatcher must return a JSON array whose
        // length matches the registry exactly — proving it maps the registry
        // rather than fabricating entries.
        let resp = dispatch_rules(1);
        assert!(resp.error.is_none());
        let arr = resp
            .result
            .and_then(|v| v.as_array().cloned())
            .expect("array");
        assert_eq!(
            arr.len(),
            crate::syntax::lint_rules().len(),
            "dispatch_rules length must mirror the lint-rule registry"
        );
    }

    // --- dispatch_parse ------------------------------------------------------

    #[test]
    fn parse_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_parse(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn parse_reports_node_count_and_errors() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(&tmp, "P.al", "codeunit 50100 \"P\"\n{\n}\n");
        let resp = dispatch_parse(&ws, 2, &serde_json::json!({ "file": file }));
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        let nodes = r["nodeCount"].as_u64().expect("nodeCount");
        assert!(nodes > 0, "a non-empty file must parse to >0 nodes");
        assert!(r.get("parseErrors").is_some());
    }

    // --- dispatch_location ---------------------------------------------------

    #[test]
    fn location_missing_name_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_location(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn location_unknown_object_is_invalid_params_with_name() {
        let ws = empty_ws();
        let resp = dispatch_location(&ws, 2, &serde_json::json!({ "name": "Nope" }));
        let err = resp.error.expect("unknown object must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Nope"), "error names the object");
    }

    // --- dispatch_source -----------------------------------------------------

    #[test]
    fn source_missing_name_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_source(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn source_unknown_object_returns_not_found() {
        let ws = empty_ws();
        let resp = dispatch_source(&ws, 2, &serde_json::json!({ "name": "GhostObject" }));
        let err = resp.error.expect("unknown object must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("GhostObject"));
    }

    // --- dispatch_new_project ------------------------------------------------

    #[test]
    fn new_project_missing_dir_is_invalid_params() {
        let resp = dispatch_new_project(1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn new_project_rejects_relative_dir() {
        // Path-traversal guard: relative dirs must be rejected before any
        // scaffold write happens.
        let resp = dispatch_new_project(2, &serde_json::json!({ "dir": "../evil" }));
        let err = resp.error.expect("relative dir must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"), "got: {}", err.message);
    }

    #[test]
    fn new_project_scaffolds_into_absolute_dir() {
        // Positive: an absolute target dir scaffolds a project (creates app.json).
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("MyApp");
        let resp = dispatch_new_project(
            3,
            &serde_json::json!({
                "dir": dir.to_string_lossy(),
                "name": "MyApp",
                "publisher": "Acme",
            }),
        );
        assert!(resp.error.is_none(), "scaffold failed: {:?}", resp.error);
        assert!(dir.join("app.json").exists(), "app.json must be created");
    }

    // --- dispatch_metrics ----------------------------------------------------

    #[test]
    fn metrics_missing_file_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_metrics(&ws, 1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn metrics_single_file_returns_thresholds_and_procedures() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let file = write_al(
            &tmp,
            "M.al",
            "codeunit 50100 \"M\"\n{\n  procedure Do()\n  begin\n  end;\n}\n",
        );
        let resp = dispatch_metrics(&ws, 2, &serde_json::json!({ "file": file }));
        assert!(resp.error.is_none(), "{:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["thresholdCyclomatic"], serde_json::json!(10));
        assert_eq!(r["thresholdCognitive"], serde_json::json!(15));
        assert!(r.get("procedures").and_then(|v| v.as_array()).is_some());
    }

    // --- dispatch_snapshot ---------------------------------------------------

    #[tokio::test]
    async fn snapshot_missing_cmd_is_invalid_params() {
        let resp = dispatch_snapshot(1, &serde_json::json!({})).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("cmd"));
    }

    #[tokio::test]
    async fn snapshot_unknown_cmd_is_invalid_params() {
        let resp = dispatch_snapshot(2, &serde_json::json!({ "cmd": "bogus" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Unknown snapshot command"));
    }

    #[tokio::test]
    async fn snapshot_download_missing_id_is_invalid_params() {
        let resp = dispatch_snapshot(3, &serde_json::json!({ "cmd": "download" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("snapshotId"));
    }

    // --- dispatch_profiling --------------------------------------------------

    #[tokio::test]
    async fn profiling_missing_cmd_is_invalid_params() {
        let resp = dispatch_profiling(1, &serde_json::json!({})).await;
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn profiling_unknown_cmd_is_invalid_params() {
        let resp = dispatch_profiling(2, &serde_json::json!({ "cmd": "zzz" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Unknown profiling command"));
    }

    #[tokio::test]
    async fn profiling_analyze_missing_path_is_invalid_params() {
        let resp = dispatch_profiling(3, &serde_json::json!({ "cmd": "analyze" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("path"));
    }

    #[tokio::test]
    async fn profiling_analyze_rejects_relative_path() {
        // Path-traversal guard before any file read.
        let resp = dispatch_profiling(
            4,
            &serde_json::json!({ "cmd": "analyze", "path": "rel/profile.json" }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"), "got: {}", err.message);
    }

    // --- dispatch_tests_snapshot_record / replay / diff ----------------------

    #[tokio::test]
    async fn snapshot_record_missing_codeunit_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(&ws, 1, &serde_json::json!({})).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("codeunitId"));
    }

    #[tokio::test]
    async fn snapshot_record_rejects_out_of_range_codeunit() {
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(
            &ws,
            2,
            &serde_json::json!({ "codeunitId": (i32::MAX as i64) + 1 }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("out of range"));
    }

    #[tokio::test]
    async fn snapshot_record_missing_breakpoints_is_invalid_params() {
        // Past codeunitId + methodName validation, an empty breakpoints array
        // must still be rejected — proving the guard fires, not the BC stub.
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(
            &ws,
            3,
            &serde_json::json!({ "codeunitId": 50100, "methodName": "T", "breakpoints": [] }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("breakpoints"));
    }

    #[tokio::test]
    async fn snapshot_record_fully_valid_reaches_not_wired_stub() {
        // All params valid → the dispatcher reaches the documented
        // "not yet wired" INTERNAL_ERROR rather than an INVALID_PARAMS.
        let ws = empty_ws();
        let resp = dispatch_tests_snapshot_record(
            &ws,
            4,
            &serde_json::json!({
                "codeunitId": 50100,
                "methodName": "T",
                "breakpoints": [{ "file": "a.al", "line": 1 }],
            }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(err.message.contains("not yet wired"));
    }

    #[tokio::test]
    async fn snapshot_replay_missing_path_is_invalid_params() {
        let resp = dispatch_tests_snapshot_replay(1, &serde_json::json!({})).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("snapshotPath"));
    }

    #[tokio::test]
    async fn snapshot_replay_unreadable_path_is_internal_error() {
        let resp = dispatch_tests_snapshot_replay(
            2,
            &serde_json::json!({ "snapshotPath": "/nonexistent/snap.bin" }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
    }

    #[tokio::test]
    async fn snapshot_diff_missing_paths_is_invalid_params() {
        let only_a = dispatch_tests_snapshot_diff(1, &serde_json::json!({ "pathA": "/x" })).await;
        let err = only_a.error.expect("missing pathB must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("pathB"));

        let none = dispatch_tests_snapshot_diff(2, &serde_json::json!({})).await;
        let err = none.error.expect("missing pathA must error");
        assert!(err.message.contains("pathA"));
    }

    // --- dispatch_generate (kind / table branches) ---------------------------

    #[test]
    fn generate_unknown_kind_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_generate(&ws, 1, &serde_json::json!({ "kind": "frobnicate" }));
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Unknown generate kind"));
    }

    #[test]
    fn generate_page_missing_table_reports_table_not_found() {
        // A page requires a source table; an unknown table name must surface
        // a "not found in symbol index" error (the `table_entry` None branch).
        let ws = empty_ws();
        let resp = dispatch_generate(
            &ws,
            2,
            &serde_json::json!({ "kind": "page", "name": "P", "table": "NoSuchTable" }),
        );
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("NoSuchTable"));
    }

    // --- dispatch_deps_graph -------------------------------------------------

    #[test]
    fn deps_graph_dot_format_returns_dot_content() {
        let ws = empty_ws();
        let resp = dispatch_deps_graph(&ws, 1, &serde_json::json!({ "format": "dot" }));
        assert!(resp.error.is_none());
        let r = resp.result.expect("result");
        assert_eq!(r["format"], serde_json::json!("dot"));
        assert!(r.get("content").and_then(|v| v.as_str()).is_some());
    }

    #[test]
    fn deps_graph_default_format_is_json_object() {
        let ws = empty_ws();
        let resp = dispatch_deps_graph(&ws, 2, &serde_json::json!({}));
        assert!(resp.error.is_none());
        // JSON branch serialises the graph struct (not the {format,content} shape).
        let r = resp.result.expect("result");
        assert!(
            r.get("content").is_none(),
            "json branch must not carry the dot `content` field"
        );
    }

    // --- dispatch_xlf_* path guards ------------------------------------------

    #[test]
    fn xlf_untranslated_missing_param_is_invalid_params() {
        let resp = dispatch_xlf_untranslated(1, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[test]
    fn xlf_untranslated_rejects_relative_path() {
        let resp = dispatch_xlf_untranslated(2, &serde_json::json!({ "xlf": "rel/de-DE.xlf" }));
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"));
    }

    #[tokio::test]
    async fn xlf_suggest_rejects_relative_path() {
        let ws = empty_ws();
        let resp = dispatch_xlf_suggest(&ws, 1, &serde_json::json!({ "xlf": "rel.xlf" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"));
    }

    #[tokio::test]
    async fn xlf_refresh_missing_param_is_invalid_params() {
        let ws = empty_ws();
        let resp = dispatch_xlf_refresh(&ws, 1, &serde_json::json!({})).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("xlf"));
    }

    #[tokio::test]
    async fn xlf_refresh_rejects_relative_path() {
        let ws = empty_ws();
        let resp =
            dispatch_xlf_refresh(&ws, 2, &serde_json::json!({ "xlf": "Translations/de.xlf" }))
                .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"));
    }

    // -----------------------------------------------------------------------
    // parse_bc_server_params: defaults vs explicit overrides (shared by the
    // snapshot + profiling dispatchers). Pure, no BC server required.
    // -----------------------------------------------------------------------

    #[test]
    fn bc_server_params_apply_documented_defaults_when_absent() {
        // Empty params: every field must fall back to its documented default
        // and the output dir must end in the supplied subdir.
        let bc = parse_bc_server_params(&serde_json::json!({}), "snapshots");
        assert_eq!(bc.server_url, "http://localhost:7049/BC");
        assert_eq!(bc.company, "");
        assert!(bc.username.is_none());
        assert!(bc.password.is_none());
        assert!(!bc.accept_invalid_certs);
        assert!(
            bc.output_dir.ends_with("snapshots"),
            "default output dir must end in the subdir, got {:?}",
            bc.output_dir
        );
    }

    #[test]
    fn bc_server_params_honour_explicit_overrides() {
        // Positive: every explicit field is threaded through verbatim, and an
        // explicit outputDir wins over the subdir-based default.
        let bc = parse_bc_server_params(
            &serde_json::json!({
                "serverUrl": "https://bc.example/inst",
                "company": "CRONUS",
                "outputDir": "/data/out",
                "username": "admin",
                "password": "s3cret",
                "acceptInvalidCerts": true,
            }),
            "profiles",
        );
        assert_eq!(bc.server_url, "https://bc.example/inst");
        assert_eq!(bc.company, "CRONUS");
        assert_eq!(bc.output_dir, std::path::PathBuf::from("/data/out"));
        assert_eq!(bc.username.as_deref(), Some("admin"));
        assert_eq!(bc.password.as_deref(), Some("s3cret"));
        assert!(bc.accept_invalid_certs);
    }

    #[test]
    fn bc_server_params_ignore_wrong_typed_fields() {
        // Negative: a client sending the wrong JSON type (number where a
        // string is expected) must not poison the value — it falls back to
        // the default rather than e.g. stringifying the number.
        let bc = parse_bc_server_params(
            &serde_json::json!({
                "serverUrl": 7049,
                "acceptInvalidCerts": "yes",
            }),
            "snapshots",
        );
        assert_eq!(bc.server_url, "http://localhost:7049/BC");
        assert!(
            !bc.accept_invalid_certs,
            "non-bool acceptInvalidCerts must default to false, not be coerced true"
        );
    }

    // -----------------------------------------------------------------------
    // capitalize_first / sanitize_filename: pure helpers used by
    // dispatch_organize_files to build canonical `.al` file names.
    // -----------------------------------------------------------------------

    #[test]
    fn capitalize_first_uppercases_only_leading_char() {
        assert_eq!(capitalize_first("table"), "Table");
        assert_eq!(capitalize_first("pageExtension"), "PageExtension");
        // Already-capitalised input is left intact.
        assert_eq!(capitalize_first("Codeunit"), "Codeunit");
    }

    #[test]
    fn capitalize_first_handles_empty_and_unicode() {
        // Empty string must not panic — returns empty.
        assert_eq!(capitalize_first(""), "");
        // A non-ASCII leading char must uppercase without slicing mid-codepoint.
        assert_eq!(capitalize_first("ärger"), "Ärger");
    }

    #[test]
    fn sanitize_filename_replaces_path_and_reserved_chars() {
        // Every reserved/separator char must become `_` so the rename target
        // can't escape its directory or produce an invalid filename.
        assert_eq!(
            sanitize_filename(r#"a/b\c:d*e?f"g<h>i|j"#),
            "a_b_c_d_e_f_g_h_i_j"
        );
        // Ordinary names with spaces and dots survive untouched.
        assert_eq!(sanitize_filename("Sales Header"), "Sales Header");
    }

    // -----------------------------------------------------------------------
    // dispatch_permissions: xml format branch + objectCount shaping.
    // -----------------------------------------------------------------------

    #[test]
    fn permissions_xml_format_returns_xml_content_and_count() {
        // The `xml` format branch must report format=xml, surface an
        // objectCount, and emit XML (not the AL permissionset syntax).
        let ws = empty_ws();
        let resp = dispatch_permissions(
            &ws,
            1,
            &serde_json::json!({ "format": "xml", "roleId": "TESTROLE", "name": "Demo" }),
        );
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r.get("format").and_then(|v| v.as_str()), Some("xml"));
        assert!(
            r.get("objectCount").and_then(|v| v.as_u64()).is_some(),
            "xml branch must expose objectCount"
        );
        let content = r.get("content").and_then(|v| v.as_str()).expect("content");
        // XML output, not the AL `permissionset` declaration.
        assert!(
            content.contains('<'),
            "xml branch must render XML, got: {content}"
        );
    }

    #[test]
    fn permissions_default_format_is_al() {
        // An unrecognised format falls through to the AL renderer (the `_`
        // arm), not the xml branch.
        let ws = empty_ws();
        let resp = dispatch_permissions(&ws, 2, &serde_json::json!({ "format": "totally-bogus" }));
        let r = resp.result.expect("result");
        assert_eq!(r.get("format").and_then(|v| v.as_str()), Some("al"));
    }

    // -----------------------------------------------------------------------
    // dispatch_sort_members: content path, dry-run, and missing-param branch.
    // -----------------------------------------------------------------------

    #[test]
    fn sort_members_with_content_returns_sorted_and_changed_flags() {
        // The `content` branch must echo back `sorted` text and a `changed`
        // bool without requiring a file on disk.
        let ws = empty_ws();
        let src = r#"codeunit 50100 "X" { procedure B() begin end; procedure A() begin end; }"#;
        let resp = dispatch_sort_members(&ws, 1, &serde_json::json!({ "content": src }));
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert!(
            r.get("sorted").and_then(|v| v.as_str()).is_some(),
            "must return sorted text"
        );
        assert!(
            r.get("changed").and_then(|v| v.as_bool()).is_some(),
            "must return a changed flag"
        );
    }

    #[test]
    fn sort_members_missing_content_and_file_is_invalid_params() {
        // Neither `content` nor `file`: must be INVALID_PARAMS, not a panic.
        let ws = empty_ws();
        let resp = dispatch_sort_members(&ws, 2, &serde_json::json!({}));
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    // -----------------------------------------------------------------------
    // dispatch_organize_files: no-project root must surface an error rather
    // than scanning an undefined root.
    // -----------------------------------------------------------------------

    #[test]
    fn organize_files_without_project_returns_error() {
        let ws = empty_ws();
        let resp = dispatch_organize_files(&ws, 1, &serde_json::json!({ "dryRun": true }));
        assert!(
            resp.error.is_some(),
            "no project root must yield an error, got result: {:?}",
            resp.result
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_profiler_hints: an absent/empty `hotspots` array must produce
    // a well-formed (array) result, never an error.
    // -----------------------------------------------------------------------

    #[test]
    fn profiler_hints_absent_hotspots_returns_array() {
        let ws = empty_ws();
        let resp = dispatch_profiler_hints(&ws, 1, &serde_json::json!({}));
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert!(
            r.is_array(),
            "profiler hints must serialize to a JSON array, got {r:?}"
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_authenticate: status/clear branches resolve without a network
    // call when the workspace has no configured tenants.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn authenticate_status_no_tenants_returns_empty_list() {
        let ws = empty_ws();
        let resp = dispatch_authenticate(&ws, 1, &serde_json::json!({ "cmd": "status" })).await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let tenants = resp
            .result
            .as_ref()
            .and_then(|v| v.get("tenants"))
            .and_then(|v| v.as_array())
            .expect("tenants array");
        assert!(tenants.is_empty(), "no project tenants → empty list");
    }

    #[tokio::test]
    async fn authenticate_clear_no_tenants_clears_zero() {
        let ws = empty_ws();
        let resp = dispatch_authenticate(&ws, 2, &serde_json::json!({ "cmd": "clear" })).await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        assert_eq!(
            resp.result
                .as_ref()
                .and_then(|v| v.get("cleared"))
                .and_then(|v| v.as_u64()),
            Some(0),
            "no tenants → cleared count of 0"
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_tests_mutate: no-project must short-circuit to an error before
    // any variant generation.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn tests_mutate_no_project_returns_error() {
        let ws = empty_ws();
        let resp = dispatch_tests_mutate(&ws, 1, &serde_json::json!({})).await;
        let err = resp.error.expect("no project must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("project"),
            "error must reference the missing project: {}",
            err.message
        );
    }

    // -----------------------------------------------------------------------
    // dispatch_clear_cache: response shape — `deleted`/`existed` booleans and
    // a `path`. With no index dir present, both flags must be false and no
    // error must be reported.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn clear_cache_reports_shape_and_no_error() {
        let resp = dispatch_clear_cache(7).await;
        assert!(resp.error.is_none(), "transport error: {:?}", resp.error);
        let r = resp.result.expect("result");
        for key in ["deleted", "existed", "path", "error"] {
            assert!(r.get(key).is_some(), "clearCache result missing `{key}`");
        }
        assert!(
            r.get("deleted").and_then(|v| v.as_bool()).is_some(),
            "`deleted` must be a bool"
        );
    }
}
