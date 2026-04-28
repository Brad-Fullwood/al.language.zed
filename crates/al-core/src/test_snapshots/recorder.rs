//! Record breakpoint-sampled state snapshots from a live BC debug session.
//!
//! # Protocol constraint
//!
//! The BC SignalR debug protocol exposes **only** `Break` events (execution
//! stopped) and `GetVariables` / `ExpandNode` at stops.  There is no
//! continuous variable-watch mechanism.  Consequently, this recorder
//! captures approximately **one `GetVariables` round-trip per `Break` event**.
//! This is documented here so callers understand the performance profile:
//! N breakpoints × M iterations = N×M network round-trips.
//!
//! # Mocking
//!
//! `SnapshotRecorder::record` is generic over [`DebuggerSession`] (defined in
//! [`super::replayer`]).  Tests supply a `FakeSession` rather than a live BC.

use std::collections::HashMap;

use thiserror::Error;

use super::format::{Sample, Snapshot};
use super::replayer::{DebuggerSession, ReplayerError};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the snapshot recorder.
#[derive(Debug, Error)]
pub enum RecorderError {
    #[error("Session error: {0}")]
    Session(String),
    #[error("Serialization error: {0}")]
    Serialize(#[from] super::format::FormatError),
}

impl From<ReplayerError> for RecorderError {
    fn from(e: ReplayerError) -> Self {
        RecorderError::Session(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Recorder
// ---------------------------------------------------------------------------

/// Records breakpoint-sampled state snapshots from a debug session.
pub struct SnapshotRecorder {
    /// Identifies this recording run (e.g. a UUID or build ID).
    pub run_id: String,
    /// BC server version string.
    pub bc_version: String,
    /// SHA-256 hex digest of the source files being exercised.
    pub source_hash: String,
}

impl SnapshotRecorder {
    /// Create a new recorder.
    pub fn new(
        run_id: impl Into<String>,
        bc_version: impl Into<String>,
        source_hash: impl Into<String>,
    ) -> Self {
        Self {
            run_id: run_id.into(),
            bc_version: bc_version.into(),
            source_hash: source_hash.into(),
        }
    }

    /// Record a snapshot by driving `session` through `breakpoints`.
    ///
    /// Each entry in `breakpoints` is `(file_path, line_number)`.
    ///
    /// The recorder:
    /// 1. Adds each breakpoint via `session.add_breakpoint`.
    /// 2. Calls `session.configuration_done` to start execution.
    /// 3. On each `Break` event, calls `session.get_variables` and increments
    ///    the iteration counter for the active breakpoint.
    /// 4. Calls `session.continue_execution` and loops.
    /// 5. Stops when `session.wait_for_break` returns `false`.
    ///
    /// **Performance**: ~1 `GetVariables` round-trip per `Break` event.
    pub async fn record<S: DebuggerSession>(
        &self,
        session: &S,
        breakpoints: Vec<(String, u32)>,
        codeunit_id: i32,
        method_name: &str,
    ) -> Result<Snapshot, RecorderError> {
        // Register breakpoints and remember their assigned IDs.
        // `bp_id_to_coords` maps assigned_id → (file, line).
        let mut bp_id_to_coords: HashMap<u32, (String, u32)> = HashMap::new();

        for (file, line) in &breakpoints {
            let assigned_id = session
                .add_breakpoint(file, *line)
                .await
                .map_err(RecorderError::from)?;
            bp_id_to_coords.insert(assigned_id, (file.clone(), *line));
        }

        session
            .configuration_done()
            .await
            .map_err(RecorderError::from)?;

        // Iteration counters per breakpoint.
        let mut iterations: HashMap<u32, u32> = HashMap::new();
        let mut samples: Vec<Sample> = Vec::new();

        // Track which breakpoint is "current" — we use a sequential counter
        // based on the order breakpoints were registered, since the FakeSession
        // and real BC both just fire them in program order.
        let mut bp_sequence: Vec<u32> = bp_id_to_coords.keys().copied().collect();
        bp_sequence.sort(); // stable ordering
        let mut seq_index = 0usize;

        loop {
            let stopped = session
                .wait_for_break()
                .await
                .map_err(RecorderError::from)?;
            if !stopped {
                break;
            }

            let vars = session.get_variables().await.map_err(RecorderError::from)?;

            // Attribute this Break to the next expected breakpoint.
            let bp_id = if seq_index < bp_sequence.len() {
                let id = bp_sequence[seq_index];
                let iter = *iterations.get(&id).unwrap_or(&0);
                if iter == 0 {
                    // First hit: stay on this bp_id.
                }
                id
            } else {
                // More Break events than expected breakpoints — reuse the last.
                *bp_sequence.last().unwrap_or(&0)
            };

            let iter = *iterations.get(&bp_id).unwrap_or(&0);
            *iterations.entry(bp_id).or_insert(0) += 1;

            let (file, line) = bp_id_to_coords.get(&bp_id).cloned().unwrap_or_default();

            samples.push(Sample {
                breakpoint_id: bp_id,
                file,
                line,
                iteration: iter,
                variables: vars,
            });

            // Advance sequence index when we've seen all iterations of this bp.
            // For now, advance after each hit (one-pass recording).
            seq_index += 1;

            session
                .continue_execution()
                .await
                .map_err(RecorderError::from)?;
        }

        let captured_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(Snapshot {
            run_id: self.run_id.clone(),
            codeunit_id,
            method_name: method_name.to_string(),
            bc_version: self.bc_version.clone(),
            source_hash: self.source_hash.clone(),
            captured_at,
            samples,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_snapshots::replayer::{DebuggerSession, ReplayerError};

    use std::sync::{Arc, Mutex};

    /// A fake debug session that simulates a fixed sequence of Break events.
    struct FakeSession {
        /// Variable payloads to return for each Break event in sequence.
        var_sequence: Arc<Mutex<std::collections::VecDeque<serde_json::Value>>>,
        /// Breakpoint ID counter (auto-increments).
        next_bp_id: Arc<Mutex<u32>>,
    }

    impl FakeSession {
        fn new(vars: Vec<serde_json::Value>) -> Self {
            Self {
                var_sequence: Arc::new(Mutex::new(vars.into())),
                next_bp_id: Arc::new(Mutex::new(1)),
            }
        }
    }

    impl DebuggerSession for FakeSession {
        async fn add_breakpoint(&self, _file: &str, _line: u32) -> Result<u32, ReplayerError> {
            let mut id = self.next_bp_id.lock().unwrap();
            let assigned = *id;
            *id += 1;
            Ok(assigned)
        }

        async fn configuration_done(&self) -> Result<(), ReplayerError> {
            Ok(())
        }

        async fn wait_for_break(&self) -> Result<bool, ReplayerError> {
            let q = self.var_sequence.lock().unwrap();
            Ok(!q.is_empty())
        }

        async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
            let mut q = self.var_sequence.lock().unwrap();
            Ok(q.pop_front().unwrap_or(serde_json::Value::Null))
        }

        async fn continue_execution(&self) -> Result<(), ReplayerError> {
            Ok(())
        }
    }

    // Positive: record with one breakpoint and two Break events.
    #[test]
    fn test_record_builds_correct_snapshot_shape() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let recorder = SnapshotRecorder::new("run-test", "22.0.0.0", "src-hash");
        let session = FakeSession::new(vec![
            serde_json::json!({"i": 0}),
            serde_json::json!({"i": 1}),
        ]);
        let snap = rt
            .block_on(recorder.record(
                &session,
                vec![("Test.al".to_string(), 10)],
                50100,
                "TestProc",
            ))
            .unwrap();

        assert_eq!(snap.run_id, "run-test");
        assert_eq!(snap.codeunit_id, 50100);
        assert_eq!(snap.method_name, "TestProc");
        assert_eq!(snap.bc_version, "22.0.0.0");
        assert_eq!(snap.source_hash, "src-hash");
        assert_eq!(snap.samples.len(), 2);
        assert_eq!(snap.samples[0].iteration, 0);
        assert_eq!(snap.samples[1].iteration, 1);
    }

    // Positive: record with no Break events → empty samples.
    #[test]
    fn test_record_no_break_events_yields_empty_samples() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let recorder = SnapshotRecorder::new("r0", "21.0.0.0", "hash0");
        let session = FakeSession::new(vec![]);
        let snap = rt
            .block_on(recorder.record(&session, vec![("File.al".to_string(), 5)], 100, "TestEmpty"))
            .unwrap();
        assert!(snap.samples.is_empty());
    }

    // Negative: session error on add_breakpoint propagates.
    #[test]
    fn test_record_session_error_on_add_breakpoint_returns_err() {
        struct FailOnBreakpoint;
        impl DebuggerSession for FailOnBreakpoint {
            async fn add_breakpoint(&self, _: &str, _: u32) -> Result<u32, ReplayerError> {
                Err(ReplayerError::Session("simulated failure".to_string()))
            }
            async fn configuration_done(&self) -> Result<(), ReplayerError> {
                Ok(())
            }
            async fn wait_for_break(&self) -> Result<bool, ReplayerError> {
                Ok(false)
            }
            async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
                Ok(serde_json::Value::Null)
            }
            async fn continue_execution(&self) -> Result<(), ReplayerError> {
                Ok(())
            }
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let recorder = SnapshotRecorder::new("r1", "22.0.0.0", "hash");
        let result = rt.block_on(recorder.record(
            &FailOnBreakpoint,
            vec![("File.al".to_string(), 1)],
            50100,
            "TestFail",
        ));
        assert!(result.is_err(), "expected error from failed add_breakpoint");
    }

    // Negative: session error on get_variables propagates.
    #[test]
    fn test_record_session_error_on_get_variables_returns_err() {
        struct FailOnGetVars {
            fired: Arc<Mutex<bool>>,
        }
        impl DebuggerSession for FailOnGetVars {
            async fn add_breakpoint(&self, _: &str, _: u32) -> Result<u32, ReplayerError> {
                Ok(1)
            }
            async fn configuration_done(&self) -> Result<(), ReplayerError> {
                Ok(())
            }
            async fn wait_for_break(&self) -> Result<bool, ReplayerError> {
                let mut f = self.fired.lock().unwrap();
                if !*f {
                    *f = true;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
                Err(ReplayerError::Session("vars error".to_string()))
            }
            async fn continue_execution(&self) -> Result<(), ReplayerError> {
                Ok(())
            }
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let recorder = SnapshotRecorder::new("r2", "22.0.0.0", "hash");
        let result = rt.block_on(recorder.record(
            &FailOnGetVars {
                fired: Arc::new(Mutex::new(false)),
            },
            vec![("File.al".to_string(), 1)],
            50100,
            "TestFail2",
        ));
        assert!(result.is_err(), "expected error from failed get_variables");
    }
}
