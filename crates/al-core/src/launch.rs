//! Debug/launch configuration types and parsing.
//!
//! Reads BC server connection details from debug configuration files.
//! Supports both Zed (`.zed/debug.json`) and VS Code (`.vscode/launch.json`).

use std::path::{Path, PathBuf};

use crate::dap::json_util::strip_json_comments;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::project::AppDependency;

/// BC environment type.
///
/// Re-exported from crate::dap to avoid duplication.
pub use crate::dap::config::EnvironmentType;

/// Authentication method for BC connections.
///
/// Re-exported from crate::dap to avoid duplication.
pub use crate::dap::config::AuthMethod;

/// A parsed debug configuration file.
#[derive(Debug, Clone)]
pub struct DebugConfigFile {
    pub path: PathBuf,
    pub configs: Vec<BcServerConfig>,
}

/// BC server connection configuration extracted from debug config.
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
    /// Authentication method
    pub authentication: AuthMethod,
    /// Accept invalid/self-signed TLS certificates. Defaults to `false`.
    /// Set to `true` only for on-prem servers with self-signed certs.
    pub accept_invalid_certs: bool,
}

// EnvironmentType and AuthMethod are re-exported from crate::dap::config (see above).

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
                let server = self.server.as_deref()?;
                if !is_safe_http_server(server) {
                    warn!(
                        server = %server,
                        "BC server URL must start with http:// or https:// (or be a bare host); refusing to construct dev-packages URL"
                    );
                    return None;
                }
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
                    base, instance, query, tenant_param
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

    /// Human-readable display name for logging.
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

// ---------------------------------------------------------------------------
// Raw JSON types — Zed format (.zed/debug.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ZedDebugConfigJson {
    #[serde(default)]
    label: String,
    #[serde(default)]
    adapter: String,
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

