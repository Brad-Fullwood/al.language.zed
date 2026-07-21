//! Shared daemon IPC client for al-lsp thin adapters.
//!
//! Provides deterministic local-endpoint computation, JSON-RPC message types,
//! and a synchronous local-socket client with auto-start and retry logic.
//! The transport is a Unix-domain socket on Linux/macOS and a named pipe on
//! Windows.
//! Used by al-cli, al-explorer, and al-lsp.

pub mod client;
pub mod jsonrpc;
pub mod socket;

pub use client::DaemonClient;
pub use socket::{socket_path, socket_path_with_runtime_dir};
