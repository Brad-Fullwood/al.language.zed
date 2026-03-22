//! JSON-RPC types for daemon protocol communication.
//!
//! Re-exports the canonical definitions from al-daemon-client so that al-lsp
//! and al-core code can use a single, deduplicated set of types.

pub use al_daemon_client::jsonrpc::{error_codes, Request, Response, RpcError};
