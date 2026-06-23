//! `al-dap`: the AL debug-adapter layer (tier 3).
//!
//! - `dap` — the native Business Central debug adapter (`bc_debug`, speaks REST +
//!   SignalR directly, no external binary), the low-level DAP `client` for the
//!   legacy EditorServices.Host proxy, plus `framing`, `protocol`, `types`,
//!   `config`, and the `json_util` JSONC re-exports.
//! - `native_debug` — the in-process native debug session built on `dap::bc_debug`.
//!
//! The compiler / emitter pipeline is deliberately NOT a dependency:
//! `dap::native_dap::run_native_dap` takes injected `compile` / `find_app`
//! callbacks, so this crate never names the build or emit modules (those stay
//! parked in al-core). Connection enums and JSONC helpers live in `al-types`;
//! the BC REST client + TLS/auth helpers live in `al-bc`.

pub mod dap;
pub mod native_debug;