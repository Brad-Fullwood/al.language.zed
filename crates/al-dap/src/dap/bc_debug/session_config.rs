//! DAP-args parsing and BC connection-string construction for the BC debug client.
//! Split out of the former monolithic `bc_debug.rs` (pure move, no behavior change).

use super::wire::percent_encode_url;

/// Parse a DAP arg value that may be a `bool` or a `string` ("none"/"false" → false).
/// `default` is returned for non-bool, non-string variants.
fn parse_bool_or_string(v: &serde_json::Value, default: bool) -> bool {
    match v {
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => {
            !s.eq_ignore_ascii_case("none") && !s.eq_ignore_ascii_case("false")
        }
        _ => default,
    }
}

#[derive(Debug, Clone)]
pub struct BcDebugConfig {
    pub server: Option<String>,
    pub server_instance: Option<String>,
    pub port: u16,
    pub tenant: String,
    pub environment_type: String,
    pub environment_name: Option<String>,
    pub authentication: String,
    pub break_on_error: bool,
    pub break_on_record_write: bool,
    pub break_on_next: Option<String>,
    /// `sessionId` — a specific BC client session to attach to. `None` (the
    /// schema's `-1` sentinel) means "no specific session", in which case the
    /// `break_on_next` selector decides which upcoming session to break into.
    /// Only consumed by `attach`.
    pub session_id: Option<i64>,
    pub startup_object_type: String,
    pub startup_object_id: i64,
    pub launch_browser: bool,
    pub schema_update_mode: String,
    pub dependency_publishing_option: String,
    pub accept_invalid_certs: bool,
}

impl Default for BcDebugConfig {
    fn default() -> Self {
        Self {
            server: None,
            server_instance: None,
            port: 7049,
            tenant: "default".to_string(),
            environment_type: "Sandbox".to_string(),
            environment_name: None,
            authentication: "UserPassword".to_string(),
            break_on_error: true,
            break_on_record_write: false,
            break_on_next: None,
            session_id: None,
            startup_object_type: "Page".to_string(),
            startup_object_id: 22,
            launch_browser: true,
            schema_update_mode: "Synchronize".to_string(),
            dependency_publishing_option: "Default".to_string(),
            accept_invalid_certs: false,
        }
    }
}

impl BcDebugConfig {
    pub fn from_dap_args(args: &serde_json::Value) -> Self {
        let mut cfg = Self::default();
        if let Some(s) = args.get("server").and_then(|v| v.as_str()) {
            cfg.server = Some(s.to_string());
        }
        if let Some(s) = args.get("serverInstance").and_then(|v| v.as_str()) {
            cfg.server_instance = Some(s.to_string());
        }
        if let Some(n) = args.get("port").and_then(|v| v.as_u64()) {
            if let Ok(port) = u16::try_from(n) {
                cfg.port = port;
            }
        }
        if let Some(s) = args.get("tenant").and_then(|v| v.as_str()) {
            cfg.tenant = s.to_string();
        }
        if let Some(s) = args.get("environmentType").and_then(|v| v.as_str()) {
            cfg.environment_type = s.to_string();
        }
        if let Some(s) = args.get("environmentName").and_then(|v| v.as_str()) {
            cfg.environment_name = Some(s.to_string());
        }
        if let Some(s) = args.get("authentication").and_then(|v| v.as_str()) {
            cfg.authentication = s.to_string();
        }
        if let Some(v) = args.get("breakOnError") {
            cfg.break_on_error = parse_bool_or_string(v, true);
        }
        if let Some(v) = args.get("breakOnRecordWrite") {
            cfg.break_on_record_write = parse_bool_or_string(v, false);
        }
        if let Some(s) = args.get("breakOnNext").and_then(|v| v.as_str()) {
            cfg.break_on_next = Some(s.to_string());
        }
        // `sessionId` selects one specific BC client session to attach to. The
        // schema default is `-1` ("no specific session" → use breakOnNext), so
        // map any negative value to None and only carry a real (>= 0) id.
        if let Some(n) = args.get("sessionId").and_then(|v| v.as_i64()) {
            cfg.session_id = (n >= 0).then_some(n);
        }
        if let Some(s) = args.get("startupObjectType").and_then(|v| v.as_str()) {
            cfg.startup_object_type = s.to_string();
        }
        if let Some(n) = args.get("startupObjectId").and_then(|v| v.as_i64()) {
            cfg.startup_object_id = n;
        }
        if let Some(v) = args.get("launchBrowser") {
            cfg.launch_browser = parse_bool_or_string(v, cfg.launch_browser);
        }
        if let Some(s) = args.get("schemaUpdateMode").and_then(|v| v.as_str()) {
            cfg.schema_update_mode = s.to_string();
        }
        if let Some(s) = args
            .get("dependencyPublishingOption")
            .and_then(|v| v.as_str())
        {
            cfg.dependency_publishing_option = s.to_string();
        }
        if let Some(v) = args.get("validateServerCertificate") {
            cfg.accept_invalid_certs = !parse_bool_or_string(v, !cfg.accept_invalid_certs);
        }
        cfg
    }

