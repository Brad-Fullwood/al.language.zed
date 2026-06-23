//! Unified error hierarchy for the AL workspace.
//!
//! `AlError` wraps crate-level errors from crate::semantic and crate::symbols so that
//! al-core functions can use `Result<T, AlError>` with `?` conversion throughout.
//! Query functions that return `Option<T>` for "nothing found" cases do NOT use
//! AlError — Option is the correct type there.

use std::path::PathBuf;

use thiserror::Error;

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

#[derive(Error, Debug)]
pub enum AlError {
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),

    /// Semantic bridge errors (.NET CLR failure, bridge crash, etc.)
    #[error(transparent)]
    Semantic(#[from] al_semantic::SemanticError),

    #[error("document not open: {0}")]
    DocumentNotOpen(String),

    #[error("bridge restart limit exceeded ({attempts} attempts, max {max})")]
    BridgeRestartLimitExceeded { attempts: u32, max: u32 },

    #[error("no toolchain available")]
    NoToolchain,

    #[error("semantic bridge task panicked")]
    BridgePanicked,

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// AL compilation (`alc`) exceeded the configured timeout.
    #[error("alc compile timed out after {0} seconds")]
    BuildTimeout(u64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    #[test]
    fn from_discovery_error() {
        let discovery_err = DiscoveryError::NoProjectFound {
            start: PathBuf::from("/tmp/test"),
            searched: "/tmp, /".to_string(),
        };
        let al_err: AlError = discovery_err.into();
        assert!(matches!(al_err, AlError::Discovery(_)));
        let msg = al_err.to_string();
        assert!(msg.contains("No AL project found"), "got: {msg}");
    }

    #[test]
    fn from_semantic_error() {
        let sem_err = al_semantic::SemanticError::Timeout(Duration::from_secs(5));
        let al_err: AlError = sem_err.into();
        assert!(matches!(al_err, AlError::Semantic(_)));
        let msg = al_err.to_string();
        assert!(msg.contains("timed out"), "got: {msg}");
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let al_err: AlError = io_err.into();
        assert!(matches!(al_err, AlError::Io(_)));
        assert!(al_err.to_string().contains("file missing"));
    }

    #[test]
    fn document_not_open_display() {
        let err = AlError::DocumentNotOpen("file:///test.al".to_string());
        assert_eq!(err.to_string(), "document not open: file:///test.al");
    }

    #[test]
    fn bridge_restart_limit_display() {
        let err = AlError::BridgeRestartLimitExceeded {
            attempts: 4,
            max: 3,
        };
        assert_eq!(
            err.to_string(),
            "bridge restart limit exceeded (4 attempts, max 3)"
        );
    }

    #[test]
    fn no_toolchain_display() {
        let err = AlError::NoToolchain;
        assert_eq!(err.to_string(), "no toolchain available");
    }

    #[test]
    fn question_mark_conversion_compiles() {
        // Verify `?` works for each From impl in a function returning AlError
        fn _discovery() -> Result<(), AlError> {
            Err(DiscoveryError::DotNetNotInstalled)?
        }
        fn _semantic() -> Result<(), AlError> {
            Err(al_semantic::SemanticError::NotInitialized)?
        }
        fn _io() -> Result<(), AlError> {
            Err(std::io::Error::other("test"))?
        }
        assert!(_discovery().is_err());
        assert!(_semantic().is_err());
        assert!(_io().is_err());
    }
}
