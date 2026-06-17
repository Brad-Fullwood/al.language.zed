//! Build, package, metrics, snapshot/profiling, sort/organize, and BC-server dispatchers.

use super::super::{ensure_document, file_not_found, file_uri_from_params, invalid_params};
use super::{ERR_INITIALIZING, ERR_NO_PROJECT};
use crate::workspace::Workspace;
use al_protocol::jsonrpc::{error_codes, Response, RpcError};

/// Common BC server connection parameters extracted from JSON-RPC params.
struct BcServerParams {
    server_url: String,
    company: String,
    output_dir: std::path::PathBuf,
    username: Option<String>,
    password: Option<String>,
    accept_invalid_certs: bool,
}
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

/// SSRF guard: reject a `serverUrl` whose scheme is not http(s) before any
/// network use. The daemon socket is same-user only, but a `serverUrl` of
/// `file://`, `gopher://`, etc. would otherwise be handed straight to the BC
/// HTTP client. Reuses the launch-config allowlist so both surfaces agree.
/// Returns `Some(INVALID_PARAMS error)` when the URL must be rejected.
fn reject_unsafe_server_url(id: u64, server_url: &str) -> Option<Response> {
    if crate::launch::is_safe_http_server(server_url) {
        return None;
    }
    Some(Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INVALID_PARAMS,
            message: format!("serverUrl '{server_url}' is not an http(s) URL; refusing to connect"),
        }),
        ..Default::default()
    })
}