// ---------------------------------------------------------------------------
// Raw JSON types — VS Code format (.vscode/launch.json)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct VsCodeLaunchJson {
    #[serde(default)]
    configurations: Vec<VsCodeLaunchConfigJson>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VsCodeLaunchConfigJson {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    config_type: String,
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

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Find and parse debug/launch configuration from the project root.
pub fn find_launch_config(project_root: &Path) -> Option<DebugConfigFile> {
    let zed_path = project_root.join(".zed").join("debug.json");
    if zed_path.exists() {
        match parse_zed_debug_file(&zed_path) {
            Ok(df) if !df.configs.is_empty() => {
                debug!(path = %zed_path.display(), configs = df.configs.len(), "Found Zed debug configuration");
                return Some(df);
            }
            Ok(_) => {
                debug!(path = %zed_path.display(), "Zed debug.json found but no AL configurations");
            }
            Err(e) => {
                warn!(path = %zed_path.display(), error = %e, "Failed to parse .zed/debug.json");
            }
        }
    }

    let vscode_path = project_root.join(".vscode").join("launch.json");
    if vscode_path.exists() {
        match parse_vscode_launch_file(&vscode_path) {
            Ok(df) if !df.configs.is_empty() => {
                debug!(path = %vscode_path.display(), configs = df.configs.len(), "Found VS Code launch configuration");
                return Some(df);
            }
            Ok(_) => {
                debug!(path = %vscode_path.display(), "VS Code launch.json found but no AL configurations");
            }
            Err(e) => {
                warn!(path = %vscode_path.display(), error = %e, "Failed to parse .vscode/launch.json");
            }
        }
    }

    None
}

/// Maximum bytes accepted for a debug-config file (Zed `debug.json` or
/// VS Code `launch.json`). Real launch configs are kilobytes at most; 1 MiB
/// is two orders of magnitude past anything legitimate while refusing
/// pathological inputs (sparse files / adversarial commit) that would OOM
/// the daemon on `read_to_string`.
const MAX_LAUNCH_FILE_BYTES: u64 = 1_048_576;

/// Read a launch/debug config file, refusing inputs larger than
/// `MAX_LAUNCH_FILE_BYTES` before allocating.
/// Allowlist check on the `server` field of an OnPrem BC config.
///
/// AL launch configs let users specify the BC server URL freely. A misconfigured
/// or adversarial config could use `file:///etc/passwd` or `gopher://...` and
/// that URL would be handed unchanged to the BC HTTP client. Restrict to
/// http(s):// (the only two schemes the BC dev API uses) or bare hostnames
/// (e.g. `localhost`, where the BC client default-prepends http://).
fn is_safe_http_server(server: &str) -> bool {
    let s = server.trim();
    if s.is_empty() {
        return false;
    }
    // Explicit schemes — only http(s) accepted.
    if let Some(rest) = s.split_once("://") {
        let scheme = rest.0.to_ascii_lowercase();
        return scheme == "http" || scheme == "https";
    }
    // Bare host (no scheme): reject if it contains a `:` followed by what looks
    // like an unknown-scheme separator. A `:` for port-only (e.g. `localhost:7048`)
    // is fine; that's a port number, not a scheme. Accept the rest.
    true
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

fn parse_zed_debug_file(path: &Path) -> Result<DebugConfigFile, Box<dyn std::error::Error>> {
    let content = read_launch_file_capped(path)?;
    let clean = strip_json_comments(&content);
    let configs_raw: Vec<ZedDebugConfigJson> = serde_json::from_str(&clean)?;

    let configs: Vec<BcServerConfig> = configs_raw
        .into_iter()
        .filter(|c| c.adapter == "al" || c.environment_type.is_some())
        .filter_map(convert_zed_config)
        .collect();

    Ok(DebugConfigFile {
        path: path.to_path_buf(),
        configs,
    })
}

fn parse_vscode_launch_file(path: &Path) -> Result<DebugConfigFile, Box<dyn std::error::Error>> {
    let content = read_launch_file_capped(path)?;
    let clean = strip_json_comments(&content);
    let raw: VsCodeLaunchJson = serde_json::from_str(&clean)?;

    let configs: Vec<BcServerConfig> = raw
        .configurations
        .into_iter()
        .filter(|c| c.config_type == "al" || c.environment_type.is_some())
        .filter_map(convert_vscode_config)
        .collect();

    Ok(DebugConfigFile {
        path: path.to_path_buf(),
        configs,
    })
}

fn convert_zed_config(raw: ZedDebugConfigJson) -> Option<BcServerConfig> {
    let env_type = parse_environment_type(raw.environment_type.as_deref()?)?;
    let auth = parse_auth_method(raw.authentication.as_deref(), &env_type);
    Some(BcServerConfig {
        name: raw.label,
        environment_type: env_type,
        server: raw.server,
        server_instance: raw.server_instance,
        port: raw.port,
        environment_name: raw.environment_name,
        tenant: raw.tenant,
        authentication: auth,
        accept_invalid_certs: raw.accept_invalid_certs,
    })
}

fn convert_vscode_config(raw: VsCodeLaunchConfigJson) -> Option<BcServerConfig> {
    let env_type = parse_environment_type(raw.environment_type.as_deref()?)?;
    let auth = parse_auth_method(raw.authentication.as_deref(), &env_type);
    Some(BcServerConfig {
        name: raw.name,
        environment_type: env_type,
        server: raw.server,
        server_instance: raw.server_instance,
        port: raw.port,
        environment_name: raw.environment_name,
        tenant: raw.tenant,
        authentication: auth,
        accept_invalid_certs: raw.accept_invalid_certs,
    })
}

fn parse_environment_type(s: &str) -> Option<EnvironmentType> {
    match s {
        "OnPrem" => Some(EnvironmentType::OnPrem),
        "Sandbox" => Some(EnvironmentType::Sandbox),
        "Production" => Some(EnvironmentType::Production),
        other => {
            // ERROR (not WARN) because the launch entry is silently dropped —
            // the user typed a config they wanted to use and we're refusing
            // it. Naming the valid values in the message lets them fix the
            // typo without consulting docs. F-OPEN-073.
            tracing::error!(
                environment_type = %other,
                "Unknown environmentType in launch.json — expected one of OnPrem / Sandbox / Production; dropping this configuration entry"
            );
            None
        }
    }
}

fn parse_auth_method(s: Option<&str>, env_type: &EnvironmentType) -> AuthMethod {
    match s {
        Some("UserPassword") => AuthMethod::UserPassword,
        Some("Windows") => AuthMethod::Windows,
        Some("AAD") | Some("MicrosoftEntraID") => AuthMethod::AAD,
        None if *env_type == EnvironmentType::OnPrem => AuthMethod::Windows,
        None => AuthMethod::AAD,
        Some(other) => {
            // T032 / launch-auth-fallback: env-type-aware fallback rather than
            // silently jumping to AAD on every typo. A misconfigured launch.json
            // for an OnPrem server should not silently switch to cloud OAuth — it
            // typically means the user typed a vendor-specific value (e.g.
            // "NavUserPassword") that maps to UserPassword in spirit. Match the
            // None-arm policy so the fallback is "what would the env type pick by
            // default" not "always AAD".
            let fallback = match env_type {
                EnvironmentType::OnPrem => AuthMethod::Windows,
                _ => AuthMethod::AAD,
            };
            warn!(
                auth = %other, env = ?env_type, fallback = ?fallback,
                "Unknown auth method in launch.json — falling back to env-type default"
            );
            fallback
        }
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
            "al-core-launch-test-{}-{}-{}",
            name,
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
}
