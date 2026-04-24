//! Shared daemon IPC client for al-lsp thin adapters.
//!
//! Provides the deterministic socket path computation, JSON-RPC message types,
//! and a synchronous Unix socket client with auto-start and retry logic.
//! Used by al-cli, al-explorer, and al-lsp.

#[cfg(not(unix))]
compile_error!("al-daemon-client requires Unix (Unix domain sockets)");

pub mod client;
pub mod jsonrpc;
pub mod socket;

// Convenience re-exports
pub use client::DaemonClient;
pub use socket::{socket_path, socket_path_with_runtime_dir};
