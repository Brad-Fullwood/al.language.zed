//! Facade over `al_project::toolchain`.
//!
//! The toolchain module (`AlToolchain`, `find_toolchain`, `validate_toolchain`,
//! the discovery helpers, `dotnet_command*`, `official_lsp_command`) lives in
//! the tier-2 `al-project` crate and is re-exported below. Only `doctor()`
//! stays in al-core: it reads the tier-4 `al_workspace::Workspace` hub,
//! which `al-project` must never reference (the same parking pattern as
//! al-semantic's lifecycle glue). The glob `pub use` keeps every
//! `crate::toolchain::…` path resolving unchanged.

pub use al_project::toolchain::*;

pub use al_workspace::{doctor, DoctorReport, ProjectInfo, ToolchainInfo};
