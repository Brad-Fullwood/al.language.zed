//! Business Central performance-profiling dispatcher. `start`, `status` and `stop`
//! are the snapshot debugger attach, status and finish calls with ExecutionContext
//! 2 (Profiling), implemented in `daemon/snapshot_dispatch.rs` on the shared
//! `bc_debug::snapshot_debugger` client, so there is one token path
//! (`BcDebugConfig`). `analyze` reads a `.alcpuprofile` file.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};

use crate::server::daemon::snapshot_dispatch::{
    answer, profile_start, profile_status, profile_stop,
};
use crate::server::daemon::{optional_bounded_usize_param, rpc_error};

pub(in crate::server::daemon) async fn dispatch_profiling(
    workspace: &al_workspace::Workspace,
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
                    message: "Missing 'cmd' parameter (expected: start, status, stop, analyze)"
                        .to_string(),
                }),
                ..Default::default()
            };
        }
    };

    match cmd {
        "start" => answer(id, profile_start(workspace, params).await),
        "status" => answer(id, profile_status(workspace, params).await),
        "stop" => answer(id, profile_stop(workspace, params).await),

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
            // A relative path would resolve against the daemon process cwd,
            // which is not the project; containment then keeps the read inside
            // the project the daemon was started for.
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
            let profile_path =
                match crate::server::daemon::containment::resolve_param_within_project(
                    workspace,
                    "path",
                    &profile_path,
                ) {
                    Ok(path) => path,
                    Err(rejection) => return rejection.into_response(id),
                };
            let top_n = match optional_bounded_usize_param(params, "topN", 20, 1000) {
                Ok(top_n) => top_n,
                Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
            };

            match al_bc::profiling::analyze_profile_file(&profile_path, top_n).await {
                Ok(result) => match serde_json::to_value(&result.hotspots) {
                    Ok(hotspots) => Response {
                        id,
                        result: Some(serde_json::json!({
                            "cmd": "analyze",
                            "durationMs": result.duration_ms,
                            "hotspots": hotspots,
                            "profilePath": result.profile_path.as_ref().map(|p| p.display().to_string()),
                        })),
                        error: None,
                        ..Default::default()
                    },
                    Err(error) => rpc_error(
                        id,
                        error_codes::INTERNAL_ERROR,
                        &format!("serialize profiling hotspots failed: {error}"),
                    ),
                },
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

#[cfg(test)]
mod tests {
    use super::*;
    use al_protocol::jsonrpc::error_codes;

    /// A workspace with a project root, plus the temporary directory backing
    /// it. `outputDir` and `path` parameters are contained to that root.
    fn project_ws() -> (tempfile::TempDir, al_workspace::Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = al_workspace::Workspace::new();
        crate::server::daemon::set_test_project_root(&workspace, dir.path());
        (dir, workspace)
    }

    #[tokio::test]
    async fn profiling_missing_cmd_is_invalid_params() {
        let (_project, ws) = project_ws();
        let resp = dispatch_profiling(&ws, 1, &serde_json::json!({})).await;
        assert_eq!(resp.error.expect("err").code, error_codes::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn profiling_unknown_cmd_is_invalid_params() {
        let (_project, ws) = project_ws();
        let resp = dispatch_profiling(&ws, 2, &serde_json::json!({ "cmd": "zzz" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Unknown profiling command"));
    }

    #[tokio::test]
    async fn profiling_analyze_missing_path_is_invalid_params() {
        let (_project, ws) = project_ws();
        let resp = dispatch_profiling(&ws, 3, &serde_json::json!({ "cmd": "analyze" })).await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("path"));
    }

    #[tokio::test]
    async fn profiling_analyze_rejects_relative_path() {
        let (_project, ws) = project_ws();
        // Path-traversal guard before any file read.
        let resp = dispatch_profiling(
            &ws,
            4,
            &serde_json::json!({ "cmd": "analyze", "path": "rel/profile.json" }),
        )
        .await;
        let err = resp.error.expect("err");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("absolute"), "got: {}", err.message);
    }

    #[tokio::test]
    async fn profiling_rejects_malformed_optional_params() {
        let (project, ws) = project_ws();
        // Inside the project, so the request gets past the path check to `topN`.
        let profile = project.path().join("profile.json");
        for top_n in [serde_json::json!("many"), serde_json::json!(1001)] {
            let response = dispatch_profiling(
                &ws,
                21,
                &serde_json::json!({
                    "cmd": "analyze",
                    "path": profile.to_str().unwrap(),
                    "topN": top_n
                }),
            )
            .await;
            assert_eq!(
                response.error.expect("malformed topN").code,
                error_codes::INVALID_PARAMS
            );
        }
    }
}
