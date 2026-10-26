//! Business Central performance-profiling dispatcher. It shares `BcServerParams` parsing and
//! the SSRF `serverUrl` guard. Snapshot debugging lives in `daemon/snapshot_dispatch.rs`.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};

use super::bc_server_params::{
    authorize_bc_server, parse_bc_server_params, reject_unsafe_server_url,
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
                    message: "Missing 'cmd' parameter (expected: start, stop, analyze)".to_string(),
                }),
                ..Default::default()
            };
        }
    };

    let mut bc = match parse_bc_server_params(workspace, params, "profiles") {
        Ok(config) => config,
        Err(rejection) => return rejection.into_response(id),
    };
    if let Some(err) = reject_unsafe_server_url(id, &bc.server_url) {
        return err;
    }
    if let Err(message) = authorize_bc_server(workspace, params, &mut bc) {
        return rpc_error(id, error_codes::INVALID_PARAMS, &message);
    }
    let config = al_bc::profiling::ProfilingConfig {
        server_url: bc.server_url,
        company: bc.company,
        output_dir: bc.output_dir,
        username: bc.username,
        password: bc.password,
        accept_invalid_certs: bc.accept_invalid_certs,
    };

    match cmd {
        "start" => match al_bc::profiling::start_profiling(&config).await {
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
            let session_id = match params
                .get("sessionId")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
            {
                Some(session_id) => session_id.to_string(),
                None => {
                    return Response {
                        id,
                        result: None,
                        error: Some(RpcError {
                            code: error_codes::INVALID_PARAMS,
                            message: "Missing 'sessionId' parameter for stop command".to_string(),
                        }),
                        ..Default::default()
                    };
                }
            };

            match al_bc::profiling::stop_profiling(&config, &session_id).await {
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

    /// A project that names `server_uri` in its launch file and is recorded as
    /// trusted, which is what an inline `serverUrl` now needs.
    ///
    /// Returned as a guard because it points `XDG_CONFIG_HOME` at a scratch
    /// directory for the life of the test. Every test that uses it is
    /// `#[serial_test::serial]`, because that variable is process-wide.
    struct TrustingProject {
        project: tempfile::TempDir,
        _config: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    impl Drop for TrustingProject {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    fn trusting_project_ws(server_uri: &str) -> (TrustingProject, al_workspace::Workspace) {
        let config = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", config.path());

        let project = tempfile::tempdir().unwrap();
        let url = url::Url::parse(server_uri).expect("mock server uri");
        std::fs::create_dir_all(project.path().join(".vscode")).unwrap();
        std::fs::write(
            project.path().join(".vscode/launch.json"),
            serde_json::to_string(&serde_json::json!({
                "version": "0.2.0",
                "configurations": [{
                    "type": "al",
                    "request": "launch",
                    "name": "mock",
                    "environmentType": "OnPrem",
                    "server": format!("{}://{}", url.scheme(), url.host_str().unwrap()),
                    "port": url.port().unwrap_or(80),
                    "serverInstance": "BC",
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(project.path().join("app.json"), "{}").unwrap();

        let decision = al_project::trust::decide(project.path()).expect("decide");
        al_project::trust::trust_project(&decision.root, &decision.digest).expect("trust");

        let workspace = al_workspace::Workspace::new();
        crate::server::daemon::set_test_project_root(&workspace, project.path());
        (
            TrustingProject {
                project,
                _config: config,
                previous,
            },
            workspace,
        )
    }

    impl TrustingProject {
        fn path(&self) -> &std::path::Path {
            self.project.path()
        }
    }

    #[tokio::test]
    async fn dispatch_profiling_rejects_non_http_serverurl() {
        let (_project, ws) = project_ws();
        let params = serde_json::json!({ "cmd": "start", "serverUrl": "gopher://internal" });
        let resp = dispatch_profiling(&ws, 7, &params).await;
        let err = resp.error.expect("must error");
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
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

    // =======================================================================
    // Mock-harness coverage for the live-infra dispatchers.
    //
    // These exercise the REAL code paths of `dispatch_snapshot` and
    // `dispatch_profiling` without a live BC server:
    //   * BC HTTP is mocked with wiremock — the dispatcher's `serverUrl`
    //     param points at the mock, so the request shape it builds and the
    //     response it parses are asserted end-to-end.
    // No production behaviour is changed by any of this.
    // =======================================================================

    use wiremock::matchers::{body_json, method as wm_method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[serial_test::serial]
    #[tokio::test]
    async fn profiling_start_posts_and_parses_session_id() {
        let server = MockServer::start().await;
        let (_project, ws) = trusting_project_ws(&server.uri());
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
            &ws,
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

    #[serial_test::serial]
    #[tokio::test]
    async fn profiling_start_server_error_maps_to_internal_error() {
        let server = MockServer::start().await;
        let (_project, ws) = trusting_project_ws(&server.uri());
        Mock::given(wm_method("POST"))
            .and(wm_path("/dev/profiler/start"))
            .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
            .mount(&server)
            .await;

        let resp = dispatch_profiling(
            &ws,
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

    #[serial_test::serial]
    #[tokio::test]
    async fn profiling_stop_posts_session_and_writes_profile() {
        let server = MockServer::start().await;
        let (project, ws) = trusting_project_ws(&server.uri());
        Mock::given(wm_method("POST"))
            .and(wm_path("/dev/profiler/stop"))
            .and(body_json(serde_json::json!({ "sessionId": "sess-42" })))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PROFILE".to_vec()))
            .expect(1)
            .mount(&server)
            .await;

        let out = project.path().join("profiles");
        let resp = dispatch_profiling(
            &ws,
            3,
            &serde_json::json!({
                "cmd": "stop",
                "sessionId": "sess-42",
                "serverUrl": server.uri(),
                "company": "CRONUS",
                "outputDir": out.to_string_lossy(),
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
