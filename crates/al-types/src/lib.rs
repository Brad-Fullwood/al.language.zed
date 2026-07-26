//! Shared data types for the AL (Business Central) toolchain.

pub mod app;
pub mod bc;
pub mod filename;
pub mod jsonc;
pub mod procedure;
pub mod profiler;
pub mod test_result;

pub use app::AppDependency;
pub use bc::{AuthMethod, EnvironmentType};
pub use filename::{app_package_filename, sanitize_filename_component};
pub use jsonc::{strip_json_comments, strip_trailing_commas};
pub use procedure::ProcedureSource;
pub use profiler::{ProfilerHint, ProfilerSession};
pub use test_result::{
    PersistenceError, TestCodeunitResult, TestMethodResult, TestRunRecord, TestStatus,
};
