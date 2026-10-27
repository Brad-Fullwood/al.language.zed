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
use tracing::{info, warn};

use al_bc::bc_client::{BcClient, BcClientError};
use al_bc::launch::{find_launch_config, BcServerConfig};
use al_compile::{CompileDiagnostic, CompileResult};
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
    #[error(transparent)]
    InvalidLaunchConfig(#[from] al_bc::launch::LaunchConfigError),
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
    #[error("{0}")]
    Unauthorized(String),
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

    let bc_client = BcClient::new(&server_config).map_err(PublishError::Unauthorized)?;

    let upload_success = if config.incremental {
        // RAD is `POST dev/apps` with `IsRad=true` (AppsApiClient.cs:46-49).
        // The id check stays so a malformed app.json is refused before upload.
        extract_app_id_from_manifest(&config.project_root)?;
        do_rad_publish(&bc_client, &app_path, &mut steps).await
    } else {
        do_standard_publish(&bc_client, &app_path, &mut steps).await
    };
    // BC's reply is not read (see `BcClient::publish_extension`), so the
    // identity in the result comes from the manifest that was built.
    let (app_id, app_version) = manifest_identity(&config.project_root);

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

/// Standard full-upload publish: `POST dev/apps`. Returns whether BC answered
/// with a success status, which is Microsoft's rule (AppsApiClient.cs:66).
async fn do_standard_publish(
    bc_client: &BcClient,
    app_path: &Path,
    steps: &mut Vec<PublishStep>,
) -> bool {
    record_upload(
        PublishPhase::Upload,
        bc_client.publish_extension(app_path).await,
        steps,
    )
}

/// RAD incremental deploy: `POST dev/apps` with `IsRad=true`.
async fn do_rad_publish(
    bc_client: &BcClient,
    app_path: &Path,
    steps: &mut Vec<PublishStep>,
) -> bool {
    record_upload(
        PublishPhase::Rad,
        bc_client.rad_publish(app_path).await,
        steps,
    )
}

fn record_upload(
    phase: PublishPhase,
    result: Result<(), al_bc::bc_client::BcClientError>,
    steps: &mut Vec<PublishStep>,
) -> bool {
    let (success, message) = match result {
        Ok(()) => (true, "BC accepted the package".to_string()),
        Err(e) => {
            warn!(error = %e, "Extension upload failed");
            (false, e.to_string())
        }
    };
    steps.push(PublishStep {
        phase,
        success,
        message: Some(message),
    });
    success
}

/// Compile the project, using the workspace's toolchain.
///
/// Native-first: the default path is the pure-Rust native `.app` emitter — no
/// Microsoft `alc`, no C# bridge. `al.useOfficialCompiler: true` opts into
/// Microsoft's `dotnet alc` subprocess instead (logged at WARN as the non-native
/// path). The native path performs syntax/project/binding verification itself
/// and refuses to produce an artifact when it reports blocking diagnostics.
async fn run_compile(
    workspace: &Workspace,
    project_root: &Path,
) -> Result<CompileResult, PublishError> {
    let config = workspace.config.read().await.clone();
    let backend =
        al_compile::BuildBackend::from_use_official_compiler(config.use_official_compiler);
    let (package_cache, dependency_packages) = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| (Some(project.packages_dir.clone()), project.packages.clone()))
        .unwrap_or_default();
    let toolchain = if backend == al_compile::BuildBackend::Alc {
        // Use blocking .read().await rather than try_read() — contention and a
        // genuinely missing toolchain must not collapse into the same error.
        let toolchain = workspace.toolchain.read().await;
        Some(toolchain.clone().ok_or(PublishError::NoToolchain)?)
    } else {
        None
    };
    if backend == al_compile::BuildBackend::Alc {
        warn!(
            "al.useOfficialCompiler=true - publishing with the NON-NATIVE `dotnet alc` subprocess"
        );
    }

    let analyzers = config.effective_code_analyzers(project_root);
    al_compile::build(al_compile::BuildRequest {
        project_root,
        backend,
        toolchain: toolchain.as_ref(),
        dependency_packages: Some(dependency_packages.as_slice()),
        package_cache: package_cache.as_deref(),
        analyzers: Some(analyzers.as_slice()),
        config: al_compile::CompilationConfigOptions::from(&config),
    })
    .await
    .map_err(|error| PublishError::Build(error.to_string()))
}

