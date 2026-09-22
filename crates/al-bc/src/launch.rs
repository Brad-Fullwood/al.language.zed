//! Debug/launch configuration types and parsing.
//!
//! Reads BC server connection details from debug configuration files.
//! Supports both Zed (`.zed/debug.json`) and VS Code (`.vscode/launch.json`).

use std::path::{Path, PathBuf};

use al_types::strip_json_comments;
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use al_types::AppDependency;

pub use al_types::EnvironmentType;

pub use al_types::AuthMethod;

#[derive(Debug, Clone)]
pub struct DebugConfigFile {
    pub path: PathBuf,
    pub configs: Vec<BcServerConfig>,
}

#[derive(Debug, Error)]
#[error("Invalid AL launch configuration at '{}': {message}", path.display())]
pub struct LaunchConfigError {
    pub path: PathBuf,
    pub message: String,
}

impl LaunchConfigError {
    fn new(path: &Path, error: impl std::fmt::Display) -> Self {
        Self {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BcServerConfig {
    pub name: String,
    pub environment_type: EnvironmentType,
    /// On-prem server URL (e.g., "https://erp.example.com")
    pub server: Option<String>,
    /// On-prem server instance name (e.g., "BC")
    pub server_instance: Option<String>,
    /// Dev services port (default 7049)
    pub port: Option<u16>,
    /// Cloud environment name (e.g., "Sandbox")
    pub environment_name: Option<String>,
    /// Tenant ID (Azure AD domain or "default" for on-prem)
    pub tenant: Option<String>,
    pub authentication: AuthMethod,
    /// Accept invalid/self-signed TLS certificates. Defaults to `false`.
    /// Set to `true` only for on-prem servers with self-signed certs.
    pub accept_invalid_certs: bool,
    /// Complete launch/debug object as written by the user. Connection,
    /// publishing, test, and native-DAP consumers share this parsed source so
    /// debug-only fields are not silently discarded by the connection model.
    pub debug_args: serde_json::Value,
}

impl BcServerConfig {
    /// Construct the `/dev/packages` URL for downloading a single dependency.
    pub fn dev_packages_url(&self, dep: &AppDependency) -> Option<String> {
        let query = format!(
            "publisher={}&appName={}&versionText={}",
            urlencoding::encode(&dep.publisher),
            urlencoding::encode(&dep.name),
            urlencoding::encode(&dep.version),
        );

        match self.environment_type {
            EnvironmentType::OnPrem => {
                let server = server_with_scheme(self.server.as_deref()?)?;
                let instance = self.server_instance.as_deref()?;
                let base = if let Some(port) = self.port {
                    format!("{}:{}", server.trim_end_matches('/'), port)
                } else {
                    server.trim_end_matches('/').to_string()
                };
                let tenant_param = self
                    .tenant
                    .as_deref()
                    .map(|t| format!("&tenant={}", urlencoding::encode(t)))
                    .unwrap_or_default();
                Some(format!(
                    "{}/{}/dev/packages?{}{}",
                    base,
                    urlencoding::encode(instance),
                    query,
                    tenant_param
                ))
            }
            EnvironmentType::Sandbox | EnvironmentType::Production => {
                let tenant = self.tenant.as_deref()?;
                let env_name = self.environment_name.as_deref()?;
                Some(format!(
                    "https://api.businesscentral.dynamics.com/v2.0/{}/{}/dev/packages?{}",
                    urlencoding::encode(tenant),
                    urlencoding::encode(env_name),
                    query
                ))
            }
        }
    }

    pub fn display_name(&self) -> String {
        match self.environment_type {
            EnvironmentType::OnPrem => {
                let server = self.server.as_deref().unwrap_or("?");
                let instance = self.server_instance.as_deref().unwrap_or("?");
                format!("{}/{}", server, instance)
            }
            EnvironmentType::Sandbox | EnvironmentType::Production => {
                let env = self.environment_name.as_deref().unwrap_or("?");
                format!("BC Cloud ({})", env)
            }
        }
    }
}

/// Choose the launch configuration a command should act on.
///
/// A named configuration must match exactly: falling back to the first entry
/// would mask a typo and could route a symbol download or a test run at the
/// wrong Business Central environment. With no name, the first entry is the
/// project's own default, and the caller reports which one that was so the
/// choice is never silent.
pub fn pick_config<'a>(
    configs: &'a [BcServerConfig],
    requested_name: Option<&str>,
) -> Result<&'a BcServerConfig, String> {
    match requested_name {
        Some(name) => configs.iter().find(|c| c.name == name).ok_or_else(|| {
            let known: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
            format!("Debug config {name:?} not found. Known configs: {known:?}")
        }),
        None => configs
            .first()
            .ok_or_else(|| "Project debug configuration file has no configs".to_string()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ZedDebugConfigJson {
    #[serde(default)]
    label: String,
    #[serde(default)]
    environment_type: Option<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    server_instance: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    environment_name: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    authentication: Option<String>,
    #[serde(default)]
    accept_invalid_certs: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VsCodeLaunchConfigJson {
    #[serde(default)]
    name: String,
    #[serde(default)]
    environment_type: Option<String>,
    #[serde(default)]
    server: Option<String>,
    #[serde(default)]
    server_instance: Option<String>,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    environment_name: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    authentication: Option<String>,
    #[serde(default)]
    accept_invalid_certs: bool,
}

pub fn find_launch_config(
    project_root: &Path,
) -> Result<Option<DebugConfigFile>, LaunchConfigError> {
    let zed_path = project_root.join(".zed").join("debug.json");
    if launch_file_exists(&zed_path)? {
        match parse_zed_debug_file(&zed_path) {
            Ok(df) if !df.configs.is_empty() => {
                debug!(path = %zed_path.display(), configs = df.configs.len(), "Found Zed debug configuration");
                return Ok(Some(df));
            }
            Ok(_) => {
                debug!(path = %zed_path.display(), "Zed debug.json found but no AL configurations");
            }
            Err(e) => {
                return Err(LaunchConfigError::new(&zed_path, e));
            }
        }
    }

    let vscode_path = project_root.join(".vscode").join("launch.json");
    if launch_file_exists(&vscode_path)? {
        match parse_vscode_launch_file(&vscode_path) {
            Ok(df) if !df.configs.is_empty() => {
                debug!(path = %vscode_path.display(), configs = df.configs.len(), "Found VS Code launch configuration");
                return Ok(Some(df));
            }
            Ok(_) => {
                debug!(path = %vscode_path.display(), "VS Code launch.json found but no AL configurations");
            }
            Err(e) => {
                return Err(LaunchConfigError::new(&vscode_path, e));
            }
        }
    }

    Ok(None)
}

fn launch_file_exists(path: &Path) -> Result<bool, LaunchConfigError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(LaunchConfigError::new(path, "path is not a regular file")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(LaunchConfigError::new(path, error)),
    }
}

/// Maximum bytes accepted for a debug-config file (Zed `debug.json` or
/// VS Code `launch.json`). Real launch configs are kilobytes at most; 1 MiB
/// is two orders of magnitude past anything legitimate while refusing
/// pathological inputs such as sparse or untrusted files that would OOM
/// the daemon on `read_to_string`.
const MAX_LAUNCH_FILE_BYTES: u64 = 1_048_576;

/// Allowlist check on the `server` field of an OnPrem BC config.
///
/// AL launch configs let users specify the BC server URL freely. A misconfigured
/// or untrusted config could use `file:///etc/passwd` or `gopher://...` and
/// that URL would be handed unchanged to the BC HTTP client. Restrict to
/// http(s):// (the only two schemes the BC dev API uses) or bare hostnames
/// (e.g. `localhost`, where the BC client default-prepends http://).
pub fn is_safe_http_server(server: &str) -> bool {
    let s = server.trim();
    if s.is_empty() {
        return false;
    }
    if let Some((scheme, _)) = s.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        return scheme == "http" || scheme == "https";
    }
    // A bare host has no `://`, so the only `:` it can carry is a port
    // separator. Reject anything else after it, which catches a scheme written
    // without slashes (`javascript:alert(1)`, `file:/etc/passwd`).
    match s.split_once(':') {
        Some((host, port)) => {
            !host.is_empty() && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit())
        }
        None => true,
    }
}

/// `server` with a scheme, or `None` when it is not an acceptable BC server.
///
/// Both URL builders need this: `build_base_url` prepends `http://` to a bare
/// host, and `dev_packages_url` used not to, so a `launch.json` with
/// `"server": "bc.example.com"` published fine and then failed symbol download
/// with `url::Url` reading `bc.example.com` as the scheme.
pub fn server_with_scheme(server: &str) -> Option<String> {
    let server = server.trim();
    if !is_safe_http_server(server) {
        warn!(
            server = %server,
            "BC server URL must be http(s):// or a bare host; refusing to build a request URL"
        );
        return None;
    }
    if server.starts_with("http://") || server.starts_with("https://") {
        return Some(server.to_string());
    }
    // Not silent: defaulting to http:// here means Basic (UserPassword/Windows)
    // credentials go out Base64-in-cleartext. Say so, so an operator who wanted
    // TLS notices a plain hostname was misread as http.
    warn!(
        server = %server,
        "BC server URL has no scheme — defaulting to http:// (cleartext); Basic/Windows \
         credentials will be sent unencrypted. Use an explicit https:// URL to avoid this."
    );
    Some(format!("http://{server}"))
}

fn read_launch_file_capped(path: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_LAUNCH_FILE_BYTES {
        return Err(format!(
            "{} is {} bytes — refusing to parse (cap = {} bytes)",
            path.display(),
            size,
            MAX_LAUNCH_FILE_BYTES
        )
        .into());
    }
    Ok(std::fs::read_to_string(path)?)
}

/// Whether a raw debug configuration is an AL one. `kind_key` is `type` in a
/// VS Code `launch.json` and `adapter` in a Zed `debug.json`.
///
/// This has to run before typed deserialization. One unrelated configuration
/// with a same-named key of a different type used to fail `from_value` for the
/// whole file, so no AL configuration was found at all: the VS Code Java
/// "Attach to Remote Program" snippet writes `"port": "<debug port of
/// debuggee>"` verbatim, and `port` is an `Option<u16>` here.
fn is_al_debug_config(value: &serde_json::Value, kind_key: &str) -> bool {
    value.get(kind_key).and_then(serde_json::Value::as_str) == Some("al")
        || value
            .get("environmentType")
            .and_then(serde_json::Value::as_str)
            .is_some()
}

fn parse_zed_debug_file(path: &Path) -> Result<DebugConfigFile, Box<dyn std::error::Error>> {
    let content = read_launch_file_capped(path)?;
    let clean = strip_json_comments(&content);
    let configs_raw: Vec<serde_json::Value> = serde_json::from_str(&clean)?;

    let configs: Vec<BcServerConfig> = configs_raw
        .into_iter()
        .enumerate()
        .filter(|(_, debug_args)| is_al_debug_config(debug_args, "adapter"))
        .map(|(index, debug_args)| {
            let config: ZedDebugConfigJson = serde_json::from_value(debug_args.clone())
                .map_err(|error| format!("configuration {index}: {error}"))?;
            convert_zed_config(config, debug_args)
                .map_err(|error| format!("configuration {index}: {error}"))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(DebugConfigFile {
        path: path.to_path_buf(),
        configs,
    })
}

fn parse_vscode_launch_file(path: &Path) -> Result<DebugConfigFile, Box<dyn std::error::Error>> {
    let content = read_launch_file_capped(path)?;
    let clean = strip_json_comments(&content);
    let raw_value: serde_json::Value = serde_json::from_str(&clean)?;
    let raw_configs = raw_value
        .get("configurations")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    let configs: Vec<BcServerConfig> = raw_configs
        .into_iter()
        .enumerate()
        .filter(|(_, debug_args)| is_al_debug_config(debug_args, "type"))
        .map(|(index, debug_args)| {
            let config: VsCodeLaunchConfigJson = serde_json::from_value(debug_args.clone())
                .map_err(|error| format!("configuration {index}: {error}"))?;
            convert_vscode_config(config, debug_args)
                .map_err(|error| format!("configuration {index}: {error}"))
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(DebugConfigFile {
        path: path.to_path_buf(),
        configs,
    })
}

fn convert_zed_config(
    raw: ZedDebugConfigJson,
    debug_args: serde_json::Value,
) -> Result<BcServerConfig, String> {
    let environment_type = raw
        .environment_type
        .as_deref()
        .ok_or_else(|| "AL configuration is missing environmentType".to_string())?;
    let env_type = parse_environment_type(environment_type)?;
    let auth = parse_auth_method(raw.authentication.as_deref(), &env_type)?;
    Ok(BcServerConfig {
        name: raw.label,
        environment_type: env_type,
        server: raw.server,
        server_instance: raw.server_instance,
        port: raw.port,
        environment_name: raw.environment_name,
        tenant: raw.tenant,
        authentication: auth,
        accept_invalid_certs: raw.accept_invalid_certs,
        debug_args,
    })
}

fn convert_vscode_config(
    raw: VsCodeLaunchConfigJson,
    debug_args: serde_json::Value,
) -> Result<BcServerConfig, String> {
    let environment_type = raw
        .environment_type
        .as_deref()
        .ok_or_else(|| "AL configuration is missing environmentType".to_string())?;
    let env_type = parse_environment_type(environment_type)?;
    let auth = parse_auth_method(raw.authentication.as_deref(), &env_type)?;
    Ok(BcServerConfig {
        name: raw.name,
        environment_type: env_type,
        server: raw.server,
        server_instance: raw.server_instance,
        port: raw.port,
        environment_name: raw.environment_name,
        tenant: raw.tenant,
        authentication: auth,
        accept_invalid_certs: raw.accept_invalid_certs,
        debug_args,
    })
}

fn parse_environment_type(s: &str) -> Result<EnvironmentType, String> {
    match s {
        "OnPrem" => Ok(EnvironmentType::OnPrem),
        "Sandbox" => Ok(EnvironmentType::Sandbox),
        "Production" => Ok(EnvironmentType::Production),
        other => Err(format!(
            "unknown environmentType {other:?}; expected OnPrem, Sandbox, or Production"
        )),
    }
}

fn parse_auth_method(s: Option<&str>, env_type: &EnvironmentType) -> Result<AuthMethod, String> {
    match s {
        Some("UserPassword") => Ok(AuthMethod::UserPassword),
        Some("Windows") => Ok(AuthMethod::Windows),
        Some("AAD") | Some("MicrosoftEntraID") => Ok(AuthMethod::AAD),
        None if *env_type == EnvironmentType::OnPrem => Ok(AuthMethod::Windows),
        None => Ok(AuthMethod::AAD),
        Some(other) => Err(format!(
            "unknown authentication {other:?}; expected UserPassword, Windows, AAD, or MicrosoftEntraID"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tempdir(name: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "al-bc-launch-test-{}-{}-{}",
            name,
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn named_config(name: &str) -> BcServerConfig {
        BcServerConfig {
            name: name.to_string(),
            environment_type: EnvironmentType::Sandbox,
            server: None,
            server_instance: None,
            port: None,
            environment_name: Some(name.to_string()),
            tenant: None,
            authentication: AuthMethod::AAD,
            accept_invalid_certs: false,
            debug_args: serde_json::Value::Null,
        }
    }

    #[test]
    fn pick_config_without_a_name_takes_the_projects_first_entry() {
        let configs = [named_config("Sandbox"), named_config("Production")];
        assert_eq!(pick_config(&configs, None).unwrap().name, "Sandbox");
    }

    #[test]
    fn pick_config_matches_a_name_exactly_and_never_falls_back() {
        let configs = [named_config("Sandbox"), named_config("Production")];
        assert_eq!(
            pick_config(&configs, Some("Production")).unwrap().name,
            "Production"
        );
        let error = pick_config(&configs, Some("Prodction")).unwrap_err();
        assert!(error.contains("Prodction"), "{error}");
        assert!(
            error.contains("Sandbox") && error.contains("Production"),
            "the error must list the known configs: {error}"
        );
    }

    #[test]
    fn pick_config_on_an_empty_list_is_an_error() {
        assert!(pick_config(&[], None).is_err());
    }

    #[test]
    fn vscode_launch_under_cap_parses() {
        let dir = make_tempdir("under-cap");
        let path = dir.join("launch.json");
        let body = r#"{
            "version": "0.2.0",
            "configurations": [
                {
                    "type": "al",
                    "name": "OnPrem",
                    "environmentType": "OnPrem",
                    "server": "http://localhost",
                    "serverInstance": "BC"
                }
            ]
        }"#;
        std::fs::write(&path, body).unwrap();
        let result = parse_vscode_launch_file(&path).expect("under-cap must parse");
        assert_eq!(result.configs.len(), 1);
    }

    #[test]
    fn launch_parsers_preserve_complete_debug_objects() {
        let dir = make_tempdir("preserve-debug-objects");
        let zed_path = dir.join("debug.json");
        let zed_debug = serde_json::json!({
            "adapter": "al",
            "label": "Zed Native",
            "environmentType": "Sandbox",
            "environmentName": "Dev",
            "tenant": "tenant.example",
            "authentication": "AAD",
            "breakOnError": "ExcludeTry",
            "breakOnRecordWrite": "ExcludeTemporary",
            "startupObjectType": "Report",
            "startupObjectId": 50100,
            "startupCompany": "CRONUS UK",
            "enableSqlInformationDebugger": false,
            "longRunningSqlStatementsThreshold": 900
        });
        std::fs::write(
            &zed_path,
            serde_json::to_vec(&serde_json::json!([zed_debug.clone()])).unwrap(),
        )
        .unwrap();
        let zed = parse_zed_debug_file(&zed_path).expect("Zed debug config must parse");
        assert_eq!(zed.configs.len(), 1);
        assert_eq!(zed.configs[0].debug_args, zed_debug);

        let vscode_path = dir.join("launch.json");
        let vscode_debug = serde_json::json!({
            "type": "al",
            "request": "launch",
            "name": "VS Code Native",
            "environmentType": "OnPrem",
            "server": "https://bc.example.test",
            "serverInstance": "BC",
            "authentication": "AAD",
            "breakOnNext": "Background",
            "sessionId": 42,
            "startupObjectType": "Query",
            "startupObjectId": 50101,
            "numberOfSqlStatements": 25,
            "validateServerCertificate": false
        });
        std::fs::write(
            &vscode_path,
            serde_json::to_vec(&serde_json::json!({
                "version": "0.2.0",
                "configurations": [vscode_debug.clone()]
            }))
            .unwrap(),
        )
        .unwrap();
        let vscode =
            parse_vscode_launch_file(&vscode_path).expect("VS Code launch config must parse");
        assert_eq!(vscode.configs.len(), 1);
        assert_eq!(vscode.configs[0].debug_args, vscode_debug);
    }

    #[test]
    fn vscode_launch_oversize_is_rejected() {
        let dir = make_tempdir("over-cap");
        let path = dir.join("launch.json");
        // 2 MiB of valid JSON wrapping — well past the 1 MiB cap.
        let body = format!(
            r#"{{"version":"0.2.0","_pad":"{}","configurations":[]}}"#,
            "x".repeat(2 * 1024 * 1024)
        );
        std::fs::write(&path, body).unwrap();
        let err = parse_vscode_launch_file(&path).unwrap_err().to_string();
        assert!(
            err.contains("refusing to parse"),
            "error must mention size refusal: {err}"
        );
    }

    #[test]
    fn zed_debug_oversize_is_rejected() {
        let dir = make_tempdir("zed-over-cap");
        let path = dir.join("debug.json");
        let body = format!(
            r#"[{{"_pad":"{}","adapter":"al","environmentType":"OnPrem","label":"x","server":"http://x","serverInstance":"BC"}}]"#,
            "x".repeat(2 * 1024 * 1024)
        );
        std::fs::write(&path, body).unwrap();
        let err = parse_zed_debug_file(&path).unwrap_err().to_string();
        assert!(
            err.contains("refusing to parse"),
            "error must mention size refusal: {err}"
        );
    }

    #[test]
    fn malformed_zed_config_blocks_vscode_fallback() {
        let dir = make_tempdir("malformed-zed");
        std::fs::create_dir_all(dir.join(".zed")).unwrap();
        std::fs::create_dir_all(dir.join(".vscode")).unwrap();
        std::fs::write(dir.join(".zed/debug.json"), "{not json").unwrap();
        std::fs::write(
            dir.join(".vscode/launch.json"),
            r#"{"configurations":[{"type":"al","environmentType":"Sandbox"}]}"#,
        )
        .unwrap();

        let error = find_launch_config(&dir).expect_err("invalid preferred config must fail");
        assert!(error.path.ends_with(".zed/debug.json"));
    }

    #[test]
    fn valid_non_al_zed_config_can_fall_back_to_vscode() {
        let dir = make_tempdir("non-al-zed");
        std::fs::create_dir_all(dir.join(".zed")).unwrap();
        std::fs::create_dir_all(dir.join(".vscode")).unwrap();
        std::fs::write(
            dir.join(".zed/debug.json"),
            r#"[{"adapter":"debugpy","label":"Python"}]"#,
        )
        .unwrap();
        std::fs::write(
            dir.join(".vscode/launch.json"),
            r#"{"configurations":[{"name":"AL","type":"al","environmentType":"Sandbox"}]}"#,
        )
        .unwrap();

        let file = find_launch_config(&dir)
            .expect("both files are valid")
            .expect("VS Code AL config");
        assert_eq!(file.configs[0].name, "AL");
        assert!(file.path.ends_with(".vscode/launch.json"));
    }

    #[test]
    fn invalid_al_literals_are_configuration_errors() {
        let dir = make_tempdir("invalid-literals");
        std::fs::create_dir_all(dir.join(".vscode")).unwrap();
        let path = dir.join(".vscode/launch.json");
        std::fs::write(
            &path,
            r#"{"configurations":[{"type":"al","environmentType":"Sandbx"}]}"#,
        )
        .unwrap();
        let error = find_launch_config(&dir).expect_err("unknown environment must fail");
        assert!(error.message.contains("unknown environmentType"));

        std::fs::write(
            &path,
            r#"{"configurations":[{"type":"al","environmentType":"OnPrem","authentication":"NavUserPassword"}]}"#,
        )
        .unwrap();
        let error = find_launch_config(&dir).expect_err("unknown auth must fail");
        assert!(error.message.contains("unknown authentication"));
    }

    #[test]
    fn missing_launch_files_are_distinct_from_invalid_files() {
        let dir = make_tempdir("missing");
        assert!(find_launch_config(&dir)
            .expect("missing files are valid absence")
            .is_none());

        std::fs::create_dir_all(dir.join(".zed/debug.json")).unwrap();
        let error = find_launch_config(&dir).expect_err("directory is not a config file");
        assert!(error.message.contains("not a regular file"));
    }

    #[test]
    fn server_scheme_allowlist_accepts_http_and_bare_host() {
        assert!(is_safe_http_server("http://localhost"));
        assert!(is_safe_http_server("https://bc.example.com"));
        assert!(is_safe_http_server("HTTP://CASE-INSENSITIVE"));
        assert!(is_safe_http_server("localhost"));
        assert!(is_safe_http_server("localhost:7048"));
        assert!(is_safe_http_server("bc.example.com"));
    }

    #[test]
    fn server_scheme_allowlist_rejects_unsafe_schemes() {
        assert!(!is_safe_http_server("file:///etc/passwd"));
        assert!(!is_safe_http_server("gopher://example.com"));
        assert!(!is_safe_http_server("javascript://alert(1)"));
        assert!(!is_safe_http_server("ftp://example.com"));
        assert!(!is_safe_http_server(""));
        assert!(!is_safe_http_server("   "));
    }

    fn make_dep() -> AppDependency {
        AppDependency {
            id: "00000000-0000-0000-0000-000000000000".to_string(),
            name: "My App".to_string(),
            publisher: "Acme".to_string(),
            version: "1.0.0.0".to_string(),
        }
    }

    fn onprem_config() -> BcServerConfig {
        BcServerConfig {
            name: "OnPrem".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: None,
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::Windows,
            accept_invalid_certs: false,
            debug_args: serde_json::json!({}),
        }
    }

    #[test]
    fn dev_packages_url_onprem_minimal() {
        let url = onprem_config().dev_packages_url(&make_dep()).unwrap();
        assert!(
            url.starts_with("http://localhost/BC/dev/packages?"),
            "unexpected URL: {url}"
        );
        // Query params are percent-encoded ("My App" -> "My%20App").
        assert!(url.contains("appName=My%20App"), "unexpected URL: {url}");
        assert!(url.contains("publisher=Acme"), "unexpected URL: {url}");
        assert!(!url.contains("tenant="), "no tenant expected: {url}");
    }

    #[test]
    fn dev_packages_url_onprem_with_port_and_tenant() {
        let mut cfg = onprem_config();
        cfg.port = Some(7049);
        cfg.tenant = Some("default".to_string());
        let url = cfg.dev_packages_url(&make_dep()).unwrap();
        assert!(
            url.starts_with("http://localhost:7049/BC/dev/packages?"),
            "unexpected URL: {url}"
        );
        assert!(url.contains("&tenant=default"), "unexpected URL: {url}");
    }

    #[test]
    fn dev_packages_url_onprem_instance_path_encoded() {
        let mut cfg = onprem_config();
        cfg.server_instance = Some("../admin".to_string());
        let url = cfg.dev_packages_url(&make_dep()).unwrap();
        assert!(
            !url.contains("/../admin/"),
            "instance path components must be percent-encoded: {url}"
        );
        assert!(
            url.contains("%2F") || url.contains("..%2Fadmin") || url.contains("%2E"),
            "instance reserved chars must be encoded: {url}"
        );
    }

    #[test]
    fn dev_packages_url_onprem_missing_instance_returns_none() {
        let mut cfg = onprem_config();
        cfg.server_instance = None;
        assert!(cfg.dev_packages_url(&make_dep()).is_none());
    }

    #[test]
    fn dev_packages_url_onprem_unsafe_server_returns_none() {
        for server in [
            "file:///etc/passwd",
            // A scheme written without slashes reached `is_safe_http_server`'s
            // bare-host branch, which returned true unconditionally.
            "javascript:alert(1)",
            "file:/etc/passwd",
            "localhost:notaport",
        ] {
            let mut cfg = onprem_config();
            cfg.server = Some(server.to_string());
            assert!(
                cfg.dev_packages_url(&make_dep()).is_none(),
                "{server} must be refused"
            );
        }
    }

    #[test]
    fn dev_packages_url_adds_a_scheme_to_a_bare_host() {
        // `build_base_url` prepended `http://` and this did not, so publishing
        // worked and symbol download from the same config failed with
        // `url::Url` reading `bc.example.com` as the scheme.
        let mut cfg = onprem_config();
        cfg.server = Some("bc.example.com".to_string());
        cfg.port = Some(7049);
        let url = cfg.dev_packages_url(&make_dep()).unwrap();
        assert!(
            url.starts_with("http://bc.example.com:7049/BC/dev/packages?"),
            "unexpected URL: {url}"
        );
        assert_eq!(url::Url::parse(&url).unwrap().scheme(), "http");
    }

    #[test]
    fn one_broken_non_al_configuration_does_not_hide_the_al_one() {
        // VS Code's Java "Attach to Remote Program" snippet writes
        // `"port": "<debug port of debuggee>"` verbatim. `port` is an
        // `Option<u16>` here, so deserializing every configuration before
        // filtering on `type` failed the whole file.
        let dir = make_tempdir("broken-sibling");
        std::fs::create_dir_all(dir.join(".vscode")).unwrap();
        std::fs::write(
            dir.join(".vscode/launch.json"),
            r#"{"configurations":[
                {"name":"Java","type":"java","request":"attach",
                 "port":"<debug port of debuggee>"},
                {"name":"AL","type":"al","environmentType":"Sandbox",
                 "tenant":"contoso","environmentName":"Sandbox"}
            ]}"#,
        )
        .unwrap();

        let file = find_launch_config(&dir)
            .expect("a non-AL configuration must not fail the file")
            .expect("AL config");
        assert_eq!(file.configs.len(), 1);
        assert_eq!(file.configs[0].name, "AL");
    }

    #[test]
    fn a_broken_al_configuration_is_still_an_error() {
        let dir = make_tempdir("broken-al");
        std::fs::create_dir_all(dir.join(".vscode")).unwrap();
        std::fs::write(
            dir.join(".vscode/launch.json"),
            r#"{"configurations":[{"name":"AL","type":"al","port":"nope"}]}"#,
        )
        .unwrap();

        find_launch_config(&dir).expect_err("a malformed AL configuration must be reported");
    }

    #[test]
    fn dev_packages_url_cloud() {
        let cfg = BcServerConfig {
            name: "Cloud".to_string(),
            environment_type: EnvironmentType::Sandbox,
            server: None,
            server_instance: None,
            port: None,
            environment_name: Some("My Sandbox".to_string()),
            tenant: Some("contoso.onmicrosoft.com".to_string()),
            authentication: AuthMethod::AAD,
            accept_invalid_certs: false,
            debug_args: serde_json::json!({}),
        };
        let url = cfg.dev_packages_url(&make_dep()).unwrap();
        assert!(
            url.starts_with(
                "https://api.businesscentral.dynamics.com/v2.0/contoso.onmicrosoft.com/My%20Sandbox/dev/packages?"
            ),
            "unexpected URL: {url}"
        );
    }

    #[test]
    fn dev_packages_url_cloud_missing_fields_returns_none() {
        let mut cfg = BcServerConfig {
            name: "Cloud".to_string(),
            environment_type: EnvironmentType::Production,
            server: None,
            server_instance: None,
            port: None,
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::AAD,
            accept_invalid_certs: false,
            debug_args: serde_json::json!({}),
        };
        assert!(cfg.dev_packages_url(&make_dep()).is_none());
        cfg.tenant = Some("t".to_string());
        assert!(cfg.dev_packages_url(&make_dep()).is_none());
    }
}
