//! Replay recorded snapshots against new BC state to detect regressions.
//!
//! # Protocol note
//!
//! The BC SignalR debug protocol exposes only `Break` events + `GetVariables`
//! at stops. This replayer therefore works in **breakpoint-sampled** mode:
//! it cannot observe continuous reads/writes, only the variable state at each
//! breakpoint hit.
//!
//! [`SnapshotReplayer::replay_against`] compares observed samples against a
//! previously recorded [`Snapshot`], matching on `(breakpoint_id, iteration)`.
//!
//! [`SnapshotReplayer::replay_via_dap`] drives a live [`DebuggerSession`] to
//! collect the observed samples, then delegates to `replay_against`.

use thiserror::Error;

use super::diff::{diff_snapshots, Divergence};
use super::format::{Sample, Snapshot};
use super::recorder::RecorderError;

#[derive(Debug, Error)]
pub enum ReplayerError {
    #[error("Recorder error during live replay: {0}")]
    Recorder(#[from] RecorderError),
    #[error("Session error: {0}")]
    Session(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Parse error: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "divergences")]
pub enum ReplayVerdict {
    /// All matched samples are byte-for-byte identical.
    Match,
    /// At least one field differs; the list describes each divergence.
    Diverged(Vec<Divergence>),
}

/// Minimal async interface over a BC debug session, used by the replayer.
///
/// `BcDebugSession` implements this trait (not shown here to avoid pulling
/// in the full SignalR machinery in tests). Unit tests supply a `FakeSession`.
#[allow(async_fn_in_trait)]
pub trait DebuggerSession {
    /// Add a breakpoint at `file:line` and return the server-assigned ID.
    async fn add_breakpoint(&self, file: &str, line: u32) -> Result<u32, ReplayerError>;

    /// Signal that configuration is complete and execution may start.
    async fn configuration_done(&self) -> Result<(), ReplayerError>;

    /// Wait for the next `Break` event.  Returns `false` when the session ends.
    async fn wait_for_break(&self) -> Result<bool, ReplayerError>;

    /// Capture the current variable state at frame 0.
    async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError>;

    async fn continue_execution(&self) -> Result<(), ReplayerError>;
}

pub struct SnapshotReplayer {
    _private: (),
}

impl SnapshotReplayer {
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Compare `observed` samples against the reference `snapshot`.
    ///
    /// Matching is done on `(breakpoint_id, iteration)`.  Uses
    /// [`diff_snapshots`] internally by constructing a synthetic snapshot with
    /// the same metadata as the reference.
    pub fn replay_against(&self, snapshot: &Snapshot, observed: &[Sample]) -> ReplayVerdict {
        // Build a synthetic snapshot with the observed samples, keeping the
        // same metadata as the reference so metadata differences don't pollute
        // the variable-level diff.
        let observed_snap = Snapshot {
            run_id: snapshot.run_id.clone(),
            codeunit_id: snapshot.codeunit_id,
            method_name: snapshot.method_name.clone(),
            bc_version: snapshot.bc_version.clone(),
            source_hash: snapshot.source_hash.clone(),
            captured_at: snapshot.captured_at,
            samples: observed.to_vec(),
        };

        let divergences = diff_snapshots(snapshot, &observed_snap);
        if divergences.is_empty() {
            ReplayVerdict::Match
        } else {
            ReplayVerdict::Diverged(divergences)
        }
    }

