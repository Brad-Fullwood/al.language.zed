//! AL test discovery, routing, execution, mutation, and report generation.

pub mod backends;
pub mod error;
pub mod mutate;
pub mod output;
pub mod persistence;
pub mod result;
pub mod router;
pub mod session;
pub mod test_runner;

pub use error::TestRunnerError;
pub use persistence::{PersistenceError, TestResultStore, TestRunRecord};
pub use result::{TestCodeunitResult, TestMethodResult, TestStatus};
pub use session::{RunOptions, TestEvent, TestId, TestSession};
