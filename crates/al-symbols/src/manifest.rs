//! NavxManifest.xml parsing from .app files.

use serde::{Deserialize, Serialize};

/// Parsed NavxManifest.xml from inside a .app file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavxManifest {
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}
