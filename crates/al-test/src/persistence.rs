//! Persistent test result history — record + error types, the unix-seconds
//! helper, and a re-export of the async store.
//!
//! The append-only `tokio::fs` store (`TestResultStore`) now lives in the
//! `al-workspace` hub crate so the `Workspace` hub can own per-project test
//! state without al-workspace depending on this tier-6 `al-test` crate. It is
//! re-exported below so existing `crate::TestResultStore` and
//! `crate::persistence::{TestRunRecord, now_secs}` paths keep
//! resolving unchanged.

use std::time::{SystemTime, UNIX_EPOCH};

// The persisted record + error types live in the tier-0 `al-types` crate.
pub use al_types::{PersistenceError, TestRunRecord};

// The async append-only store now lives in the al-workspace hub crate.
pub use al_workspace::TestResultStore;

/// Unix-seconds timestamp (recorded at append time by the test dispatcher).
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
