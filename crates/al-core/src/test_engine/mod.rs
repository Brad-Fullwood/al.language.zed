//! Test engine — orchestration of AL codeunit test execution.
//!
//! See `.agentic/<run-id>/arch/designs/plan.md` for the multi-phase roadmap.
//! Phase 1 ships: result types, TestSession trait + LiveBcMode backend,
//! JUnit/Cobertura output, persisted result history. Future phases add
//! the router, interpreter, snapshot replay, mutation testing.

pub mod backends;
pub mod error;
pub mod mutate;
pub mod output;
pub mod persistence;
pub mod result;
pub mod router;
pub mod session;

pub use error::TestRunnerError;
pub use persistence::{PersistenceError, TestResultStore, TestRunRecord};
pub use result::{TestCodeunitResult, TestMethodResult, TestStatus};
pub use session::{RunOptions, TestEvent, TestId, TestSession};
