//! al-dap-client: Headless DAP control for AL debugging.
//!
//! Provides a `DapClient` for low-level DAP communication and a
//! `DebugSession` (T404b) for high-level AL debug lifecycle management.
//!
//! This crate communicates with Microsoft's EditorServices.Host binary
//! via the Debug Adapter Protocol over stdio. It does NOT depend on
//! al-core, al-syntax, or al-symbols.

pub mod client;
pub mod config;
pub mod editor_services;
pub mod framing;
pub mod json_util;
pub mod protocol;
pub mod session;
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
}

/// Convenience alias for `Result<T, DapError>`.
pub type Result<T> = std::result::Result<T, DapError>;
