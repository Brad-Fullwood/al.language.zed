//! Test execution results and persisted history.

use serde::{Deserialize, Serialize};
use thiserror::Error;

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

/// One persisted test-run record (append-only history).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunRecord {
    /// Unix epoch seconds when the record was appended.
    pub timestamp: u64,
    pub codeunit_id: i32,
    pub codeunit_name: String,
    pub method_name: String,
    pub status: TestStatus,
    /// Test duration in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Failure message, only present for failed tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("test results IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("test results JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
