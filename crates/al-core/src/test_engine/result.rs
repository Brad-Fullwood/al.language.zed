//! Canonical result types for test execution.
//!
//! The value types now live in the tier-0 `al-types` crate; this module
//! re-exports them so existing `crate::test_engine::result::…` paths keep
//! working.

// Re-export error type so callers can import everything from this module.
pub use crate::test_engine::error::TestRunnerError;

pub use al_types::{TestCodeunitResult, TestMethodResult, TestStatus};
