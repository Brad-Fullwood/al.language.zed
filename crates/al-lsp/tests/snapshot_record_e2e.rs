//! End-to-end integration test for the snapshot record→serialize→deserialize→replay→diff cycle.
//!
//! Uses a `FakeSession` (mock `DebuggerSession`) that emits scripted Break events
//! with synthetic variable values.  No live BC instance is required.
//!
//! # Test plan
//!
//! ## Positive cases
//! - `e2e_clean_replay`: record a snapshot with two breakpoints, serialize it,
//!   deserialize it, replay with identical synthetic variables, assert `Match`.
//! - `e2e_multi_bp_clean_replay`: two distinct breakpoints, correct ordering.
//! - `e2e_empty_session_clean_replay`: session with no Break events → empty
//!   snapshot round-trips cleanly and replays as `Match`.
//!
//! ## Negative cases
//! - `e2e_injected_divergence_detected`: mutate one field in the replayed
//!   variable state → assert `Diverged` with the expected `field_path`.
//! - `e2e_missing_sample_on_replay_detected`: replay session fires fewer Break
//!   events than recorded → assert `Diverged` (missing sample).
//! - `e2e_extra_sample_on_replay_detected`: replay session fires more Break
//!   events than recorded → assert `Diverged` (extra sample).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use al_snapshot::replayer::{DebuggerSession, ReplayerError};
use al_snapshot::{
    deserialize_snapshot, diff_snapshots, serialize_snapshot, ReplayVerdict, SnapshotRecorder,
    SnapshotReplayer,
};

/// A fake debug session that emits a predetermined sequence of variable payloads
/// when Break events are requested, then terminates.
struct FakeSession {
    /// Remaining variable payloads; each entry represents one Break event.
    /// `wait_for_break` returns `true` and `get_variables` returns the front
    /// value, until the deque is exhausted.
    events: Arc<Mutex<VecDeque<serde_json::Value>>>,
    /// Auto-incrementing breakpoint ID counter.
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
        let q = self.events.lock().unwrap();
        Ok(!q.is_empty())
    }

    async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
        let mut q = self.events.lock().unwrap();
        Ok(q.pop_front().unwrap_or(serde_json::Value::Null))
    }

    async fn continue_execution(&self) -> Result<(), ReplayerError> {
        Ok(())
    }
}

fn make_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("failed to create Tokio runtime")
}

#[test]
fn e2e_clean_replay() {
    let rt = make_runtime();

    let recorder = SnapshotRecorder::new("e2e-run-001", "22.0.0.0", "src-hash-aaa");

    let record_session = FakeSession::new(vec![
        serde_json::json!({"counter": 0, "label": "first"}),
        serde_json::json!({"counter": 1, "label": "second"}),
    ]);

    let snapshot = rt
        .block_on(recorder.record(
            &record_session,
            vec![("SomeCodeunit.al".to_string(), 42)],
            50200,
            "MyTestProcedure",
        ))
        .expect("record should succeed");

    assert_eq!(snapshot.run_id, "e2e-run-001");
    assert_eq!(snapshot.codeunit_id, 50200);
    assert_eq!(snapshot.method_name, "MyTestProcedure");
    assert_eq!(snapshot.bc_version, "22.0.0.0");
    assert_eq!(snapshot.source_hash, "src-hash-aaa");
    assert_eq!(
        snapshot.samples.len(),
        2,
        "expected 2 samples for 2 Break events"
    );
    assert_eq!(snapshot.samples[0].iteration, 0);
    assert_eq!(snapshot.samples[1].iteration, 1);
    assert_eq!(
        snapshot.samples[0].variables,
        serde_json::json!({"counter": 0, "label": "first"})
    );
    assert_eq!(
        snapshot.samples[1].variables,
        serde_json::json!({"counter": 1, "label": "second"})
    );

    let bytes = serialize_snapshot(&snapshot).expect("serialize should succeed");
    assert!(!bytes.is_empty(), "serialized bytes should not be empty");

    let recovered = deserialize_snapshot(&bytes).expect("deserialize should succeed");
    assert_eq!(
        recovered, snapshot,
        "deserialized snapshot must equal original"
    );

    let replayer = SnapshotReplayer::new();

    let replay_session = FakeSession::new(vec![
        serde_json::json!({"counter": 0, "label": "first"}),
        serde_json::json!({"counter": 1, "label": "second"}),
    ]);

    let verdict = rt
        .block_on(replayer.replay_via_dap(&recovered, &replay_session))
        .expect("replay_via_dap should succeed");

    assert_eq!(
        verdict,
        ReplayVerdict::Match,
        "clean replay with identical vars must yield Match"
    );
}

