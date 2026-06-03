//! Local debug/launch configuration types and parsing.
//!
//! These types mirror the relevant parts of `al_protocol::launch` but are
//! defined locally so that `crate::dap` does not depend on `al-protocol`.
//!
//! Reads BC server connection details from debug configuration files.
//! Supports both Zed (`.zed/debug.json`) and VS Code (`.vscode/launch.json`).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{debug, warn};

use super::json_util::strip_json_comments;

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
/// Contains only the fields that `crate::dap` needs to construct DAP
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

    Ok(DebugConfigFile {
        path: path.to_path_buf(),
        configs,
    })
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

    Ok(DebugConfigFile {
        path: path.to_path_buf(),
        configs,
    })
}

/// Shared constructor: resolve env type + auth, then build a [`DapLaunchConfig`].
///
/// Returns `None` if `environment_type_str` is absent or unrecognised.
// Every parameter corresponds to a distinct JSON field in launch.json — the
// shape is dictated externally by VS Code's DAP launch config schema. A
// struct here would just mirror the schema in less-clear form.
#[allow(clippy::too_many_arguments)]
fn build_launch_config(
    name: String,
    environment_type_str: Option<&str>,
    authentication_str: Option<&str>,
    server: Option<String>,
    server_instance: Option<String>,
    port: Option<u16>,
    environment_name: Option<String>,
    tenant: Option<String>,
) -> Option<DapLaunchConfig> {
    let env_type = parse_environment_type(environment_type_str?)?;
    let auth = parse_auth_method(authentication_str, &env_type);
    Some(DapLaunchConfig {
        name,
        environment_type: env_type,
        server,
        server_instance,
        port,
        environment_name,
        tenant,
        authentication: auth,
    })
}

fn convert_zed_config(raw: ZedDebugConfigJson) -> Option<DapLaunchConfig> {
    build_launch_config(
        raw.label,
        raw.environment_type.as_deref(),
        raw.authentication.as_deref(),
        raw.server,
        raw.server_instance,
        raw.port,
        raw.environment_name,
        raw.tenant,
    )
}

fn convert_vscode_config(raw: VsCodeLaunchConfigJson) -> Option<DapLaunchConfig> {
    build_launch_config(
        raw.name,
        raw.environment_type.as_deref(),
        raw.authentication.as_deref(),
        raw.server,
        raw.server_instance,
        raw.port,
        raw.environment_name,
        raw.tenant,
    )
}

