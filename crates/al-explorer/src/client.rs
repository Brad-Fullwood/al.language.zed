//! Daemon client — re-exports from al-daemon-client.
#![allow(unused_imports)]
pub use al_daemon_client::client::find_al_lsp_binary;
pub use al_daemon_client::jsonrpc::{Request as RpcRequest, Response as RpcResponse, RpcError};
pub use al_daemon_client::socket_path;
pub use al_daemon_client::DaemonClient;