#[test]
fn e2e_multi_bp_clean_replay() {
    let rt = make_runtime();

    let recorder = SnapshotRecorder::new("e2e-run-002", "22.0.0.0", "hash-bbb");

    let record_session = FakeSession::new(vec![
        serde_json::json!({"step": "A", "value": 10}),
        serde_json::json!({"step": "B", "value": 20}),
        serde_json::json!({"step": "C", "value": 30}),
    ]);

    let snapshot = rt
        .block_on(recorder.record(
            &record_session,
            vec![
                ("CuA.al".to_string(), 10),
                ("CuA.al".to_string(), 20),
                ("CuA.al".to_string(), 30),
            ],
            50201,
            "MultiStepProc",
        ))
        .expect("record should succeed");

    assert_eq!(snapshot.samples.len(), 3);

    let bytes = serialize_snapshot(&snapshot).expect("serialize");
    let recovered = deserialize_snapshot(&bytes).expect("deserialize");
    assert_eq!(recovered, snapshot);

    let replayer = SnapshotReplayer::new();
    let replay_session = FakeSession::new(vec![
        serde_json::json!({"step": "A", "value": 10}),
        serde_json::json!({"step": "B", "value": 20}),
        serde_json::json!({"step": "C", "value": 30}),
    ]);

    let verdict = rt
        .block_on(replayer.replay_via_dap(&recovered, &replay_session))
        .expect("replay_via_dap should succeed");

    assert_eq!(
        verdict,
        ReplayVerdict::Match,
        "multi-bp clean replay must Match"
    );
}

#[test]
fn e2e_empty_session_clean_replay() {
    let rt = make_runtime();

    let recorder = SnapshotRecorder::new("e2e-run-003", "21.0.0.0", "hash-ccc");

    let record_session = FakeSession::new(vec![]);

    let snapshot = rt
        .block_on(recorder.record(
            &record_session,
            vec![("EmptyFile.al".to_string(), 5)],
            50202,
            "EmptyProc",
        ))
        .expect("record should succeed even with no breaks");

    assert!(
        snapshot.samples.is_empty(),
        "empty session must produce empty samples"
    );

    let bytes = serialize_snapshot(&snapshot).expect("serialize");
    let recovered = deserialize_snapshot(&bytes).expect("deserialize");
    assert_eq!(recovered, snapshot);

    let replayer = SnapshotReplayer::new();
    let replay_session = FakeSession::new(vec![]);

    let verdict = rt
        .block_on(replayer.replay_via_dap(&recovered, &replay_session))
        .expect("replay_via_dap should succeed");

    assert_eq!(
        verdict,
        ReplayVerdict::Match,
        "empty replay of empty snapshot must Match"
    );
}

#[test]
fn e2e_direct_replay_against_identical_samples() {
    let rt = make_runtime();

    let recorder = SnapshotRecorder::new("e2e-run-da", "22.0.0.0", "hash-da");

    let record_session = FakeSession::new(vec![serde_json::json!({"qty": 5, "name": "Item"})]);

    let snapshot = rt
        .block_on(recorder.record(
            &record_session,
            vec![("Proc.al".to_string(), 15)],
            50203,
            "DirectProc",
        ))
        .expect("record should succeed");

    let bytes = serialize_snapshot(&snapshot).expect("serialize");
    let recovered = deserialize_snapshot(&bytes).expect("deserialize");

    let replayer = SnapshotReplayer::new();
    // Replay directly against the same samples (synchronous path).
    let verdict = replayer.replay_against(&recovered, &recovered.samples);

    assert_eq!(
        verdict,
        ReplayVerdict::Match,
        "replay_against with own samples must Match"
    );
}

#[test]
fn e2e_injected_divergence_detected() {
    let rt = make_runtime();

    let recorder = SnapshotRecorder::new("e2e-run-div", "22.0.0.0", "hash-div");

    let record_session =
        FakeSession::new(vec![serde_json::json!({"amount": 100, "status": "open"})]);

    let snapshot = rt
        .block_on(recorder.record(
            &record_session,
            vec![("PostingCu.al".to_string(), 55)],
            50210,
            "PostProc",
        ))
        .expect("record should succeed");

    let bytes = serialize_snapshot(&snapshot).expect("serialize");
    let recovered = deserialize_snapshot(&bytes).expect("deserialize");

    let replayer = SnapshotReplayer::new();

    let replay_session =
        FakeSession::new(vec![serde_json::json!({"amount": 999, "status": "open"})]);

    let verdict = rt
        .block_on(replayer.replay_via_dap(&recovered, &replay_session))
        .expect("replay_via_dap should succeed");

    match &verdict {
        ReplayVerdict::Diverged(divergences) => {
            assert!(
                !divergences.is_empty(),
                "expected at least one divergence, got empty list"
            );
            let amount_divergence = divergences.iter().find(|d| d.field_path == "/amount");
            assert!(
                amount_divergence.is_some(),
                "expected divergence on /amount field, divergences: {divergences:?}"
            );
            let d = amount_divergence.unwrap();
            assert_eq!(
                d.old_value,
                serde_json::json!(100),
                "reference value must be 100"
            );
            assert_eq!(d.new_value, serde_json::json!(999), "new value must be 999");
        }
        ReplayVerdict::Match => panic!("expected Diverged when amount was mutated, got Match"),
    }
}

