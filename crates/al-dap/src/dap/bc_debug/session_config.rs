//! DAP-args parsing and BC connection-string construction for the BC debug client.
//! Split out of the former monolithic `bc_debug.rs` (pure move, no behavior change).

use super::wire::percent_encode_url;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakOnError {
    None,
    All,
    ExcludeTry,
}

impl BreakOnError {
    pub const fn enabled(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn wire_value(self) -> u8 {
        match self {
            Self::None => 1,
            Self::All => 2,
            Self::ExcludeTry => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakOnRecordWrite {
    None,
    All,
    ExcludeTemporary,
}

impl BreakOnRecordWrite {
    pub const fn enabled(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn wire_value(self) -> u8 {
        match self {
            Self::None => 1,
            Self::All => 2,
            Self::ExcludeTemporary => 3,
        }
    }
}

fn parse_break_on_error(value: &serde_json::Value) -> Result<BreakOnError, String> {
    match value {
        serde_json::Value::Bool(false) => Ok(BreakOnError::None),
        serde_json::Value::Bool(true) => Ok(BreakOnError::All),
        serde_json::Value::String(value)
            if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("false") =>
        {
            Ok(BreakOnError::None)
        }
        serde_json::Value::String(value)
            if value.eq_ignore_ascii_case("all") || value.eq_ignore_ascii_case("true") =>
        {
            Ok(BreakOnError::All)
        }
        serde_json::Value::String(value) if value.eq_ignore_ascii_case("excludetry") => {
            Ok(BreakOnError::ExcludeTry)
        }
        _ => Err(
            "breakOnError must be a boolean or one of None, False, All, True, ExcludeTry"
                .to_string(),
        ),
    }
}

fn parse_break_on_record_write(value: &serde_json::Value) -> Result<BreakOnRecordWrite, String> {
    match value {
        serde_json::Value::Bool(false) => Ok(BreakOnRecordWrite::None),
        serde_json::Value::Bool(true) => Ok(BreakOnRecordWrite::All),
        serde_json::Value::String(value)
            if value.eq_ignore_ascii_case("none") || value.eq_ignore_ascii_case("false") =>
        {
            Ok(BreakOnRecordWrite::None)
        }
        serde_json::Value::String(value)
            if value.eq_ignore_ascii_case("all") || value.eq_ignore_ascii_case("true") =>
        {
            Ok(BreakOnRecordWrite::All)
        }
        serde_json::Value::String(value) if value.eq_ignore_ascii_case("excludetemporary") => {
            Ok(BreakOnRecordWrite::ExcludeTemporary)
        }
        _ => Err(
            "breakOnRecordWrite must be a boolean or one of None, False, All, True, \
             ExcludeTemporary"
                .to_string(),
        ),
    }
}

fn optional_string(
    args: &serde_json::Value,
    key: &str,
    errors: &mut Vec<String>,
) -> Option<String> {
    match args.get(key) {
        None => None,
        Some(serde_json::Value::String(value)) => Some(value.clone()),
        Some(_) => {
            errors.push(format!("{key} must be a string"));
            None
        }
    }
}

fn optional_bool(args: &serde_json::Value, key: &str, errors: &mut Vec<String>) -> Option<bool> {
    match args.get(key) {
        None => None,
        Some(serde_json::Value::Bool(value)) => Some(*value),
        Some(_) => {
            errors.push(format!("{key} must be a boolean"));
            None
        }
    }
}

fn optional_i64(args: &serde_json::Value, key: &str, errors: &mut Vec<String>) -> Option<i64> {
    match args.get(key) {
        None => None,
        Some(value) => match value.as_i64() {
            Some(value) => Some(value),
            None => {
                errors.push(format!("{key} must be an integer"));
                None
            }
        },
    }
}

fn optional_u64(args: &serde_json::Value, key: &str, errors: &mut Vec<String>) -> Option<u64> {
    match args.get(key) {
        None => None,
        Some(value) => match value.as_u64() {
            Some(value) => Some(value),
            None => {
                errors.push(format!("{key} must be a non-negative integer"));
                None
            }
        },
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
    pub break_on_error: BreakOnError,
    pub break_on_record_write: BreakOnRecordWrite,
    pub break_on_next: Option<String>,
    /// `sessionId` — a specific BC client session to attach to. `None` (the
    /// schema's `-1` sentinel) means "no specific session", in which case the
    /// `break_on_next` selector decides which upcoming session to break into.
    /// Only consumed by `attach`.
    pub session_id: Option<i64>,
    pub startup_object_type: String,
    pub startup_object_id: i64,
    pub startup_company: Option<String>,
    pub launch_browser: bool,
    pub schema_update_mode: String,
    pub dependency_publishing_option: String,
    pub enable_sql_information_debugger: bool,
    pub enable_long_running_sql_statements: bool,
    pub long_running_sql_statements_threshold: u64,
    pub number_of_sql_statements: u64,
    pub accept_invalid_certs: bool,
    #[doc(hidden)]
    pub validation_errors: Vec<String>,
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
            authentication: "MicrosoftEntraID".to_string(),
            break_on_error: BreakOnError::All,
            break_on_record_write: BreakOnRecordWrite::None,
            break_on_next: None,
            session_id: None,
            startup_object_type: "Page".to_string(),
            startup_object_id: 22,
            startup_company: None,
            launch_browser: true,
            schema_update_mode: "Synchronize".to_string(),
            dependency_publishing_option: "Default".to_string(),
            enable_sql_information_debugger: true,
            enable_long_running_sql_statements: true,
            long_running_sql_statements_threshold: 500,
            number_of_sql_statements: 10,
            accept_invalid_certs: false,
            validation_errors: Vec::new(),
        }
    }
}

impl BcDebugConfig {
    pub fn from_dap_args(args: &serde_json::Value) -> Self {
        let mut cfg = Self::default();
        if !args.is_object() {
            cfg.validation_errors
                .push("debug configuration must be a JSON object".to_string());
            return cfg;
        }
        if let Some(value) = optional_string(args, "server", &mut cfg.validation_errors) {
            cfg.server = Some(value);
        }
        if let Some(value) = optional_string(args, "serverInstance", &mut cfg.validation_errors) {
            cfg.server_instance = Some(value);
        }
        if let Some(value) = optional_i64(args, "port", &mut cfg.validation_errors) {
            match u16::try_from(value) {
                Ok(0) | Err(_) => cfg
                    .validation_errors
                    .push("port must be an integer from 1 through 65535".to_string()),
                Ok(port) => cfg.port = port,
            }
        }
        if let Some(value) = optional_string(args, "tenant", &mut cfg.validation_errors) {
            cfg.tenant = value;
        }
        if let Some(value) = optional_string(args, "environmentType", &mut cfg.validation_errors) {
            cfg.environment_type = value;
        }
        if let Some(value) = optional_string(args, "environmentName", &mut cfg.validation_errors) {
            cfg.environment_name = Some(value);
        }
        if let Some(value) = optional_string(args, "authentication", &mut cfg.validation_errors) {
            cfg.authentication = value;
        }
        if let Some(v) = args.get("breakOnError") {
            match parse_break_on_error(v) {
                Ok(mode) => cfg.break_on_error = mode,
                Err(error) => cfg.validation_errors.push(error),
            }
        }
        if let Some(v) = args.get("breakOnRecordWrite") {
            match parse_break_on_record_write(v) {
                Ok(mode) => cfg.break_on_record_write = mode,
                Err(error) => cfg.validation_errors.push(error),
            }
        }
        if let Some(value) = optional_string(args, "breakOnNext", &mut cfg.validation_errors) {
            cfg.break_on_next = Some(value);
        }
        // `sessionId` selects one specific BC client session to attach to. The
        // schema default is `-1` ("no specific session" → use breakOnNext), so
        // map any negative value to None and only carry a real (>= 0) id.
        if let Some(value) = optional_i64(args, "sessionId", &mut cfg.validation_errors) {
            match value {
                -1 => cfg.session_id = None,
                0.. => cfg.session_id = Some(value),
                _ => cfg
                    .validation_errors
                    .push("sessionId must be -1 or a non-negative integer".to_string()),
            }
        }
        if let Some(value) = optional_string(args, "startupObjectType", &mut cfg.validation_errors)
        {
            cfg.startup_object_type = value;
        }
        if let Some(value) = optional_i64(args, "startupObjectId", &mut cfg.validation_errors) {
            if value < 0 {
                cfg.validation_errors
                    .push("startupObjectId must be a non-negative integer".to_string());
            } else {
                cfg.startup_object_id = value;
            }
        }
        if let Some(value) = optional_string(args, "startupCompany", &mut cfg.validation_errors) {
            cfg.startup_company = Some(value);
        }
        if let Some(value) = optional_bool(args, "launchBrowser", &mut cfg.validation_errors) {
            cfg.launch_browser = value;
        }
        if let Some(value) = optional_string(args, "schemaUpdateMode", &mut cfg.validation_errors) {
            cfg.schema_update_mode = value;
        }
        if let Some(value) = optional_string(
            args,
            "dependencyPublishingOption",
            &mut cfg.validation_errors,
        ) {
            cfg.dependency_publishing_option = value;
        }
        if let Some(value) = optional_bool(
            args,
            "enableSqlInformationDebugger",
            &mut cfg.validation_errors,
        ) {
            cfg.enable_sql_information_debugger = value;
        }
        if let Some(value) = optional_bool(
            args,
            "enableLongRunningSqlStatements",
            &mut cfg.validation_errors,
        ) {
            cfg.enable_long_running_sql_statements = value;
        }
        if let Some(value) = optional_u64(
            args,
            "longRunningSqlStatementsThreshold",
            &mut cfg.validation_errors,
        ) {
            cfg.long_running_sql_statements_threshold = value;
        }
        if let Some(value) = optional_u64(args, "numberOfSqlStatements", &mut cfg.validation_errors)
        {
            cfg.number_of_sql_statements = value;
        }
        let validate_server_certificate = optional_bool(
            args,
            "validateServerCertificate",
            &mut cfg.validation_errors,
        );
        let accept_invalid_certs =
            optional_bool(args, "acceptInvalidCerts", &mut cfg.validation_errors);
        match (validate_server_certificate, accept_invalid_certs) {
            (Some(validate), Some(accept)) if accept == validate => {
                cfg.validation_errors.push(
                    "acceptInvalidCerts must be the inverse of validateServerCertificate when \
                     both fields are present"
                        .to_string(),
                );
            }
            (Some(validate), _) => cfg.accept_invalid_certs = !validate,
            (None, Some(accept)) => cfg.accept_invalid_certs = accept,
            (None, None) => {}
        }
        cfg
    }

    /// Validate behavior that is specific to this native adapter.
    ///
    /// The Microsoft legacy DAP remains available for Windows and
    /// NavUserPassword authentication. The native REST/SignalR client supports
    /// OAuth bearer authentication only and must fail before attempting an
    /// OAuth flow when a different mode is requested.
    pub fn validate_native(&self) -> Result<(), String> {
        if !self.validation_errors.is_empty() {
            return Err(self.validation_errors.join("; "));
        }
        if !matches!(
            self.authentication.to_ascii_lowercase().as_str(),
            "microsoftentraid" | "aad"
        ) {
            return Err(format!(
                "Native AL DAP does not support authentication mode `{}`. Use \
                 MicrosoftEntraID/AAD, or set al.useOfficialDap=true for the Microsoft \
                 adapter's Windows/UserPassword support.",
                self.authentication
            ));
        }
        if !matches!(
            self.environment_type.to_ascii_lowercase().as_str(),
            "onprem" | "sandbox" | "production"
        ) {
            return Err(format!(
                "environmentType must be OnPrem, Sandbox, or Production; got `{}`",
                self.environment_type
            ));
        }
        if !matches!(
            self.startup_object_type.to_ascii_lowercase().as_str(),
            "page" | "table" | "report" | "query"
        ) {
            return Err(format!(
                "startupObjectType must be one of Page, Table, Report, Query; got `{}`",
                self.startup_object_type
            ));
        }
        if let Some(break_on_next) = &self.break_on_next {
            let normalized = break_on_next
                .replace([' ', '-', '_'], "")
                .to_ascii_lowercase();
            if !matches!(
                normalized.as_str(),
                "webserviceclient" | "webclient" | "background" | "clientservice" | "agent"
            ) {
                return Err(format!(
                    "breakOnNext must be one of WebServiceClient, WebClient, Background, \
                     ClientService, Agent; got `{break_on_next}`"
                ));
            }
        }
        if !matches!(
            self.schema_update_mode.to_ascii_lowercase().as_str(),
            "synchronize" | "recreate" | "forcesync"
        ) {
            return Err(format!(
                "schemaUpdateMode must be Synchronize, Recreate, or ForceSync; got `{}`",
                self.schema_update_mode
            ));
        }
        if !matches!(
            self.dependency_publishing_option
                .to_ascii_lowercase()
                .as_str(),
            "default" | "ignore" | "strict"
        ) {
            return Err(format!(
                "dependencyPublishingOption must be Default, Ignore, or Strict; got `{}`",
                self.dependency_publishing_option
            ));
        }
        Ok(())
    }

    /// Build the base URL prefix for on-prem: `{server}:{port}/{instance}`.
    /// Uses the same pattern as the BC server client.
    pub(crate) fn onprem_base(&self) -> String {
        let server = self.server.as_deref().unwrap_or("http://localhost");
        let instance = self.server_instance.as_deref().unwrap_or("BC");
        let host = format!("{}:{}", server.trim_end_matches('/'), self.port);
        format!("{host}/{instance}")
    }

    pub fn base_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            format!("{}/dev", self.onprem_base())
        } else {
            // URL-encode tenant and environment name so values containing special
            // characters (spaces, dots, slashes) produce valid URLs.
            let tenant = percent_encode_url(&self.tenant);
            let env = percent_encode_url(self.environment_name.as_deref().unwrap_or("sandbox"));
            format!("https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/dev")
        }
    }

    pub fn debug_hub_url(&self) -> String {
        if self.environment_type.eq_ignore_ascii_case("OnPrem") {
            format!("{}/dev/DebuggerHub", self.onprem_base())
        } else {
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
    use std::collections::BTreeSet;

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
        // Cloud URLs include the tenant before the environment name.
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
        // On-premises URLs include the configured port.
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
        assert!(cfg.validate_native().unwrap_err().contains("port"));
    }

    #[test]
    fn break_modes_accept_boolean_variants() {
        assert_eq!(
            parse_break_on_error(&serde_json::json!(true)),
            Ok(BreakOnError::All)
        );
        assert_eq!(
            parse_break_on_record_write(&serde_json::json!(false)),
            Ok(BreakOnRecordWrite::None)
        );
    }

    #[test]
    fn break_modes_preserve_every_documented_enum_value() {
        for (value, expected) in [
            ("None", BreakOnError::None),
            ("False", BreakOnError::None),
            ("All", BreakOnError::All),
            ("True", BreakOnError::All),
            ("ExcludeTry", BreakOnError::ExcludeTry),
        ] {
            assert_eq!(
                parse_break_on_error(&serde_json::json!(value)),
                Ok(expected)
            );
        }
        for (value, expected) in [
            ("None", BreakOnRecordWrite::None),
            ("False", BreakOnRecordWrite::None),
            ("All", BreakOnRecordWrite::All),
            ("True", BreakOnRecordWrite::All),
            ("ExcludeTemporary", BreakOnRecordWrite::ExcludeTemporary),
        ] {
            assert_eq!(
                parse_break_on_record_write(&serde_json::json!(value)),
                Ok(expected)
            );
        }
        assert_eq!(BreakOnError::ExcludeTry.wire_value(), 3);
        assert_eq!(BreakOnRecordWrite::ExcludeTemporary.wire_value(), 3);
    }

    #[test]
    fn invalid_break_mode_is_retained_as_a_validation_error() {
        let config = BcDebugConfig::from_dap_args(&serde_json::json!({
            "breakOnError": "Sometimes"
        }));
        assert!(config
            .validate_native()
            .expect_err("invalid mode must be rejected")
            .contains("breakOnError"));
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
            "breakOnNext": "WebClient",
            "startupObjectType": "Table",
            "startupObjectId": 18,
            "startupCompany": "CRONUS UK",
            "launchBrowser": false,
            "schemaUpdateMode": "Recreate",
            "dependencyPublishingOption": "Ignore",
            "enableSqlInformationDebugger": false,
            "enableLongRunningSqlStatements": false,
            "longRunningSqlStatementsThreshold": 750,
            "numberOfSqlStatements": 25,
            "validateServerCertificate": false,
        });
        let cfg = BcDebugConfig::from_dap_args(&args);
        assert_eq!(cfg.server.as_deref(), Some("http://bc.local"));
        assert_eq!(cfg.server_instance.as_deref(), Some("BC240"));
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.tenant, "mytenant");
        assert_eq!(cfg.environment_type, "OnPrem");
        assert_eq!(cfg.environment_name.as_deref(), Some("Prod"));
        assert_eq!(cfg.authentication, "AAD");
        assert_eq!(cfg.break_on_next.as_deref(), Some("WebClient"));
        assert_eq!(cfg.startup_object_type, "Table");
        assert_eq!(cfg.startup_object_id, 18);
        assert_eq!(cfg.startup_company.as_deref(), Some("CRONUS UK"));
        assert!(!cfg.launch_browser);
        assert_eq!(cfg.schema_update_mode, "Recreate");
        assert_eq!(cfg.dependency_publishing_option, "Ignore");
        assert!(!cfg.enable_sql_information_debugger);
        assert!(!cfg.enable_long_running_sql_statements);
        assert_eq!(cfg.long_running_sql_statements_threshold, 750);
        assert_eq!(cfg.number_of_sql_statements, 25);
        assert!(cfg.accept_invalid_certs);
        assert!(cfg.validate_native().is_ok());
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

        let wrong_type = BcDebugConfig::from_dap_args(&serde_json::json!({ "sessionId": "nope" }));
        assert_eq!(wrong_type.session_id, None);
        assert!(wrong_type
            .validate_native()
            .unwrap_err()
            .contains("sessionId"));

        let below_sentinel = BcDebugConfig::from_dap_args(&serde_json::json!({ "sessionId": -2 }));
        assert!(below_sentinel
            .validate_native()
            .unwrap_err()
            .contains("sessionId"));
    }