fn resolve_server_config(
    project_root: &Path,
    config_name: Option<&str>,
) -> Result<BcServerConfig, PublishError> {
    let debug_config = find_launch_config(project_root)?.ok_or(PublishError::NoLaunchConfig)?;

    let chosen = match config_name {
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
    }?;

    // Publishing sends the user's Business Central credential to the server
    // this repository's launch file names. The credential comes from the
    // environment (`BC_ACCESS_TOKEN`, or `BC_USERNAME` and `BC_PASSWORD`), so
    // it is the target the repository chose that needs authorising, not a
    // cached token: nothing in publish reads the OAuth cache.
    al_project::trust::authorize_cached_credential(
        project_root,
        &al_project::trust::BcTarget::from_launch(&chosen),
        al_project::trust::CredentialKind::Environment,
        al_project::trust::TargetSource::Repository,
    )
    .map_err(PublishError::Unauthorized)?;
    Ok(chosen)
}

/// Read `id` and `version` from app.json for the result. Missing or unreadable
/// values give `None`.
fn manifest_identity(project_root: &Path) -> (Option<String>, Option<String>) {
    let json = std::fs::read(project_root.join("app.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let field = |name: &str| {
        json.as_ref()
            .and_then(|j| j.get(name))
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    (field("id"), field("version"))
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
    let id = json
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or_default();
    // BC requires a GUID here.
    if !al_bc::bc_client::is_guid(id) {
        return Err(PublishError::InvalidManifest(format!(
            "`id` must be a GUID, got {id:?}"
        )));
    }
    Ok(id.to_owned())
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
            r#"{"id":"33333333-4444-5555-6666-777777777773","name":"Test","publisher":"Me","version":"1.0.0"}"#,
        )
        .unwrap();
        let id = extract_app_id_from_manifest(dir.path()).unwrap();
        assert_eq!(id, "33333333-4444-5555-6666-777777777773");
    }

    #[test]
    fn extract_app_id_rejects_an_id_that_steers_the_request_path() {
        // BC requires a GUID id. Anything else is refused before upload.
        for id in [
            "../../../admin/SomeEndpoint",
            "33333333-4444-5555-6666-777777777773/../admin",
            "33333333-4444-5555-6666-777777777773?x=1",
            "",
            "   ",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("app.json"),
                serde_json::json!({ "id": id, "name": "Test" }).to_string(),
            )
            .unwrap();
            assert!(
                matches!(
                    extract_app_id_from_manifest(dir.path()),
                    Err(PublishError::InvalidManifest(_))
                ),
                "{id:?} must be refused"
            );
        }
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

    /// `XDG_CONFIG_HOME` is process-wide, so the tests that redirect the trust
    /// store hold this rather than running beside each other.
    static TRUST_STORE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A scratch trust store for one test. Dropping it restores the previous
    /// `XDG_CONFIG_HOME` and releases the lock.
    struct ScratchTrustStore {
        _dir: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl ScratchTrustStore {
        fn new() -> Self {
            let guard = TRUST_STORE_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let dir = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("XDG_CONFIG_HOME");
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
            Self {
                _dir: dir,
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for ScratchTrustStore {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    /// Record `project_root` the way `al-explorer trust` does, against a
    /// scratch store the test owns.
    fn trust_for_test(project_root: &Path) -> ScratchTrustStore {
        let store = ScratchTrustStore::new();
        al_project::trust::grant(project_root).unwrap();
        store
    }

    #[test]
    fn resolve_config_named_match_returns_that_config() {
        let dir = tempfile::tempdir().unwrap();
        write_zed_debug(
            dir.path(),
            r#"[
                {"label":"First","adapter":"al","environmentType":"OnPrem",
                 "server":"https://first.example.com","serverInstance":"BC"},
                {"label":"Second","adapter":"al","environmentType":"OnPrem",
                 "server":"https://second.example.com","serverInstance":"NAV"}
            ]"#,
        );
        let _config = trust_for_test(dir.path());

        let cfg = resolve_server_config(dir.path(), Some("Second")).unwrap();
        assert_eq!(cfg.name, "Second");
        assert_eq!(cfg.server.as_deref(), Some("https://second.example.com"));
        // display_name should reflect the matched (second) config.
        assert_eq!(cfg.display_name(), "https://second.example.com/NAV");
    }

    #[test]
    fn resolve_config_refuses_an_untrusted_on_premises_server() {
        let dir = tempfile::tempdir().unwrap();
        write_zed_debug(
            dir.path(),
            r#"[
                {"label":"Only","adapter":"al","environmentType":"OnPrem",
                 "server":"https://erp.example.com","serverInstance":"BC"}
            ]"#,
        );
        let _store = ScratchTrustStore::new();

        match resolve_server_config(dir.path(), None) {
            Err(PublishError::Unauthorized(message)) => {
                assert!(message.contains("not trusted"), "{message}");
                // Publish reads `BC_ACCESS_TOKEN` or `BC_USERNAME`/`BC_PASSWORD`
                // from the environment and never touches the OAuth cache, so a
                // refusal that names a cached token describes the wrong thing.
                assert!(
                    message.contains("Business Central credentials"),
                    "{message}"
                );
                assert!(!message.contains("cached"), "{message}");
            }
            other => panic!("expected Unauthorized, got {other:?}"),
        }
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
                 "environmentName":"MySandbox","tenant":"1b7a9471-d313-4aa4-99e8-7a6c762c8e7d"},
                {"label":"Beta","adapter":"al","environmentType":"OnPrem",
                 "server":"http://beta.example.com","serverInstance":"BC"}
            ]"#,
        );

        al_bc::tenant_guard::install_test_registry();
        let cfg = resolve_server_config(dir.path(), None).unwrap();
        assert_eq!(cfg.name, "Alpha");
        assert_eq!(cfg.display_name(), "BC Cloud (MySandbox)");
    }

    #[test]
    fn resolve_config_invalid_entry_returns_configuration_error() {
        let dir = tempfile::tempdir().unwrap();
        write_zed_debug(
            dir.path(),
            r#"[
                {"label":"Bad","adapter":"al","environmentType":"NotARealType"}
            ]"#,
        );

        let result = resolve_server_config(dir.path(), None);
        assert!(matches!(result, Err(PublishError::InvalidLaunchConfig(_))));
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
    /// publish endpoint is `{uri}/BC/dev/apps`. Windows auth means no
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
            debug_args: serde_json::json!({}),
        }
    }

    /// Create a throwaway .app file with some bytes for the uploader to read.
    fn dummy_app(dir: &Path) -> PathBuf {
        let p = dir.join("MyApp.app");
        std::fs::write(&p, b"NAVX-fake-app-bytes").unwrap();
        p
    }

    // fixture source: AppsApiClient.cs:38-72 (Microsoft.Dynamics.Nav.Deployment.ApiClients).
    // Microsoft treats the HTTP status as the whole result and never reads the
    // body, so the mocks return an empty body or an arbitrary one.

    #[tokio::test]
    async fn do_standard_publish_succeeds_on_http_200_with_empty_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/apps"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri())).unwrap();
        let mut steps = Vec::new();

        assert!(do_standard_publish(&client, &app, &mut steps).await);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].phase, PublishPhase::Upload);
        assert!(steps[0].success);
    }

    #[tokio::test]
    async fn do_standard_publish_ignores_a_status_field_in_the_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/apps"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "Failed"})),
            )
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri())).unwrap();
        let mut steps = Vec::new();

        assert!(do_standard_publish(&client, &app, &mut steps).await);
    }

    #[tokio::test]
    async fn do_standard_publish_server_error_records_failure_step() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/apps"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri())).unwrap();
        let mut steps = Vec::new();

        assert!(!do_standard_publish(&client, &app, &mut steps).await);
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].phase, PublishPhase::Upload);
        assert!(!steps[0].success);
        assert!(steps[0].message.as_deref().unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn do_rad_publish_sends_is_rad_and_succeeds_on_204() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/apps"))
            .and(query_param("IsRad", "true"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri())).unwrap();
        let mut steps = Vec::new();

        assert!(do_rad_publish(&client, &app, &mut steps).await);
        assert_eq!(steps[0].phase, PublishPhase::Rad);
        assert!(steps[0].success);
    }

    #[tokio::test]
    async fn do_rad_publish_server_error_records_failure_step() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/BC/dev/apps"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let app = dummy_app(dir.path());
        let client = BcClient::new(&mock_config(&server.uri())).unwrap();
        let mut steps = Vec::new();

        assert!(!do_rad_publish(&client, &app, &mut steps).await);
        assert_eq!(steps[0].phase, PublishPhase::Rad);
        assert!(!steps[0].success);
    }

    #[test]
    fn manifest_identity_reads_id_and_version() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("app.json"),
            r#"{"id":"33333333-4444-5555-6666-777777777773","version":"1.2.3.4"}"#,
        )
        .unwrap();
        let (id, version) = manifest_identity(dir.path());
        assert_eq!(id.as_deref(), Some("33333333-4444-5555-6666-777777777773"));
        assert_eq!(version.as_deref(), Some("1.2.3.4"));
        assert_eq!(manifest_identity(&dir.path().join("none")), (None, None));
    }
}
