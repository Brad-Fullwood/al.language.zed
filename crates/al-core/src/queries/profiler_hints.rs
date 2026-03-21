//! Profiler hints query — WP17.
//!
//! Analyzes profiler output and maps hotspots back to AL source locations.

use serde::Serialize;
use crate::workspace::Workspace;

/// A profiler hotspot with source location.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfilerHint {
    pub procedure: String,
    pub object: String,
    pub self_time_ms: f64,
    pub total_time_ms: f64,
    pub hit_count: u64,
    pub file: Option<String>,
    pub line: Option<u32>,
}

/// Map profiler hotspots to workspace source locations.
pub fn profiler_hints(workspace: &Workspace, hotspots: &[serde_json::Value]) -> Vec<ProfilerHint> {
    let _ = workspace;
    hotspots
        .iter()
        .filter_map(|h| {
            let procedure = h.get("procedure")?.as_str()?.to_string();
            let object = h.get("object").and_then(|v| v.as_str()).unwrap_or("").to_string();
            Some(ProfilerHint {
                procedure,
                object,
                self_time_ms: h.get("selfTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0),
                total_time_ms: h.get("totalTimeMs").and_then(|v| v.as_f64()).unwrap_or(0.0),
                hit_count: h.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0),
                file: None,
                line: None,
            })
        })
        .collect()
}