    #[test]
    fn from_dap_args_break_flags_accept_bool_and_string() {
        let bool_args = serde_json::json!({
            "breakOnError": false,
            "breakOnRecordWrite": true,
        });
        let cfg = BcDebugConfig::from_dap_args(&bool_args);
        assert_eq!(cfg.break_on_error, BreakOnError::None);
        assert_eq!(cfg.break_on_record_write, BreakOnRecordWrite::All);

        let str_args = serde_json::json!({
            "breakOnError": "none",
            "breakOnRecordWrite": "all",
        });
        let cfg = BcDebugConfig::from_dap_args(&str_args);
        assert_eq!(cfg.break_on_error, BreakOnError::None);
        assert_eq!(cfg.break_on_record_write, BreakOnRecordWrite::All);
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

        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({
            "acceptInvalidCerts": true,
        }));
        assert!(
            cfg.accept_invalid_certs,
            "Zed's acceptInvalidCerts spelling must be consumed too"
        );

        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({
            "validateServerCertificate": true,
            "acceptInvalidCerts": true,
        }));
        assert!(cfg
            .validate_native()
            .expect_err("contradictory certificate settings must be rejected")
            .contains("inverse"));
    }

    #[test]
    fn native_authentication_accepts_oauth_and_rejects_legacy_modes_before_network_io() {
        for mode in ["MicrosoftEntraID", "AAD"] {
            let config = BcDebugConfig::from_dap_args(&serde_json::json!({
                "authentication": mode
            }));
            assert!(config.validate_native().is_ok(), "{mode}");
        }
        for mode in ["Windows", "UserPassword"] {
            let config = BcDebugConfig::from_dap_args(&serde_json::json!({
                "authentication": mode
            }));
            let error = config
                .validate_native()
                .expect_err("unsupported native mode must fail");
            assert!(error.contains(mode));
            assert!(error.contains("al.useOfficialDap=true"));
        }
    }

    #[test]
    fn from_dap_args_rejects_wrong_typed_values() {
        let cfg = BcDebugConfig::from_dap_args(&serde_json::json!({
            "port": "not-a-number",
            "startupObjectId": "nope",
            "server": 123,
            "launchBrowser": "yes",
            "enableSqlInformationDebugger": 1,
            "enableLongRunningSqlStatements": null,
            "longRunningSqlStatementsThreshold": 1.5,
            "numberOfSqlStatements": -1,
        }));
        let def = BcDebugConfig::default();
        assert_eq!(cfg.port, def.port);
        assert_eq!(cfg.startup_object_id, def.startup_object_id);
        assert_eq!(cfg.server, def.server);
        let error = cfg
            .validate_native()
            .expect_err("every schema type mismatch must fail closed");
        for field in [
            "port",
            "startupObjectId",
            "server",
            "launchBrowser",
            "enableSqlInformationDebugger",
            "enableLongRunningSqlStatements",
            "longRunningSqlStatementsThreshold",
            "numberOfSqlStatements",
        ] {
            assert!(error.contains(field), "missing {field} in: {error}");
        }
    }

    #[test]
    fn native_validation_rejects_unknown_enum_values_and_non_object_args() {
        for (field, value) in [
            ("environmentType", "Container"),
            ("startupObjectType", "Codeunit"),
            ("breakOnNext", "RecordWrite"),
            ("schemaUpdateMode", "Merge"),
            ("dependencyPublishingOption", "Lenient"),
        ] {
            let config = BcDebugConfig::from_dap_args(&serde_json::json!({ (field): value }));
            let error = config
                .validate_native()
                .expect_err("unknown enum must fail");
            assert!(error.contains(field), "missing {field} in: {error}");
        }

        let config = BcDebugConfig::from_dap_args(&serde_json::json!(["not", "an", "object"]));
        assert!(config
            .validate_native()
            .unwrap_err()
            .contains("JSON object"));
    }

    #[test]
    fn every_advertised_debug_schema_field_has_an_owner() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../../../debug_adapter_schemas/al.json"))
                .expect("debug adapter schema must be valid JSON");
        let actual = schema["properties"]
            .as_object()
            .expect("debug adapter schema must expose properties")
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();

        // Zed consumes these before it starts the adapter. Every other field is
        // parsed above and consumed by the native debug session.
        let zed_owned = ["adapter", "build", "label", "request"];
        let native_owned = [
            "authentication",
            "breakOnError",
            "breakOnNext",
            "breakOnRecordWrite",
            "dependencyPublishingOption",
            "enableLongRunningSqlStatements",
            "enableSqlInformationDebugger",
            "environmentName",
            "environmentType",
            "launchBrowser",
            "longRunningSqlStatementsThreshold",
            "numberOfSqlStatements",
            "port",
            "schemaUpdateMode",
            "server",
            "serverInstance",
            "sessionId",
            "startupCompany",
            "startupObjectId",
            "startupObjectType",
            "tenant",
            "validateServerCertificate",
        ];
        let owned = zed_owned
            .into_iter()
            .chain(native_owned)
            .collect::<BTreeSet<_>>();

        assert_eq!(
            actual, owned,
            "schema fields must never advertise native no-ops"
        );
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
