//! Debug/launch configuration parsing.
//!
//! Reads BC server connection details from debug configuration files.
//! Supports both Zed (`.zed/debug.json`) and VS Code (`.vscode/launch.json`)
//! formats, preferring the Zed format.
//!
//! # Search order
//! 1. `.zed/debug.json` — Zed's native format (flat JSON array, `adapter` field)
//! 2. `.vscode/launch.json` — VS Code format (nested `configurations`, `type` field)

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

use crate::project::AppDependency;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

impl BcServerConfig {
    /// Construct the `/dev/packages` URL for downloading a single dependency.
    ///
    /// On-prem: `{server}:{port}/{serverInstance}/dev/packages?publisher=...&appName=...&versionText=...&tenant=...`
    /// Cloud:   `https://api.businesscentral.dynamics.com/v2.0/{tenant}/{environmentName}/dev/packages?...`
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

/// Zed debug.json is a flat array: `[{ "adapter": "al", "label": "...", ... }]`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ZedDebugConfigJson {
    #[serde(default)]
    label: String,
    #[serde(default)]
    adapter: String,
    // BC-specific fields (pass-through in Zed's schema)
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
/// Search order:
/// 1. `.zed/debug.json` — Zed native format
/// 2. `.vscode/launch.json` — VS Code format (fallback, common in existing BC projects)
pub fn find_launch_config(project_root: &Path) -> Option<DebugConfigFile> {
    // 1. Try Zed's .zed/debug.json first
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
                debug!(path = %zed_path.display(), "Zed debug.json found but no AL configurations");
            }
            Err(e) => {
                warn!(path = %zed_path.display(), error = %e, "Failed to parse .zed/debug.json");
            }
        }
    }

    // 2. Fall back to VS Code's .vscode/launch.json
    let vscode_path = project_root.join(".vscode").join("launch.json");
    if vscode_path.exists() {
        match parse_vscode_launch_file(&vscode_path) {
            Ok(df) if !df.configs.is_empty() => {
                debug!(
                    path = %vscode_path.display(),
                    configs = df.configs.len(),
                    "Found VS Code launch configuration (fallback)"
                );
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

/// Parse Zed's `.zed/debug.json` — a flat JSON array of debug configurations.
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

/// Parse VS Code's `.vscode/launch.json` — nested `{ "configurations": [...] }`.
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
    })
}

fn parse_environment_type(s: &str) -> Option<EnvironmentType> {
    match s {
        "OnPrem" => Some(EnvironmentType::OnPrem),
        "Sandbox" => Some(EnvironmentType::Sandbox),
        "Production" => Some(EnvironmentType::Production),
        other => {
            warn!(environment_type = %other, "Unknown environment type in debug config");
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
            warn!(auth = %other, "Unknown authentication method, defaulting to AAD");
            AuthMethod::AAD
        }
    }
}

/// Strip single-line comments from JSON (both Zed and VS Code allow them).
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
            // Skip rest of line
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

    // Strip trailing commas before ] and } (Zed config files allow them)
    strip_trailing_commas(&result)
}

