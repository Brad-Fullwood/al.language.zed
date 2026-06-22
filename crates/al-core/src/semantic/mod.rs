//! Facade over the `al-semantic` crate, plus the Workspace-bound bridge
//! lifecycle glue.
//!
//! The bridge, host, disk cache, `SemanticCache`, and all data types live in
//! the standalone `al-semantic` crate and are re-exported here so existing
//! `crate::semantic::…` paths keep resolving. The lifecycle functions that
//! operate on the `Workspace` hub live in `glue` (they cannot sink into a
//! tier-0 crate).

pub use al_semantic::*;

mod glue;
pub use glue::{get_or_init_bridge, restart_bridge, set_builtins, shutdown_bridge};
