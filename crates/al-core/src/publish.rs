//! AL extension publish to BC server.
//!
//! Implements the publish workflow:
//! 1. Compile the AL project into a `.app` file (using `al-core::build`).
//! 2. Upload + publish the `.app` to the BC Dev API endpoint.
//! 3. Optionally use RAD (Rapid Application Development) for incremental deploys.
//!
//! Reads BC server connection details from `launch.json` / `.zed/debug.json`
//! via the `launch` module. First valid config is used unless a name is given.

use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::bc_client::{BcClient, BcClientError};
use crate::build::{CompileDiagnostic, CompileResult};
use crate::launch::{find_launch_config, BcServerConfig};
use crate::toolchain::AlToolchain;
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Options for a publish operation.
#[derive(Debug, Clone)]
pub struct PublishConfig {
    /// Project root directory.
    pub project_root: PathBuf,
    /// Launch configuration name to use (uses first AL config if None).
    pub config_name: Option<String>,
    /// Skip attaching the debugger after publish.
    pub no_debug: bool,
    /// Use the RAD API for incremental (delta) deploy instead of full upload.
    pub incremental: bool,
}

impl PublishConfig {
    /// Build a config for the given project root with default options.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            config_name: None,
            no_debug: false,
            incremental: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// Phase of the publish pipeline where an operation occurred.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PublishPhase {
    Compile,
    Upload,
    Install,
    Rad,
}

