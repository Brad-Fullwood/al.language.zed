//! Canonical result types for test execution.
//!
//! Moved from `test_runner.rs`. `test_runner.rs` re-exports these via
//! `pub use crate::test_engine::result::{TestStatus, TestMethodResult, TestCodeunitResult}`
//! for back-compat.

use serde::{Deserialize, Serialize};

// Re-export error type so callers can import everything from this module.
pub use crate::test_engine::error::TestRunnerError;

/// Execution status of a single test method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TestStatus {
    /// Test passed.
    Pass,
    /// Test failed with an error message.
    Fail,
    /// Test was skipped (e.g., missing permissions, no `[Test]` subtype active).
    Skip,
}

/// Result of a single test method execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestMethodResult {
    /// Test method name (procedure name).
    pub name: String,
    /// Execution status.
    pub status: TestStatus,
    /// Error message, if the test failed.
    pub error: Option<String>,
    /// Execution duration in milliseconds.
    pub duration_ms: Option<u64>,
}

/// Result of running all tests in a codeunit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCodeunitResult {
    /// Codeunit name.
    pub name: String,
    /// Codeunit object ID.
    pub id: i32,
    /// Results per test method.
    pub methods: Vec<TestMethodResult>,
    /// Summary: total methods run.
    pub total: usize,
    /// Number of passing tests.
    pub passed: usize,
    /// Number of failing tests.
    pub failed: usize,
    /// Number of skipped tests.
    pub skipped: usize,
}

impl TestCodeunitResult {
    pub fn from_methods(name: String, id: i32, methods: Vec<TestMethodResult>) -> Self {
        let total = methods.len();
        let passed = methods
            .iter()
            .filter(|m| m.status == TestStatus::Pass)
            .count();
        let failed = methods
            .iter()
            .filter(|m| m.status == TestStatus::Fail)
            .count();
        let skipped = methods
            .iter()
            .filter(|m| m.status == TestStatus::Skip)
            .count();
        Self {
            name,
            id,
            methods,
            total,
            passed,
            failed,
            skipped,
        }
    }
}
