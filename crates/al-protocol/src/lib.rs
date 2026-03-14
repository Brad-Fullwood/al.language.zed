//! Shared types and discovery for the AL language server ecosystem.
//!
//! This is a leaf-level crate with no workspace dependencies. All crates
//! can depend on it without creating circular dependencies.
//!
//! - **Types**: `AlToolchain`, `AlProject`, `AppDependency`, `BcServerConfig`, etc.
//! - **Discovery**: `find_project()`, `find_toolchain()`, `find_launch_config()`
//! - **JSON-RPC**: `Request`, `Response`, `RpcError`, `error_codes`

pub mod errors;
pub mod jsonrpc;
pub mod launch;
pub mod project;
pub mod toolchain;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Toolchain types
// ---------------------------------------------------------------------------

/// Paths to the AL toolchain components.
#[derive(Debug, Clone)]
pub struct AlToolchain {
    pub alc: PathBuf,
    pub aldoc: Option<PathBuf>,
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

// ---------------------------------------------------------------------------
// Project types
// ---------------------------------------------------------------------------

/// A discovered AL project on disk.
#[derive(Debug, Clone)]
pub struct AlProject {
    pub root: PathBuf,
    pub app_json: AppManifest,
    pub packages_dir: PathBuf,
    pub packages: Vec<PathBuf>,
    /// Server configs from launch.json for downloading symbols from a BC instance.
    pub server_configs: Vec<launch::BcServerConfig>,
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

// Well-known BC package GUIDs for implicit dependencies.
const APPLICATION_APP_ID: &str = "c1335042-3002-4257-bf8a-75c898ccb1b8";
const BASE_APPLICATION_APP_ID: &str = "437dbf0e-84ff-417a-965d-ed2bb9650972";
const BUSINESS_FOUNDATION_APP_ID: &str = "f3552374-a1f2-4356-848e-196002525837";
const SYSTEM_APPLICATION_APP_ID: &str = "63ca2fa4-4f03-4f2b-a480-172fef340d3f";
const SYSTEM_APP_ID: &str = "8874ed3a-0643-4247-9ced-7a7002f7135d";

impl AlProject {
    /// Compute the full dependency list including implicit BC dependencies.
    pub fn all_dependencies(&self) -> Vec<AppDependency> {
        let mut deps = self.app_json.dependencies.clone();

        if let Some(app_version) = &self.app_json.application {
            for (id, name) in [
                (APPLICATION_APP_ID, "Application"),
                (BASE_APPLICATION_APP_ID, "Base Application"),
                (BUSINESS_FOUNDATION_APP_ID, "Business Foundation"),
                (SYSTEM_APPLICATION_APP_ID, "System Application"),
            ] {
                if !deps.iter().any(|d| d.id == id) {
                    deps.push(AppDependency {
                        id: id.to_string(),
                        name: name.to_string(),
                        publisher: "Microsoft".to_string(),
                        version: app_version.clone(),
                    });
                }
            }
        }

        if self.app_json.platform.is_some()
            && !deps.iter().any(|d| d.id == SYSTEM_APP_ID)
        {
            let platform_version = self
                .app_json
                .application
                .as_ref()
                .and_then(|v| v.split('.').next())
                .map(|major| format!("{}.0.0.0", major))
                .unwrap_or_else(|| "26.0.0.0".to_string());
            deps.push(AppDependency {
                id: SYSTEM_APP_ID.to_string(),
                name: "System".to_string(),
                publisher: "Microsoft".to_string(),
                version: platform_version,
            });
        }

        deps
    }
}
