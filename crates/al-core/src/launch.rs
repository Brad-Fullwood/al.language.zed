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

fn parse_zed_debug_file(path: &Path) -> Result<DebugConfigFile, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
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
    let content = std::fs::read_to_string(path)?;
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
            warn!(environment_type = %other, "Unknown environment type");
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
            warn!(auth = %other, "Unknown auth method, defaulting to AAD");
            AuthMethod::AAD
        }
    }
}
