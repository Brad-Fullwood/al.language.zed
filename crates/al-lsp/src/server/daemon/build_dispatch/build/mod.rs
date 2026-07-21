//! Build, package, metrics, snapshot/profiling, sort/organize, and BC-server dispatchers.

mod bc_server_params;
mod compile;
mod file_refresh;
mod metrics;
mod organize;
mod snapshot_profiling;
mod source_lookup;

pub(in crate::server::daemon) use compile::{dispatch_compile, dispatch_package};
pub(in crate::server::daemon) use metrics::{dispatch_metrics, dispatch_profiler_hints};
pub(in crate::server::daemon) use organize::{dispatch_organize_files, dispatch_sort_members};
pub(in crate::server::daemon) use snapshot_profiling::{dispatch_profiling, dispatch_snapshot};
pub(in crate::server::daemon) use source_lookup::{
    dispatch_event_source, dispatch_location, dispatch_source,
};

// re-exported at the `build` module's own root so `fixes.rs`'s direct
// `use super::build::write_al_file_and_refresh;` import keeps resolving
// unchanged after `build.rs` became this directory module.
// (`rename_al_file_and_refresh` doesn't need a root re-export — `organize.rs`,
// its only caller, imports it directly from `file_refresh`.)
pub(crate) use file_refresh::write_al_file_and_refresh;
