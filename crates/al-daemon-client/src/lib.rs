//! Shared daemon IPC client for al-lsp thin adapters.
//!
//! Provides the deterministic socket path computation, JSON-RPC message types,
//! and a synchronous Unix socket client with auto-start and retry logic.
//! Used by al-cli, al-explorer, and al-lsp.

pub mod socket;
pub mod jsonrpc;
pub mod client;

// Convenience re-exports
pub use socket::socket_path;
pub use client::DaemonClient;
