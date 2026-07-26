//! Business Central snapshot-debugging and performance-profiling dispatchers.
//! Both share `BcServerParams` parsing and the SSRF `serverUrl` guard.

use al_protocol::jsonrpc::{error_codes, Response, RpcError};

use super::bc_server_params::{parse_bc_server_params, reject_unsafe_server_url};
use crate::server::daemon::{optional_bounded_usize_param, rpc_error};

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

    let bc = match parse_bc_server_params(params, "snapshots") {
        Ok(config) => config,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if let Some(err) = reject_unsafe_server_url(id, &bc.server_url) {
        return err;
    }
    let config = al_bc::snapshot::SnapshotConfig {
        server_url: bc.server_url,
        company: bc.company,
        output_dir: bc.output_dir,
        username: bc.username,
        password: bc.password,
        accept_invalid_certs: bc.accept_invalid_certs,
    };

    match cmd {
        "start" => {
            let description = match params.get("description") {
                None => None,
                Some(value) => match value.as_str() {
                    Some(description) => Some(description),
                    None => {
                        return rpc_error(
                            id,
                            error_codes::INVALID_PARAMS,
                            "'description' must be a string when supplied",
                        );
                    }
                },
            };
            match al_bc::snapshot::start_snapshot(&config, description).await {
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

        "list" => match al_bc::snapshot::list_snapshots(&config).await {
            Ok(snapshots) => match serde_json::to_value(&snapshots) {
                Ok(items) => Response {
                    id,
                    result: Some(serde_json::json!({
                        "cmd": "list",
                        "snapshots": items,
                    })),
                    error: None,
                    ..Default::default()
                },
                Err(error) => rpc_error(
                    id,
                    error_codes::INTERNAL_ERROR,
                    &format!("serialize snapshot list failed: {error}"),
                ),
            },
            Err(e) => Response {
                id,
                result: None,
                error: Some(RpcError {
                    code: error_codes::INTERNAL_ERROR,
                    message: format!("snapshot list failed: {e}"),
                }),
                ..Default::default()
            },
        },

        "download" => {
            let snapshot_id = match params
                .get("snapshotId")
                .and_then(|value| value.as_str())
                .filter(|value| !value.trim().is_empty())
            {
                Some(snapshot_id) => snapshot_id.to_string(),
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

            match al_bc::snapshot::download_snapshot(&config, &snapshot_id).await {
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

    let bc = match parse_bc_server_params(params, "profiles") {
        Ok(config) => config,
        Err(message) => return rpc_error(id, error_codes::INVALID_PARAMS, &message),
    };
    if let Some(err) = reject_unsafe_server_url(id, &bc.server_url) {
        return err;
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

    #[tokio::test]
    async fn snapshot_and_profiling_reject_malformed_optional_params() {
        for params in [
            serde_json::json!({ "cmd": "list", "serverUrl": 7049 }),
            serde_json::json!({ "cmd": "list", "acceptInvalidCerts": "yes" }),
            serde_json::json!({ "cmd": "list", "outputDir": "relative" }),
            serde_json::json!({ "cmd": "start", "description": 7 }),
        ] {
            assert_eq!(
                dispatch_snapshot(20, &params)
                    .await
                    .error
                    .expect("malformed snapshot params")
                    .code,
                error_codes::INVALID_PARAMS
            );
        }

        for top_n in [serde_json::json!("many"), serde_json::json!(1001)] {
            let response = dispatch_profiling(
                21,
                &serde_json::json!({
                    "cmd": "analyze",
                    "path": "/nonexistent/profile.json",
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
