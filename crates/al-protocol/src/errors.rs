//! Discovery-specific errors with actionable messages.

use std::path::PathBuf;

use thiserror::Error;

/// Errors from project/toolchain discovery.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("ALTool is not installed. Install it with: {install_cmd}")]
    AlToolNotInstalled { install_cmd: String },

    #[error(".NET SDK is not installed")]
    DotNetNotInstalled,

    #[error("No AL project found (no app.json). Searched from {start} upward through: {searched}")]
    NoProjectFound { start: PathBuf, searched: String },

    #[error("Invalid app.json at {path}: {error}")]
    InvalidAppJson { path: PathBuf, error: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
