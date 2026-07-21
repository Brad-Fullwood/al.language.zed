//! On-disk format for breakpoint-sampled state snapshots.
//!
//! # Format
//!
//! A trace file is **line-delimited JSON** (newline-separated).
//!
//! - Line 1: a JSON object with the `Snapshot` header fields
//!   (`run_id`, `codeunit_id`, `method_name`, `bc_version`, `source_hash`,
//!   `captured_at`, and `samples: []` — samples field is ignored when reading
//!   the header line).
//! - Lines 2…N: one `Sample` JSON object per line.
//!
//! The format is plain JSON. Callers use `serialize_snapshot` and
//! `deserialize_snapshot`, leaving storage and compression concerns outside
//! this module.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Header record for a snapshot trace.
///
/// Represents a single recording run: one codeunit method exercised against
/// one BC version at one point in time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unique identifier for this recording run (e.g. UUID or timestamp string).
    pub run_id: String,
    pub codeunit_id: i32,
    pub method_name: String,
    /// BC server version string at record time (e.g. `"22.0.12345.0"`).
    pub bc_version: String,
    /// SHA-256 hex digest of the AL source file(s) covered by this snapshot.
    pub source_hash: String,
    /// Unix timestamp (seconds since epoch) when the snapshot was captured.
    pub captured_at: u64,
    /// All breakpoint samples captured during this run, in arrival order.
    pub samples: Vec<Sample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Numeric breakpoint identifier returned by the BC debug hub.
    pub breakpoint_id: u32,
    /// Source file path as registered with the debug hub.
    pub file: String,
    /// 1-based line number of the breakpoint.
    pub line: u32,
    /// How many times this breakpoint was hit before this sample (0 = first hit).
    pub iteration: u32,
    /// Variable state captured via `GetVariables` at this stop, as raw JSON.
    pub variables: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Trace is empty — expected at least a header line")]
    EmptyTrace,
    #[error("Header line missing or malformed: {0}")]
    BadHeader(String),
}

/// Serialize a `Snapshot` to a byte vector in the line-delimited JSON format.
///
/// Line 1: header JSON (all `Snapshot` fields, `samples` is included but ignored
/// by `deserialize_snapshot` which reads samples from subsequent lines).
/// Lines 2…N: one `Sample` per line.
pub fn serialize_snapshot(snap: &Snapshot) -> Result<Vec<u8>, FormatError> {
    let mut out = Vec::new();

    let header = serde_json::to_string(snap)?;
    out.extend_from_slice(header.as_bytes());
    out.push(b'\n');

    for sample in &snap.samples {
        let line = serde_json::to_string(sample)?;
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }

    Ok(out)
}

/// Deserialize a `Snapshot` from line-delimited JSON bytes.
///
/// The first line must be the header (`Snapshot` JSON). Subsequent lines are
/// `Sample` objects and **override** the `samples` field from the header —
/// this lets the format stay forward-compatible if the header `samples` array
/// is omitted in a future version.
pub fn deserialize_snapshot(bytes: &[u8]) -> Result<Snapshot, FormatError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| FormatError::BadHeader(format!("UTF-8 error: {e}")))?;

    let mut lines = text.lines().filter(|l| !l.trim().is_empty());

    let header_line = lines.next().ok_or(FormatError::EmptyTrace)?;
    let mut snap: Snapshot = serde_json::from_str(header_line)
        .map_err(|e| FormatError::BadHeader(format!("{e}: {header_line}")))?;

    // Remaining lines are samples.  If there are explicit sample lines,
    // they replace the embedded samples from the header.
    let samples: Vec<Sample> = lines
        .map(|l| serde_json::from_str::<Sample>(l).map_err(FormatError::Json))
        .collect::<Result<Vec<_>, _>>()?;

    if !samples.is_empty() {
        snap.samples = samples;
    }

    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_snapshot() -> Snapshot {
        Snapshot {
            run_id: "run-001".to_string(),
            codeunit_id: 50100,
            method_name: "TestMyProcedure".to_string(),
            bc_version: "22.0.12345.0".to_string(),
            source_hash: "abc123".to_string(),
            captured_at: 1_700_000_000,
            samples: vec![
                Sample {
                    breakpoint_id: 1,
                    file: "src/MyCodeunit.al".to_string(),
                    line: 42,
                    iteration: 0,
                    variables: serde_json::json!({"x": 1, "y": "hello"}),
                },
                Sample {
                    breakpoint_id: 1,
                    file: "src/MyCodeunit.al".to_string(),
                    line: 42,
                    iteration: 1,
                    variables: serde_json::json!({"x": 2, "y": "world"}),
                },
            ],
        }
    }

    #[test]
    fn test_format_round_trip_identity() {
        let original = make_snapshot();
        let bytes = serialize_snapshot(&original).expect("serialize failed");
        let recovered = deserialize_snapshot(&bytes).expect("deserialize failed");
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_format_round_trip_empty_samples() {
        let snap = Snapshot {
            run_id: "run-empty".to_string(),
            codeunit_id: 100,
            method_name: "TestEmpty".to_string(),
            bc_version: "21.0.0.0".to_string(),
            source_hash: "deadbeef".to_string(),
            captured_at: 0,
            samples: vec![],
        };
        let bytes = serialize_snapshot(&snap).expect("serialize failed");
        let recovered = deserialize_snapshot(&bytes).expect("deserialize failed");
        assert_eq!(snap, recovered);
    }

    #[test]
    fn test_deserialize_invalid_empty_bytes_returns_error() {
        let result = deserialize_snapshot(b"");
        assert!(result.is_err(), "expected error on empty input, got Ok");
        assert!(matches!(result.unwrap_err(), FormatError::EmptyTrace));
    }

    #[test]
    fn test_deserialize_invalid_malformed_header_returns_error() {
        let result = deserialize_snapshot(b"not valid json\n");
        assert!(result.is_err(), "expected error on malformed JSON");
        assert!(matches!(result.unwrap_err(), FormatError::BadHeader(_)));
    }

    #[test]
    fn test_deserialize_invalid_malformed_sample_returns_error() {
        let snap = make_snapshot();
        let mut bytes = serialize_snapshot(&snap).expect("serialize");
        bytes.extend_from_slice(b"<<not json>>\n");
        let result = deserialize_snapshot(&bytes);
        assert!(result.is_err(), "expected error on malformed sample line");
    }

    #[test]
    fn test_deserialize_invalid_non_utf8_returns_error() {
        let result = deserialize_snapshot(b"\xFF\xFE bad bytes\n");
        assert!(result.is_err(), "expected error on non-UTF-8 input");
        assert!(matches!(result.unwrap_err(), FormatError::BadHeader(_)));
    }
}
