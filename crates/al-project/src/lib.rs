//! `al-project` (tier 2): AL project discovery, workspace configuration,
//! toolchain discovery/validation, and the unified error hierarchy.
//!
//! - `project`   — `app.json` discovery, `.alpackages` scanning, NuGet feeds
//!   (`AlProject`, `AppManifest`, `find_project`, `nuget_feeds`, `home_dir`)
//! - `config`    — merged `AlConfig` workspace settings (persist/load/merge)
//! - `toolchain` — ALTool discovery + validation (`AlToolchain`,
//!   `find_toolchain`, `validate_toolchain`, `dotnet_command*`). NOTE: the
//!   `doctor()` health check is NOT here — it reads the tier-4 `Workspace`
//!   hub, so it is parked in al-core's `crate::toolchain` facade.
//! - `errors`    — `AlError` / `DiscoveryError`
//!
//! Depends only on tier-0/1 crates (al-types, al-bc, al-semantic); it must
//! never reference the tier-4 `Workspace`.

pub mod config;
pub mod errors;
pub mod project;
pub mod toolchain;
