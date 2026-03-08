//! .app file reader — NAVX header + ZIP extraction.

use std::path::Path;

use crate::model::SymbolPackage;

/// Read a .app file from disk.
pub fn read_app(path: &Path) -> Result<SymbolPackage, AppReaderError> {
    let data = std::fs::read(path)?;
    read_app_bytes(&data)
}

/// Read a .app file from bytes.
pub fn read_app_bytes(data: &[u8]) -> Result<SymbolPackage, AppReaderError> {
    let _ = data;
    todo!("Port .app reader from v2")
}

#[derive(Debug, thiserror::Error)]
pub enum AppReaderError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Invalid NAVX header")]
    InvalidHeader,
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