fn parse_environment_type(s: &str) -> Option<EnvironmentType> {
    match s {
        "OnPrem" => Some(EnvironmentType::OnPrem),
        "Sandbox" => Some(EnvironmentType::Sandbox),
        "Production" => Some(EnvironmentType::Production),
        other => {
            // ERROR (not WARN) because the config entry is silently dropped —
            // the user typed a config they wanted to use and we're refusing it.
            // Naming the valid values lets them fix the typo without docs.
            // Mirrors launch.rs::parse_environment_type (F-OPEN-073).
            tracing::error!(
                environment_type = %other,
                "Unknown environmentType — expected one of OnPrem / Sandbox / Production; dropping this configuration entry"
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
            // for an OnPrem server must not silently switch to cloud OAuth —
            // match the None-arm policy so the fallback is "what would the env
            // type pick by default" not "always AAD". (Mirrors launch.rs.)
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

    #[test]
    fn auth_unknown_value_falls_back_by_env_type() {
        // OnPrem with an unknown/typo auth value must NOT silently use cloud AAD;
        // it falls back to Windows (env-type default). Regression for the T032 fix.
        assert_eq!(
            parse_auth_method(Some("NavUserPassword"), &EnvironmentType::OnPrem),
            AuthMethod::Windows
        );
        assert_eq!(
            parse_auth_method(Some("bogus"), &EnvironmentType::Sandbox),
            AuthMethod::AAD
        );
        assert_eq!(
            parse_auth_method(Some("bogus"), &EnvironmentType::Production),
            AuthMethod::AAD
        );
    }

    #[test]
    fn auth_known_values_and_none_unchanged() {
        assert_eq!(
            parse_auth_method(Some("Windows"), &EnvironmentType::Sandbox),
            AuthMethod::Windows
        );
        assert_eq!(
            parse_auth_method(Some("UserPassword"), &EnvironmentType::OnPrem),
            AuthMethod::UserPassword
        );
        assert_eq!(
            parse_auth_method(Some("MicrosoftEntraID"), &EnvironmentType::OnPrem),
            AuthMethod::AAD
        );
        assert_eq!(
            parse_auth_method(None, &EnvironmentType::OnPrem),
            AuthMethod::Windows
        );
        assert_eq!(
            parse_auth_method(None, &EnvironmentType::Production),
            AuthMethod::AAD
        );
    }

    // -- parse_environment_type ------------------------------------------

    #[test]
    fn env_type_known_values_parse() {
        assert_eq!(
            parse_environment_type("OnPrem"),
            Some(EnvironmentType::OnPrem)
        );
        assert_eq!(
            parse_environment_type("Sandbox"),
            Some(EnvironmentType::Sandbox)
        );
        assert_eq!(
            parse_environment_type("Production"),
            Some(EnvironmentType::Production)
        );
    }

    #[test]
    fn env_type_unknown_and_case_mismatch_drop() {
        // Unknown value is dropped (returns None) — config entry is refused.
        assert_eq!(parse_environment_type("Cloud"), None);
        assert_eq!(parse_environment_type(""), None);
        // Matching is case-sensitive: "sandbox" (lowercase) is not "Sandbox".
        assert_eq!(parse_environment_type("sandbox"), None);
        assert_eq!(parse_environment_type("ONPREM"), None);
    }

    // -- build_launch_config ---------------------------------------------

    #[test]
    fn build_launch_config_requires_env_type() {
        // No environmentType => whole config entry is dropped.
        let result = build_launch_config(
            "no-env".into(),
            None,
            Some("Windows"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_none());

        // Unrecognised environmentType => also dropped.
        let result = build_launch_config(
            "bad-env".into(),
            Some("Nope"),
            Some("Windows"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_none());
    }

    #[test]
    fn build_launch_config_populates_all_fields() {
        let cfg = build_launch_config(
            "OnPrem BC".into(),
            Some("OnPrem"),
            Some("UserPassword"),
            Some("https://erp.example.com".into()),
            Some("BC".into()),
            Some(7049),
            Some("Sandbox".into()),
            Some("default".into()),
        )
        .expect("valid OnPrem config should build");

        assert_eq!(cfg.name, "OnPrem BC");
        assert_eq!(cfg.environment_type, EnvironmentType::OnPrem);
        assert_eq!(cfg.authentication, AuthMethod::UserPassword);
        assert_eq!(cfg.server.as_deref(), Some("https://erp.example.com"));
        assert_eq!(cfg.server_instance.as_deref(), Some("BC"));
        assert_eq!(cfg.port, Some(7049));
        assert_eq!(cfg.environment_name.as_deref(), Some("Sandbox"));
        assert_eq!(cfg.tenant.as_deref(), Some("default"));
    }

    #[test]
    fn build_launch_config_applies_auth_env_default_when_auth_absent() {
        // OnPrem with no auth => Windows (env default), not AAD.
        let onprem = build_launch_config(
            "x".into(),
            Some("OnPrem"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(onprem.authentication, AuthMethod::Windows);

        // Sandbox with no auth => AAD.
        let sandbox = build_launch_config(
            "y".into(),
            Some("Sandbox"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(sandbox.authentication, AuthMethod::AAD);
    }

    // -- parse_zed_debug_file --------------------------------------------

    fn write(dir: &std::path::Path, rel: &str, content: &str) -> PathBuf {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn zed_file_parses_al_adapter_entry_with_jsonc() {
        let tmp = tempfile::tempdir().unwrap();
        // JSONC: includes a // comment and a trailing comma — both must be tolerated.
        let path = write(
            tmp.path(),
            ".zed/debug.json",
            r#"[
              // AL cloud sandbox
              {
                "label": "Cloud",
                "adapter": "al",
                "environmentType": "Sandbox",
                "environmentName": "MySandbox",
                "tenant": "contoso.com",
              }
            ]"#,
        );
        let df = parse_zed_debug_file(&path).unwrap();
        assert_eq!(df.path, path);
        assert_eq!(df.configs.len(), 1);
        let c = &df.configs[0];
        assert_eq!(c.name, "Cloud");
        assert_eq!(c.environment_type, EnvironmentType::Sandbox);
        assert_eq!(c.environment_name.as_deref(), Some("MySandbox"));
        assert_eq!(c.tenant.as_deref(), Some("contoso.com"));
        // No auth specified + Sandbox => AAD default.
        assert_eq!(c.authentication, AuthMethod::AAD);
    }

    #[test]
    fn zed_file_filters_non_al_entries_without_env_type() {
        let tmp = tempfile::tempdir().unwrap();
        // First entry is a non-AL adapter with no environmentType => filtered out.
        // Second entry has environmentType set (no adapter) => kept by the
        // `adapter == "al" || environment_type.is_some()` filter.
        let path = write(
            tmp.path(),
            ".zed/debug.json",
            r#"[
              { "label": "Python", "adapter": "debugpy" },
              { "label": "OnPremBC", "environmentType": "OnPrem" }
            ]"#,
        );
        let df = parse_zed_debug_file(&path).unwrap();
        assert_eq!(df.configs.len(), 1);
        assert_eq!(df.configs[0].name, "OnPremBC");
        assert_eq!(df.configs[0].environment_type, EnvironmentType::OnPrem);
        // OnPrem with no auth => Windows.
        assert_eq!(df.configs[0].authentication, AuthMethod::Windows);
    }

    #[test]
    fn zed_file_drops_entry_with_unknown_env_type() {
        let tmp = tempfile::tempdir().unwrap();
        // adapter == "al" passes the first filter, but the bad environmentType
        // makes convert/build return None, so it's filtered by filter_map.
        let path = write(
            tmp.path(),
            ".zed/debug.json",
            r#"[ { "label": "Bad", "adapter": "al", "environmentType": "Galaxy" } ]"#,
        );
        let df = parse_zed_debug_file(&path).unwrap();
        assert!(df.configs.is_empty());
    }

    #[test]
    fn zed_file_malformed_json_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), ".zed/debug.json", "{ not an array");
        assert!(parse_zed_debug_file(&path).is_err());
    }

    // -- parse_vscode_launch_file ----------------------------------------

    #[test]
    fn vscode_file_parses_configurations_array() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{
              "version": "0.2.0",
              "configurations": [
                {
                  "name": "On-prem",
                  "type": "al",
                  "environmentType": "OnPrem",
                  "server": "https://erp.example.com",
                  "serverInstance": "BC",
                  "port": 7049,
                  "authentication": "UserPassword"
                }
              ]
            }"#,
        );
        let df = parse_vscode_launch_file(&path).unwrap();
        assert_eq!(df.configs.len(), 1);
        let c = &df.configs[0];
        assert_eq!(c.name, "On-prem");
        assert_eq!(c.environment_type, EnvironmentType::OnPrem);
        assert_eq!(c.server.as_deref(), Some("https://erp.example.com"));
        assert_eq!(c.server_instance.as_deref(), Some("BC"));
        assert_eq!(c.port, Some(7049));
        assert_eq!(c.authentication, AuthMethod::UserPassword);
    }

    #[test]
    fn vscode_file_filters_non_al_type() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "Node", "type": "node" },
                { "name": "AL", "type": "al", "environmentType": "Production" }
            ] }"#,
        );
        let df = parse_vscode_launch_file(&path).unwrap();
        assert_eq!(df.configs.len(), 1);
        assert_eq!(df.configs[0].name, "AL");
        assert_eq!(df.configs[0].environment_type, EnvironmentType::Production);
    }

    #[test]
    fn vscode_file_missing_configurations_yields_empty() {
        let tmp = tempfile::tempdir().unwrap();
        // `configurations` has #[serde(default)] => absent key is an empty Vec,
        // not a parse error.
        let path = write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "version": "0.2.0" }"#,
        );
        let df = parse_vscode_launch_file(&path).unwrap();
        assert!(df.configs.is_empty());
    }

    #[test]
    fn vscode_file_malformed_json_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), ".vscode/launch.json", "}{");
        assert!(parse_vscode_launch_file(&path).is_err());
    }

    // -- find_launch_config ----------------------------------------------

    #[test]
    fn find_returns_none_when_no_config_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(find_launch_config(tmp.path()).is_none());
    }

    #[test]
    fn find_prefers_zed_over_vscode() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".zed/debug.json",
            r#"[ { "label": "ZedCfg", "adapter": "al", "environmentType": "Sandbox" } ]"#,
        );
        write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "VsCodeCfg", "type": "al", "environmentType": "OnPrem" }
            ] }"#,
        );
        let df = find_launch_config(tmp.path()).expect("should find a config");
        // Zed takes precedence when both exist and Zed has configs.
        assert_eq!(df.configs.len(), 1);
        assert_eq!(df.configs[0].name, "ZedCfg");
        assert!(df.path.ends_with("debug.json"));
    }

    #[test]
    fn find_falls_back_to_vscode_when_zed_has_no_al_configs() {
        let tmp = tempfile::tempdir().unwrap();
        // Zed file exists but contains only a non-AL entry => empty configs =>
        // function continues on to the VS Code file.
        write(
            tmp.path(),
            ".zed/debug.json",
            r#"[ { "label": "Py", "adapter": "debugpy" } ]"#,
        );
        write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "VsCodeCfg", "type": "al", "environmentType": "OnPrem" }
            ] }"#,
        );
        let df = find_launch_config(tmp.path()).expect("should fall back to VS Code");
        assert_eq!(df.configs.len(), 1);
        assert_eq!(df.configs[0].name, "VsCodeCfg");
        assert!(df.path.ends_with("launch.json"));
    }

    #[test]
    fn find_falls_back_to_vscode_when_zed_is_malformed() {
        let tmp = tempfile::tempdir().unwrap();
        // Malformed Zed file => parse error is logged and swallowed; the
        // function still continues to the VS Code file.
        write(tmp.path(), ".zed/debug.json", "{ broken");
        write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "VsCodeCfg", "type": "al", "environmentType": "Sandbox" }
            ] }"#,
        );
        let df = find_launch_config(tmp.path()).expect("should fall back to VS Code");
        assert_eq!(df.configs[0].name, "VsCodeCfg");
    }

    // -- parse_auth_method: remaining literal arms -----------------------

    #[test]
    fn auth_literal_aad_arm_matches() {
        // The match has two cloud spellings: "AAD" and "MicrosoftEntraID".
        // The existing suite only covers "MicrosoftEntraID"; assert the bare
        // "AAD" literal resolves too, independent of env type.
        assert_eq!(
            parse_auth_method(Some("AAD"), &EnvironmentType::OnPrem),
            AuthMethod::AAD
        );
        assert_eq!(
            parse_auth_method(Some("AAD"), &EnvironmentType::Sandbox),
            AuthMethod::AAD
        );
    }

    // -- convert_zed_config: full field passthrough ----------------------

    #[test]
    fn zed_file_passes_through_onprem_server_port_and_explicit_auth() {
        // Exercises convert_zed_config carrying server/serverInstance/port and an
        // explicit authentication value (rather than the env-type default) — a
        // combination the existing Zed tests don't cover.
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            ".zed/debug.json",
            r#"[
              {
                "label": "OnPrem Win",
                "adapter": "al",
                "environmentType": "OnPrem",
                "server": "https://erp.example.com",
                "serverInstance": "BC",
                "port": 7049,
                "authentication": "Windows"
              }
            ]"#,
        );
        let df = parse_zed_debug_file(&path).unwrap();
        assert_eq!(df.configs.len(), 1);
        let c = &df.configs[0];
        assert_eq!(c.name, "OnPrem Win");
        assert_eq!(c.environment_type, EnvironmentType::OnPrem);
        assert_eq!(c.server.as_deref(), Some("https://erp.example.com"));
        assert_eq!(c.server_instance.as_deref(), Some("BC"));
        assert_eq!(c.port, Some(7049));
        // Explicit Windows auth is honoured verbatim (not an env-type default).
        assert_eq!(c.authentication, AuthMethod::Windows);
    }

    #[test]
    fn zed_empty_array_yields_no_configs() {
        // A syntactically valid but empty Zed array parses cleanly and produces
        // zero configs (distinct from a parse error).
        let tmp = tempfile::tempdir().unwrap();
        let path = write(tmp.path(), ".zed/debug.json", "[]");
        let df = parse_zed_debug_file(&path).unwrap();
        assert!(df.configs.is_empty());
    }

    #[test]
    fn zed_port_out_of_u16_range_is_error() {
        // `port` is typed u16; a value above 65535 must surface as a parse error,
        // not silently truncate or default.
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            ".zed/debug.json",
            r#"[ { "label": "x", "adapter": "al", "environmentType": "OnPrem", "port": 70000 } ]"#,
        );
        assert!(parse_zed_debug_file(&path).is_err());
    }

    // -- parse_vscode_launch_file: multi-config --------------------------

    #[test]
    fn vscode_file_keeps_multiple_al_configs_and_filters_others() {
        // Multiple AL configurations in one launch.json must all be retained in
        // order, while non-AL entries are dropped.
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "Cloud", "type": "al", "environmentType": "Sandbox", "environmentName": "S1" },
                { "name": "Node", "type": "node" },
                { "name": "Prod", "type": "al", "environmentType": "Production", "tenant": "contoso.com" }
            ] }"#,
        );
        let df = parse_vscode_launch_file(&path).unwrap();
        assert_eq!(df.configs.len(), 2);
        assert_eq!(df.configs[0].name, "Cloud");
        assert_eq!(df.configs[0].environment_type, EnvironmentType::Sandbox);
        assert_eq!(df.configs[0].environment_name.as_deref(), Some("S1"));
        assert_eq!(df.configs[1].name, "Prod");
        assert_eq!(df.configs[1].environment_type, EnvironmentType::Production);
        assert_eq!(df.configs[1].tenant.as_deref(), Some("contoso.com"));
    }

    #[test]
    fn vscode_port_out_of_u16_range_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "x", "type": "al", "environmentType": "OnPrem", "port": 99999 }
            ] }"#,
        );
        assert!(parse_vscode_launch_file(&path).is_err());
    }

    // -- find_launch_config: vscode-only & empty-zed paths ---------------

    #[test]
    fn find_uses_vscode_when_no_zed_file_present() {
        // No .zed/debug.json at all: the Zed branch is skipped entirely and the
        // VS Code file is used. Distinct from the malformed/empty-zed fallbacks.
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "OnlyVsCode", "type": "al", "environmentType": "OnPrem" }
            ] }"#,
        );
        let df = find_launch_config(tmp.path()).expect("should use VS Code file");
        assert_eq!(df.configs.len(), 1);
        assert_eq!(df.configs[0].name, "OnlyVsCode");
        assert!(df.path.ends_with("launch.json"));
    }

    #[test]
    fn find_returns_none_when_only_empty_zed_present() {
        // Zed file exists but yields zero AL configs and there is no VS Code
        // file => the whole function returns None (no panic, no fallback).
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".zed/debug.json",
            r#"[ { "label": "Py", "adapter": "debugpy" } ]"#,
        );
        assert!(find_launch_config(tmp.path()).is_none());
    }

    // -- find_launch_config: log call sites execute under a subscriber ---

    /// Minimal `tracing::Subscriber` that claims every level is enabled, forcing
    /// `debug!`/`warn!` argument closures to actually run. No-ops everything
    /// else. Used to drive the diagnostic branches inside `find_launch_config`.
    struct AlwaysOnSubscriber;

    impl tracing::Subscriber for AlwaysOnSubscriber {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, _: &tracing::Event<'_>) {}
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn find_drives_log_branches_with_active_subscriber() {
        // The success and error diagnostic branches in find_launch_config only
        // evaluate their log-argument closures when a subscriber accepts the
        // level. Run the function end-to-end under such a subscriber so those
        // call sites are genuinely exercised, then assert the real return value
        // is still correct (the logging must not alter behaviour).
        let tmp = tempfile::tempdir().unwrap();
        // Malformed Zed (hits the warn! error arm) then a valid VS Code file
        // (hits the debug! success arm) — both diagnostic branches in one run.
        write(tmp.path(), ".zed/debug.json", "{ broken");
        write(
            tmp.path(),
            ".vscode/launch.json",
            r#"{ "configurations": [
                { "name": "Logged", "type": "al", "environmentType": "Sandbox" }
            ] }"#,
        );

        let df = tracing::subscriber::with_default(AlwaysOnSubscriber, || {
            find_launch_config(tmp.path())
        })
        .expect("should fall back to VS Code even with logging active");
        assert_eq!(df.configs.len(), 1);
        assert_eq!(df.configs[0].name, "Logged");
    }

    #[test]
    fn find_drives_zed_success_and_empty_log_branches() {
        // Cover the Zed success debug! branch (configs present) and the Zed
        // "no AL configurations" debug! branch (empty after filtering) under an
        // active subscriber, in two separate find_launch_config runs.
        // 1) Zed with a real AL config => success debug! branch.
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            ".zed/debug.json",
            r#"[ { "label": "ZedOk", "adapter": "al", "environmentType": "Sandbox" } ]"#,
        );
        let df = tracing::subscriber::with_default(AlwaysOnSubscriber, || {
            find_launch_config(tmp.path())
        })
        .expect("zed config should be found");
        assert_eq!(df.configs[0].name, "ZedOk");

        // 2) Zed present but empty-after-filter, no vscode => debug! "no AL
        //    configurations" branch, then None.
        let tmp2 = tempfile::tempdir().unwrap();
        write(
            tmp2.path(),
            ".zed/debug.json",
            r#"[ { "label": "Py", "adapter": "debugpy" } ]"#,
        );
        let none = tracing::subscriber::with_default(AlwaysOnSubscriber, || {
            find_launch_config(tmp2.path())
        });
        assert!(none.is_none());
    }
}