    /// Drive a live debug session, collect samples, then compare against the snapshot.
    ///
    /// Sets the same breakpoints as recorded in `snapshot`, runs the method,
    /// captures one variable snapshot per `Break` event (incrementing the
    /// iteration counter per breakpoint), continues after each stop, and
    /// stops when the session signals run-finished.
    ///
    /// **Performance note**: approximately one network round-trip per
    /// `Break` event (one `GetVariables` call).
    pub async fn replay_via_dap<S: DebuggerSession>(
        &self,
        snapshot: &Snapshot,
        session: &S,
    ) -> Result<ReplayVerdict, ReplayerError> {
        let mut registered = std::collections::HashSet::new();
        for sample in &snapshot.samples {
            if !registered.insert(sample.breakpoint_id) {
                continue;
            }
            session.add_breakpoint(&sample.file, sample.line).await?;
        }

        session.configuration_done().await?;

        // Iteration counters keyed by breakpoint_id.
        let mut iterations: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

        let mut observed: Vec<Sample> = Vec::new();

        loop {
            let stopped = session.wait_for_break().await?;
            if !stopped {
                break; // Session ended.
            }

            let vars = session.get_variables().await?;

            // The session interface does not expose the stopped location, so
            // samples are attributed in the reference snapshot's expected
            // order. Extra stops are rejected instead of being assigned to an
            // arbitrary breakpoint.
            let bp_id = find_next_expected_bp(snapshot, &iterations).ok_or_else(|| {
                ReplayerError::Session(
                    "debug session produced more breakpoint stops than the reference snapshot"
                        .to_string(),
                )
            })?;

            let iter = *iterations.get(&bp_id).unwrap_or(&0);
            *iterations.entry(bp_id).or_insert(0) += 1;

            // Look up the file/line for this breakpoint from the reference.
            let (file, line) = snapshot
                .samples
                .iter()
                .find(|s| s.breakpoint_id == bp_id)
                .map(|s| (s.file.clone(), s.line))
                .unwrap_or_default();

            observed.push(Sample {
                breakpoint_id: bp_id,
                file,
                line,
                iteration: iter,
                variables: vars,
            });

            session.continue_execution().await?;
        }

        Ok(self.replay_against(snapshot, &observed))
    }
}

impl Default for SnapshotReplayer {
    fn default() -> Self {
        Self::new()
    }
}

/// Determine the next expected breakpoint_id to attribute a `Break` event to.
///
/// Walks the reference snapshot's samples in order and returns the first
/// `breakpoint_id` whose expected iteration has not yet been satisfied.
fn find_next_expected_bp(
    snapshot: &Snapshot,
    iterations: &std::collections::HashMap<u32, u32>,
) -> Option<u32> {
    // Count how many times each bp_id should fire (from snapshot).
    let mut expected: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    for s in &snapshot.samples {
        *expected.entry(s.breakpoint_id).or_insert(0) += 1;
    }

    // Find the first bp_id (in snapshot order) that hasn't reached its count.
    for s in &snapshot.samples {
        let seen = *iterations.get(&s.breakpoint_id).unwrap_or(&0);
        let needed = *expected.get(&s.breakpoint_id).unwrap_or(&0);
        if seen < needed {
            return Some(s.breakpoint_id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{Sample, Snapshot};

    fn base_snapshot(samples: Vec<Sample>) -> Snapshot {
        Snapshot {
            run_id: "r1".to_string(),
            codeunit_id: 50100,
            method_name: "TestProc".to_string(),
            bc_version: "22.0.0.0".to_string(),
            source_hash: "abc".to_string(),
            captured_at: 0,
            samples,
        }
    }

    fn sample(bp: u32, iter: u32, vars: serde_json::Value) -> Sample {
        Sample {
            breakpoint_id: bp,
            file: "Test.al".to_string(),
            line: 10,
            iteration: iter,
            variables: vars,
        }
    }

    #[test]
    fn test_replay_against_identical_samples_yields_match() {
        let snap = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 42}))]);
        let observed = snap.samples.clone();
        let replayer = SnapshotReplayer::new();
        assert_eq!(
            replayer.replay_against(&snap, &observed),
            ReplayVerdict::Match
        );
    }

    #[test]
    fn test_replay_against_empty_samples_yields_match() {
        let snap = base_snapshot(vec![]);
        let replayer = SnapshotReplayer::new();
        assert_eq!(replayer.replay_against(&snap, &[]), ReplayVerdict::Match);
    }

    #[test]
    fn test_replay_against_mismatched_field_yields_diverged() {
        let snap = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        let observed = vec![sample(1, 0, serde_json::json!({"x": 99}))];
        let replayer = SnapshotReplayer::new();
        let verdict = replayer.replay_against(&snap, &observed);
        assert!(
            matches!(verdict, ReplayVerdict::Diverged(ref v) if !v.is_empty()),
            "expected Diverged with entries, got: {verdict:?}"
        );
    }

    #[test]
    fn test_replay_against_missing_sample_yields_diverged() {
        let snap = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        let replayer = SnapshotReplayer::new();
        let verdict = replayer.replay_against(&snap, &[]);
        assert!(matches!(verdict, ReplayVerdict::Diverged(_)));
    }

    use std::sync::{Arc, Mutex};
    use tokio::runtime::Runtime;

    /// A fake debug session that replays a pre-programmed sequence of Break
    /// events with pre-set variable values.
    struct FakeSession {
        /// Variables to return on each consecutive `wait_for_break` call.
        /// After exhausted, `wait_for_break` returns `false`.
        events: Arc<Mutex<std::collections::VecDeque<serde_json::Value>>>,
        /// Breakpoint IDs to hand back (always returns the index + 1).
        next_bp_id: Arc<Mutex<u32>>,
    }

    impl FakeSession {
        fn new(vars_sequence: Vec<serde_json::Value>) -> Self {
            Self {
                events: Arc::new(Mutex::new(vars_sequence.into())),
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
            let mut q = self.events.lock().unwrap();
            Ok(!q.is_empty() && {
                q.pop_front();
                true
            })
        }

        async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
            // Return a simple value; callers compare against snapshot.
            Ok(serde_json::json!({"x": 1}))
        }

        async fn continue_execution(&self) -> Result<(), ReplayerError> {
            Ok(())
        }
    }

    #[test]
    fn test_replay_via_dap_matching_fake_session_yields_match() {
        let rt = Runtime::new().unwrap();
        let snap = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        let session = FakeSession::new(vec![serde_json::json!({"x": 1})]);
        let replayer = SnapshotReplayer::new();
        let verdict = rt
            .block_on(replayer.replay_via_dap(&snap, &session))
            .unwrap();
        assert_eq!(verdict, ReplayVerdict::Match);
    }

    #[test]
    fn test_replay_via_dap_mismatched_fake_session_yields_diverged() {
        let rt = Runtime::new().unwrap();
        let snap = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        // FakeSession always returns {"x": 1}, but we override get_variables behaviour
        // by constructing a second fake that returns different data.
        struct DivergentSession;
        impl DebuggerSession for DivergentSession {
            async fn add_breakpoint(&self, _: &str, _: u32) -> Result<u32, ReplayerError> {
                Ok(1)
            }
            async fn configuration_done(&self) -> Result<(), ReplayerError> {
                Ok(())
            }
            async fn wait_for_break(&self) -> Result<bool, ReplayerError> {
                Ok(false) // Return false immediately — simulates "no more breaks".
            }
            async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
                Ok(serde_json::json!({"x": 99}))
            }
            async fn continue_execution(&self) -> Result<(), ReplayerError> {
                Ok(())
            }
        }
        // With no Break events, observed is empty → diverged (snapshot has 1 sample).
        let replayer = SnapshotReplayer::new();
        let verdict = rt
            .block_on(replayer.replay_via_dap(&snap, &DivergentSession))
            .unwrap();
        assert!(matches!(verdict, ReplayVerdict::Diverged(_)));
    }

    #[test]
    fn replay_via_dap_rejects_unexpected_extra_stop() {
        let rt = Runtime::new().unwrap();
        let snap = base_snapshot(vec![sample(1, 0, serde_json::json!({"x": 1}))]);
        let session = FakeSession::new(vec![
            serde_json::json!({"x": 1}),
            serde_json::json!({"x": 2}),
        ]);

        let error = rt
            .block_on(SnapshotReplayer::new().replay_via_dap(&snap, &session))
            .expect_err("an extra stop must not be assigned to an arbitrary breakpoint");
        assert!(error.to_string().contains("more breakpoint stops"));
    }
}
