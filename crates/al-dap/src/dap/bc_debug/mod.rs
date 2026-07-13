//! Native Business Central debug client.
//!
//! Communicates directly with BC via REST API (publish) and SignalR (debug session),
//! eliminating the dependency on Microsoft's EditorServices.Host binary.
//!
//! Protocol (reverse-engineered from EditorServices.Host):
//! 1. REST: POST /v2.0/{env}/dev/apps — publish .app package
//! 2. SignalR: Connect to debug hub for breakpoints, stepping, variables
//!
//! SignalR methods (from EditorServices.Protocol.dll):
//!   OpenConnectionAsync, Attach, ConfigurationDoneAsync, ContinueAsync,
//!   AddBreakpointAsync, RemoveBreakpointAsync, UpdateBreakpointAsync,
//!   GetVariablesAsync, ExpandGlobalsAsync, ExpandNodeAsync,
//!   GetWatchNodeAsync, GetSourceAsync, TerminateSession, IsAlive
//!
//! Split into submodules along natural seams (was a single ~3,371-line file):
//! `session_config` (DAP-args parsing + `BcDebugConfig`), `events` (`BcEvent` /
//! `BreakLocation` + wire-to-event conversion), `rest` (`publish_app` /
//! `get_metadata`), `wire` (SignalR message shape, negotiate, handshake), and
//! `session` (`BcDebugSession` + its `invoke()` loop and test doubles).

mod session_config;
mod events;
mod rest;
mod wire;
mod session;

pub use session_config::BcDebugConfig;
pub use events::{BcEvent, BreakLocation};
pub use rest::{get_metadata, publish_app};
pub(crate) use wire::percent_encode_url;
pub use session::BcDebugSession;

#[cfg(test)]
pub(crate) use session::fake;
