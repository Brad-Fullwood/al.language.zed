//! Facade over the `al-semantic` crate, plus the Workspace-bound bridge
//! lifecycle glue.
//!
//! The bridge, host, disk cache, `SemanticCache`, and all data types live in
//! the standalone `al-semantic` crate and are re-exported here so existing
//! `crate::semantic::…` paths keep resolving. The lifecycle functions that
//! operate on the `Workspace` hub live in `glue` (they cannot sink into a
//! tier-0 crate).

pub use al_semantic::*;

// The lifecycle glue (set_builtins / get_or_init_bridge / restart_bridge /
// shutdown_bridge) operates on the tier-4 `Workspace` hub, so it lives in the
// al-workspace crate; re-export it here so `crate::semantic::…` paths resolve.
pub use al_workspace::{get_or_init_bridge, restart_bridge, set_builtins, shutdown_bridge};
