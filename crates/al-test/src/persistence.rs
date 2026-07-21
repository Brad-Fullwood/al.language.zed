//! Test-result persistence types and timestamp helpers.

use std::time::{SystemTime, UNIX_EPOCH};

pub use al_types::{PersistenceError, TestRunRecord};

pub use al_workspace::TestResultStore;

/// Unix-seconds timestamp (recorded at append time by the test dispatcher).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock precedes Unix epoch")
        .as_secs()
}
