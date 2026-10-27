//! Sign-in parameters: the Entra endpoint, scope, client id and redirect URI.
//!
//! The defaults and the launch.json override rule follow Microsoft's AL client
//! (`CloudHttpClientFactory.GetAuthenticationParameters`, decompiled at
//! Deployment/Microsoft.Dynamics.Nav.Deployment.Http/CloudHttpClientFactory.cs:224-229).
//! Each `entraIdAuthentication` field in a launch configuration replaces the
//! matching default.

use super::validation::is_well_formed_guid;
use super::OAuthError;

/// The AL extension's own public client
/// (CloudHttpClientFactory.cs:228 and :31).
pub const AL_CLIENT_ID: &str = "41839ce3-4041-4bac-8c17-0941f25d7aaf";
const DEFAULT_ENDPOINT: &str = "https://login.microsoftonline.com";
const DEFAULT_SCOPE: &str = "https://api.businesscentral.dynamics.com/.default";
const DEFAULT_REDIRECT_URI: &str = "https://developer.businesscentral.dynamics.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSettings {
    /// Login host, without a tenant or path.
    pub endpoint: String,
    pub scope: String,
    pub client_id: String,
    /// Read from launch.json for parity. Microsoft's interactive sign-in uses
    /// `http://localhost` (AccessTokenManager.cs:78) and its device code flow
    /// sends no redirect, so no flow here sends this value.
    pub redirect_uri: String,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.into(),
            scope: DEFAULT_SCOPE.into(),
            client_id: AL_CLIENT_ID.into(),
            redirect_uri: DEFAULT_REDIRECT_URI.into(),
        }
    }
}

impl AuthSettings {
    /// Read `entraIdAuthentication` from one launch configuration object.
    /// Absent or blank fields keep the default. Keys match with either case of
    /// the first letter, as the AL extension's JSON binding does.
    pub fn from_launch_value(launch: &serde_json::Value) -> Result<Self, OAuthError> {
        let mut settings = Self::default();
        let Some(block) = launch
            .get("entraIdAuthentication")
            .or_else(|| launch.get("EntraIdAuthentication"))
            .filter(|v| v.is_object())
        else {
            return Ok(settings);
        };
        let field = |camel: &str, pascal: &str| -> Option<String> {
            block
                .get(camel)
                .or_else(|| block.get(pascal))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        if let Some(endpoint) = field("endpoint", "Endpoint") {
            // The token request carries the user's credentials, so the host
            // must be https and a bare origin.
            let trimmed = endpoint.trim_end_matches('/');
            let host = trimmed.strip_prefix("https://").unwrap_or("");
            if host.is_empty() || host.contains(['/', '?', '#', '@', ' ']) || host.starts_with('.')
            {
                return Err(OAuthError::Other(format!(
                    "entraIdAuthentication.endpoint must be an https host such as {DEFAULT_ENDPOINT}, got '{endpoint}'"
                )));
            }
            settings.endpoint = trimmed.to_string();
        }
        if let Some(scope) = field("scope", "Scope") {
            settings.scope = scope;
        }
        if let Some(client_id) = field("clientId", "ClientId") {
            if !is_well_formed_guid(&client_id) {
                return Err(OAuthError::Other(
                    "entraIdAuthentication.clientId must be a well-formed GUID".into(),
                ));
            }
            settings.client_id = client_id;
        }
        if let Some(redirect) = field("redirectUri", "RedirectUri") {
            settings.redirect_uri = redirect;
        }
        Ok(settings)
    }

    /// The settings for `tenant`: the first launch configuration for that
    /// tenant that carries an `entraIdAuthentication` block, else the defaults.
    pub fn for_tenant(
        configs: &[al_bc::launch::BcServerConfig],
        tenant: &str,
    ) -> Result<Self, OAuthError> {
        configs
            .iter()
            .filter(|c| {
                c.tenant
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(tenant))
            })
            .find(|c| {
                c.debug_args.get("entraIdAuthentication").is_some()
                    || c.debug_args.get("EntraIdAuthentication").is_some()
            })
            .map_or_else(
                || Ok(Self::default()),
                |c| Self::from_launch_value(&c.debug_args),
            )
    }

