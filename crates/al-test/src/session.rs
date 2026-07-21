//! `TestSession` trait and related types for the test engine.
//!
//! Backends (live BC and the interpreter) implement `TestSession`.
//! Uses stable `async fn` in trait (Rust 1.75+, edition 2021); no `async-trait` dep.
//! Events are streamed back via a caller-provided `tokio::sync::mpsc::Sender`,
//! which lets parallel codeunit runs share one event stream cleanly.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::TestRunnerError;
use crate::result::{TestCodeunitResult, TestMethodResult};

/// Identifies a single test target: a codeunit, or a specific method within one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestId {
    pub codeunit_id: i32,
    /// The AL codeunit name (display + diagnostic mapping).
    pub codeunit_name: String,
    /// If `Some`, run only this specific test method; if `None`, run all methods.
    pub method_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOptions {
    /// Per-test timeout in milliseconds. Backends apply a default if `None`.
    pub timeout_ms: Option<u64>,
    pub parallel: bool,
    /// If set, write a JUnit XML report to this path after the run completes.
    pub junit_out: Option<PathBuf>,
    /// If set, write a Cobertura XML coverage report to this path.
    pub cobertura_out: Option<PathBuf>,
    pub filter: Option<String>,
    /// Opt-in dynamic coverage. When `true`, interpreter-routed tests
    /// run with a statement/branch collector attached (`InterpMode::with_coverage`)
    /// so the caller can surface *executed-line* coverage — over the daemon RPC
    /// result and as a dynamic-mode Cobertura document. Default `false` keeps
    /// the static call-graph path zero-cost.
    #[serde(default)]
    pub coverage: bool,
}

/// Events emitted by a running test session.
///
/// Tagged JSON for forward-compatibility on the wire — new variants don't
/// break old clients (they ignore unknown tags).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TestEvent {
    CaseStarted {
        id: TestId,
    },
    CaseResult {
        id: TestId,
        result: TestMethodResult,
    },
    SuiteComplete {
        codeunit_id: i32,
        summary: TestCodeunitResult,
    },
    SessionComplete {
        total: usize,
        passed: usize,
        failed: usize,
        skipped: usize,
    },
    /// An unrecoverable error occurred mid-run (the session may continue
    /// with reduced scope, or terminate — backend's choice).
    Error {
        message: String,
    },
}

/// Trait for running AL test sessions.
///
/// Backends (live BC and the interpreter) implement this. Events
/// are streamed via the caller-provided `tx`; the future resolves once the
/// session has emitted `SessionComplete` or fatally errored.
///
/// `Send + Sync` so a session can be shared across tokio tasks via `Arc`.
/// `&self` (not `&mut self`) so the implementor manages internal state via
/// interior mutability — keeps the trait object usable behind `Arc` without
/// an external `Mutex`.
#[allow(async_fn_in_trait)]
pub trait TestSession: Send + Sync {
    async fn run(
        &self,
        tests: Vec<TestId>,
        opts: RunOptions,
        tx: mpsc::Sender<TestEvent>,
    ) -> Result<(), TestRunnerError>;
}
