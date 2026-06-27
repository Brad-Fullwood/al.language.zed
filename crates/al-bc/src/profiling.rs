//! CPU profiling — start, stop, and analyze AL profiling sessions on a BC server.
//!
//! BC's dev endpoint exposes profiling APIs at:
//!   POST /dev/profiler/start       — begin CPU profiling
//!   POST /dev/profiler/stop        — end profiling and retrieve data
//!
//! The `.alcpuprofile` data is a JSON-encoded call tree compatible with Chrome's
//! CPU profiling format. `analyze_profile` parses it and identifies hotspot
//! AL procedures by accumulated self-time.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfilingConfig {
    /// BC server base URL, e.g. `http://localhost:7049/BC`.
    pub server_url: String,
    /// BC company name (URL-encoded on use).
    pub company: String,
    /// Output directory for downloaded `.alcpuprofile` files. Must be an absolute path.
    pub output_dir: PathBuf,
    pub username: Option<String>,
    /// Optional password for Basic auth. Never serialized to prevent credential leaks.
    #[serde(default, skip_serializing)]
    pub password: Option<String>,
    /// Accept invalid/self-signed TLS certificates. Defaults to `false`.
    #[serde(default)]
    pub accept_invalid_certs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotspot {
    /// Procedure name (e.g., `"Customer.OnAfterGetRecord"`).
    pub procedure: String,
    pub object: Option<String>,
    /// Self time in milliseconds (time spent in this node, excluding callees).
    pub self_time_ms: f64,
    /// Total time in milliseconds (self + callees).
    pub total_time_ms: f64,
    pub hit_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilingResult {
    pub session_id: Option<String>,
    /// Total recording duration in milliseconds.
    pub duration_ms: f64,
    /// Top hotspots, sorted by self_time_ms descending.
    pub hotspots: Vec<Hotspot>,
    pub profile_path: Option<PathBuf>,
}

#[derive(Debug, Error)]
pub enum ProfilingError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Server returned {status}: {message}")]
    ServerError { status: u16, message: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse profile data: {0}")]
    ParseError(String),
    #[error("No profiling session active")]
    NoActiveSession,
    #[error("output_dir must be an absolute path, got: {path}")]
    RelativeOutputDir { path: String },
}

fn make_client(config: &ProfilingConfig) -> Result<reqwest::Client, ProfilingError> {
    Ok(crate::http_auth::build_http_client(
        config.accept_invalid_certs,
        300,
    )?)
}

pub async fn start_profiling(config: &ProfilingConfig) -> Result<String, ProfilingError> {
    let client = make_client(config)?;

    let url = format!(
        "{}/dev/profiler/start?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(&config.company),
    );

    debug!(url = %url, "profiling: starting CPU profiler");

    let req = crate::http_auth::apply_basic_auth(
        client.post(&url).json(&serde_json::json!({})),
        &config.username,
        &config.password,
    );
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = crate::bc_client::read_error_body_capped(resp).await;
        warn!(status = status.as_u16(), %message, "profiling: start failed");
        return Err(ProfilingError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    // Content-Length-capped read (F-OPEN-044). BC dev API responses for
    // session start are tiny (a few hundred bytes); 16 MB is a generous
    // defence-in-depth bound. Wrap the cross-crate BcClientError into our
    // local error variant so the caller doesn't see a foreign type.
    let json: serde_json::Value = crate::bc_client::read_json_body_capped(resp)
        .await
        .map_err(|e| ProfilingError::ParseError(format!("Failed to parse start response: {e}")))?;
    let session_id = json
        .get("id")
        .or_else(|| json.get("sessionId"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(ProfilingError::NoActiveSession)?
        .to_string();

    info!(session_id = %session_id, "profiling: session started");
    Ok(session_id)
}

pub async fn stop_profiling(
    config: &ProfilingConfig,
    session_id: &str,
) -> Result<PathBuf, ProfilingError> {
    // Validate that output_dir is an absolute path before doing any work
    // (F-OPEN path traversal guard; mirrors snapshot.rs). A relative output_dir
    // would be resolved against the long-lived daemon's cwd, allowing the
    // downloaded profile to escape to an arbitrary location. Fail fast, before
    // the network round-trip.
    if !config.output_dir.is_absolute() {
        return Err(ProfilingError::RelativeOutputDir {
            path: config.output_dir.display().to_string(),
        });
    }

    let client = make_client(config)?;

    let url = format!(
        "{}/dev/profiler/stop?company={}",
        config.server_url.trim_end_matches('/'),
        urlencoding::encode(&config.company),
    );

    debug!(url = %url, session_id = session_id, "profiling: stopping profiler");

    let body = serde_json::json!({ "sessionId": session_id });
    let req = crate::http_auth::apply_basic_auth(
        client.post(&url).json(&body),
        &config.username,
        &config.password,
    );
    let resp = req.send().await?;
    let status = resp.status();

    if !status.is_success() {
        let message = crate::bc_client::read_error_body_capped(resp).await;
        warn!(status = status.as_u16(), %message, "profiling: stop failed");
        return Err(ProfilingError::ServerError {
            status: status.as_u16(),
            message,
        });
    }

    tokio::fs::create_dir_all(&config.output_dir).await?;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let file_name = format!("profile-{timestamp}.alcpuprofile");
    let dest = config.output_dir.join(&file_name);

    // Content-Length-capped binary read (F-OPEN-044 follow-up). A misbehaving
    // server could otherwise stream gigabytes through `bytes()` straight into
    // the daemon's memory; the helper enforces a 500 MB cap pre- and post-read.
    let bytes = crate::bc_client::read_binary_body_capped(resp)
        .await
        .map_err(|e| ProfilingError::ParseError(format!("Failed to read profile data: {e}")))?;
    tokio::fs::write(&dest, &bytes).await?;

    info!(
        path = %dest.display(),
        bytes = bytes.len(),
        "profiling: profile saved"
    );

    Ok(dest)
}

/// Aggregate per-node self time (in **microseconds**) from a profile's
/// `samples` + `timeDeltas` arrays.
///
/// Chrome / V8 CPU-profile association convention used here: the `i`-th recorded
/// sample (`samples[i]`, a node id) is charged the `i`-th time delta
/// (`timeDeltas[i]`, microseconds). We sum those deltas per sampled node id, so
/// a node's self time reflects how long it was actually on-CPU — not merely how
/// many times it was sampled. The two arrays are expected to be equal length; if
/// they differ (a malformed profile) we defensively iterate over the common
/// prefix (`min(len)`) so we can't panic.
///
/// Returns an empty map when either array is absent or empty, in which case the
/// caller falls back to the legacy "1 ms per hit" estimate.
fn aggregate_self_time_us(json: &serde_json::Value) -> std::collections::HashMap<u64, f64> {
    let mut by_node: std::collections::HashMap<u64, f64> = std::collections::HashMap::new();

    let (samples, deltas) = match (
        json.get("samples").and_then(|v| v.as_array()),
        json.get("timeDeltas").and_then(|v| v.as_array()),
    ) {
        (Some(s), Some(d)) if !s.is_empty() && !d.is_empty() => (s, d),
        _ => return by_node,
    };

    if samples.len() != deltas.len() {
        warn!(
            samples = samples.len(),
            time_deltas = deltas.len(),
            "profiling: samples/timeDeltas length mismatch; aggregating over common prefix"
        );
    }

    let n = samples.len().min(deltas.len());
    for i in 0..n {
        let Some(node_id) = samples[i].as_u64() else {
            continue;
        };
        // timeDeltas are integer microseconds in practice; read as f64 defensively.
        let delta_us = deltas[i].as_f64().unwrap_or(0.0);
        *by_node.entry(node_id).or_insert(0.0) += delta_us;
    }

    by_node
}

/// Parse a `.alcpuprofile` file and return the top hotspots.
///
/// `.alcpuprofile` is a Chrome-style CPU profile JSON:
/// ```json
/// { "nodes": [...], "startTime": ..., "endTime": ..., "samples": [...], "timeDeltas": [...] }
/// ```
/// Each node has `{ "id": N, "callFrame": { "functionName": ..., "url": ... }, "hitCount": N, "children": [...] }`.
///
/// Self time is computed by aggregating `timeDeltas` per node (see
/// [`aggregate_self_time_us`]); profiles that omit `samples`/`timeDeltas` fall
/// back to the legacy 1 ms-per-hit approximation. `hit_count` is always retained.
pub fn analyze_profile(
    profile_data: &[u8],
    top_n: usize,
) -> Result<ProfilingResult, ProfilingError> {
    let json: serde_json::Value = serde_json::from_slice(profile_data)
        .map_err(|e| ProfilingError::ParseError(format!("JSON parse error: {e}")))?;

    let start_time = json
        .get("startTime")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let end_time = json.get("endTime").and_then(|v| v.as_f64()).unwrap_or(0.0);

    // Duration in ms (Chrome profile times are in microseconds)
    let duration_ms = if end_time > start_time {
        (end_time - start_time) / 1000.0
    } else {
        0.0
    };

    let nodes = json
        .get("nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Accurate self time: sum the timeDeltas of each node's samples (µs).
    // Empty when the profile carries no samples/timeDeltas, in which case we
    // fall back to the old hit-count estimate below.
    let self_time_by_node = aggregate_self_time_us(&json);
    let have_time = !self_time_by_node.is_empty();

    let mut hotspots: Vec<Hotspot> = nodes
        .iter()
        .filter_map(|node| {
            let hit_count = node.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
            let node_id = node.get("id").and_then(|v| v.as_u64());
            let call_frame = node.get("callFrame")?;
            let function_name = call_frame
                .get("functionName")
                .and_then(|v| v.as_str())
                .unwrap_or("(anonymous)")
                .to_string();

            // Filter out browser/engine nodes with empty names
            if function_name.is_empty() || function_name == "(root)" || function_name == "(idle)" {
                return None;
            }

            // Extract object name from url/scriptId field (BC profiles may embed this)
            let object = call_frame
                .get("url")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .or_else(|| {
                    node.get("object")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                });

            // Self time: aggregate this node's sampled timeDeltas (µs -> ms).
            // Falls back to the legacy "1 ms per hit" estimate only when the
            // profile has no samples/timeDeltas to aggregate.
            let self_time_ms = if have_time {
                node_id
                    .and_then(|id| self_time_by_node.get(&id).copied())
                    .unwrap_or(0.0)
                    / 1000.0
            } else {
                hit_count as f64
            };
            // Total time requires tree traversal; approximate as self_time for top-level analysis
            let total_time_ms = self_time_ms;

            Some(Hotspot {
                procedure: function_name,
                object,
                self_time_ms,
                total_time_ms,
                hit_count,
            })
        })
        .collect();

    hotspots.sort_by(|a, b| {
        b.self_time_ms
            .partial_cmp(&a.self_time_ms)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    hotspots.truncate(top_n);

    Ok(ProfilingResult {
        session_id: None,
        duration_ms,
        hotspots,
        profile_path: None,
    })
}

pub async fn analyze_profile_file(
    path: &std::path::Path,
    top_n: usize,
) -> Result<ProfilingResult, ProfilingError> {
    let data = tokio::fs::read(path).await?;
    let mut result = analyze_profile(&data, top_n)?;
    result.profile_path = Some(path.to_path_buf());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ProfilingConfig {
        ProfilingConfig {
            server_url: "http://localhost:7049/BC".to_string(),
            company: "CRONUS International Ltd.".to_string(),
            output_dir: std::env::temp_dir().join("al-profiling-test"),
            username: Some("admin".to_string()),
            password: Some("password".to_string()),
            accept_invalid_certs: false,
        }
    }

    #[test]
    fn config_roundtrip() {
        let config = test_config();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ProfilingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.server_url, config.server_url);
        assert_eq!(parsed.company, config.company);
    }

    #[test]
    fn analyze_minimal_profile() {
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 5000000,  // 5 seconds in microseconds
            "nodes": [
                {
                    "id": 1,
                    "callFrame": { "functionName": "(root)", "url": "" },
                    "hitCount": 0,
                    "children": [2, 3]
                },
                {
                    "id": 2,
                    "callFrame": { "functionName": "Customer.OnAfterGetRecord", "url": "Customer.al" },
                    "hitCount": 120,
                    "children": []
                },
                {
                    "id": 3,
                    "callFrame": { "functionName": "Sales-Post.PostDocument", "url": "Sales-Post.al" },
                    "hitCount": 45,
                    "children": []
                },
                {
                    "id": 4,
                    "callFrame": { "functionName": "(idle)", "url": "" },
                    "hitCount": 835,
                    "children": []
                }
            ],
            "samples": [2, 2, 3, 2],
            "timeDeltas": [1000, 1000, 1000, 1000]
        });

        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();

        assert!((result.duration_ms - 5000.0).abs() < 1.0);

        assert_eq!(result.hotspots.len(), 2);

        assert_eq!(result.hotspots[0].procedure, "Customer.OnAfterGetRecord");
        assert_eq!(result.hotspots[0].hit_count, 120);
        assert_eq!(result.hotspots[0].object.as_deref(), Some("Customer.al"));

        assert_eq!(result.hotspots[1].procedure, "Sales-Post.PostDocument");
        assert_eq!(result.hotspots[1].hit_count, 45);
    }

    #[test]
    fn time_based_self_time_beats_hit_count_ranking() {
        // B14: the node with FEWER hits but LARGER aggregated timeDeltas must
        // rank as the bigger hotspot — proving time-based beats count-based.
        //
        //   ManyHits: hitCount 100, but sampled once for 100µs  -> 0.1 ms
        //   FewHits:  hitCount  10, but sampled once for 5000µs -> 5.0 ms
        //
        // Count-based ranking would put ManyHits first; time-based flips it.
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                { "id": 2, "callFrame": { "functionName": "ManyHits", "url": "a.al" }, "hitCount": 100 },
                { "id": 3, "callFrame": { "functionName": "FewHits",  "url": "b.al" }, "hitCount": 10 }
            ],
            "samples":    [2, 3],
            "timeDeltas": [100, 5000]
        });

        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();

        assert_eq!(result.hotspots.len(), 2);
        // Time-based: FewHits ranks first despite having far fewer hits.
        assert_eq!(result.hotspots[0].procedure, "FewHits");
        assert_eq!(result.hotspots[0].hit_count, 10);
        assert!(
            (result.hotspots[0].self_time_ms - 5.0).abs() < 1e-9,
            "FewHits self_time should be 5.0 ms, got {}",
            result.hotspots[0].self_time_ms
        );
        assert_eq!(result.hotspots[1].procedure, "ManyHits");
        assert_eq!(result.hotspots[1].hit_count, 100);
        assert!(
            (result.hotspots[1].self_time_ms - 0.1).abs() < 1e-9,
            "ManyHits self_time should be 0.1 ms, got {}",
            result.hotspots[1].self_time_ms
        );
    }

    #[test]
    fn mismatched_samples_timedeltas_lengths_handled_gracefully() {
        // B14: a malformed profile whose samples/timeDeltas arrays differ in
        // length must not panic — we aggregate over the common prefix only.
        // samples has 3 entries, timeDeltas has 1: only samples[0] (node 2) is
        // charged, for 1000µs = 1.0 ms; node 3 gets nothing.
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                { "id": 2, "callFrame": { "functionName": "Charged",   "url": "a.al" }, "hitCount": 2 },
                { "id": 3, "callFrame": { "functionName": "Uncharged", "url": "b.al" }, "hitCount": 1 }
            ],
            "samples":    [2, 2, 3],
            "timeDeltas": [1000]
        });

        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();

        // Both nodes are still reported (no panic); Charged leads on time.
        assert_eq!(result.hotspots.len(), 2);
        assert_eq!(result.hotspots[0].procedure, "Charged");
        assert!(
            (result.hotspots[0].self_time_ms - 1.0).abs() < 1e-9,
            "Charged self_time should be 1.0 ms, got {}",
            result.hotspots[0].self_time_ms
        );
        assert_eq!(result.hotspots[1].procedure, "Uncharged");
        assert!(
            result.hotspots[1].self_time_ms.abs() < 1e-9,
            "Uncharged self_time should be 0.0 ms, got {}",
            result.hotspots[1].self_time_ms
        );
    }

    #[test]
    fn analyze_profile_top_n_limit() {
        let nodes: Vec<serde_json::Value> = (1..=20)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "callFrame": { "functionName": format!("Proc{i}"), "url": "test.al" },
                    "hitCount": i as u64 * 10,
                })
            })
            .collect();

        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": nodes,
        });

        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 5).unwrap();

        assert_eq!(result.hotspots.len(), 5);
        assert!(result.hotspots[0].hit_count >= result.hotspots[1].hit_count);
        assert!(result.hotspots[1].hit_count >= result.hotspots[2].hit_count);
    }

    #[test]
    fn analyze_empty_profile() {
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 0,
            "nodes": [],
        });

        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();

        assert_eq!(result.hotspots.len(), 0);
        assert_eq!(result.duration_ms, 0.0);
    }

    #[test]
    fn analyze_profile_invalid_json() {
        let result = analyze_profile(b"not json", 10);
        assert!(matches!(result, Err(ProfilingError::ParseError(_))));
    }

    #[tokio::test]
    async fn relative_output_dir_rejected() {
        // A relative output_dir must be rejected up front, before any network
        // round-trip, to prevent the downloaded profile from escaping to an
        // arbitrary location relative to the daemon's cwd (path-traversal guard).
        let mut cfg = test_config();
        cfg.output_dir = PathBuf::from("relative/path");
        let err = stop_profiling(&cfg, "test-session").await;
        assert!(
            matches!(err, Err(ProfilingError::RelativeOutputDir { .. })),
            "expected RelativeOutputDir, got {err:?}"
        );
    }

    #[test]
    fn analyze_profile_anonymous_function_name() {
        // A node whose callFrame omits functionName falls back to "(anonymous)"
        // (not filtered, because it is neither empty nor "(root)"/"(idle)").
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                {
                    "id": 1,
                    "callFrame": { "url": "Mystery.al" },
                    "hitCount": 7,
                }
            ],
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        assert_eq!(result.hotspots.len(), 1);
        assert_eq!(result.hotspots[0].procedure, "(anonymous)");
        assert_eq!(result.hotspots[0].object.as_deref(), Some("Mystery.al"));
    }

    #[test]
    fn analyze_profile_empty_function_name_filtered() {
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                {
                    "id": 1,
                    "callFrame": { "functionName": "", "url": "x.al" },
                    "hitCount": 99,
                },
                {
                    "id": 2,
                    "callFrame": { "functionName": "Keep.Me", "url": "y.al" },
                    "hitCount": 3,
                }
            ],
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        assert_eq!(result.hotspots.len(), 1);
        assert_eq!(result.hotspots[0].procedure, "Keep.Me");
    }

    #[test]
    fn analyze_profile_object_falls_back_to_node_object_field() {
        // When callFrame.url is empty/absent, the object name is taken from the
        // node's top-level "object" field instead.
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                {
                    "id": 1,
                    "callFrame": { "functionName": "Foo.Bar", "url": "" },
                    "object": "Codeunit 50000",
                    "hitCount": 5,
                }
            ],
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        assert_eq!(result.hotspots.len(), 1);
        assert_eq!(result.hotspots[0].object.as_deref(), Some("Codeunit 50000"));
    }

    #[test]
    fn analyze_profile_no_object_when_url_and_field_absent() {
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                {
                    "id": 1,
                    "callFrame": { "functionName": "Foo.Bar" },
                    "hitCount": 5,
                }
            ],
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        assert_eq!(result.hotspots.len(), 1);
        assert!(result.hotspots[0].object.is_none());
    }

    #[test]
    fn analyze_profile_node_without_callframe_skipped() {
        // A node missing callFrame entirely is dropped (the `?` short-circuit),
        // not counted as a hotspot.
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                { "id": 1, "hitCount": 1000 },
                {
                    "id": 2,
                    "callFrame": { "functionName": "Real.Proc", "url": "r.al" },
                    "hitCount": 4,
                }
            ],
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        assert_eq!(result.hotspots.len(), 1);
        assert_eq!(result.hotspots[0].procedure, "Real.Proc");
    }

    #[test]
    fn analyze_profile_missing_nodes_key_yields_no_hotspots() {
        let profile = serde_json::json!({
            "startTime": 1000,
            "endTime": 3000,
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        assert_eq!(result.hotspots.len(), 0);
        // (3000 - 1000) / 1000 == 2.0 ms
        assert!((result.duration_ms - 2.0).abs() < 1e-9);
    }

    #[test]
    fn analyze_profile_end_before_start_clamps_duration_to_zero() {
        // endTime <= startTime must not produce a negative duration; it clamps
        // to 0.0 (the `else` branch).
        let profile = serde_json::json!({
            "startTime": 5000000,
            "endTime": 1000000,
            "nodes": [],
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        assert_eq!(result.duration_ms, 0.0);
    }

    #[test]
    fn analyze_profile_top_n_zero_returns_empty() {
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 1000000,
            "nodes": [
                {
                    "id": 1,
                    "callFrame": { "functionName": "A.B", "url": "a.al" },
                    "hitCount": 50,
                }
            ],
        });
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 0).unwrap();
        assert_eq!(result.hotspots.len(), 0);
    }

    #[tokio::test]
    async fn analyze_profile_file_reads_and_sets_path() {
        let dir = std::env::temp_dir().join(format!(
            "al-profiling-file-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.alcpuprofile");
        let profile = serde_json::json!({
            "startTime": 0,
            "endTime": 2000000,
            "nodes": [
                {
                    "id": 1,
                    "callFrame": { "functionName": "Disk.Proc", "url": "d.al" },
                    "hitCount": 11,
                }
            ],
        });
        std::fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();

        let result = analyze_profile_file(&path, 10).await.unwrap();
        assert_eq!(result.profile_path.as_deref(), Some(path.as_path()));
        assert_eq!(result.hotspots.len(), 1);
        assert_eq!(result.hotspots[0].procedure, "Disk.Proc");
        assert!((result.duration_ms - 2000.0).abs() < 1e-9);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn analyze_profile_file_missing_file_is_io_error() {
        let path = std::env::temp_dir().join("al-profiling-does-not-exist.alcpuprofile");
        let _ = std::fs::remove_file(&path);
        let err = analyze_profile_file(&path, 10).await;
        assert!(
            matches!(err, Err(ProfilingError::Io(_))),
            "expected Io error, got {err:?}"
        );
    }

    #[test]
    fn profiling_result_roundtrip() {
        let result = ProfilingResult {
            session_id: Some("session-1".to_string()),
            duration_ms: 1234.5,
            hotspots: vec![Hotspot {
                procedure: "Test.Proc".to_string(),
                object: Some("Test".to_string()),
                self_time_ms: 100.0,
                total_time_ms: 150.0,
                hit_count: 100,
            }],
            profile_path: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: ProfilingResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id.as_deref(), Some("session-1"));
        assert_eq!(parsed.hotspots.len(), 1);
        assert_eq!(parsed.hotspots[0].procedure, "Test.Proc");
    }

    #[test]
    fn config_password_never_serialized() {
        let config = test_config();
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            !json.contains("password"),
            "serialized config leaked password: {json}"
        );
    }

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// A config pointing at the given mock server URI, writing to a unique temp
    /// dir so parallel tests do not collide.
    fn config_for(server_uri: &str) -> ProfilingConfig {
        let unique = format!(
            "al-profiling-http-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        );
        ProfilingConfig {
            server_url: server_uri.to_string(),
            company: "CRONUS International Ltd.".to_string(),
            output_dir: std::env::temp_dir().join(unique),
            username: Some("admin".to_string()),
            password: Some("password".to_string()),
            accept_invalid_certs: false,
        }
    }

    #[tokio::test]
    async fn start_profiling_returns_id_from_id_field() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/dev/profiler/start"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "prof-123"
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let id = start_profiling(&config)
            .await
            .expect("start should succeed");
        assert_eq!(id, "prof-123");
    }

    #[tokio::test]
    async fn start_profiling_falls_back_to_session_id_field() {
        // BC variants return `sessionId` instead of `id`.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sessionId": "sess-999"
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let id = start_profiling(&config)
            .await
            .expect("start should succeed");
        assert_eq!(id, "sess-999");
    }

    #[tokio::test]
    async fn start_profiling_empty_id_yields_no_active_session() {
        // A 200 with an empty id string must be rejected (the `filter` drops
        // empty strings) rather than returning an empty session id.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": ""
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = start_profiling(&config)
            .await
            .expect_err("empty id should error");
        assert!(
            matches!(err, ProfilingError::NoActiveSession),
            "expected NoActiveSession, got {err:?}"
        );
    }

    #[tokio::test]
    async fn start_profiling_missing_id_yields_no_active_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "ok"
            })))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = start_profiling(&config)
            .await
            .expect_err("missing id should error");
        assert!(
            matches!(err, ProfilingError::NoActiveSession),
            "expected NoActiveSession, got {err:?}"
        );
    }

    #[tokio::test]
    async fn start_profiling_server_error_propagates_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden: no dev endpoint"))
            .mount(&server)
            .await;

        let config = config_for(&server.uri());
        let err = start_profiling(&config)
            .await
            .expect_err("403 should error");
        match err {
            ProfilingError::ServerError { status, message } => {
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
    async fn stop_profiling_writes_profile_file() {
        let server = MockServer::start().await;
        let payload = b"{\"nodes\":[]}".to_vec();
        Mock::given(method("POST"))
            .and(path("/dev/profiler/stop"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(payload.clone()))
            .mount(&server)
            .await;

        let mut config = config_for(&server.uri());
        config.output_dir = std::env::temp_dir().join(format!(
            "al-prof-stop-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&config.output_dir);

        let dest = stop_profiling(&config, "sess-1")
            .await
            .expect("stop should succeed");

        assert!(dest.starts_with(&config.output_dir));
        let file_name = dest.file_name().unwrap().to_string_lossy();
        assert!(
            file_name.starts_with("profile-") && file_name.ends_with(".alcpuprofile"),
            "unexpected filename: {file_name}"
        );

        let written = std::fs::read(&dest).expect("file should exist");
        assert_eq!(written, payload);

        let _ = std::fs::remove_dir_all(&config.output_dir);
    }

    #[tokio::test]
    async fn stop_profiling_server_error_propagates_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let mut config = config_for(&server.uri());
        config.output_dir = std::env::temp_dir().join(format!(
            "al-prof-stop-err-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));

        let err = stop_profiling(&config, "sess-1")
            .await
            .expect_err("500 should error");
        match err {
            ProfilingError::ServerError { status, message } => {
                assert_eq!(status, 500);
                assert!(message.contains("boom"), "got {message:?}");
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
        // On the error path no file dir should have been created.
        assert!(!config.output_dir.exists());
    }
}
