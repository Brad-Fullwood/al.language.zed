//! Build/toolchain/analysis dispatchers — compile, package, lint, format, fix, permissions,
//! authenticate, download symbols, snapshot, profiling, xliff, etc.

use std::path::PathBuf;

use al_core::workspace::Workspace;
use al_daemon_client::jsonrpc::{error_codes, Response, RpcError};

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
            let diagnostics = al_core::syntax::lint(&tree, &content);
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

    let result = al_core::syntax::AlParser::parse_quick(&text);
    let mut diagnostics = al_core::syntax::lint(&result.tree, &text);

    // Add parse errors
    for err in &result.errors {
        diagnostics.push(al_core::syntax::LintDiagnostic {
            code: "parse-error".to_string(),
            message: err.message.clone(),
            range: err.range,
            severity: al_core::syntax::LintSeverity::Error,
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
        .map(|root| al_core::queries::format::AlFormatConfig::load_options(&root))
        .unwrap_or_default();
    let formatted = al_core::syntax::format_al(&content, &options);
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

    let result = al_core::syntax::AlParser::parse_quick(&text);
    let diagnostics = al_core::syntax::lint(&result.tree, &text);
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
    let rules = al_core::syntax::lint_rules();
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
    let result = al_core::syntax::AlParser::parse_quick(&text);
    let elapsed = start.elapsed();

    let node_count = al_core::parsing::count_nodes(&result.tree);

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
        .and_then(|s| s.parse::<al_core::symbols::ObjectKind>().ok());

    let proc_filter = params.get("proc").and_then(|v| v.as_str());
    let trigger_filter = params.get("trigger").and_then(|v| v.as_str());

    match al_core::queries::source::source(
        workspace,
        name,
        kind_filter,
        proc_filter,
        trigger_filter,
    ) {
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
    let entries = al_core::permissions::collect_permissions(workspace);
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
            let output = al_core::permissions::render_xml(&entries, role_id, name);
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
            let output = al_core::permissions::render_al(&entries, name, perm_id);
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
        let guard = al_core::semantic::get_or_init_bridge(workspace)
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
                    al_core::build::find_app_file(&project_root).map(|p| p.display().to_string())
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

    match al_core::build::compile_project_with_analyzers(
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

    let config = al_core::scaffold::ScaffoldConfig {
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
        ..al_core::scaffold::ScaffoldConfig::default()
    };

    match al_core::scaffold::create_project(&dir, &config) {
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
    let report = al_core::toolchain::doctor(workspace);
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
                let cache_path = al_core::symbols::oauth::token_cache_path(tenant);
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
                let cache_path = al_core::symbols::oauth::token_cache_path(tenant);
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

            match al_core::symbols::oauth::acquire_token(&client, &tenant, move |msg| {
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

pub(super) fn dispatch_download_symbols(
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

    let result: Vec<serde_json::Value> = tokio::task::block_in_place(|| {
        let rt = tokio::runtime::Handle::current();
        rt.block_on(async {
            if source == "server" {
                if project_configs.is_empty() {
                    return vec![serde_json::json!({
                        "error": "No BC server config found"
                    })];
                }
                let cfg = &project_configs[0];
                let auth = match cfg.authentication {
                    al_core::launch::AuthMethod::Windows => {
                        al_core::symbols::bc_server::AuthMethod::Windows
                    }
                    al_core::launch::AuthMethod::UserPassword => {
                        al_core::symbols::bc_server::AuthMethod::UserPassword
                    }
                    al_core::launch::AuthMethod::AAD => {
                        al_core::symbols::bc_server::AuthMethod::AAD
                    }
                };
                let client = match al_core::symbols::bc_server::BcServerClient::new(
                    auth,
                    cfg.tenant.clone(),
                    std::sync::Arc::new(|msg| tracing::info!("{msg}")),
                    cfg.accept_invalid_certs,
                ) {
                    Ok(c) => c,
                    Err(e) => return vec![serde_json::json!({ "error": e.to_string() })],
                };
                let url_deps: Vec<(String, al_core::symbols::nuget::AppDependency)> = all_deps
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
                    crate::workspace::map_nuget_feeds(&al_core::project::nuget_feeds());
                let client = al_core::symbols::nuget::NuGetClient::new(nuget_feeds);
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
        })
    });

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
    let config = al_core::snapshot::SnapshotConfig {
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
            match al_core::snapshot::start_snapshot(&config, description).await {
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
            match al_core::snapshot::list_snapshots(&config).await {
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

            match al_core::snapshot::download_snapshot(&config, &snapshot_id).await {
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
    let config = al_core::profiling::ProfilingConfig {
        server_url: bc.server_url,
        company: bc.company,
        output_dir: bc.output_dir,
        username: bc.username,
        password: bc.password,
        accept_invalid_certs: bc.accept_invalid_certs,
    };

    match cmd {
        "start" => match al_core::profiling::start_profiling(&config).await {
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

            match al_core::profiling::stop_profiling(&config, &session_id).await {
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

            match al_core::profiling::analyze_profile_file(&profile_path, top_n).await {
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
                al_daemon_client::jsonrpc::error_codes::INVALID_PARAMS,
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

    match al_core::xliff::build_xliff(workspace, &project_root) {
        Some((path, count)) => Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy().as_ref(),
                "units": count,
            })),
            error: None,
        },
        None => Response {
            id,
            result: Some(serde_json::json!({
                "path": null,
                "units": 0,
                "message": "No translatable texts found (check features.TranslationFile in app.json)",
            })),
            error: None,
        },
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
                al_daemon_client::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            )
        }
    };
    if !xlf_path.is_absolute() {
        return rpc_error(
            id,
            al_daemon_client::jsonrpc::error_codes::INVALID_PARAMS,
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
                al_daemon_client::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {}: {e}", generated_path.display()),
            )
        }
    };
    let lang_content = match tokio::fs::read_to_string(&xlf_path).await {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_daemon_client::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {}: {e}", xlf_path.display()),
            )
        }
    };

    let gen_units_map = al_core::xliff::parse_xliff(&gen_content);
    let gen_units: Vec<al_core::xliff::TranslationUnit> = gen_units_map.into_values().collect();
    let lang_units = al_core::xliff::parse_xliff(&lang_content);

    let (updated_units, refresh_result) = al_core::xliff::refresh_xliff(&gen_units, &lang_units);

    // Write updated units back to the language xlf
    // We need the app name for the XLIFF header
    let app_name = xlf_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("App")
        .trim_end_matches(".g")
        .to_string();
    let new_xlf = al_core::xliff::generate_xliff(&app_name, "en-US", "en-US", &updated_units);
    if let Err(e) = tokio::task::block_in_place(|| std::fs::write(&xlf_path, new_xlf)) {
        return rpc_error(
            id,
            al_daemon_client::jsonrpc::error_codes::INTERNAL_ERROR,
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
                al_daemon_client::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            )
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_daemon_client::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }
    let xlf_content = match tokio::task::block_in_place(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_daemon_client::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            )
        }
    };
    let units_map = al_core::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<al_core::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = al_core::xliff::find_untranslated(&all_units);
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
                al_daemon_client::jsonrpc::error_codes::INVALID_PARAMS,
                "Missing 'xlf' param",
            )
        }
    };
    if !std::path::Path::new(xlf_path).is_absolute() {
        return rpc_error(
            id,
            al_daemon_client::jsonrpc::error_codes::INVALID_PARAMS,
            "'xlf' must be an absolute path",
        );
    }

    let xlf_content = match tokio::task::block_in_place(|| std::fs::read_to_string(xlf_path)) {
        Ok(c) => c,
        Err(e) => {
            return rpc_error(
                id,
                al_daemon_client::jsonrpc::error_codes::INTERNAL_ERROR,
                &format!("Cannot read {xlf_path}: {e}"),
            )
        }
    };

    let units_map = al_core::xliff::parse_xliff(&xlf_content);
    let all_units: Vec<al_core::xliff::TranslationUnit> = units_map.into_values().collect();
    let untranslated = al_core::xliff::find_untranslated(&all_units);
    let suggestions = al_core::xliff::suggest_translations(&untranslated, workspace);

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

    match al_core::queries::bulk_fix::add_application_area(&project_root, value, dry_run) {
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
            .filter(|e| e.kind == al_core::symbols::ObjectKind::Table)
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

    match al_core::queries::bulk_fix::add_tooltips(&project_root, &tooltips, dry_run) {
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

    match al_core::queries::bulk_fix::add_data_classification(&project_root, value, dry_run) {
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
            let parsed = al_core::syntax::AlParser::parse_quick(text);
            let metrics = al_core::syntax::complexity::compute_complexity(&parsed.tree, text);
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

    let parsed = al_core::syntax::AlParser::parse_quick(&text);
    let metrics = al_core::syntax::complexity::compute_complexity(&parsed.tree, &text);

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
    m: &al_core::syntax::complexity::ProcedureComplexity,
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
    let tests = al_core::queries::tests::discover_tests(workspace);
    let value = serde_json::to_value(&tests).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
    }
}

pub(super) fn dispatch_tests_coverage(workspace: &Workspace, id: u64) -> Response {
    let report = al_core::queries::test_coverage::test_coverage(workspace);
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
    use al_core::launch::find_launch_config;
    use al_core::queries::test_diagnostics::results_to_diagnostics;
    use al_core::test_runner::TestRunnerClient;

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
                e.kind == al_core::symbols::ObjectKind::Table
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
                .parse::<al_core::generators::PageType>()
                .unwrap_or_default();

            let Some(source) = table_entry else {
                return rpc_error(
                    id,
                    error_codes::INVALID_PARAMS,
                    &format!("Table '{}' not found in symbol index", table_name),
                );
            };
            let config = al_core::generators::GeneratePageConfig {
                object_id,
                page_name,
                page_type,
                source_table: (*source).clone(),
            };
            let code = al_core::generators::generate_page(&config);
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
            let config = al_core::generators::GenerateReportConfig {
                object_id,
                report_name,
                source_table: (*source).clone(),
            };
            let code = al_core::generators::generate_report(&config);
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
            let config = al_core::generators::GenerateTestConfig {
                object_id,
                test_name,
                subject,
            };
            let code = al_core::generators::generate_test(&config);
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
    let entries = al_core::queries::obsolescence::obsolescence_timeline(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
    }
}

pub(super) fn dispatch_audit_data_classification(workspace: &Workspace, id: u64) -> Response {
    let entries = al_core::queries::audit::data_classification_audit(workspace);
    let value = serde_json::to_value(&entries).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
    }
}

pub(super) fn dispatch_permission_set_audit(workspace: &Workspace, id: u64) -> Response {
    let entries = al_core::queries::audit::permission_set_audit(workspace);
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
        .and_then(|path| tokio::task::block_in_place(|| std::fs::read_to_string(path)).ok())
        .unwrap_or_default();

    // Build package list from loaded symbols — name, publisher, version, deps
    // Currently we pass the packages list without transitive dependency info;
    // the dep graph will still resolve direct dependencies from app.json.
    #[allow(clippy::type_complexity)]
    let packages: Vec<(String, String, String, Vec<(String, String, String)>)> = Vec::new();

    let graph = al_core::queries::deps::build_dependency_graph(&app_json, &packages);

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
    let current: Vec<al_core::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline: Vec<al_core::symbols::SymbolEntry> = Vec::new();
    let changes = al_core::queries::breaking_changes::analyze_breaking_changes(&baseline, &current);
    let value = serde_json::to_value(&changes).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
    }
}

pub(super) fn dispatch_arch_lint(workspace: &Workspace, id: u64) -> Response {
    // Load .alarch.json from project root if present; fall back to defaults
    let config = workspace
        .project
        .try_read()
        .ok()
        .and_then(|p| p.as_ref().map(|p| p.root.join(".alarch.json")))
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|json| al_core::queries::arch_lint::ArchConfig::from_json(&json).ok())
        .unwrap_or_default();
    let violations = al_core::queries::arch_lint::arch_lint(workspace, &config);
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
        al_core::queries::duplicates::find_duplicates(workspace, min_tokens, min_similarity);
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
    let current: Vec<al_core::symbols::SymbolEntry> = workspace
        .symbols
        .all_entries()
        .into_iter()
        .map(|a| (*a).clone())
        .collect();
    let baseline: Vec<al_core::symbols::SymbolEntry> = Vec::new();
    let issues = al_core::queries::upgrade::upgrade_report(&baseline, &current);
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
    let findings = al_core::queries::sql_patterns::detect_sql_patterns(workspace);
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

    let sorted = match al_core::syntax::sort_members(&content) {
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
                let _ = tokio::task::block_in_place(|| std::fs::write(&path, &sorted));
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
        let parsed = al_core::syntax::AlParser::parse_quick(&text);
        let obj = match al_core::syntax::find_object_declaration(&parsed.tree, &text) {
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
    let hints = al_core::queries::profiler_hints::profiler_hints(workspace, &hotspots);
    let value = serde_json::to_value(&hints).unwrap_or(serde_json::Value::Null);
    Response {
        id,
        result: Some(value),
        error: None,
    }
}
