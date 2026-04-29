//! Bridge: `BcDebugSession` → `DebuggerSession` trait.
//!
//! Adapts the live-BC SignalR debugger client to the trait the snapshot
//! recorder/replayer drives. Phase-4 ships a minimal stub: every method
//! returns `ReplayerError::Session("bc bridge not yet wired …")`.
//!
//! Why a stub: dev's `DebuggerSession::add_breakpoint(file, line)` addresses
//! breakpoints by source path, but `BcDebugSession::add_breakpoint` takes
//! `(object_type, object_number, line, column, condition)`. Translating
//! between the two requires parsing the AL source to extract object
//! declaration metadata. That's a deliberately separate change so the
//! daemon endpoints + CLI can ship and the trait surface can be exercised
//! against `FakeSession` in unit tests.

use super::replayer::{DebuggerSession, ReplayerError};
use crate::dap::bc_debug::BcDebugSession;

/// Adapter that lets a `BcDebugSession` reference satisfy the
/// `DebuggerSession` trait. Newtype to keep future evolution explicit.
pub struct BcDebugSessionAdapter<'a> {
    #[allow(dead_code)]
    session: &'a BcDebugSession,
}

impl<'a> BcDebugSessionAdapter<'a> {
    /// Wrap a `BcDebugSession` reference.
    pub fn new(session: &'a BcDebugSession) -> Self {
        Self { session }
    }
}

fn not_yet_wired(method: &str) -> ReplayerError {
    ReplayerError::Session(format!(
        "BcDebugSessionAdapter::{method} is not yet wired \
         (Phase 4 ships the daemon + CLI scaffolding; the live-BC \
         bridge needs AL source → object metadata extraction)"
    ))
}

impl<'a> DebuggerSession for BcDebugSessionAdapter<'a> {
    async fn add_breakpoint(&self, _file: &str, _line: u32) -> Result<u32, ReplayerError> {
        Err(not_yet_wired("add_breakpoint"))
    }

    async fn configuration_done(&self) -> Result<(), ReplayerError> {
        Err(not_yet_wired("configuration_done"))
    }

    async fn wait_for_break(&self) -> Result<bool, ReplayerError> {
        Err(not_yet_wired("wait_for_break"))
    }

    async fn get_variables(&self) -> Result<serde_json::Value, ReplayerError> {
        Err(not_yet_wired("get_variables"))
    }

    async fn continue_execution(&self) -> Result<(), ReplayerError> {
        Err(not_yet_wired("continue_execution"))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check that the adapter satisfies the trait bound —
    /// failing this means the trait surface drifted.
    #[allow(dead_code)]
    fn _bound_check<S: DebuggerSession>() {}

    #[test]
    fn adapter_satisfies_debugger_session_bound() {
        _bound_check::<BcDebugSessionAdapter<'static>>();
    }
}