/// A single step result in the publish pipeline.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishStep {
    pub phase: PublishPhase,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Result of a publish operation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    /// Whether the entire publish pipeline succeeded.
    pub success: bool,
    /// Server configuration used.
    pub server: String,
    /// Method used: "standard" or "rad".
    pub method: String,
    /// Path of the .app file that was published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_path: Option<String>,
    /// App ID assigned by BC server (if returned).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// App version (from BC server response, if returned).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    /// Compilation diagnostics (only present if compilation was run).
    pub diagnostics: Vec<CompileDiagnostic>,
    /// Step-by-step results.
    pub steps: Vec<PublishStep>,
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors that can occur during publish.
#[derive(Debug, Error)]
pub enum PublishError {
    #[error("No launch.json configuration found in project root")]
    NoLaunchConfig,
    #[error("Named configuration '{name}' not found in launch.json")]
    ConfigNotFound { name: String },
    #[error("Toolchain not available (ALTool not installed)")]
    NoToolchain,
    #[error("Toolchain lock is busy — another operation is in progress, please try again")]
    ToolchainBusy,
    #[error("No project loaded")]
    NoProject,
    #[error("Compilation failed with {count} error(s)")]
    CompilationFailed { count: usize },
    #[error("No .app file produced by compiler")]
    NoAppFile,
    #[error("BC server error: {0}")]
    BcServer(#[from] BcClientError),
    #[error("Build error: {0}")]
    Build(String),
}

// ---------------------------------------------------------------------------
// Core publish function
// ---------------------------------------------------------------------------

/// Run the publish pipeline for the project.
///
/// Steps:
/// 1. Resolve BC server config from `launch.json`.
/// 2. Compile the project (builds the `.app` file).
/// 3. Upload + publish to BC via the Dev API.
///
/// When `config.incremental` is true, uses the RAD delta-deploy API instead
/// of the full upload.
///
/// Returns `PublishResult` describing each step.  Does NOT fail on compilation
/// diagnostics — those are included in the result for the caller to inspect.
pub async fn publish(
    workspace: &Workspace,
    config: &PublishConfig,
) -> Result<PublishResult, PublishError> {
    // 1. Resolve server config
    let server_config = resolve_server_config(&config.project_root, config.config_name.as_deref())?;
    let server_display = server_config.display_name();
    info!(server = %server_display, incremental = config.incremental, "Starting publish");

    let method = if config.incremental { "rad" } else { "standard" };
    let mut steps: Vec<PublishStep> = Vec::new();

    // 2. Compile
    let compile_result = run_compile(workspace, &config.project_root).await?;
    let compile_success = compile_result.success;
    let app_path = compile_result.app_path.clone();
    let diagnostics = compile_result.diagnostics.clone();

    steps.push(PublishStep {
        phase: PublishPhase::Compile,
        success: compile_success,
        message: if compile_success {
            app_path.as_ref().map(|p| format!("Built: {}", p.display()))
        } else {
            Some(format!(
                "{} compilation error(s)",
                diagnostics.iter().filter(|d| matches!(d.severity, crate::build::DiagnosticSeverity::Error)).count()
            ))
        },
    });

    if !compile_success {
        return Ok(PublishResult {
            success: false,
            server: server_display,
            method: method.to_string(),
            app_path: None,
            app_id: None,
            app_version: None,
            diagnostics,
            steps,
        });
    }

    let app_path = app_path.ok_or(PublishError::NoAppFile)?;

    // 3. Upload to BC
    let bc_client = BcClient::new(&server_config);

    let (app_id, app_version, upload_success) = if config.incremental {
        // RAD: incremental deploy
        let app_id = extract_app_id_from_manifest(&config.project_root);
        match app_id {
            Some(id) => {
                debug!(app_id = %id, "Using RAD incremental deploy");
                match bc_client.rad_publish(&id, &app_path).await {
                    Ok(resp) => {
                        let success = resp.status.as_deref() != Some("Failed");
                        steps.push(PublishStep {
                            phase: PublishPhase::Rad,
                            success,
                            message: resp.status.clone(),
                        });
                        (resp.app_id, resp.version, success)
                    }
                    Err(e) => {
                        warn!(error = %e, "RAD publish failed");
                        steps.push(PublishStep {
                            phase: PublishPhase::Rad,
                            success: false,
                            message: Some(e.to_string()),
                        });
                        (None, None, false)
                    }
                }
            }
            None => {
                warn!("Cannot use RAD: app.json has no 'id' field, falling back to standard publish");
                do_standard_publish(&bc_client, &app_path, &mut steps).await
            }
        }
    } else {
        do_standard_publish(&bc_client, &app_path, &mut steps).await
    };

    let overall_success = compile_success && upload_success;
    if overall_success {
        info!(server = %server_display, method, "Publish succeeded");
    } else {
        warn!(server = %server_display, method, "Publish failed");
    }

    Ok(PublishResult {
        success: overall_success,
        server: server_display,
        method: method.to_string(),
        app_path: Some(app_path.display().to_string()),
        app_id,
        app_version,
        diagnostics,
        steps,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Standard full-upload publish pipeline.
///
/// Returns `(app_id, app_version, success)`.
async fn do_standard_publish(
    bc_client: &BcClient,
    app_path: &Path,
    steps: &mut Vec<PublishStep>,
) -> (Option<String>, Option<String>, bool) {
    match bc_client.publish_extension(app_path).await {
        Ok(resp) => {
            let success = resp
                .status
                .as_deref()
                .map(|s| s != "Failed")
                .unwrap_or(true);
            steps.push(PublishStep {
                phase: PublishPhase::Upload,
                success,
                message: resp
                    .status
                    .clone()
                    .or_else(|| Some("Uploaded".to_string())),
            });
            (resp.app_id, resp.version, success)
        }
        Err(e) => {
            warn!(error = %e, "Extension upload failed");
            steps.push(PublishStep {
                phase: PublishPhase::Upload,
                success: false,
                message: Some(e.to_string()),
            });
            (None, None, false)
        }
    }
}

/// Compile the project, using the workspace's toolchain.
async fn run_compile(
    workspace: &Workspace,
    project_root: &Path,
) -> Result<CompileResult, PublishError> {
    // Use blocking .read().await rather than try_read() — try_read() maps both
    // lock contention (WouldBlock) and a nonexistent toolchain to the same
    // NoToolchain error, making it impossible to diagnose a "server is busy"
    // situation vs. "no toolchain configured".
    let tc = workspace.toolchain.read().await;
    let toolchain: AlToolchain = tc.clone().ok_or(PublishError::NoToolchain)?;
    drop(tc);

    crate::build::compile_project(&toolchain, project_root, None)
        .await
        .map_err(|e| PublishError::Build(e.to_string()))
}

/// Find the first matching BC server config from launch.json.
fn resolve_server_config(
    project_root: &Path,
    config_name: Option<&str>,
) -> Result<BcServerConfig, PublishError> {
    let debug_config = find_launch_config(project_root).ok_or(PublishError::NoLaunchConfig)?;

    match config_name {
        Some(name) => debug_config
            .configs
            .into_iter()
            .find(|c| c.name == name)
            .ok_or_else(|| PublishError::ConfigNotFound { name: name.to_string() }),
        None => debug_config
            .configs
            .into_iter()
            .next()
            .ok_or(PublishError::NoLaunchConfig),
    }
}

/// Extract the app GUID from app.json (needed for RAD).
fn extract_app_id_from_manifest(project_root: &Path) -> Option<String> {
    let bytes = std::fs::read(project_root.join("app.json")).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("id")?.as_str().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_config_defaults() {
        let cfg = PublishConfig::new("/tmp/myproject");
        assert!(!cfg.no_debug);
        assert!(!cfg.incremental);
        assert!(cfg.config_name.is_none());
        assert_eq!(cfg.project_root, PathBuf::from("/tmp/myproject"));
    }

    #[test]
    fn publish_result_serializes() {
        let result = PublishResult {
            success: true,
            server: "localhost/BC".to_string(),
            method: "standard".to_string(),
            app_path: Some("/tmp/MyPub_MyApp_1.0.0.0.app".to_string()),
            app_id: Some("guid-123".to_string()),
            app_version: Some("1.0.0.0".to_string()),
            diagnostics: vec![],
            steps: vec![PublishStep {
                phase: PublishPhase::Compile,
                success: true,
                message: Some("Built".to_string()),
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"method\":\"standard\""));
        assert!(json.contains("\"phase\":\"compile\""));
    }

    #[test]
    fn resolve_config_no_launch_json_returns_error() {
        // Use a temp dir with no launch.json
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_server_config(dir.path(), None);
        assert!(matches!(result, Err(PublishError::NoLaunchConfig)));
    }

    #[test]
    fn extract_app_id_from_valid_manifest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"id":"test-guid-123","name":"Test","publisher":"Me","version":"1.0.0"}"#,
        )
        .unwrap();
        let id = extract_app_id_from_manifest(dir.path());
        assert_eq!(id.as_deref(), Some("test-guid-123"));
    }

    #[test]
    fn extract_app_id_missing_manifest_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let id = extract_app_id_from_manifest(dir.path());
        assert!(id.is_none());
    }
}