    /// [`Self::for_tenant`] over the launch file found under `project_root`.
    /// A missing or unreadable launch file gives the defaults. The launch
    /// parser reports its own errors where the configuration is used.
    pub fn for_project_root(
        project_root: &std::path::Path,
        tenant: &str,
    ) -> Result<Self, OAuthError> {
        match al_bc::launch::find_launch_config(project_root) {
            Ok(Some(file)) => Self::for_tenant(&file.configs, tenant),
            _ => Ok(Self::default()),
        }
    }

    /// `<endpoint>/<tenant>/oauth2/v2.0/<leaf>`.
    pub(super) fn url(&self, tenant: &str, leaf: &str) -> String {
        format!("{}/{tenant}/oauth2/v2.0/{leaf}", self.endpoint)
    }

    /// The scope with `offline_access`, which the refresh token needs.
    pub(super) fn scope_string(&self) -> String {
        if self.scope.split_whitespace().any(|s| s == "offline_access") {
            self.scope.clone()
        } else {
            format!("{} offline_access", self.scope)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_are_the_al_clients() {
        // fixture source: CloudHttpClientFactory.cs:226-229
        let d = AuthSettings::default();
        assert_eq!(d.client_id, "41839ce3-4041-4bac-8c17-0941f25d7aaf");
        assert_eq!(d.endpoint, "https://login.microsoftonline.com");
        assert_eq!(d.scope, "https://api.businesscentral.dynamics.com/.default");
        assert_eq!(
            d.redirect_uri,
            "https://developer.businesscentral.dynamics.com"
        );
        assert_eq!(
            d.url("t", "token"),
            "https://login.microsoftonline.com/t/oauth2/v2.0/token"
        );
    }

    #[test]
    fn launch_block_overrides_each_field_and_accepts_pascal_case() {
        let camel = json!({"entraIdAuthentication": {
            "endpoint": "https://login.example.test/", "scope": "api://x/.default",
            "clientId": "11111111-2222-3333-4444-555555555555", "redirectUri": "http://localhost"}});
        let s = AuthSettings::from_launch_value(&camel).unwrap();
        assert_eq!(s.endpoint, "https://login.example.test");
        assert_eq!(s.scope, "api://x/.default");
        assert_eq!(s.scope_string(), "api://x/.default offline_access");
        assert_eq!(s.client_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(s.redirect_uri, "http://localhost");
        let pascal =
            json!({"EntraIdAuthentication": {"ClientId": "11111111-2222-3333-4444-555555555555"}});
        let p = AuthSettings::from_launch_value(&pascal).unwrap();
        assert_eq!(p.client_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(p.endpoint, AuthSettings::default().endpoint);
    }

    #[test]
    fn missing_or_blank_block_keeps_defaults() {
        assert_eq!(
            AuthSettings::from_launch_value(&json!({"name": "x"})).unwrap(),
            AuthSettings::default()
        );
        let blank = json!({"entraIdAuthentication": {"clientId": " ", "scope": ""}});
        assert_eq!(
            AuthSettings::from_launch_value(&blank).unwrap(),
            AuthSettings::default()
        );
    }

    #[test]
    fn plain_http_or_pathful_endpoint_is_refused() {
        for bad in [
            "http://login.example.test",
            "https://a/b",
            "https://u@h",
            "login.example.test",
        ] {
            let v = json!({"entraIdAuthentication": {"endpoint": bad}});
            assert!(AuthSettings::from_launch_value(&v).is_err(), "{bad}");
        }
        let v = json!({"entraIdAuthentication": {"clientId": "nope"}});
        assert!(AuthSettings::from_launch_value(&v).is_err());
    }

    #[test]
    fn project_launch_file_supplies_the_block_for_its_tenant() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".vscode")).unwrap();
        std::fs::write(
            dir.path().join(".vscode/launch.json"),
            r#"{"version":"0.2.0","configurations":[{"name":"cdx","type":"al","request":"launch",
            "environmentType":"Sandbox","tenant":"1b7a9471-d313-4aa4-99e8-7a6c762c8e7d",
            "entraIdAuthentication":{"clientId":"11111111-2222-3333-4444-555555555555"}}]}"#,
        )
        .unwrap();
        let tenant = "1b7a9471-d313-4aa4-99e8-7a6c762c8e7d";
        let got = AuthSettings::for_project_root(dir.path(), tenant).unwrap();
        assert_eq!(got.client_id, "11111111-2222-3333-4444-555555555555");
        let other =
            AuthSettings::for_project_root(dir.path(), "5c823c3b-d5c9-4853-b74c-d59b81265f78")
                .unwrap();
        assert_eq!(other, AuthSettings::default());
    }
}
