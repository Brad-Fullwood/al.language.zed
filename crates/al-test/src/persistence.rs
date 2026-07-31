//! Test-result persistence types and timestamp helpers.

use std::time::{SystemTime, UNIX_EPOCH};

pub use al_types::{PersistenceError, TestRunRecord};

pub use al_workspace::TestResultStore;

/// Unix-seconds timestamp (recorded at append time by the test dispatcher).
pub fn now_secs() -> Result<u64, PersistenceError> {
    timestamp_secs(SystemTime::now())
}

fn timestamp_secs(time: SystemTime) -> Result<u64, PersistenceError> {
    Ok(time.duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamp_rejects_pre_epoch_clock_without_panicking() {
        let time = UNIX_EPOCH
            .checked_sub(Duration::from_secs(1))
            .expect("representable pre-epoch timestamp");
        assert!(matches!(
            timestamp_secs(time),
            Err(PersistenceError::Clock(_))
        ));
    }
}
