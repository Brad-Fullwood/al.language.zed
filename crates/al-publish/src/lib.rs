//! AL extension publish to BC server.
//!
//! Implements the publish workflow:
//! 1. Compile the AL project into a `.app` file (using `al_compile`).
//! 2. Upload + publish the `.app` to the BC Dev API endpoint.
//! 3. Optionally use RAD (Rapid Application Development) for incremental deploys.
//!
//! Reads BC server connection details from `launch.json` / `.zed/debug.json`
//! via the `launch` module. First valid config is used unless a name is given.

use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;
use tracing::{debug, info, warn};

use al_bc::bc_client::{BcClient, BcClientError};
use al_bc::launch::{find_launch_config, BcServerConfig};
use al_compile::{CompileDiagnostic, CompileResult};
use al_project::toolchain::AlToolchain;
use al_workspace::Workspace;

#[derive(Debug, Clone)]
pub struct PublishConfig {
    pub project_root: PathBuf,
    /// Launch configuration name to use (uses first AL config if None).
    pub config_name: Option<String>,
    /// Use the RAD API for incremental (delta) deploy instead of full upload.
    pub incremental: bool,
}

impl PublishConfig {
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            config_name: None,
            incremental: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PublishPhase {
    Compile,
    Upload,
    Rad,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishStep {
    pub phase: PublishPhase,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishResult {
    pub success: bool,
    pub server: String,
    /// Method used: "standard" or "rad".
    pub method: String,
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
    pub steps: Vec<PublishStep>,
}

#[derive(Debug, Error)]
pub enum PublishError {
    #[error("No launch.json configuration found in project root")]
    NoLaunchConfig,
    #[error("Named configuration '{name}' not found in launch.json")]
    ConfigNotFound { name: String },
    #[error("Toolchain not available (ALTool not installed)")]
    NoToolchain,
    #[error("No project loaded")]
    NoProject,
    #[error("No .app file produced by compiler")]
    NoAppFile,
    #[error("BC server error: {0}")]
    BcServer(#[from] BcClientError),
    #[error("Build error: {0}")]
    Build(String),
    #[error("Invalid app.json: {0}")]
    InvalidManifest(String),
}

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
    let server_config = resolve_server_config(&config.project_root, config.config_name.as_deref())?;
    let server_display = server_config.display_name();
    info!(server = %server_display, incremental = config.incremental, "Starting publish");

    let method = if config.incremental {
        "rad"
    } else {
        "standard"
    };
    let mut steps: Vec<PublishStep> = Vec::new();

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
                diagnostics
                    .iter()
                    .filter(|d| matches!(d.severity, al_compile::DiagnosticSeverity::Error))
                    .count()
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

    let bc_client = BcClient::new(&server_config);

    let (app_id, app_version, upload_success) = if config.incremental {
        let app_id = extract_app_id_from_manifest(&config.project_root)?;
        debug!(app_id = %app_id, "Using RAD incremental deploy");
        match bc_client.rad_publish(&app_id, &app_path).await {
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
                message: resp.status.clone().or_else(|| Some("Uploaded".to_string())),
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
///
/// Native-first: the default path is the pure-Rust native `.app` emitter — no
/// Microsoft `alc`, no C# bridge. `al.useOfficialCompiler: true` opts into
/// Microsoft's `dotnet alc` subprocess instead (logged at WARN as the non-native
/// path). The native emitter does no semantic validation; the LSP supplies
/// diagnostics, and the BC server validates on publish.
async fn run_compile(
    workspace: &Workspace,
    project_root: &Path,
) -> Result<CompileResult, PublishError> {
    let use_official_compiler = workspace.config.read().await.use_official_compiler;

    if !use_official_compiler {
        let result = al_compile::native_compile(project_root);
        if !result.success {
            return Err(PublishError::Build(result.output));
        }
        return Ok(result);
    }

    // Use blocking .read().await rather than try_read() — try_read() maps both
    // lock contention (WouldBlock) and a nonexistent toolchain to the same
    // NoToolchain error, making it impossible to diagnose a "server is busy"
    // situation vs. "no toolchain configured".
    let tc = workspace.toolchain.read().await;
    let toolchain: AlToolchain = tc.clone().ok_or(PublishError::NoToolchain)?;
    drop(tc);

    warn!("al.useOfficialCompiler=true - publishing with the NON-NATIVE `dotnet alc` subprocess");
    al_compile::compile_project(&toolchain, project_root, None)
        .await
        .map_err(|e| PublishError::Build(e.to_string()))
}

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
            .ok_or_else(|| PublishError::ConfigNotFound {
                name: name.to_string(),
            }),
        None => debug_config
            .configs
            .into_iter()
            .next()
            .ok_or(PublishError::NoLaunchConfig),
    }
}

/// Extract the app GUID from app.json (needed for RAD).
fn extract_app_id_from_manifest(project_root: &Path) -> Result<String, PublishError> {
    let path = project_root.join("app.json");
    let bytes = std::fs::read(&path).map_err(|e| {
        PublishError::InvalidManifest(format!("cannot read {}: {e}", path.display()))
    })?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        PublishError::InvalidManifest(format!("cannot parse {}: {e}", path.display()))
    })?;
    json.get("id")
        .and_then(|value| value.as_str())
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| PublishError::InvalidManifest("`id` must be a non-empty string".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_config_defaults() {
        let cfg = PublishConfig::new("/tmp/myproject");
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
        let id = extract_app_id_from_manifest(dir.path()).unwrap();
        assert_eq!(id, "test-guid-123");
    }

    #[test]
    fn extract_app_id_missing_manifest_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let id = extract_app_id_from_manifest(dir.path());
        assert!(matches!(id, Err(PublishError::InvalidManifest(_))));
    }

    #[test]
    fn extract_app_id_malformed_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.json"), b"{ this is not json ]").unwrap();
        assert!(matches!(
            extract_app_id_from_manifest(dir.path()),
            Err(PublishError::InvalidManifest(_))
        ));
    }

    #[test]
    fn extract_app_id_missing_id_field_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            br#"{"name":"Test","publisher":"Me"}"#,
        )
        .unwrap();
        assert!(matches!(
            extract_app_id_from_manifest(dir.path()),
            Err(PublishError::InvalidManifest(_))
        ));
    }

    #[test]
    fn extract_app_id_non_string_id_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.json"), br#"{"id":12345}"#).unwrap();
        assert!(matches!(
            extract_app_id_from_manifest(dir.path()),
            Err(PublishError::InvalidManifest(_))
        ));
    }

    /// Write a `.zed/debug.json` containing the given config entries.
    fn write_zed_debug(dir: &Path, body: &str) {
        let zed = dir.join(".zed");
        std::fs::create_dir_all(&zed).unwrap();
        std::fs::write(zed.join("debug.json"), body).unwrap();
    }

    #[test]
    fn resolve_config_named_match_returns_that_config() {
        let dir = tempfile::tempdir().unwrap();
        write_zed_debug(
            dir.path(),
            r#"[
                {"label":"First","adapter":"al","environmentType":"OnPrem",
                 "server":"http://first.example.com","serverInstance":"BC"},
                {"label":"Second","adapter":"al","environmentType":"OnPrem",
                 "server":"http://second.example.com","serverInstance":"NAV"}
            ]"#,
        );

        let cfg = resolve_server_config(dir.path(), Some("Second")).unwrap();
        assert_eq!(cfg.name, "Second");
        assert_eq!(cfg.server.as_deref(), Some("http://second.example.com"));
        // display_name should reflect the matched (second) config.
        assert_eq!(cfg.display_name(), "http://second.example.com/NAV");
    }

    #[test]
    fn resolve_config_named_no_match_returns_config_not_found() {
        let dir = tempfile::tempdir().unwrap();
        write_zed_debug(
            dir.path(),
            r#"[
                {"label":"First","adapter":"al","environmentType":"OnPrem",
                 "server":"http://first.example.com","serverInstance":"BC"}
            ]"#,
        );

        let result = resolve_server_config(dir.path(), Some("DoesNotExist"));
        match result {
            Err(PublishError::ConfigNotFound { name }) => assert_eq!(name, "DoesNotExist"),
            other => panic!("expected ConfigNotFound, got {other:?}"),
        }
    }

    #[test]
    fn resolve_config_none_picks_first() {
        let dir = tempfile::tempdir().unwrap();
        write_zed_debug(
            dir.path(),
            r#"[
                {"label":"Alpha","adapter":"al","environmentType":"Sandbox",
                 "environmentName":"MySandbox","tenant":"t.onmicrosoft.com"},
                {"label":"Beta","adapter":"al","environmentType":"OnPrem",
                 "server":"http://beta.example.com","serverInstance":"BC"}
            ]"#,
        );

        let cfg = resolve_server_config(dir.path(), None).unwrap();
        assert_eq!(cfg.name, "Alpha");
        assert_eq!(cfg.display_name(), "BC Cloud (MySandbox)");
    }

    #[test]
    fn resolve_config_empty_config_list_returns_no_launch_config() {
        let dir = tempfile::tempdir().unwrap();
        // File parses but every entry is dropped (unknown environmentType), so
        // find_launch_config returns None (it requires non-empty configs).
        write_zed_debug(
            dir.path(),
            r#"[
                {"label":"Bad","adapter":"al","environmentType":"NotARealType"}
            ]"#,
        );

        let result = resolve_server_config(dir.path(), None);
        assert!(matches!(result, Err(PublishError::NoLaunchConfig)));
    }

    #[test]
    fn publish_error_display_messages() {
        assert_eq!(
            PublishError::NoLaunchConfig.to_string(),
            "No launch.json configuration found in project root"
        );
        assert_eq!(
            PublishError::ConfigNotFound {
                name: "Prod".to_string()
            }
            .to_string(),
            "Named configuration 'Prod' not found in launch.json"
        );
        assert_eq!(
            PublishError::NoAppFile.to_string(),
            "No .app file produced by compiler"
        );
    }

    #[test]
    fn publish_result_omits_optional_none_fields() {
        let result = PublishResult {
            success: false,
            server: "BC Cloud (Sandbox)".to_string(),
            method: "rad".to_string(),
            app_path: None,
            app_id: None,
            app_version: None,
            diagnostics: vec![],
            steps: vec![],
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"method\":\"rad\""));
        // None optionals must be absent, not serialized as null.
        assert!(!json.contains("appPath"));
        assert!(!json.contains("appId"));
        assert!(!json.contains("appVersion"));
        assert!(json.contains("\"diagnostics\":[]"));
        assert!(json.contains("\"steps\":[]"));
    }

    #[test]
    fn publish_step_omits_none_message_and_serializes_phase() {
        let step = PublishStep {
            phase: PublishPhase::Rad,
            success: false,
            message: None,
        };
        let json = serde_json::to_string(&step).unwrap();
        assert!(json.contains("\"phase\":\"rad\""));
        assert!(json.contains("\"success\":false"));
        assert!(!json.contains("message"));
    }

    /// Config whose base_url is the wiremock server with instance "BC", so the
    /// publish endpoint is `{uri}/BC/dev/extensions`. Windows auth means no
    /// credentials are required (apply_auth only errors for UserPassword/AAD).
    fn mock_config(uri: &str) -> BcServerConfig {
        use al_bc::launch::{AuthMethod, EnvironmentType};
        BcServerConfig {
            name: "mock".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some(uri.to_string()),
            server_instance: Some("BC".to_string()),
            port: None,
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::Windows,
            accept_invalid_certs: false,
        }
    }

    /// Create a throwaway .app file with some bytes for the uploader to read.
    fn dummy_app(dir: &Path) -> PathBuf {
        let p = dir.join("MyApp.app");
        std::fs::write(&p, b"NAVX-fake-app-bytes").unwrap();
        p
    }

    #[tokio::test]
    async fn do_standard_publish_success_captures_id_and_version() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/extensions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "appId": "abc-123",
                "name": "MyApp",
                "version": "2.0.0.0",
                "status": "Completed"
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri()));
        let mut steps = Vec::new();

        let (app_id, version, success) = do_standard_publish(&client, &app, &mut steps).await;

        assert!(success);
        assert_eq!(app_id.as_deref(), Some("abc-123"));
        assert_eq!(version.as_deref(), Some("2.0.0.0"));
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].phase, PublishPhase::Upload);
        assert!(steps[0].success);
        assert_eq!(steps[0].message.as_deref(), Some("Completed"));
    }

    #[tokio::test]
    async fn do_standard_publish_failed_status_marks_step_unsuccessful() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/extensions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "status": "Failed"
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri()));
        let mut steps = Vec::new();

        let (app_id, version, success) = do_standard_publish(&client, &app, &mut steps).await;

        // A "Failed" status from a 200 response must still mark the step failed.
        assert!(!success);
        assert!(app_id.is_none());
        assert!(version.is_none());
        assert_eq!(steps.len(), 1);
        assert!(!steps[0].success);
        assert_eq!(steps[0].message.as_deref(), Some("Failed"));
    }

    #[tokio::test]
    async fn do_standard_publish_missing_status_defaults_to_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/extensions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "appId": "no-status-app"
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri()));
        let mut steps = Vec::new();

        let (app_id, _version, success) = do_standard_publish(&client, &app, &mut steps).await;

        // No status field => unwrap_or(true): treated as success, with a
        // synthesized "Uploaded" message.
        assert!(success);
        assert_eq!(app_id.as_deref(), Some("no-status-app"));
        assert_eq!(steps[0].message.as_deref(), Some("Uploaded"));
    }

    #[tokio::test]
    async fn do_standard_publish_server_error_records_failure_step() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/extensions"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri()));
        let mut steps = Vec::new();

        let (app_id, version, success) = do_standard_publish(&client, &app, &mut steps).await;

        assert!(!success);
        assert!(app_id.is_none());
        assert!(version.is_none());
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].phase, PublishPhase::Upload);
        assert!(!steps[0].success);
        // The error string from BcClientError should be propagated into the step.
        assert!(steps[0].message.is_some());
    }
}
