//! NuGet v3 client for downloading BC symbol packages.

use std::path::{Path, PathBuf};

use al_discovery::{AppManifest, NuGetFeed};

/// NuGet package reference.
#[derive(Debug, Clone)]
pub struct PackageRef {
    pub id: String,
    pub version: String,
}

/// NuGet client for symbol downloads.
pub struct NuGetClient {
    _feeds: Vec<NuGetFeed>,
    _client: reqwest::Client,
}

impl NuGetClient {
    pub async fn new(feeds: Vec<NuGetFeed>) -> Self {
        Self {
            _feeds: feeds,
            _client: reqwest::Client::new(),
        }
    }

    pub async fn resolve_dependencies(
        &self,
        manifest: &AppManifest,
    ) -> Vec<PackageRef> {
        let _ = manifest;
        todo!("Implement dependency resolution")
    }

    pub async fn download(&self, pkg: &PackageRef, dest: &Path) -> Result<PathBuf, NuGetError> {
        let _ = (pkg, dest);
        todo!("Implement package download")
    }

    pub async fn download_all(
        &self,
        manifest: &AppManifest,
        dest: &Path,
    ) -> Result<Vec<PathBuf>, NuGetError> {
        let _ = (manifest, dest);
        todo!("Implement download all")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NuGetError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Package not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