    /// Build the base URL prefix for on-prem: `{server}:{port}/{instance}`.
    /// Uses the same pattern as `al-core::launch::BcServerConfig::dev_packages_url`.
    pub(crate) fn onprem_base(&self) -> String {
        let server = self.server.as_deref().unwrap_or("http://localhost");
        let instance = self.server_instance.as_deref().unwrap_or("BC");
        let host = format!("{}:{}", server.trim_end_matches('/'), self.port);
        format!("{host}/{instance}")
    }

    pub fn base_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            // Fix #3: include port in on-prem URL
            format!("{}/dev", self.onprem_base())
        } else {
            // Fix #2: cloud URL must include tenant before environment name.
            // URL-encode tenant and environment name so values containing special
            // characters (spaces, dots, slashes) produce valid URLs.
            let tenant = percent_encode_url(&self.tenant);
            let env = percent_encode_url(self.environment_name.as_deref().unwrap_or("sandbox"));
            format!("https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev")
        }
    }

    pub fn debug_hub_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            // Fix #3: include port in on-prem URL
            format!("{}/dev/DebuggerHub", self.onprem_base())
        } else {
            // Fix #2: cloud URL must include tenant before environment name.
            // URL-encode tenant and environment name (same reason as base_url).
            let tenant = percent_encode_url(&self.tenant);
            let env = percent_encode_url(self.environment_name.as_deref().unwrap_or("sandbox"));
            format!("https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev/DebuggerHub")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cloud_config(tenant: &str, env_name: &str) -> BcDebugConfig {
        BcDebugConfig {
            environment_type: "Sandbox".to_string(),
            tenant: tenant.to_string(),
            environment_name: Some(env_name.to_string()),
            ..BcDebugConfig::default()
        }
    }

    fn onprem_config(server: &str, instance: &str, port: u16) -> BcDebugConfig {
        BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some(server.to_string()),
            server_instance: Some(instance.to_string()),
            port,
            ..BcDebugConfig::default()
        }
    }

    #[test]
    fn cloud_base_url_includes_tenant() {
        // Fix #2: cloud URL must include tenant before environment name
        let cfg = cloud_config("mytenant.onmicrosoft.com", "MySandbox");
        let url = cfg.base_url();
        assert!(
            url.contains("mytenant.onmicrosoft.com"),
            "cloud base_url should contain tenant: {url}"
        );
        assert!(
            url.contains("MySandbox"),
            "cloud base_url should contain env name: {url}"
        );
        assert_eq!(
            url,
            "https://api.businesscentral.dynamics.com/v2.0/mytenant.onmicrosoft.com/MySandbox/dev"
        );
    }

    #[test]
    fn cloud_hub_url_includes_tenant() {
        let cfg = cloud_config("contoso.com", "Production");
        let url = cfg.debug_hub_url();
        assert_eq!(
            url,
            "https://api.businesscentral.dynamics.com/v2.0/contoso.com/Production/dev/DebuggerHub"
        );
    }

    #[test]
    fn onprem_base_url_includes_port() {
        // Fix #3: on-prem URL must include port
        let cfg = onprem_config("http://erp.example.com", "BC240", 7050);
        let url = cfg.base_url();
        assert!(
            url.contains(":7050"),
            "on-prem base_url should include custom port: {url}"
        );
        assert_eq!(url, "http://erp.example.com:7050/BC240/dev");
    }

    #[test]
    fn onprem_hub_url_includes_port() {
        let cfg = onprem_config("https://bc.corp.local", "PROD", 9090);
        let url = cfg.debug_hub_url();
        assert_eq!(url, "https://bc.corp.local:9090/PROD/dev/DebuggerHub");
    }

    #[test]
    fn onprem_default_port_still_included() {
        // Default port (7049) should still appear in the URL — always include port
        let cfg = onprem_config("http://localhost", "BC", 7049);
        let url = cfg.base_url();
        assert!(
            url.contains(":7049"),
            "default port should be in URL: {url}"
        );
    }

    #[test]
    fn from_dap_args_rejects_out_of_range_port() {
        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({ "port": 65536 }));
        assert_eq!(
            cfg.port,
            BcDebugConfig::default().port,
            "out-of-range port must not truncate into a valid-looking port"
        );
    }

    #[test]
    fn parse_bool_or_string_handles_bool_variant() {
        assert!(parse_bool_or_string(&serde_json::json!(true), false));
        assert!(!parse_bool_or_string(&serde_json::json!(false), true));
    }

    #[test]
    fn parse_bool_or_string_string_none_and_false_are_falsey() {
        // The documented disabling strings "none"/"false" (case-insensitive)
        // map to false even though they are non-empty strings.
        for s in ["none", "None", "NONE", "false", "False", "FALSE"] {
            assert!(
                !parse_bool_or_string(&serde_json::json!(s), true),
                "{s:?} must parse as false"
            );
        }
    }

    #[test]
    fn parse_bool_or_string_other_strings_are_truthy() {
        // Any other string (e.g. "all", "true", an event filter name) is true.
        for s in ["true", "all", "yes", "RecordWrite"] {
            assert!(
                parse_bool_or_string(&serde_json::json!(s), false),
                "{s:?} must parse as true"
            );
        }
    }

    #[test]
    fn parse_bool_or_string_non_bool_non_string_returns_default() {
        for v in [
            serde_json::json!(1),
            serde_json::json!(null),
            serde_json::json!([1, 2]),
            serde_json::json!({"a": 1}),
        ] {
            assert!(parse_bool_or_string(&v, true), "{v} default=true");
            assert!(!parse_bool_or_string(&v, false), "{v} default=false");
        }
    }

    #[test]
    fn from_dap_args_empty_yields_defaults() {
        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({}));
        let def = BcDebugConfig::default();
        assert_eq!(cfg.port, def.port);
        assert_eq!(cfg.tenant, def.tenant);
        assert_eq!(cfg.environment_type, def.environment_type);
        assert_eq!(cfg.break_on_error, def.break_on_error);
        assert_eq!(cfg.break_on_record_write, def.break_on_record_write);
        assert_eq!(cfg.startup_object_type, def.startup_object_type);
        assert_eq!(cfg.startup_object_id, def.startup_object_id);
        assert_eq!(cfg.accept_invalid_certs, def.accept_invalid_certs);
    }

    #[test]
    fn from_dap_args_maps_all_scalar_fields() {
        let args = serde_json::json!({
            "server": "http://bc.local",
            "serverInstance": "BC240",
            "port": 8080,
            "tenant": "mytenant",
            "environmentType": "OnPrem",
            "environmentName": "Prod",
            "authentication": "AAD",
            "breakOnNext": "RecordWrite",
            "startupObjectType": "Table",
            "startupObjectId": 18,
            "schemaUpdateMode": "Recreate",
            "dependencyPublishingOption": "Ignore",
        });
        let cfg = BcDebugConfig::from_dap_args(&args);
        assert_eq!(cfg.server.as_deref(), Some("http://bc.local"));
        assert_eq!(cfg.server_instance.as_deref(), Some("BC240"));
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.tenant, "mytenant");
        assert_eq!(cfg.environment_type, "OnPrem");
        assert_eq!(cfg.environment_name.as_deref(), Some("Prod"));
        assert_eq!(cfg.authentication, "AAD");
        assert_eq!(cfg.break_on_next.as_deref(), Some("RecordWrite"));
        assert_eq!(cfg.startup_object_type, "Table");
        assert_eq!(cfg.startup_object_id, 18);
        assert_eq!(cfg.schema_update_mode, "Recreate");
        assert_eq!(cfg.dependency_publishing_option, "Ignore");
    }

    #[test]
    fn from_dap_args_parses_session_id() {
        // A real (>= 0) sessionId is carried through as Some.
        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({ "sessionId": 42 }));
        assert_eq!(cfg.session_id, Some(42));
        let zero = BcDebugConfig::from_dap_args(&serde_json::json!({ "sessionId": 0 }));
        assert_eq!(zero.session_id, Some(0), "0 is a valid session id");

        // Absent or the schema's -1 sentinel both mean "no specific session".
        assert_eq!(
            BcDebugConfig::from_dap_args(&serde_json::json!({})).session_id,
            None,
            "absent sessionId defaults to None"
        );
        assert_eq!(
            BcDebugConfig::from_dap_args(&serde_json::json!({ "sessionId": -1 })).session_id,
            None,
            "-1 sentinel maps to None (attach via breakOnNext instead)"
        );

        // A non-integer value is ignored (stays None) rather than panicking.
        assert_eq!(
            BcDebugConfig::from_dap_args(&serde_json::json!({ "sessionId": "nope" })).session_id,
            None
        );
    }

    #[test]
    fn from_dap_args_break_flags_accept_bool_and_string() {
        let bool_args = serde_json::json!({
            "breakOnError": false,
            "breakOnRecordWrite": true,
        });
        let cfg = BcDebugConfig::from_dap_args(&bool_args);
        assert!(!cfg.break_on_error);
        assert!(cfg.break_on_record_write);

        let str_args = serde_json::json!({
            "breakOnError": "none",
            "breakOnRecordWrite": "all",
        });
        let cfg = BcDebugConfig::from_dap_args(&str_args);
        assert!(!cfg.break_on_error, "\"none\" disables break_on_error");
        assert!(
            cfg.break_on_record_write,
            "\"all\" enables break_on_record_write"
        );
    }

    #[test]
    fn from_dap_args_validate_server_certificate_inverts_to_accept_invalid_certs() {
        // accept_invalid_certs is the logical inverse of validateServerCertificate.
        // validateServerCertificate=false → certs NOT validated → accept_invalid_certs=true.
        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({
            "validateServerCertificate": false,
        }));
        assert!(
            cfg.accept_invalid_certs,
            "validateServerCertificate=false must enable accept_invalid_certs"
        );

        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({
            "validateServerCertificate": true,
        }));
        assert!(
            !cfg.accept_invalid_certs,
            "validateServerCertificate=true must keep certs validated"
        );
    }

    #[test]
    fn from_dap_args_ignores_wrong_typed_values() {
        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({
            "port": "not-a-number",
            "startupObjectId": "nope",
            "server": 123,
        }));
        let def = BcDebugConfig::default();
        assert_eq!(cfg.port, def.port);
        assert_eq!(cfg.startup_object_id, def.startup_object_id);
        assert_eq!(cfg.server, def.server);
    }

    #[test]
    fn onprem_base_uses_localhost_and_bc_defaults() {
        let cfg = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            port: 7049,
            ..BcDebugConfig::default()
        };
        assert_eq!(cfg.base_url(), "http://localhost:7049/BC/dev");
        assert_eq!(
            cfg.debug_hub_url(),
            "http://localhost:7049/BC/dev/DebuggerHub"
        );
    }

    #[test]
    fn onprem_base_trims_trailing_slash_on_server() {
        let cfg = onprem_config("http://bc.local/", "BC", 7049);
        assert_eq!(cfg.base_url(), "http://bc.local:7049/BC/dev");
    }

    #[test]
    fn cloud_base_url_defaults_environment_name_to_sandbox() {
        let cfg = BcDebugConfig {
            environment_type: "Sandbox".to_string(),
            tenant: "t".to_string(),
            environment_name: None,
            ..BcDebugConfig::default()
        };
        assert_eq!(
            cfg.base_url(),
            "https://api.businesscentral.dynamics.com/v2.0/t/sandbox/dev"
        );
    }

    #[test]
    fn cloud_url_percent_encodes_tenant_and_env() {
        // Special characters in tenant / env name are percent-encoded so the
        // URL stays valid (space → %20).
        let cfg = cloud_config("my tenant", "My Env");
        let url = cfg.base_url();
        assert!(url.contains("my%20tenant"), "tenant encoded: {url}");
        assert!(url.contains("My%20Env"), "env encoded: {url}");
    }
}