/// Return the absolute file path (and line 1) for a workspace object by name.
/// Used by al-explorer to open objects in Zed via the `zed://file/path:line:col` URL scheme.
pub(in crate::server::daemon) fn dispatch_location(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => return invalid_params(id),
    };
    if let Some(path) = workspace.file_index.find_by_object_name(name) {
        return Response {
            id,
            result: Some(serde_json::json!({
                "path": path.to_string_lossy(),
                "line": 1,
            })),
            error: None,
            ..Default::default()
        };
    }

    // FB-4: not a workspace file — fall back to the symbol index and
    // materialise the package object's source as a virtual .al file, the
    // same mechanism go-to-definition uses. Without this, double-clicking
    // any object from a symbol package (i.e. almost everything in the
    // browser) silently did nothing.
    let kind_filter = params
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<crate::symbols::ObjectKind>().ok());
    let id_filter = params.get("id").and_then(|v| v.as_i64());

    let mut candidates = workspace.symbols.get_by_name(name);
    if let Some(kind) = kind_filter {
        candidates.retain(|e| e.kind == kind);
    }
    if let Some(obj_id) = id_filter {
        // Only narrow by ID when it is a real object ID — ID-less kinds
        // (interfaces & co.) carry sentinel/hash values that callers may
        // forward verbatim.
        if obj_id > 0 {
            candidates.retain(|e| i64::from(e.id) == obj_id || e.id <= 0);
        }
    }

    if let Some(entry) = candidates.first() {
        let app_path = workspace.symbols.app_path(&entry.package);
        match crate::symbols::virtual_file::get_or_create(entry, app_path.as_deref()) {
            Ok(path) => {
                return Response {
                    id,
                    result: Some(serde_json::json!({
                        "path": path.to_string_lossy(),
                        "line": 1,
                        "virtual": true,
                    })),
                    error: None,
                    ..Default::default()
                };
            }
            Err(e) => {
                return Response {
                    id,
                    result: None,
                    error: Some(RpcError {
                        code: error_codes::INTERNAL_ERROR,
                        message: format!(
                            "Object '{}' found in package '{}' but its source could not \
                             be materialised: {}",
                            name, entry.package, e
                        ),
                    }),
                    ..Default::default()
                };
            }
        }
    }

    Response {
        id,
        result: None,
        error: Some(RpcError {
            code: error_codes::INVALID_PARAMS,
            message: format!(
                "Object '{}' not found in workspace or symbol packages",
                name
            ),
        }),
        ..Default::default()
    }
}
pub(in crate::server::daemon) fn dispatch_source(
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
pub(in crate::server::daemon) fn dispatch_event_source(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let Some(file) = params.get("file").and_then(|v| v.as_str()) else {
        return invalid_params(id);
    };
    let Some(line) = params.get("line").and_then(|v| v.as_u64()) else {
        return invalid_params(id);
    };
    match crate::queries::source::event_source(
        workspace,
        std::path::Path::new(file),
        line.min(u64::from(u32::MAX)) as u32,
    ) {
        Ok(result) => Response {
            id,
            result: Some(serde_json::to_value(&result).unwrap_or_default()),
            error: None,
            ..Default::default()
        },
        Err(msg) => Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INVALID_PARAMS,
                message: msg,
            }),
            ..Default::default()
        },
    }
}
pub(in crate::server::daemon) async fn dispatch_compile(
    workspace: &Workspace,
    id: u64,
) -> Response {
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
    let Some(toolchain) = tc.clone() else {
        return Response {
            id,
            result: None,
            error: Some(RpcError {
                code: error_codes::INTERNAL_ERROR,
                message: "No toolchain loaded".to_string(),
            }),
            ..Default::default()
        };
    };
    let package_cache = project.as_ref().map(|p| p.packages_dir.clone());
    // Drop the read guards before acquiring async locks
    drop(tc);
    drop(project);

    // Native-first compile policy. Default keeps compilation on the pure-Rust
    // `.app` emitter; `al.useOfficialCompiler: true` opts into
    // Microsoft's `dotnet alc` subprocess. There is no silent fallback.
    let use_official_compiler = workspace.config.read().await.use_official_compiler;

    let result: Result<serde_json::Value, String> = async {
        // Native-first: the pure-Rust native `.app` emitter is the default — no
        // `alc`, no C# bridge. `al.useOfficialCompiler: true` opts into the
        // Microsoft `dotnet alc` subprocess. The native emitter does no semantic
        // analysis, so structured diagnostics come from the LSP, not this step.
        if !use_official_compiler {
            let compile_result = crate::build::native_compile(&project_root);
            return Ok(serde_json::json!({
                "success": compile_result.success,
                "diagnostics": [],
                "appPath": compile_result.app_path.as_ref().map(|p| p.display().to_string()),
                "output": compile_result.output,
            }));
        }

        // Opted into Microsoft's compiler subprocess (non-native). Warn so this
        // is never mistaken for the native `.app` emitter.
        tracing::warn!(
            "al.useOfficialCompiler=true - compiling via the NON-NATIVE Microsoft \
             `dotnet alc` subprocess instead of the native `.app` emitter"
        );
        let compile_result =
            crate::build::compile_project(&toolchain, &project_root, package_cache.as_deref())
                .await
                .map_err(|e| format!("Compilation failed: {}", e))?;
        let app_path = compile_result
            .app_path
            .as_ref()
            .map(|p| p.display().to_string());
        Ok(serde_json::json!({
            "success": compile_result.success,
            "diagnostics": compile_result.diagnostics.iter().map(|d| serde_json::json!({
                "file": d.file,
                "line": d.line,
                "column": d.column,
                // alc output carries no end positions; keep the wire shape.
                "endLine": serde_json::Value::Null,
                "endColumn": serde_json::Value::Null,
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
pub(in crate::server::daemon) async fn dispatch_package(
    workspace: &Workspace,
    id: u64,
) -> Response {
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

pub(in crate::server::daemon) async fn dispatch_snapshot(
    id: u64,
    params: &serde_json::Value,
) -> Response {
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
    if let Some(err) = reject_unsafe_server_url(id, &bc.server_url) {
        return err;
    }
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
pub(in crate::server::daemon) async fn dispatch_profiling(
    id: u64,
    params: &serde_json::Value,
) -> Response {
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
    if let Some(err) = reject_unsafe_server_url(id, &bc.server_url) {
        return err;
    }
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
// Metrics: cyclomatic/cognitive complexity per procedure (T1802)

pub(in crate::server::daemon) fn dispatch_metrics(
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

/// Sort members (variables, triggers, procedures) in canonical order.
/// Params: `file` (URI) or `content` (raw text). If `file` specified, writes back.
pub(in crate::server::daemon) fn dispatch_sort_members(
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
pub(in crate::server::daemon) fn dispatch_organize_files(
    workspace: &Workspace,
    id: u64,
    params: &serde_json::Value,
) -> Response {
    let dry_run = params
        .get("dryRun")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let root: std::path::PathBuf = match super::super::require_project_root(workspace, id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let mut results = Vec::new();

    // Snapshot (path, text) pairs BEFORE the rename loop (F-OPEN-271):
    // `rename_al_file_and_refresh` mutates `file_index.files`, and holding
    // the DashMap iter guard across those writes deadlocked the daemon —
    // clients sat in their 30s read timeout and surfaced a raw EAGAIN.
    let snapshot: Vec<(std::path::PathBuf, String)> = workspace
        .file_index
        .files
        .iter()
        .map(|e| (e.key().clone(), e.value().clone()))
        .collect();

    for (path, text) in snapshot {
        let parsed = crate::syntax::AlParser::parse_quick(&text);
        let obj = match crate::syntax::find_object_declaration(&parsed.tree, &text) {
            Some(o) => o,
            None => continue,
        };

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
pub(in crate::server::daemon) fn dispatch_profiler_hints(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use al_protocol::jsonrpc::error_codes;

    fn empty_ws() -> Workspace {
        Workspace::new()
    }

    #[test]
    fn reject_unsafe_server_url_blocks_non_http_schemes() {
        for bad in ["file:///etc/passwd", "gopher://internal", "ftp://host", ""] {
            let resp = reject_unsafe_server_url(1, bad)
                .unwrap_or_else(|| panic!("{bad:?} must be rejected"));
            assert_eq!(resp.error.unwrap().code, error_codes::INVALID_PARAMS);
            assert!(resp.result.is_none());
        }
    }

    #[test]
    fn reject_unsafe_server_url_allows_http_and_https() {
        for ok in [
            "http://localhost:7049/BC",
            "https://bc.example/inst",
            "localhost:7048",
        ] {
            assert!(
                reject_unsafe_server_url(1, ok).is_none(),
                "{ok:?} must be allowed"
            );
        }
    }

    #[tokio::test]
    async fn dispatch_snapshot_rejects_non_http_serverurl() {
        // A file:// serverUrl returns INVALID_PARAMS (the guard) rather than an
        // INTERNAL_ERROR from a connection attempt — proves the dispatcher
        // refuses before touching the network.
        let params = serde_json::json!({ "cmd": "list", "serverUrl": "file:///etc/passwd" });
        let resp = dispatch_snapshot(42, &params).await;
        let err = resp.error.expect("must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("http(s)"));
    }

    #[tokio::test]
    async fn dispatch_profiling_rejects_non_http_serverurl() {
        let params = serde_json::json!({ "cmd": "start", "serverUrl": "gopher://internal" });
        let resp = dispatch_profiling(7, &params).await;
        let err = resp.error.expect("must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
    }

    fn write_al(tmp: &tempfile::TempDir, name: &str, content: &str) -> String {
        let path = tmp.path().join(name);
        std::fs::write(&path, content).unwrap();
        path.canonicalize().unwrap().to_string_lossy().to_string()
    }

    /// F-OPEN-271: organize-files deadlocked the daemon — the dispatcher held
    /// a `file_index.files` DashMap shard guard across the rename loop while
    /// `rename_al_file_and_refresh` mutated the same map (the documented
    /// DashMap gotcha). Clients then hit their 30s read timeout and surfaced
    /// a raw EAGAIN. The 10s timeout here turns the hang into a clean failure.
    #[tokio::test(flavor = "multi_thread")]
    async fn organize_files_renames_without_deadlocking() {
        let ws = std::sync::Arc::new(empty_ws());
        let tmp = tempfile::TempDir::new().unwrap();
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
        // A real on-disk file whose name does NOT match <Kind><Id>.<Name>.al.
        let source = "codeunit 50100 \"Hello World\"\n{\n}\n";
        let wrong_path = tmp.path().join("misnamed.al");
        std::fs::write(&wrong_path, source).unwrap();
        ws.file_index
            .add_file(wrong_path.clone(), source.to_string());

        let ws2 = std::sync::Arc::clone(&ws);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::task::spawn_blocking(move || {
                dispatch_organize_files(&ws2, 1, &serde_json::json!({}))
            }),
        )
        .await;
        let resp = result
            .expect("organize-files deadlocked (held DashMap guard across rename)")
            .expect("task panicked");
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let files = resp.result.expect("result")["files"].clone();
        let arr = files.as_array().expect("files array");
        assert_eq!(arr.len(), 1, "one rename expected: {arr:?}");
        assert_eq!(arr[0]["renamed"], true, "rename must succeed: {arr:?}");
        assert!(
            tmp.path().join("Codeunit50100.Hello World.al").exists()
                || arr[0]["to"]
                    .as_str()
                    .map(|p| std::path::Path::new(p).exists())
                    .unwrap_or(false),
            "renamed file must exist on disk"
        );
    }

    /// F-011 positive: write_al_file_and_refresh writes to disk AND
    /// updates documents + file_index + invalidates insight graph.
    #[test]
    fn f011_write_helper_refreshes_documents_and_file_index() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("Foo.al");
        let content = r#"codeunit 50100 "Foo" { }"#.to_string();
        write_al_file_and_refresh(&ws, &path, content.clone()).expect("helper succeeds");
        let on_disk = std::fs::read_to_string(&path).expect("file written");
        assert_eq!(on_disk, content);
        let uri = url::Url::from_file_path(&path).unwrap();
        assert_eq!(
            ws.documents.get_text(&uri).as_deref(),
            Some(content.as_str())
        );
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
        assert_eq!(sanitize_filename("Sales Header"), "Sales Header");
    }

    #[test]
    fn sort_members_with_content_returns_sorted_and_changed_flags() {
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

    // =======================================================================
    // Mock-harness coverage for the live-infra dispatchers.
    //
    // These exercise the REAL code paths of `dispatch_compile`,
    // `dispatch_package`, `dispatch_snapshot`, and `dispatch_profiling`
    // without a live BC server or a real AL toolchain:
    //   * BC HTTP is mocked with wiremock — the dispatcher's `serverUrl`
    //     param points at the mock, so the request shape it builds and the
    //     response it parses are asserted end-to-end.
    //   * The AL toolchain is obtained through the `AL_TOOL_PATH` seam
    //     (`toolchain::find_toolchain`) pointed at a fixture dir holding
    //     empty `alc.dll` + `CodeAnalysis.dll` files. No subprocess is
    //     spawned: `compile_project_with_analyzers` rejects the missing
    //     `app.json` before it ever shells out to `dotnet`.
    // No production behaviour is changed by any of this.
    // =======================================================================

    use wiremock::matchers::{body_json, method as wm_method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Serializes the tests that mutate the process-global `AL_TOOL_PATH` env
    /// var so they can't observe each other's half-set state. Mirrors the
    /// `ENV_LOCK` pattern in `toolchain.rs`.
    static AL_TOOL_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn make_project(root: &std::path::Path) -> crate::project::AlProject {
        crate::project::AlProject {
            root: root.to_path_buf(),
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
            packages_dir: root.join(".alpackages"),
            packages: Vec::new(),
            server_configs: Vec::new(),
        }
    }

    /// Write an empty fixture toolchain (`alc.dll` + CodeAnalysis.dll) into
    /// `dir` so `toolchain::find_toolchain` accepts it via the `AL_TOOL_PATH`
    /// seam. The files only need to exist — discovery checks `is_file()`.
    fn write_fixture_toolchain(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("alc.dll"), b"").unwrap();
        std::fs::write(dir.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll"), b"").unwrap();
    }

    #[tokio::test]
    async fn compile_no_project_returns_internal_error() {
        let ws = empty_ws();
        let resp = dispatch_compile(&ws, 1).await;
        let err = resp.error.expect("no project must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert_eq!(err.message, ERR_NO_PROJECT);
    }

    #[tokio::test]
    async fn compile_project_without_toolchain_reports_no_toolchain() {
        // A loaded project but no toolchain must surface the explicit
        // "No toolchain loaded" error — proving the toolchain guard fires
        // AFTER the project check and BEFORE any semantic-bridge work.
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        {
            let mut g = ws.project.write().await;
            *g = Some(make_project(tmp.path()));
        }
        // toolchain stays None.
        let resp = dispatch_compile(&ws, 2).await;
        let err = resp.error.expect("missing toolchain must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("No toolchain"),
            "expected a toolchain error, got: {}",
            err.message
        );
    }

    /// Native-first compile policy: the default `compile` path uses the pure-Rust
    /// native `.app` emitter — no C# bridge, no `dotnet alc`. It must succeed and
    /// produce an `appPath` from project source alone (no bridge available here).
    #[tokio::test]
    async fn compile_uses_native_emitter_without_bridge_or_alc() {
        let ws = empty_ws();
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("app.json"),
            r#"{"id":"aaaaaaaa-1111-2222-3333-444444444444","name":"t","publisher":"p","version":"1.0.0.0","runtime":"14.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src").join("Lib.al"),
            "codeunit 50100 \"T\" { procedure P() begin end; }",
        )
        .unwrap();
        {
            let mut g = ws.project.write().await;
            *g = Some(make_project(tmp.path()));
        }
        let tc_dir = tempfile::TempDir::new().unwrap();
        write_fixture_toolchain(tc_dir.path());
        let tc = {
            let _lock = AL_TOOL_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("AL_TOOL_PATH");
            // SAFETY: serialized via env_lock; restored immediately after.
            unsafe { std::env::set_var("AL_TOOL_PATH", tc_dir.path()) };
            let tc = crate::toolchain::find_toolchain()
                .expect("fixture AL_TOOL_PATH toolchain must be discovered");
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                    None => std::env::remove_var("AL_TOOL_PATH"),
                }
            }
            tc
        };
        {
            let mut g = ws.toolchain.write().await;
            *g = Some(tc);
        }
        let resp = dispatch_compile(&ws, 3).await;
        assert!(
            resp.error.is_none(),
            "native compile should succeed: {:?}",
            resp.error
        );
        let result = resp.result.expect("compile result");
        assert_eq!(
            result["success"], true,
            "native emit should succeed: {result}"
        );
        let app_path = result["appPath"]
            .as_str()
            .expect("native emitter must produce an appPath");
        assert!(
            std::path::Path::new(app_path).is_file(),
            "the native `.app` must exist on disk at {app_path}"
        );
    }

    #[tokio::test]
    async fn package_without_toolchain_reports_setup_hint() {
        let ws = empty_ws();
        let resp = dispatch_package(&ws, 1).await;
        let err = resp.error.expect("missing toolchain must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("No toolchain available"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn package_with_toolchain_but_no_project_reports_no_project() {
        // Toolchain present (constructed via the AL_TOOL_PATH fixture) but no
        // project loaded → "No project loaded", proving the project guard runs
        // after the toolchain guard.
        let ws = empty_ws();
        let tc_dir = tempfile::TempDir::new().unwrap();
        write_fixture_toolchain(tc_dir.path());
        let tc = {
            let _lock = AL_TOOL_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("AL_TOOL_PATH");
            // SAFETY: serialized via env_lock; restored immediately after.
            unsafe { std::env::set_var("AL_TOOL_PATH", tc_dir.path()) };
            let tc = crate::toolchain::find_toolchain()
                .expect("fixture AL_TOOL_PATH toolchain must be discovered");
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                    None => std::env::remove_var("AL_TOOL_PATH"),
                }
            }
            tc
        };
        {
            let mut g = ws.toolchain.write().await;
            *g = Some(tc);
        }
        // project stays None.
        let resp = dispatch_package(&ws, 2).await;
        let err = resp.error.expect("missing project must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert_eq!(err.message, ERR_NO_PROJECT);
    }

    #[tokio::test]
    async fn package_missing_app_json_propagates_build_error() {
        // Toolchain + project both present, but the project root has no
        // app.json. `compile_project_with_analyzers` rejects this BEFORE
        // spawning the compiler, and the dispatcher must propagate that error
        // verbatim as an INTERNAL_ERROR (exercising the real build wiring +
        // empty analyzer-filter branch, with no subprocess).
        let ws = empty_ws();
        let tc_dir = tempfile::TempDir::new().unwrap();
        write_fixture_toolchain(tc_dir.path());
        let tc = {
            let _lock = AL_TOOL_PATH_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var_os("AL_TOOL_PATH");
            // SAFETY: serialized via env_lock; restored immediately after.
            unsafe { std::env::set_var("AL_TOOL_PATH", tc_dir.path()) };
            let tc = crate::toolchain::find_toolchain().expect("fixture toolchain");
            unsafe {
                match prev {
                    Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                    None => std::env::remove_var("AL_TOOL_PATH"),
                }
            }
            tc
        };
        let proj = tempfile::TempDir::new().unwrap(); // intentionally no app.json
        {
            let mut g = ws.toolchain.write().await;
            *g = Some(tc);
        }
        {
            let mut g = ws.project.write().await;
            *g = Some(make_project(proj.path()));
        }
        let resp = dispatch_package(&ws, 3).await;
        let err = resp.error.expect("missing app.json must error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("No app.json"),
            "build error must propagate verbatim, got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn snapshot_start_posts_and_parses_id() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/dev/snapshot"))
            .and(query_param("company", "CRONUS"))
            .and(body_json(serde_json::json!({ "description": "dbg" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "id": "snap-77" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let resp = dispatch_snapshot(
            1,
            &serde_json::json!({
                "cmd": "start",
                "serverUrl": server.uri(),
                "company": "CRONUS",
                "description": "dbg",
            }),
        )
        .await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["cmd"], "start");
        assert_eq!(r["snapshotId"], "snap-77");
        assert_eq!(r["status"], "started");
    }

    #[tokio::test]
    async fn snapshot_start_server_error_maps_to_internal_error() {
        // Negative: a 500 from BC must become an INTERNAL_ERROR whose message
        // names the failed operation — not a silent success.
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/dev/snapshot"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let resp = dispatch_snapshot(
            2,
            &serde_json::json!({
                "cmd": "start",
                "serverUrl": server.uri(),
                "company": "CRONUS",
            }),
        )
        .await;
        let err = resp.error.expect("500 must surface an error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("snapshot start failed"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn snapshot_list_parses_value_envelope() {
        // The OData `{ "value": [...] }` envelope must be parsed into the
        // dispatcher's `snapshots` array with id/description carried through.
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/dev/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "id": "s1", "description": "first" },
                    { "id": "s2", "size": 1024 },
                ]
            })))
            .mount(&server)
            .await;

        let resp = dispatch_snapshot(
            3,
            &serde_json::json!({
                "cmd": "list",
                "serverUrl": server.uri(),
                "company": "CRONUS",
            }),
        )
        .await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["cmd"], "list");
        let snaps = r["snapshots"].as_array().expect("snapshots array");
        assert_eq!(snaps.len(), 2, "both entries must be parsed");
        assert_eq!(snaps[0]["id"], "s1");
        assert_eq!(snaps[0]["description"], "first");
    }

    #[tokio::test]
    async fn snapshot_download_writes_file_and_returns_path() {
        let server = MockServer::start().await;
        Mock::given(wm_method("GET"))
            .and(wm_path("/dev/snapshots/snap-9"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"BINARY".to_vec()))
            .mount(&server)
            .await;

        let out = tempfile::TempDir::new().unwrap();
        let resp = dispatch_snapshot(
            4,
            &serde_json::json!({
                "cmd": "download",
                "snapshotId": "snap-9",
                "serverUrl": server.uri(),
                "company": "CRONUS",
                "outputDir": out.path().to_string_lossy(),
            }),
        )
        .await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["status"], "downloaded");
        let written = r["path"].as_str().expect("path string");
        assert_eq!(
            std::fs::read(written).expect("downloaded file must exist"),
            b"BINARY"
        );
    }

    #[tokio::test]
    async fn profiling_start_posts_and_parses_session_id() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/dev/profiler/start"))
            .and(query_param("company", "CRONUS"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "sessionId": "sess-1" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        let resp = dispatch_profiling(
            1,
            &serde_json::json!({
                "cmd": "start",
                "serverUrl": server.uri(),
                "company": "CRONUS",
            }),
        )
        .await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["cmd"], "start");
        assert_eq!(r["sessionId"], "sess-1");
        assert_eq!(r["status"], "profiling");
    }

    #[tokio::test]
    async fn profiling_start_server_error_maps_to_internal_error() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/dev/profiler/start"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;

        let resp = dispatch_profiling(
            2,
            &serde_json::json!({
                "cmd": "start",
                "serverUrl": server.uri(),
                "company": "CRONUS",
            }),
        )
        .await;
        let err = resp.error.expect("503 must surface an error");
        assert_eq!(err.code, error_codes::INTERNAL_ERROR);
        assert!(
            err.message.contains("profiling start failed"),
            "got: {}",
            err.message
        );
    }

    #[tokio::test]
    async fn profiling_stop_posts_session_and_writes_profile() {
        let server = MockServer::start().await;
        Mock::given(wm_method("POST"))
            .and(wm_path("/dev/profiler/stop"))
            .and(body_json(serde_json::json!({ "sessionId": "sess-42" })))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PROFILE".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let out = tempfile::TempDir::new().unwrap();
        let resp = dispatch_profiling(
            3,
            &serde_json::json!({
                "cmd": "stop",
                "sessionId": "sess-42",
                "serverUrl": server.uri(),
                "company": "CRONUS",
                "outputDir": out.path().to_string_lossy(),
            }),
        )
        .await;
        assert!(resp.error.is_none(), "got error: {:?}", resp.error);
        let r = resp.result.expect("result");
        assert_eq!(r["cmd"], "stop");
        assert_eq!(r["status"], "stopped");
        let written = r["path"].as_str().expect("path string");
        assert_eq!(
            std::fs::read(written).expect("profile file must exist"),
            b"PROFILE"
        );
    }
}
