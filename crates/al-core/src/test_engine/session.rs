//! `TestSession` trait and related types for the test engine.
//!
//! Backends (live BC, interpreter, snapshot replay) implement `TestSession`.
//! Uses stable `async fn` in trait (Rust 1.75+, edition 2021); no `async-trait` dep.
//! Events are streamed back via a caller-provided `tokio::sync::mpsc::Sender`,
//! which lets parallel codeunit runs share one event stream cleanly.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::test_engine::error::TestRunnerError;
use crate::test_engine::result::{TestCodeunitResult, TestMethodResult};

/// Identifies a single test target: a codeunit, or a specific method within one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestId {
    /// The AL codeunit object ID.
    pub codeunit_id: i32,
    /// The AL codeunit name (display + diagnostic mapping).
    pub codeunit_name: String,
    /// If `Some`, run only this specific test method; if `None`, run all methods.
    pub method_name: Option<String>,
}

/// Inputs to a test run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOptions {
    /// Per-test timeout in milliseconds. Backends apply a default if `None`.
    pub timeout_ms: Option<u64>,
    /// Whether to run codeunits in parallel.
    pub parallel: bool,
    /// If set, write a JUnit XML report to this path after the run completes.
    pub junit_out: Option<PathBuf>,
    /// If set, write a Cobertura XML coverage report to this path.
    pub cobertura_out: Option<PathBuf>,
    /// Optional glob filter on method names.
    pub filter: Option<String>,
}

/// Events emitted by a running test session.
///
/// Tagged JSON for forward-compatibility on the wire — new variants don't
/// break old clients (they ignore unknown tags).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TestEvent {
    /// A test case has started.
    CaseStarted { id: TestId },
    /// A test case has produced a result.
    CaseResult {
        id: TestId,
        result: TestMethodResult,
    },
    /// All tests in a codeunit have completed.
    SuiteComplete {
        codeunit_id: i32,
        summary: TestCodeunitResult,
    },
    /// The entire session is complete.
    SessionComplete {
        total: usize,
        passed: usize,
        failed: usize,
        skipped: usize,
    },
    /// An unrecoverable error occurred mid-run (the session may continue
    /// with reduced scope, or terminate — backend's choice).
    Error { message: String },
}

/// Trait for running AL test sessions.
///
/// Backends (live BC, interpreter, snapshot replay) implement this. Events
/// are streamed via the caller-provided `tx`; the future resolves once the
/// session has emitted `SessionComplete` or fatally errored.
///
/// `Send + Sync` so a session can be shared across tokio tasks via `Arc`.
/// `&self` (not `&mut self`) so the implementor manages internal state via
/// interior mutability — keeps the trait object usable behind `Arc` without
/// an external `Mutex`.
#[allow(async_fn_in_trait)]
pub trait TestSession: Send + Sync {
    /// Execute `tests` according to `opts`, streaming events to `tx`.
    async fn run(
        &self,
        tests: Vec<TestId>,
        opts: RunOptions,
        tx: mpsc::Sender<TestEvent>,
    ) -> Result<(), TestRunnerError>;
}
