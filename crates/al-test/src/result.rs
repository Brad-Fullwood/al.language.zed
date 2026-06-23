//! Canonical result types for test execution.
//!
//! The value types now live in the tier-0 `al-types` crate; this module
//! re-exports them so existing `crate::result::…` paths keep
//! working.

// Re-export error type so callers can import everything from this module.
pub use crate::error::TestRunnerError;

pub use al_types::{TestCodeunitResult, TestMethodResult, TestStatus};
