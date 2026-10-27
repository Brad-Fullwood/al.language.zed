//! Shared data types for the AL (Business Central) toolchain.

pub mod app;
pub mod bc;
pub mod browser;
pub mod cache_root;
pub mod filename;
pub mod jsonc;
pub mod procedure;
pub mod profile_timing;
pub mod profiler;
pub mod test_result;

pub use app::AppDependency;
pub use bc::{AuthMethod, EnvironmentType};
pub use browser::{open_in_browser, OpenUrlError};
pub use cache_root::{cache_root, cache_root_or_tmp, CACHE_DIR_ENV};
pub use filename::{app_package_filename, sanitize_filename_component};
pub use jsonc::{strip_json_comments, strip_trailing_commas};
pub use procedure::ProcedureSource;
pub use profiler::{ProfilerHint, ProfilerSession, MAX_PROFILE_FILE_BYTES};
pub use test_result::{
    PersistenceError, TestCodeunitResult, TestFailureKind, TestMethodResult, TestRunRecord,
    TestStatus,
};
