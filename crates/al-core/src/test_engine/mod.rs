//! Test engine — skeleton for AL codeunit test execution.
//!
//! This module will grow in subsequent tasks (p1-2 through p1-5). For now it
//! defines the public surface and re-exports all types so callers can import
//! via either `al_core::test_engine::*` or the back-compat paths in
//! `al_core::test_runner::*`.

pub mod backends;
pub mod error;
pub mod result;
pub mod session;

pub use error::TestRunnerError;
pub use result::{TestCodeunitResult, TestMethodResult, TestStatus};
pub use session::{RunOptions, TestEvent, TestId, TestSession};

/// Placeholder — real implementation lands in p1-4 (persistence.rs).
///
/// The actual fields and append-only JSON storage are defined when the
/// persistence layer is implemented. This stub allows `Workspace::test_results`
/// to exist and compile now.
pub struct TestResultStore;
