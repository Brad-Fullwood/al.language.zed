//! Snapshot debugging — start, list, and download BC snapshot sessions.
//!
//! BC's dev endpoint exposes snapshot APIs at:
//!   POST /dev/snapshot             — start a snapshot session
//!   GET  /dev/snapshots            — list available snapshots
//!   GET  /dev/snapshots/{id}       — download a single .alvsc snapshot file
//!
//! Snapshots are stored locally in the output directory for offline analysis.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Configuration for a snapshot debugging session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotConfig {
    /// BC server base URL, e.g. `http://localhost:7049/BC`.
    pub server_url: String,
    /// BC company name (URL-encoded on use).
    pub company: String,
    /// Output directory for downloaded .alvsc files. Must be an absolute path.
    pub output_dir: PathBuf,
    /// Optional username for Basic auth (Windows auth used when absent).
    pub username: Option<String>,
    /// Optional password for Basic auth. Never serialized to prevent credential leaks.
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
    /// Accept invalid/self-signed TLS certificates. Defaults to `false`.
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

/// Metadata about a snapshot available on the BC server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotInfo {
    /// Server-assigned snapshot ID.
    pub id: String,
    /// Human-readable description supplied at snapshot start, if any.
    pub description: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: Option<String>,
    /// Snapshot file size in bytes, if known.
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Server returned {status}: {message}")]
    ServerError { status: u16, message: String },
    #[error("I/O error writing snapshot: {0}")]
    Io(#[from] std::io::Error),
    #[error("Missing snapshot ID in server response")]
    MissingId,
    #[error("No snapshots available on server")]
    NoSnapshots,
    #[error("output_dir must be an absolute path, got: {path}")]
    RelativeOutputDir { path: String },
}

fn make_client(config: &SnapshotConfig) -> Result<reqwest::Client, SnapshotError> {
    Ok(crate::http_auth::build_http_client(
        config.accept_invalid_certs,
        120,
    )?)
}

/// Wrap a body-level failure from `bc_client`'s capped readers
/// (`read_json_body_capped`/`read_binary_body_capped`), preserving the real
/// HTTP status those readers already carry instead of discarding it as `0`.
/// `bc_client::BcClientError` deliberately keeps the response status through
/// a parse failure (see `read_json_body_capped_preserves_status_on_parse_failure`)
/// — re-wrapping it here as `status: 0` threw that away and reported "Server
/// returned 0" for what was actually e.g. a 503.
fn server_error_from_body_failure(error: crate::bc_client::BcClientError) -> SnapshotError {
    let status = match &error {
        crate::bc_client::BcClientError::ServerError { status, .. }
        | crate::bc_client::BcClientError::AuthenticationFailed { status, .. } => *status,
        _ => 0,
    };
    SnapshotError::ServerError {
        status,
        message: error.to_string(),
    }
}

