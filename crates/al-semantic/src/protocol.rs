//! JSON-RPC message types for .NET bridge communication.
//!
//! The bridge communicates via line-delimited JSON over stdin/stdout.
//! Each message is a single JSON object terminated by a newline.
//!
//! Core types are defined in `al_protocol::jsonrpc` and re-exported here.

pub use al_protocol::jsonrpc::{error_codes, Request, Response, RpcError};
