//! Canonical result types for test execution.
//!
//! Moved from `test_runner.rs`. `test_runner.rs` re-exports these via
//! `pub use crate::test_engine::result::{TestStatus, TestMethodResult, TestCodeunitResult}`
//! for back-compat.

use serde::{Deserialize, Serialize};

// Re-export error type so callers can import everything from this module.
pub use crate::test_engine::error::TestRunnerError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TestStatus {
    Pass,
    Fail,
    /// Test was skipped (e.g., missing permissions, no `[Test]` subtype active).
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestMethodResult {
    /// Test method name (procedure name).
    pub name: String,
    pub status: TestStatus,
    pub error: Option<String>,
    /// Execution duration in milliseconds.
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestCodeunitResult {
    pub name: String,
    pub id: i32,
    pub methods: Vec<TestMethodResult>,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
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
