//! al-test: AL test-execution engine (tier 6).
//!
//! Runs AL `[Test]` procedures through the pure-Rust tree-walking interpreter
//! backend or a live Business Central server, statically routes each discovered
//! test to the right backend, drives mutation testing, and serialises results
//! to JUnit / Cobertura XML.
//!
//! Extracted from al-core's `test_engine/` directory (flattened to this crate
//! root) plus the sibling `test_runner.rs`. al-core keeps the old paths working
//! by re-exporting this crate: `pub use al_test as test_engine;` and
//! `pub use al_test::test_runner;`.

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