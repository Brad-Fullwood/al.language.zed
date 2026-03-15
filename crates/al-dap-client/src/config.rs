//! Local debug/launch configuration types and parsing.
//!
//! These types mirror the relevant parts of `al_protocol::launch` but are
//! defined locally so that `al-dap-client` does not depend on `al-protocol`.
//!
//! Reads BC server connection details from debug configuration files.
//! Supports both Zed (`.zed/debug.json`) and VS Code (`.vscode/launch.json`).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A parsed debug configuration file.
#[derive(Debug, Clone)]
pub struct DebugConfigFile {
    pub path: PathBuf,
    pub configs: Vec<DapLaunchConfig>,
}

/// BC server connection configuration extracted from a debug config file.
///
/// Contains only the fields that `al-dap-client` needs to construct DAP
/// launch arguments. This is intentionally minimal — it does not include
/// fields like `dev_packages_url` that belong in `al-core`.
#[derive(Debug, Clone)]
pub struct DapLaunchConfig {
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
}

/// BC environment type.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentType {
    OnPrem,
    Sandbox,
    Production,
}

/// Authentication method for BC connections.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthMethod {
    Windows,
    UserPassword,
    AAD,
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
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Find and parse debug/launch configuration from the project root.
///
/// Searches for `.zed/debug.json` first, then `.vscode/launch.json`.
pub fn find_launch_config(project_root: &Path) -> Option<DebugConfigFile> {
    let zed_path = project_root.join(".zed").join("debug.json");
    if zed_path.exists() {
        match parse_zed_debug_file(&zed_path) {
            Ok(df) if !df.configs.is_empty() => {
                debug!(
                    path = %zed_path.display(),
                    configs = df.configs.len(),
                    "Found Zed debug configuration"
                );
                return Some(df);
            }
            Ok(_) => {
                debug!(
                    path = %zed_path.display(),
                    "Zed debug.json found but no AL configurations"
                );
            }
            Err(e) => {
                warn!(
                    path = %zed_path.display(),
                    error = %e,
                    "Failed to parse .zed/debug.json"
                );
            }
        }
    }

    let vscode_path = project_root.join(".vscode").join("launch.json");
    if vscode_path.exists() {
        match parse_vscode_launch_file(&vscode_path) {
            Ok(df) if !df.configs.is_empty() => {
                debug!(
                    path = %vscode_path.display(),
                    configs = df.configs.len(),
                    "Found VS Code launch configuration"
                );
                return Some(df);
            }
            Ok(_) => {
                debug!(
                    path = %vscode_path.display(),
                    "VS Code launch.json found but no AL configurations"
                );
            }
            Err(e) => {
                warn!(
                    path = %vscode_path.display(),
                    error = %e,
                    "Failed to parse .vscode/launch.json"
                );
            }
        }
    }

    None
}

fn parse_zed_debug_file(path: &Path) -> Result<DebugConfigFile, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let clean = strip_json_comments(&content);
    let configs_raw: Vec<ZedDebugConfigJson> = serde_json::from_str(&clean)?;

    let configs: Vec<DapLaunchConfig> = configs_raw
        .into_iter()
        .filter(|c| c.adapter == "al" || c.environment_type.is_some())
        .filter_map(convert_zed_config)
        .collect();

    Ok(DebugConfigFile { path: path.to_path_buf(), configs })
}

fn parse_vscode_launch_file(path: &Path) -> Result<DebugConfigFile, Box<dyn std::error::Error>> {
    let content = std::fs::read_to_string(path)?;
    let clean = strip_json_comments(&content);
    let raw: VsCodeLaunchJson = serde_json::from_str(&clean)?;

    let configs: Vec<DapLaunchConfig> = raw
        .configurations
        .into_iter()
        .filter(|c| c.config_type == "al" || c.environment_type.is_some())
        .filter_map(convert_vscode_config)
        .collect();

    Ok(DebugConfigFile { path: path.to_path_buf(), configs })
}

fn convert_zed_config(raw: ZedDebugConfigJson) -> Option<DapLaunchConfig> {
    let env_type = parse_environment_type(raw.environment_type.as_deref()?)?;
    let auth = parse_auth_method(raw.authentication.as_deref(), &env_type);
    Some(DapLaunchConfig {
        name: raw.label,
        environment_type: env_type,
        server: raw.server,
        server_instance: raw.server_instance,
        port: raw.port,
        environment_name: raw.environment_name,
        tenant: raw.tenant,
        authentication: auth,
    })
}

fn convert_vscode_config(raw: VsCodeLaunchConfigJson) -> Option<DapLaunchConfig> {
    let env_type = parse_environment_type(raw.environment_type.as_deref()?)?;
    let auth = parse_auth_method(raw.authentication.as_deref(), &env_type);
    Some(DapLaunchConfig {
        name: raw.name,
        environment_type: env_type,
        server: raw.server,
        server_instance: raw.server_instance,
        port: raw.port,
        environment_name: raw.environment_name,
        tenant: raw.tenant,
        authentication: auth,
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

// ---------------------------------------------------------------------------
// JSON comment stripping (same logic as al-protocol::launch)
// ---------------------------------------------------------------------------

fn strip_json_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape_next = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if escape_next {
            result.push(c);
            escape_next = false;
            continue;
        }
        if c == '\\' && in_string {
            result.push(c);
            escape_next = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            result.push(c);
            continue;
        }
        if !in_string && c == '/' && chars.peek() == Some(&'/') {
            for cc in chars.by_ref() {
                if cc == '\n' {
                    result.push('\n');
                    break;
                }
            }
            continue;
        }
        result.push(c);
    }

    strip_trailing_commas(&result)
}

fn strip_trailing_commas(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape_next = false;
    let bytes = input.as_bytes();
    let len = bytes.len();

    for i in 0..len {
        let c = bytes[i] as char;
        if escape_next {
            result.push(c);
            escape_next = false;
            continue;
        }
        if c == '\\' && in_string {
            result.push(c);
            escape_next = true;
            continue;
        }
        if c == '"' {
            in_string = !in_string;
            result.push(c);
            continue;
        }
        if !in_string && c == ',' {
            let mut j = i + 1;
            while j < len && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
                j += 1;
            }
            if j < len && (bytes[j] == b']' || bytes[j] == b'}') {
                continue;
            }
        }
        result.push(c);
    }

    result
}