#[test]
fn e2e_diff_direct_field_change_detected() {
    use al_snapshot::format::{Sample, Snapshot};

    let make_snap = |status: &str| Snapshot {
        run_id: "r".to_string(),
        codeunit_id: 50211,
        method_name: "DirectDiff".to_string(),
        bc_version: "22.0.0.0".to_string(),
        source_hash: "hash-dd".to_string(),
        captured_at: 0,
        samples: vec![Sample {
            breakpoint_id: 1,
            file: "File.al".to_string(),
            line: 7,
            iteration: 0,
            variables: serde_json::json!({"status": status}),
        }],
    };

    let reference = make_snap("open");
    let diverged = make_snap("closed");

    let diffs = diff_snapshots(&reference, &diverged);
    assert!(!diffs.is_empty(), "diff must detect status change");

    let status_diff = diffs.iter().find(|d| d.field_path == "/status");
    assert!(
        status_diff.is_some(),
        "expected divergence on /status, got: {diffs:?}"
    );
    assert_eq!(status_diff.unwrap().old_value, serde_json::json!("open"));
    assert_eq!(status_diff.unwrap().new_value, serde_json::json!("closed"));
}

#[test]
fn e2e_missing_sample_on_replay_detected() {
    let rt = make_runtime();

    let recorder = SnapshotRecorder::new("e2e-run-miss", "22.0.0.0", "hash-miss");

    let record_session = FakeSession::new(vec![
        serde_json::json!({"i": 1}),
        serde_json::json!({"i": 2}),
    ]);

    let snapshot = rt
        .block_on(recorder.record(
            &record_session,
            vec![("Cu.al".to_string(), 10)],
            50220,
            "MissingProc",
        ))
        .expect("record should succeed");

    let bytes = serialize_snapshot(&snapshot).expect("serialize");
    let recovered = deserialize_snapshot(&bytes).expect("deserialize");

    let replayer = SnapshotReplayer::new();
    let replay_session = FakeSession::new(vec![
        serde_json::json!({"i": 1}),
        // second event is missing
    ]);

    let verdict = rt
        .block_on(replayer.replay_via_dap(&recovered, &replay_session))
        .expect("replay_via_dap should succeed");

    assert!(
        matches!(verdict, ReplayVerdict::Diverged(_)),
        "missing sample must yield Diverged, got: {verdict:?}"
    );
}

#[test]
fn e2e_session_error_on_replay_propagates() {
    struct FailingSession;

    impl DebuggerSession for FailingSession {
        async fn add_breakpoint(&self, _: &str, _: u32) -> Result<u32, ReplayerError> {
            Err(ReplayerError::Session("simulated bp failure".to_string()))
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

    use al_snapshot::format::{Sample, Snapshot};

    let snap = Snapshot {
        run_id: "r-err".to_string(),
        codeunit_id: 50230,
        method_name: "ErrProc".to_string(),
        bc_version: "22.0.0.0".to_string(),
        source_hash: "hash-err".to_string(),
        captured_at: 0,
        samples: vec![Sample {
            breakpoint_id: 1,
            file: "Err.al".to_string(),
            line: 1,
            iteration: 0,
            variables: serde_json::json!({}),
        }],
    };

    let rt = make_runtime();
    let replayer = SnapshotReplayer::new();
    let result = rt.block_on(replayer.replay_via_dap(&snap, &FailingSession));

    assert!(
        result.is_err(),
        "session error during add_breakpoint must propagate as Err, got: {result:?}"
    );
}

#[test]
fn e2e_deserialize_empty_bytes_returns_error() {
    let result = deserialize_snapshot(b"");
    assert!(result.is_err(), "deserialize of empty bytes must fail");
}

#[test]
fn e2e_diff_bc_version_change_detected() {
    use al_snapshot::format::Snapshot;

    let make_snap = |ver: &str| Snapshot {
        run_id: "r".to_string(),
        codeunit_id: 50212,
        method_name: "VersionProc".to_string(),
        bc_version: ver.to_string(),
        source_hash: "same-hash".to_string(),
        captured_at: 0,
        samples: vec![],
    };

    let a = make_snap("22.0.0.0");
    let b = make_snap("23.0.0.0");

    let diffs = diff_snapshots(&a, &b);
    let meta = diffs
        .iter()
        .find(|d| d.field_path == "/metadata/bc_version");
    assert!(
        meta.is_some(),
        "bc_version change must produce /metadata/bc_version divergence, diffs: {diffs:?}"
    );
}