/// Remove trailing commas before `]` or `}` that serde_json rejects.
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
            // Look ahead past whitespace for ] or }
            let mut j = i + 1;
            while j < len && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r') {
                j += 1;
            }
            if j < len && (bytes[j] == b']' || bytes[j] == b'}') {
                // Skip this trailing comma
                continue;
            }
        }

        result.push(c);
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Zed format tests --

    #[test]
    fn parse_zed_onprem_config() {
        let json = r#"[
            {
                "label": "On-prem BC",
                "adapter": "al",
                "request": "launch",
                "environmentType": "OnPrem",
                "server": "https://erp.example.com",
                "serverInstance": "BC",
                "port": 9149,
                "authentication": "UserPassword",
                "tenant": "default"
            }
        ]"#;

        let configs_raw: Vec<ZedDebugConfigJson> = serde_json::from_str(json).unwrap();
        let configs: Vec<_> = configs_raw
            .into_iter()
            .filter_map(convert_zed_config)
            .collect();

        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.name, "On-prem BC");
        assert_eq!(c.environment_type, EnvironmentType::OnPrem);
        assert_eq!(c.server.as_deref(), Some("https://erp.example.com"));
        assert_eq!(c.server_instance.as_deref(), Some("BC"));
        assert_eq!(c.port, Some(9149));
        assert_eq!(c.authentication, AuthMethod::UserPassword);
    }

    #[test]
    fn parse_zed_cloud_config() {
        let json = r#"[
            {
                "label": "Cloud Sandbox",
                "adapter": "al",
                "request": "launch",
                "environmentType": "Sandbox",
                "environmentName": "Sandbox",
                "tenant": "2c53f084-1676-40ab-8c23-f0e8a466f455"
            }
        ]"#;

        let configs_raw: Vec<ZedDebugConfigJson> = serde_json::from_str(json).unwrap();
        let configs: Vec<_> = configs_raw
            .into_iter()
            .filter_map(convert_zed_config)
            .collect();

        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.environment_type, EnvironmentType::Sandbox);
        assert_eq!(c.authentication, AuthMethod::AAD);
        assert_eq!(
            c.tenant.as_deref(),
            Some("2c53f084-1676-40ab-8c23-f0e8a466f455")
        );
    }

    #[test]
    fn zed_filters_non_al_adapters() {
        let json = r#"[
            {
                "label": "Python",
                "adapter": "debugpy",
                "request": "launch",
                "program": "main.py"
            },
            {
                "label": "AL Server",
                "adapter": "al",
                "request": "launch",
                "environmentType": "OnPrem",
                "server": "http://localhost",
                "serverInstance": "BC"
            }
        ]"#;

        let configs_raw: Vec<ZedDebugConfigJson> = serde_json::from_str(json).unwrap();
        let configs: Vec<_> = configs_raw
            .into_iter()
            .filter(|c| c.adapter == "al" || c.environment_type.is_some())
            .filter_map(convert_zed_config)
            .collect();

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "AL Server");
    }

    #[test]
    fn parse_zed_config_with_trailing_commas() {
        let json = r#"[
            {
                "adapter": "al",
                "label": "Cloud Sandbox",
                "request": "launch",
                "environmentType": "Sandbox",
                "environmentName": "sandbox",
                "tenant": "2c53f084-1676-40ab-8c23-f0e8a466f455",
                "startupObjectId": 22,
            },
        ]"#;

        let clean = strip_json_comments(json);
        let configs_raw: Vec<ZedDebugConfigJson> = serde_json::from_str(&clean).unwrap();
        let configs: Vec<_> = configs_raw
            .into_iter()
            .filter_map(convert_zed_config)
            .collect();

        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.environment_type, EnvironmentType::Sandbox);
        assert_eq!(
            c.tenant.as_deref(),
            Some("2c53f084-1676-40ab-8c23-f0e8a466f455")
        );
    }

    #[test]
    fn strip_trailing_commas_preserves_commas_in_strings() {
        let input = r#"{"key": "value,}"#;
        let result = strip_trailing_commas(input);
        assert_eq!(result, input);
    }

    // -- VS Code format tests (fallback) --

    #[test]
    fn parse_vscode_onprem_config() {
        let json = r#"{
            "version": "0.2.0",
            "configurations": [{
                "name": "On-prem",
                "type": "al",
                "request": "launch",
                "environmentType": "OnPrem",
                "server": "https://erp.example.com",
                "serverInstance": "BC",
                "port": 9149,
                "authentication": "UserPassword",
                "tenant": "default"
            }]
        }"#;

        let raw: VsCodeLaunchJson = serde_json::from_str(json).unwrap();
        let configs: Vec<_> = raw
            .configurations
            .into_iter()
            .filter_map(convert_vscode_config)
            .collect();

        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.environment_type, EnvironmentType::OnPrem);
        assert_eq!(c.server.as_deref(), Some("https://erp.example.com"));
        assert_eq!(c.server_instance.as_deref(), Some("BC"));
        assert_eq!(c.port, Some(9149));
        assert_eq!(c.authentication, AuthMethod::UserPassword);
    }

    #[test]
    fn parse_vscode_cloud_config() {
        let json = r#"{
            "version": "0.2.0",
            "configurations": [{
                "name": "Cloud Sandbox",
                "type": "al",
                "request": "launch",
                "environmentType": "Sandbox",
                "environmentName": "Sandbox",
                "tenant": "2c53f084-1676-40ab-8c23-f0e8a466f455"
            }]
        }"#;

        let raw: VsCodeLaunchJson = serde_json::from_str(json).unwrap();
        let configs: Vec<_> = raw
            .configurations
            .into_iter()
            .filter_map(convert_vscode_config)
            .collect();

        assert_eq!(configs.len(), 1);
        let c = &configs[0];
        assert_eq!(c.environment_type, EnvironmentType::Sandbox);
        assert_eq!(c.authentication, AuthMethod::AAD);
        assert_eq!(
            c.tenant.as_deref(),
            Some("2c53f084-1676-40ab-8c23-f0e8a466f455")
        );
    }

    // -- Shared tests --

    #[test]
    fn dev_packages_url_onprem() {
        let config = BcServerConfig {
            name: "test".into(),
            environment_type: EnvironmentType::OnPrem,
            server: Some("https://erp.example.com".into()),
            server_instance: Some("BC".into()),
            port: Some(9149),
            environment_name: None,
            tenant: Some("default".into()),
            authentication: AuthMethod::UserPassword,
        };

        let dep = AppDependency {
            id: "xxx".into(),
            name: "System".into(),
            publisher: "Microsoft".into(),
            version: "26.0.0.0".into(),
        };

        let url = config.dev_packages_url(&dep).unwrap();
        assert!(url.starts_with("https://erp.example.com:9149/BC/dev/packages?"));
        assert!(url.contains("publisher=Microsoft"));
        assert!(url.contains("appName=System"));
        assert!(url.contains("versionText=26.0.0.0"));
        assert!(url.contains("tenant=default"));
    }

    #[test]
    fn dev_packages_url_cloud() {
        let config = BcServerConfig {
            name: "test".into(),
            environment_type: EnvironmentType::Sandbox,
            server: None,
            server_instance: None,
            port: None,
            environment_name: Some("Sandbox".into()),
            tenant: Some("my-tenant.onmicrosoft.com".into()),
            authentication: AuthMethod::AAD,
        };

        let dep = AppDependency {
            id: "xxx".into(),
            name: "Application".into(),
            publisher: "Microsoft".into(),
            version: "26.5.0.0".into(),
        };

        let url = config.dev_packages_url(&dep).unwrap();
        assert!(url.starts_with(
            "https://api.businesscentral.dynamics.com/v2.0/my-tenant.onmicrosoft.com/Sandbox/dev/packages?"
        ));
        assert!(url.contains("appName=Application"));
    }

    #[test]
    fn strip_comments() {
        let input = r#"{
            // This is a comment
            "key": "value" // trailing comment
        }"#;
        let clean = strip_json_comments(input);
        assert!(!clean.contains("comment"));
        assert!(clean.contains("\"key\": \"value\""));
    }

    #[test]
    fn vscode_filters_non_al_configs() {
        let json = r#"{
            "version": "0.2.0",
            "configurations": [
                {
                    "name": "Node.js",
                    "type": "node",
                    "request": "launch"
                },
                {
                    "name": "AL Server",
                    "type": "al",
                    "request": "launch",
                    "environmentType": "OnPrem",
                    "server": "http://localhost",
                    "serverInstance": "BC"
                }
            ]
        }"#;

        let raw: VsCodeLaunchJson = serde_json::from_str(json).unwrap();
        let configs: Vec<_> = raw
            .configurations
            .into_iter()
            .filter(|c| c.config_type == "al" || c.environment_type.is_some())
            .filter_map(convert_vscode_config)
            .collect();

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "AL Server");
    }

    #[test]
    fn find_prefers_zed_over_vscode() {
        let tmp = tempdir();

        let zed_dir = tmp.join(".zed");
        std::fs::create_dir_all(&zed_dir).unwrap();
        std::fs::write(
            zed_dir.join("debug.json"),
            r#"[{
                "label": "Zed Config",
                "adapter": "al",
                "environmentType": "OnPrem",
                "server": "https://zed.example.com",
                "serverInstance": "BC"
            }]"#,
        )
        .unwrap();

        let vscode_dir = tmp.join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        std::fs::write(
            vscode_dir.join("launch.json"),
            r#"{
                "configurations": [{
                    "name": "VS Code Config",
                    "type": "al",
                    "environmentType": "OnPrem",
                    "server": "https://vscode.example.com",
                    "serverInstance": "BC"
                }]
            }"#,
        )
        .unwrap();

        let result = find_launch_config(&tmp).unwrap();
        assert!(result.path.ends_with("debug.json"));
        assert_eq!(result.configs[0].name, "Zed Config");
        assert_eq!(
            result.configs[0].server.as_deref(),
            Some("https://zed.example.com")
        );
    }

    #[test]
    fn find_falls_back_to_vscode() {
        let tmp = tempdir();

        let vscode_dir = tmp.join(".vscode");
        std::fs::create_dir_all(&vscode_dir).unwrap();
        std::fs::write(
            vscode_dir.join("launch.json"),
            r#"{
                "configurations": [{
                    "name": "VS Code Only",
                    "type": "al",
                    "environmentType": "Sandbox",
                    "environmentName": "Sandbox",
                    "tenant": "test-tenant"
                }]
            }"#,
        )
        .unwrap();

        let result = find_launch_config(&tmp).unwrap();
        assert!(result.path.ends_with("launch.json"));
        assert_eq!(result.configs[0].name, "VS Code Only");
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "al-core-launch-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
