//! Shared daemon IPC client for al-lsp thin adapters.
//!
//! Provides the deterministic socket path computation, JSON-RPC message types,
//! and a synchronous Unix socket client with auto-start and retry logic.
//! Used by al-cli, al-explorer, and al-lsp.

#[cfg(unix)]
pub mod client;
pub mod jsonrpc;
pub mod socket;

// Convenience re-exports
#[cfg(unix)]
pub use client::DaemonClient;
pub use socket::{socket_path, socket_path_with_runtime_dir};