/// Initiate a snapshot debugging session on the BC server.
///
/// Returns the server-assigned snapshot ID on success.
pub async fn start_snapshot(
    config: &SnapshotConfig,
    description: Option<&str>,
) -> Result<String, SnapshotError> {
    let client = make_client(config)?;

    let url = format!(
        "{}/dev/snapshot?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(&config.company),
    );

    let body = description
        .map(|d| serde_json::json!({ "description": d }))
        .unwrap_or_else(|| serde_json::json!({}));

    debug!(url = %url, "snapshot: starting snapshot session");

    let req = crate::http_auth::apply_basic_auth(
        client.post(&url).json(&body),
        &config.username,
        &config.password,
    );
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = crate::bc_client::read_error_body_capped(resp).await;
        warn!(status = status.as_u16(), %message, "snapshot: start failed");
        return Err(SnapshotError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    // Content-Length-capped read. Same defence-in-depth as
    // the NuGet metadata cap: refuse server responses without a length
    // header or exceeding 16 MB.
    let json: serde_json::Value = crate::bc_client::read_json_body_capped(resp)
        .await
        .map_err(server_error_from_body_failure)?;
    let id = json
        .get("id")
        .or_else(|| json.get("snapshotId"))
        .and_then(|v| v.as_str())
        .ok_or(SnapshotError::MissingId)?
        .to_string();

    info!(id = %id, "snapshot: session started");
    Ok(id)
}

pub async fn list_snapshots(config: &SnapshotConfig) -> Result<Vec<SnapshotInfo>, SnapshotError> {
    let client = make_client(config)?;

    let url = format!(
        "{}/dev/snapshots?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(&config.company),
    );

    debug!(url = %url, "snapshot: listing snapshots");

    let req =
        crate::http_auth::apply_basic_auth(client.get(&url), &config.username, &config.password);
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = crate::bc_client::read_error_body_capped(resp).await;
        return Err(SnapshotError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    // Content-Length-capped read.
    let json: serde_json::Value = crate::bc_client::read_json_body_capped(resp)
        .await
        .map_err(server_error_from_body_failure)?;

    // BC may return either an array or { "value": [...] } (OData envelope).
    let entries = if let Some(arr) = json.as_array() {
        arr.clone()
    } else if let Some(arr) = json.get("value").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        return Err(SnapshotError::ServerError {
            status: status.as_u16(),
            message: "snapshot list response must be an array or an OData `value` array"
                .to_string(),
        });
    };

    let snapshots = entries
        .iter()
        .filter_map(|e| {
            // Skip entries that have no usable ID — downloading them would be nonsensical.
            let id = e
                .get("id")
                .or_else(|| e.get("snapshotId"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?
                .to_string();

            Some(SnapshotInfo {
                id,
                description: e
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(String::from),
                created_at: e
                    .get("createdAt")
                    .or_else(|| e.get("timestamp"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                size_bytes: e
                    .get("sizeBytes")
                    .or_else(|| e.get("size"))
                    .and_then(|v| v.as_u64()),
            })
        })
        .collect();

    Ok(snapshots)
}

/// Download a specific snapshot by ID and save it as a `.alvsc` file.
///
/// Returns the path to the downloaded file.
pub async fn download_snapshot(
    config: &SnapshotConfig,
    snapshot_id: &str,
) -> Result<PathBuf, SnapshotError> {
    let client = make_client(config)?;

    let url = format!(
        "{}/dev/snapshots/{}?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(snapshot_id),
        urlencoding::encode(&config.company),
    );

    debug!(url = %url, id = snapshot_id, "snapshot: downloading");

    let req =
        crate::http_auth::apply_basic_auth(client.get(&url), &config.username, &config.password);
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = crate::bc_client::read_error_body_capped(resp).await;
        return Err(SnapshotError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    if !config.output_dir.is_absolute() {
        return Err(SnapshotError::RelativeOutputDir {
            path: config.output_dir.display().to_string(),
        });
    }

    tokio::fs::create_dir_all(&config.output_dir).await?;

    let safe_id: String = snapshot_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let file_name = format!("{safe_id}.alvsc");
    let dest = config.output_dir.join(&file_name);

    // A misbehaving server could otherwise stream gigabytes through
    // `bytes()` straight into
    // the daemon's memory; the helper enforces a 500 MB cap pre- and post-read.
    let bytes = crate::bc_client::read_binary_body_capped(resp)
        .await
        .map_err(server_error_from_body_failure)?;
    tokio::fs::write(&dest, &bytes).await?;

    info!(
        path = %dest.display(),
        bytes = bytes.len(),
        "snapshot: downloaded"
    );

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SnapshotConfig {
        SnapshotConfig {
            server_url: "http://localhost:7049/BC".to_string(),
            company: "CRONUS International Ltd.".to_string(),
            output_dir: std::env::temp_dir().join("al-snapshots-test"),
            username: Some("admin".to_string()),
            password: Some("password".to_string()),
            accept_invalid_certs: false,
        }
    }

    #[test]
    fn config_roundtrip() {
        let config = test_config();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: SnapshotConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.server_url, config.server_url);
        assert_eq!(parsed.company, config.company);
        assert_eq!(parsed.username, config.username);
    }

    #[test]
    fn snapshot_info_roundtrip() {
        let info = SnapshotInfo {
            id: "snap-001".to_string(),
            description: Some("My snapshot".to_string()),
            created_at: Some("2024-01-01T12:00:00Z".to_string()),
            size_bytes: Some(4096),
        };
        let json = serde_json::to_string(&info).unwrap();
        let parsed: SnapshotInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "snap-001");
        assert_eq!(parsed.description.as_deref(), Some("My snapshot"));
        assert_eq!(parsed.size_bytes, Some(4096));
    }

    #[test]
    fn list_snapshots_odata_envelope() {
        // This is a pure JSON-parsing test, not a live HTTP call.
        let json = serde_json::json!({
            "value": [
                { "id": "s1", "description": "first" },
                { "id": "s2", "description": "second", "sizeBytes": 1024 },
            ]
        });

        let entries = if let Some(arr) = json.get("value").and_then(|v| v.as_array()) {
            arr.clone()
        } else {
            vec![]
        };

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].get("id").and_then(|v| v.as_str()), Some("s1"));
        assert_eq!(
            entries[1].get("sizeBytes").and_then(|v| v.as_u64()),
            Some(1024)
        );
    }

    // These exercise the real request construction, status handling, response
    // parsing, and file-writing logic in `start_snapshot`, `list_snapshots`,
    // and `download_snapshot` against a local mock server. They do not need a
    // live BC instance. wiremock's `set_body_*` helpers set `Content-Length`
    // automatically, which the capped-read helpers require.

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A config pointing at the given mock server URI, writing to a unique temp
    /// dir so parallel tests do not collide.
    fn config_for(server_uri: &str) -> SnapshotConfig {
        let unique = format!(
            "al-snapshots-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        SnapshotConfig {
            server_url: server_uri.to_string(),
            company: "CRONUS International Ltd.".to_string(),
            output_dir: std::env::temp_dir().join(unique),
            username: Some("admin".to_string()),
            password: Some("password".to_string()),
            accept_invalid_certs: false,
        }
    }

    #[tokio::test]
    async fn start_snapshot_returns_id_from_id_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/dev/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "snap-123"
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let id = start_snapshot(&config, Some("my debug session"))
            .await
            .expect("start should succeed");
        assert_eq!(id, "snap-123");
    }

    #[tokio::test]
    async fn start_snapshot_falls_back_to_snapshot_id_field() {
        // BC variants return `snapshotId` instead of `id`. The fallback
        // accessor must pick it up.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "snapshotId": "alt-999"
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let id = start_snapshot(&config, None)
            .await
            .expect("start should succeed");
        assert_eq!(id, "alt-999");
    }

    #[tokio::test]
    async fn start_snapshot_missing_id_errors() {
        // A 200 response that carries no id/snapshotId must yield MissingId,
        // not a silently-empty string.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok"
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = start_snapshot(&config, None)
            .await
            .expect_err("missing id should error");
        assert!(
            matches!(err, SnapshotError::MissingId),
            "expected MissingId, got {err:?}"
        );
    }

    #[tokio::test]
    async fn start_snapshot_server_error_propagates_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden: no dev endpoint"))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = start_snapshot(&config, None)
            .await
            .expect_err("403 should error");
        match err {
            SnapshotError::ServerError { status, message } => {
                assert_eq!(status, 403);
                assert!(
                    message.contains("forbidden"),
                    "message should include server body, got {message:?}"
                );
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_snapshots_parses_bare_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dev/snapshots"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "s1", "description": "first", "createdAt": "2024-01-01T00:00:00Z", "sizeBytes": 10 },
                { "snapshotId": "s2", "timestamp": "2024-02-02T00:00:00Z", "size": 20 },
            ])))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let snaps = list_snapshots(&config).await.expect("list should succeed");
        assert_eq!(snaps.len(), 2);

        assert_eq!(snaps[0].id, "s1");
        assert_eq!(snaps[0].description.as_deref(), Some("first"));
        assert_eq!(snaps[0].created_at.as_deref(), Some("2024-01-01T00:00:00Z"));
        assert_eq!(snaps[0].size_bytes, Some(10));

        // Second entry exercises every fallback accessor: snapshotId/timestamp/size.
        assert_eq!(snaps[1].id, "s2");
        assert_eq!(snaps[1].description, None);
        assert_eq!(snaps[1].created_at.as_deref(), Some("2024-02-02T00:00:00Z"));
        assert_eq!(snaps[1].size_bytes, Some(20));
    }

    #[tokio::test]
    async fn list_snapshots_skips_entries_without_id() {
        // Entries with no id, empty id, or non-string id are nonsensical to
        // download and must be filtered out.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "id": "keep" },
                    { "id": "" },
                    { "description": "no id at all" },
                    { "id": 12345 },
                ]
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let snaps = list_snapshots(&config).await.expect("list should succeed");
        assert_eq!(snaps.len(), 1, "only the entry with a usable id survives");
        assert_eq!(snaps[0].id, "keep");
    }

    #[tokio::test]
    async fn list_snapshots_unrecognized_shape_returns_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "unexpected": "shape"
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let error = list_snapshots(&config)
            .await
            .expect_err("an unknown response shape must not look like an empty list");
        assert!(error.to_string().contains("must be an array"));
    }

    #[tokio::test]
    async fn list_snapshots_server_error_propagates_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = list_snapshots(&config).await.expect_err("500 should error");
        assert!(
            matches!(err, SnapshotError::ServerError { status: 500, .. }),
            "expected ServerError 500, got {err:?}"
        );
    }

    #[tokio::test]
    async fn download_snapshot_writes_file_with_sanitized_name() {
        let server = MockServer::start().await;
        let payload = b"ALVSC-binary-payload".to_vec();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
            .mount(&server)
            .await;

        let mut config = config_for(&server.uri());
        // Use a dedicated dir so we can assert on the produced filename.
        config.output_dir = std::env::temp_dir().join(format!(
            "al-snap-dl-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&config.output_dir);

        // An id containing path-traversal + illegal filename chars must be
        // sanitized to underscores so it cannot escape output_dir.
        let dest = download_snapshot(&config, "../danger id/42")
            .await
            .expect("download should succeed");

        let file_name = dest.file_name().unwrap().to_string_lossy();
        // Every char that is not alphanumeric / '-' / '_' becomes '_', so the
        // two dots, the slash, and the space all collapse to underscores. The
        // result is a single flat filename that cannot escape output_dir.
        assert_eq!(file_name, "___danger_id_42.alvsc");
        assert!(dest.starts_with(&config.output_dir));

        let written = std::fs::read(&dest).expect("file should exist");
        assert_eq!(written, payload);

        let _ = std::fs::remove_dir_all(&config.output_dir);
    }

    #[tokio::test]
    async fn download_snapshot_rejects_relative_output_dir() {
        // The guard must fire even when the server responded 200 — a relative
        // output_dir is a misconfiguration we refuse to write through.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"data".to_vec()))
            .mount(&server)
            .await;

        let mut config = config_for(&server.uri());
        config.output_dir = PathBuf::from("relative/output");

        let err = download_snapshot(&config, "snap-1")
            .await
            .expect_err("relative output_dir must error");
        match err {
            SnapshotError::RelativeOutputDir { path } => {
                assert_eq!(path, "relative/output");
            }
            other => panic!("expected RelativeOutputDir, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn download_snapshot_server_error_propagates_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = download_snapshot(&config, "missing")
            .await
            .expect_err("404 should error");
        assert!(
            matches!(err, SnapshotError::ServerError { status: 404, .. }),
            "expected ServerError 404, got {err:?}"
        );
    }

    #[tokio::test]
    async fn download_snapshot_body_level_failure_preserves_real_http_status() {
        // A 200 response whose *advertised* Content-Length lies past the
        // binary-body cap fails inside `read_binary_body_capped`, not the
        // earlier `!status.is_success()` check. That body-level failure must
        // keep the real HTTP status (200) rather than reporting "Server
        // returned 0", matching the same fix applied to the JSON-parsing path.
        let oversize = (crate::bc_client::MAX_BC_BINARY_RESPONSE_BYTES + 1).to_string();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Length", oversize.as_str())
                    .set_body_bytes(b"small".to_vec()),
            )
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = download_snapshot(&config, "snap-1").await;
        match err {
            Err(SnapshotError::ServerError { status, message }) => {
                assert_eq!(
                    status, 200,
                    "the real HTTP status (200) must survive a body-level cap failure"
                );
                assert!(message.contains("exceeds"), "got: {message}");
            }
            other => {
                // A transport-level abort on the length mismatch is also
                // acceptable — the oversize check fired one layer below —
                // but a bare success is not.
                assert!(
                    other.is_err(),
                    "an oversize body must not be downloaded successfully"
                );
            }
        }
    }

    #[tokio::test]
    async fn start_snapshot_malformed_json_preserves_real_http_status() {
        // A 200 whose body is not valid JSON must not panic or be silently
        // swallowed, AND must not discard the real HTTP status the server
        // returned (previously this path re-wrapped the error as `status: 0`,
        // reporting "Server returned 0" instead of the actual status). This
        // exercises the `server_error_from_body_failure` mapping on the
        // `read_json_body_capped` result, which is otherwise never hit by the
        // well-formed mocks.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/dev/snapshot"))
            .respond_with(ResponseTemplate::new(200).set_body_string("this is not json{{{"))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = start_snapshot(&config, Some("desc"))
            .await
            .expect_err("malformed JSON should error");
        match err {
            SnapshotError::ServerError { status, message } => {
                assert_eq!(
                    status, 200,
                    "the real HTTP status (200) must survive the body-level parse failure"
                );
                assert!(
                    message.contains("parse") || message.contains("JSON"),
                    "message should mention the parse failure, got {message:?}"
                );
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_snapshots_malformed_json_preserves_real_http_status() {
        // Same body-level parse-failure path as start_snapshot, but on the
        // list endpoint, and on a distinct 2xx status (201, not 200) this
        // time — proving the preserved status isn't just an accidental match
        // on 200. (A non-2xx status is caught earlier by this function's own
        // `!status.is_success()` branch and never reaches the JSON parser at
        // all, so it can't exercise this particular mapping.)
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/dev/snapshots"))
            .respond_with(ResponseTemplate::new(201).set_body_string("<html>not json</html>"))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = list_snapshots(&config)
            .await
            .expect_err("malformed JSON should error");
        match err {
            SnapshotError::ServerError { status, message } => {
                assert_eq!(
                    status, 201,
                    "the real HTTP status (201) must survive the body-level parse failure"
                );
                assert!(
                    message.contains("parse") || message.contains("JSON"),
                    "message should mention the parse failure, got {message:?}"
                );
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn start_snapshot_network_failure_yields_http_error() {
        // Point at a port nobody is listening on: the `send().await?` must
        // bubble up as the `Http` (reqwest) error variant, not a ServerError.
        // This is the transport-failure branch — distinct from an HTTP error
        // status, which would be a ServerError.
        // 127.0.0.1:1 is the canonical "connection refused" target.
        let config = config_for("http://127.0.0.1:1");

        let err = start_snapshot(&config, None)
            .await
            .expect_err("connection to a dead port must fail");
        assert!(
            matches!(err, SnapshotError::Http(_)),
            "expected Http transport error, got {err:?}"
        );
    }

    #[tokio::test]
    async fn download_snapshot_unsanitized_id_writes_verbatim_filename() {
        // An id consisting only of allowed chars (alphanumeric, '-', '_') must
        // pass through the sanitizer unchanged and produce "<id>.alvsc". This
        // covers the identity branch of the per-char map and the successful
        // file-write + info! logging tail of download_snapshot.
        let server = MockServer::start().await;
        let payload = b"clean-payload-bytes".to_vec();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
            .mount(&server)
            .await;

        let mut config = config_for(&server.uri());
        config.output_dir = std::env::temp_dir().join(format!(
            "al-snap-clean-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&config.output_dir);

        let dest = download_snapshot(&config, "Session-2024_01")
            .await
            .expect("download should succeed");

        assert_eq!(
            dest.file_name().unwrap().to_string_lossy(),
            "Session-2024_01.alvsc",
            "clean id must be preserved verbatim"
        );
        assert!(dest.starts_with(&config.output_dir));
        assert_eq!(std::fs::read(&dest).unwrap(), payload);

        let _ = std::fs::remove_dir_all(&config.output_dir);
    }
}
