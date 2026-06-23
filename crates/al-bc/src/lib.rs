//! `al-bc`: the Business Central client layer.
//!
//! - `bc_client` — BC Dev API REST client + capped body readers
//! - `http_auth` — OAuth/Basic/NTLM auth helpers + TLS-insecurity warnings
//! - `snapshot` — snapshot REST
//! - `profiling` — profiler REST
//! - `launch` — `.vscode/launch.json` / `.zed/debug.json` parsing + `BcServerConfig`
//!
//! Connection enums (`EnvironmentType`/`AuthMethod`), `AppDependency`, and the
//! JSONC helpers live in `al-types`; this crate re-exports the enums from
//! `launch` for back-compat.

pub mod bc_client;
pub mod http_auth;
pub mod launch;
pub mod profiling;
pub mod snapshot;
