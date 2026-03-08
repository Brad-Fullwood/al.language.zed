//! AL toolchain and project discovery.
//!
//! Finds ALTool installation, parses app.json manifests,
//! locates .alpackages, and provides NuGet feed URLs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Paths to the AL toolchain components.
#[derive(Debug, Clone)]
pub struct AlToolchain {
    pub alc: PathBuf,
    pub aldoc: PathBuf,
    pub code_analysis: PathBuf,
    pub analyzers: AnalyzerPaths,
    pub dotnet_root: PathBuf,
    pub version: String,
}

/// Paths to the official Microsoft analyzers.
#[derive(Debug, Clone)]
pub struct AnalyzerPaths {
    pub code_cop: PathBuf,
    pub app_source_cop: PathBuf,
    pub ui_cop: PathBuf,
    pub per_tenant_cop: PathBuf,
    pub common: PathBuf,
}

/// A discovered AL project on disk.
#[derive(Debug, Clone)]
pub struct AlProject {
    pub root: PathBuf,
    pub app_json: AppManifest,
    pub packages_dir: PathBuf,
    pub packages: Vec<PathBuf>,
}

/// Parsed app.json manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    #[serde(default)]
    pub dependencies: Vec<AppDependency>,
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
}

/// A dependency entry in app.json.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDependency {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

/// A NuGet feed for BC symbol packages.
#[derive(Debug, Clone)]
pub struct NuGetFeed {
    pub name: String,
    pub index_url: String,
}

/// Errors with actionable messages.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("ALTool is not installed. Install it with: {install_cmd}")]
    AlToolNotInstalled { install_cmd: String },

    #[error(".NET SDK is not installed")]
    DotNetNotInstalled,

    #[error("No AL project found. Searched: {searched:?}")]
    NoProjectFound { searched: Vec<PathBuf> },

    #[error("Invalid app.json at {path}: {error}")]
    InvalidAppJson { path: PathBuf, error: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Discover the AL toolchain (ALTool installation).
pub fn find_toolchain() -> Result<AlToolchain, DiscoveryError> {
    todo!("Implement toolchain discovery")
}

/// Find an AL project starting from the given directory, searching upward.
pub fn find_project(start: &Path) -> Result<AlProject, DiscoveryError> {
    let _ = start;
    todo!("Implement project discovery")
}

/// Returns the 3 public BC NuGet feeds.
pub fn nuget_feeds() -> Vec<NuGetFeed> {
    vec![
        NuGetFeed {
            name: "AppSource Symbols".into(),
            index_url: "https://dynamicssmb.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/AppSourceSymbols/nuget/v3/index.json".into(),
        },
        NuGetFeed {
            name: "BC Symbols".into(),
            index_url: "https://dynamicssmb.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/MSSymbols/nuget/v3/index.json".into(),
        },
        NuGetFeed {
            name: "BC Public".into(),
            index_url: "https://dynamicssmb.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/BCPublic/nuget/v3/index.json".into(),
        },
    ]
}
