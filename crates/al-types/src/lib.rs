//! `al-types`: the shared, dependency-free data types of the AL (Business
//! Central) toolchain.
//!
//! This is the bottom of the crate graph. It holds the plain value types that
//! were previously defined inside higher-tier modules but consumed by lower
//! ones — the placement that manufactured most of the dependency cycles in the
//! monolith. Keeping them here lets every consumer depend *downward*.
//!
//! It deliberately depends only on `serde` and `thiserror`.

pub mod app;
pub mod bc;
pub mod jsonc;
pub mod procedure;
pub mod profiler;
pub mod test_result;

pub use app::AppDependency;
pub use procedure::ProcedureSource;
pub use bc::{AuthMethod, EnvironmentType};
pub use jsonc::{strip_json_comments, strip_trailing_commas};
pub use profiler::{ProfilerHint, ProfilerSession};
pub use test_result::{
    PersistenceError, TestCodeunitResult, TestMethodResult, TestRunRecord, TestStatus,
};
