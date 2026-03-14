//! Unified error hierarchy for the AL workspace.
//!
//! All errors from al-core operations flow through `AlError`.
//! This replaces scattered error types across crates with a single,
//! structured error enum that supports both human-readable messages
//! and machine-readable error codes.

use std::path::PathBuf;

use thiserror::Error;

/// Discovery-specific errors with actionable messages.
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

/// Unified error type for al-core operations.
#[derive(Error, Debug)]
pub enum AlError {
    /// Project discovery errors (app.json not found, parse failure, etc.)
    #[error("project error: {0}")]
    Project(String),

    /// Toolchain errors (ALTool not found, wrong version, etc.)
    #[error("toolchain error: {0}")]
    Toolchain(String),

    /// Document errors (file not open, parse failure, etc.)
    #[error("document error: {0}")]
    Document(String),

    /// Symbol resolution errors (symbol not found, ambiguous, etc.)
    #[error("symbol error: {0}")]
    Symbol(String),

    /// Semantic bridge errors (.NET CLR failure, bridge crash, etc.)
    #[error("semantic error: {0}")]
    Semantic(String),

    /// IO errors (file read/write, socket, etc.)
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization errors.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
