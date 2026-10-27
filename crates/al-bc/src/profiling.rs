//! Analysis of AL CPU profiles (`.alcpuprofile`).
//!
//! Capturing a profile is not done here. Profiling start, status and stop are the
//! snapshot debugger calls with ExecutionContext 2 in
//! `al_dap::dap::bc_debug::snapshot_capture`, and the stop saves the
//! `<debuggingcontext>.alcpuprofile` that the sampling zip holds.
//!
//! The `.alcpuprofile` data is a JSON-encoded call tree compatible with Chrome's
//! CPU profiling format. `analyze_profile` parses it and identifies hotspot
//! AL procedures by accumulated self-time.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Failed to parse profile data: {0}")]
    ParseError(String),
}

/// Parse a `.alcpuprofile` file and return the top hotspots.
///
/// `.alcpuprofile` is a Chrome-style CPU profile JSON:
/// ```json
/// { "nodes": [...], "startTime": ..., "endTime": ..., "samples": [...], "timeDeltas": [...] }
/// ```
/// Each node has `{ "id": N, "callFrame": { "functionName": ..., "url": ... }, "hitCount": N, "children": [...] }`.
///
/// Times come from `sampleExecutionTimes` (inclusive per node), with self time
/// as inclusive minus children. See `al_types::profile_timing`. Profiles without
/// those arrays fall back to 1 ms per hit. `hit_count` is always retained.
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

    // Inclusive time per node from sampleExecutionTimes, self = inclusive minus
    // children (see al_types::profile_timing).
    let times = al_types::profile_timing::node_times(&json, &nodes);

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

            let time = node_id.and_then(|id| times.get(&id).copied());
            let self_time_ms = time.map_or(0.0, |t| t.self_ms);
            let total_time_ms = time.map_or(self_time_ms, |t| t.total_ms);

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

pub use al_types::MAX_PROFILE_FILE_BYTES;

pub async fn analyze_profile_file(
    path: &std::path::Path,
    top_n: usize,
) -> Result<ProfilingResult, ProfilingError> {
    let size = tokio::fs::metadata(path).await?.len();
    if size > MAX_PROFILE_FILE_BYTES {
        return Err(ProfilingError::ParseError(format!(
            "profile file {} is {size} bytes, exceeds {MAX_PROFILE_FILE_BYTES} byte limit",
            path.display()
        )));
    }
    let data = tokio::fs::read(path).await?;
    let mut result = analyze_profile(&data, top_n)?;
    result.profile_path = Some(path.to_path_buf());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // The two profiles below are built from the decompiled CpuProfile and
    // CpuProfileGenerator types, not captured from a host. Replace them with a
    // real CDX capture when one exists. `timeDeltas` holds absolute differences
    // of neighbouring inclusive times (CpuProfileGenerator.cs:190-197) and must
    // not be read as time per node.
    //   root(1) 10 ms inclusive -> A(2) 8 ms -> B(3) 3 ms
    fn generator_profile() -> serde_json::Value {
        serde_json::json!({
            "startTime": 0,
            "endTime": 10000,
            "nodes": [
                { "id": 1, "callFrame": { "functionName": "(root)", "url": "" }, "hitCount": 0, "children": [2] },
                { "id": 2, "callFrame": { "functionName": "A", "url": "a.al" }, "hitCount": 2, "children": [3] },
                { "id": 3, "callFrame": { "functionName": "B", "url": "b.al" }, "hitCount": 3, "children": [] }
            ],
            "samples": [1, 2, 3],
            "timeDeltas": [10000, 2000, 5000],
            "sampleExecutionTimes": [10000, 8000, 3000]
        })
    }

    #[test]
    fn self_time_is_inclusive_minus_children_not_the_time_deltas() {
        let bytes = serde_json::to_vec(&generator_profile()).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        let a = result.hotspots.iter().find(|h| h.procedure == "A").unwrap();
        let b = result.hotspots.iter().find(|h| h.procedure == "B").unwrap();
        // A: 8 ms inclusive minus B's 3 ms. The timeDeltas entry for A is 2000.
        assert!((a.self_time_ms - 5.0).abs() < 1e-9, "{}", a.self_time_ms);
        assert!((a.total_time_ms - 8.0).abs() < 1e-9, "{}", a.total_time_ms);
        assert!((b.self_time_ms - 3.0).abs() < 1e-9, "{}", b.self_time_ms);
        assert!((b.total_time_ms - 3.0).abs() < 1e-9, "{}", b.total_time_ms);
        assert_eq!(result.hotspots[0].procedure, "A");
    }

    #[test]
    fn profile_without_execution_times_falls_back_to_one_ms_per_hit() {
        let mut profile = generator_profile();
        profile
            .as_object_mut()
            .unwrap()
            .remove("sampleExecutionTimes");
        let bytes = serde_json::to_vec(&profile).unwrap();
        let result = analyze_profile(&bytes, 10).unwrap();
        let a = result.hotspots.iter().find(|h| h.procedure == "A").unwrap();
        assert!((a.self_time_ms - 2.0).abs() < 1e-9);
        assert!((a.total_time_ms - 5.0).abs() < 1e-9);
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

    #[tokio::test]
    async fn analyze_profile_file_rejects_oversize_file_without_reading() {
        // A `.alcpuprofile` past the 500 MB cap must be refused via its
        // metadata size BEFORE the file is read into memory. A sparse file
        // (via `set_len`) reports a huge size while using one disk block, so
        // this test doesn't actually allocate 500+ MB.
        let dir = std::env::temp_dir().join(format!(
            "al-profiling-oversize-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("huge.alcpuprofile");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PROFILE_FILE_BYTES + 1).unwrap();
        drop(file);

        let err = analyze_profile_file(&path, 10).await;
        match err {
            Err(ProfilingError::ParseError(message)) => {
                assert!(message.contains("exceeds"), "got: {message}");
            }
            other => panic!("expected ParseError for oversize file, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
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
}
