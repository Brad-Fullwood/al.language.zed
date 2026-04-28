//! Build/toolchain/analysis dispatchers — compile, package, lint, format, fix, permissions,
//! authenticate, download symbols, snapshot, profiling, xliff, etc.

use std::path::PathBuf;

use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response, RpcError};

use super::{
    ensure_document, file_not_found, file_uri_from_params, invalid_params, lint_diag_to_json,
    require_document_text, rpc_error,
};

const ERR_INITIALIZING: &str = "Workspace is initializing, try again";
const ERR_NO_PROJECT: &str = "No project loaded";

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
        }
    } else {
        // If a file was specified, write back
        if let Some(uri) = file_uri_from_params(params) {
            if let Ok(path) = uri.to_file_path() {
                if changed {
                    if let Err(e) =
                        tokio::task::block_in_place(|| std::fs::write(&path, &formatted))
                    {
                        return rpc_error(
                            id,
                            -32000,
                            &format!("Failed to write formatted file: {e}"),
                        );
                    }
                    // Update document store
                    workspace.documents.open(uri, formatted.clone());
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
        },
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Object '{}' not found in workspace", name),
            }),
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
        },
        None => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Object '{}' not found", name),
            }),
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
    let perm_id = params.get("id").and_then(|v| v.as_i64()).unwrap_or(50100);
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
        },
        Err(msg) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::CODE_ANALYSIS_ERROR,
                message: msg,
            }),
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
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e.to_string(),
            }),
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
        },
        Err(e) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: e,
            }),
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
    }
}

pub(super) fn dispatch_setup(workspace: &Workspace, id: u64) -> Response {
    let report = crate::toolchain::doctor(workspace);
    Response {
        id,
        result: Some(serde_json::to_value(&report).unwrap_or(serde_json::Value::Null)),
        error: None,
    }
}

pub(super) fn dispatch_clear_cache(id: u64) -> Response {
    let cache_dir = dirs::cache_dir()
        .map(|d| d.join("al-lsp").join("index"))
        .unwrap_or_else(|| PathBuf::from("/tmp/al-lsp/index"));

    let existed = cache_dir.exists();
    if existed {
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    Response {
        id,
        result: Some(serde_json::json!({
            "deleted": existed,
            "path": cache_dir.display().to_string(),
        })),
        error: None,
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
                };
            };

            let client = reqwest::Client::new();
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
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("Authentication failed: {e}"),
                    }),
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

    Response {
        id,
        result: Some(serde_json::json!({
            "source": source,
            "downloaded": success,
            "failed": failed,
            "results": result,
        })),
        error: None,
    }
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
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot start failed: {e}"),
                    }),
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
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot list failed: {e}"),
                    }),
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
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("snapshot download failed: {e}"),
                    }),
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
            },
            Err(e) => Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("profiling start failed: {e}"),
                }),
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
                },
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("profiling stop failed: {e}"),
                    }),
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
                    }
                }
                Err(e) => Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!("profiling analyze failed: {e}"),
                    }),
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
        },
        Ok(None) => Response {
            id,
            result: Some(serde_json::json!({
                "path": null,
                "units": 0,
                "message": "No translatable texts found (check features.TranslationFile in app.json)",
            })),
            error: None,
        },
        Err(e) => rpc_error(id, -32000, &format!("xlf-build failed: {e}")),
    }
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
    let new_xlf = crate::xliff::generate_xliff(&app_name, "en-US", "en-US", &updated_units);
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
    }
}

pub(super) fn dispatch_tests_coverage(workspace: &Workspace, id: u64) -> Response {
    let report = crate::queries::test_coverage::test_coverage(workspace);
    let value = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
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
        Some(n) => n as i32,
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
        .unwrap_or(&codeunit_id.to_string())
        .to_string();
    // Rebind after borrow ends
    let codeunit_name = if codeunit_name == codeunit_id.to_string() {
        codeunit_id.to_string()
    } else {
        codeunit_name
    };
    let method = params
        .get("method")
        .and_then(|v| v.as_str())
        .map(String::from);
    let config_name = params.get("config").and_then(|v| v.as_str());

    // -- Find launch config ----------------------------------------------------
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

    let result_json = serde_json::to_value(&result).unwrap_or(serde_json::Value::Null);
    let diag_json = serde_json::to_value(&diagnostics).unwrap_or(serde_json::json!([]));

    Response {
        id,
        result: Some(serde_json::json!({
            "result": result_json,
            "diagnostics": diag_json,
        })),
        error: None,
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
    use std::path::PathBuf;
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
            Some(n) => n as i32,
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
        timeout_ms: params.get("timeoutMs").and_then(|v| v.as_u64()),
        parallel: params
            .get("parallel")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        junit_out: params
            .get("junitOut")
            .and_then(|v| v.as_str())
            .map(PathBuf::from),
        cobertura_out: params
            .get("coberturaOut")
            .and_then(|v| v.as_str())
            .map(PathBuf::from),
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
        return match store.last_for(cu as i32, method).await {
            Ok(opt) => Response {
                id,
                result: Some(serde_json::json!({ "lastResult": opt })),
                error: None,
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
        Some(cu) => all
            .into_iter()
            .filter(|r| r.codeunit_id == cu as i32)
            .collect(),
        None => all,
    };
    Response {
        id,
        result: Some(serde_json::json!({ "results": filtered })),
        error: None,
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
    let object_id = params.get("id").and_then(|v| v.as_i64()).unwrap_or(50100) as i32;
    let table_name = params.get("table").and_then(|v| v.as_str()).unwrap_or("");

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
            }
        }
        other => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: format!("Unknown generate kind: {other}. Use page, report, or test"),
            }),
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
    }
}

pub(super) fn dispatch_audit_data_classification(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::audit::data_classification_audit(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
    }
}

pub(super) fn dispatch_permission_set_audit(workspace: &Workspace, id: u64) -> Response {
    let entries = crate::queries::audit::permission_set_audit(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
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

    // Build package list from loaded symbols — name, publisher, version, deps
    // Currently we pass the packages list without transitive dependency info;
    // the dep graph will still resolve direct dependencies from app.json.
    #[allow(clippy::type_complexity)]
    let packages: Vec<(String, String, String, Vec<(String, String, String)>)> = Vec::new();

    let graph = crate::queries::deps::build_dependency_graph(&app_json, &packages);

    if format == "dot" {
        let dot = graph.to_dot();
        Response {
            id,
            result: Some(serde_json::json!({ "format": "dot", "content": dot })),
            error: None,
        }
    } else {
        let value = serde_json::to_value(&graph).unwrap_or(serde_json::Value::Null);
        Response {
            id,
            result: Some(value),
            error: None,
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
    }
}

pub(super) fn dispatch_find_duplicates(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let min_tokens = params
        .get("minTokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(20) as usize;
    let min_similarity = params
        .get("minSimilarity")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.8) as f32;
    let duplicates =
        crate::queries::duplicates::find_duplicates(workspace, min_tokens, min_similarity);
    let value = serde_json::to_value(&duplicates).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
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
                if let Err(e) = tokio::task::block_in_place(|| std::fs::write(&path, &sorted)) {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INTERNAL_ERROR,
                            message: format!("Failed to write sorted file: {e}"),
                        }),
                    };
                }
                workspace.documents.open(uri, sorted.clone());
            }
        }
    }

    Response {
        id,
        result: Some(serde_json::json!({ "sorted": sorted, "changed": changed })),
        error: None,
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

        let renamed = if !dry_run {
            tokio::task::block_in_place(|| std::fs::rename(&path, &new_path)).is_ok()
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
    }
}

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
}
