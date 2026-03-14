//! Unified error hierarchy for the AL workspace.
//!
//! All errors from al-core operations flow through `AlError`.
//! This replaces scattered error types across crates with a single,
//! structured error enum that supports both human-readable messages
//! and machine-readable error codes.

use thiserror::Error;

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
