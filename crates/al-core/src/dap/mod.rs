//! Headless DAP control for AL debugging.
//!
//! `bc_debug` module talks directly to BC via REST + SignalR (no external binary).
//! `client` provides low-level DAP communication for EditorServices.Host.
//!
//! Folded into al-core in stage 3 of the crate consolidation; previously the
//! standalone `al-dap-client` crate.

pub mod bc_debug;
pub mod client;
pub mod config;
pub mod framing;
pub mod json_util;
pub mod native_dap;
pub mod protocol;
pub mod types;

use std::time::Duration;

use thiserror::Error;

/// Errors from DAP operations.
#[derive(Debug, Error)]
pub enum DapError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("EditorServices.Host not found: {0}")]
    EditorServicesNotFound(String),

    #[error("Failed to spawn subprocess: {0}")]
    SpawnFailed(String),

    #[error("AL compilation failed: {0}")]
    CompilationFailed(String),

    #[error("DAP protocol error in {command}: {message}")]
    DapProtocolError { command: String, message: String },

    #[error("Debug session is not paused")]
    SessionNotPaused,

    #[error("No active debug session")]
    NoActiveSession,

    #[error("Operation timed out after {0:?}")]
    Timeout(Duration),

    #[error("Publish failed: {0}")]
    PublishFailed(String),

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Server error: {0}")]
    ServerError(String),
}

/// Convenience alias for `Result<T, DapError>`.
pub type Result<T> = std::result::Result<T, DapError>;
