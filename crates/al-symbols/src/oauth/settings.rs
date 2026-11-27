//! Sign-in parameters: the Entra endpoint, scope, client id and redirect URI.
//!
//! The defaults and the launch.json override rule follow Microsoft's AL client
//! (`CloudHttpClientFactory.GetAuthenticationParameters`, decompiled at
//! Deployment/Microsoft.Dynamics.Nav.Deployment.Http/CloudHttpClientFactory.cs:224-229).
//! Each `entraIdAuthentication` field in a launch configuration replaces the
//! matching default.

use super::validation::is_well_formed_guid;
use super::OAuthError;
use std::time::Duration;

/// The AL extension's own public client
/// (CloudHttpClientFactory.cs:228 and :31).
pub const AL_CLIENT_ID: &str = "41839ce3-4041-4bac-8c17-0941f25d7aaf";
const DEFAULT_ENDPOINT: &str = "https://login.microsoftonline.com";
const DEFAULT_SCOPE: &str = "https://api.businesscentral.dynamics.com/.default";
const DEFAULT_REDIRECT_URI: &str = "https://developer.businesscentral.dynamics.com";
/// The Entra sign-in hosts an endpoint may name: public cloud, US Government and China.
pub const ENTRA_AUTHORITY_HOSTS: [&str; 3] = [
    "login.microsoftonline.com",
    "login.microsoftonline.us",
    "login.partner.microsoftonline.cn",
];

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
            check_entra_authority(trimmed)?;
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

/// Accepts only an https origin on one of [`ENTRA_AUTHORITY_HOSTS`], with no
/// user, port, path, query or fragment. The refresh token is sent to this host,
/// so a launch file must not be able to name another one.
pub fn check_entra_authority(endpoint: &str) -> Result<(), OAuthError> {
    let refuse = || {
        OAuthError::Other(format!(
            "entraIdAuthentication.endpoint '{endpoint}' is not an allowed Entra sign-in endpoint. \
             Use https with one of these hosts, and no user, port, path or query: {}",
            ENTRA_AUTHORITY_HOSTS.join(", ")
        ))
    };
    let url = url::Url::parse(endpoint).map_err(|_| refuse())?;
    let host = url.host_str().map(str::to_ascii_lowercase);
    let allowed = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none()
        && host
            .as_deref()
            .is_some_and(|h| ENTRA_AUTHORITY_HOSTS.contains(&h));
    if allowed {
        Ok(())
    } else {
        Err(refuse())
    }
}

/// The client for sign-in and token requests. Certificate checks stay on, and
/// a redirect may only stay on the origin of the first request.
pub(super) fn oauth_http_client() -> Result<reqwest::Client, OAuthError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(same_origin_redirect))
        .build()
        .map_err(|e| OAuthError::Other(format!("could not build the sign-in HTTP client: {e}")))
}

fn same_origin_redirect(attempt: reqwest::redirect::Attempt) -> reqwest::redirect::Action {
    if attempt.url().origin() != attempt.previous()[0].origin() || attempt.previous().len() > 5 {
        return attempt.error("sign-in redirect to another origin or too many redirects");
    }
    attempt.follow()
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
            "endpoint": "https://login.microsoftonline.us/", "scope": "api://x/.default",
            "clientId": "11111111-2222-3333-4444-555555555555", "redirectUri": "http://localhost"}});
        let s = AuthSettings::from_launch_value(&camel).unwrap();
        assert_eq!(s.endpoint, "https://login.microsoftonline.us");
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

    #[test]
    fn endpoint_outside_entra_hosts_is_refused() {
        let v = json!({"entraIdAuthentication": {"endpoint": "https://evil.example"}});
        let err = AuthSettings::from_launch_value(&v).unwrap_err();
        assert!(
            err.to_string().contains("login.microsoftonline.com"),
            "{err}"
        );
    }

    #[test]
    fn sovereign_entra_endpoint_is_kept() {
        let v = json!({"entraIdAuthentication": {"endpoint": "https://login.microsoftonline.us/"}});
        let s = AuthSettings::from_launch_value(&v).unwrap();
        assert_eq!(s.endpoint, "https://login.microsoftonline.us");
    }

    #[test]
    fn entra_authority_check_is_exact() {
        for bad in [
            "http://login.microsoftonline.com",
            "https://login.microsoftonline.com:8443",
            "https://user@login.microsoftonline.com",
            "https://login.microsoftonline.com.evil.example",
            "https://evilogin.microsoftonline.com",
            "https://login.microsoftonline.com/x",
        ] {
            assert!(
                check_entra_authority(bad).is_err(),
                "{bad} should be refused"
            );
        }
        for good in [
            "https://login.microsoftonline.com",
            "https://LOGIN.microsoftonline.com",
            "https://login.partner.microsoftonline.cn",
        ] {
            assert!(check_entra_authority(good).is_ok(), "{good} should be kept");
        }
    }

    #[tokio::test]
    async fn same_origin_redirect_policy_refuses_other_origin() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let a = MockServer::start().await;
        let b = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/t"))
            .respond_with(
                ResponseTemplate::new(307).insert_header("Location", format!("{}/t", b.uri())),
            )
            .mount(&a)
            .await;
        Mock::given(method("POST"))
            .and(path("/t"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&b)
            .await;
        let client = oauth_http_client().unwrap();

        let refused = client.post(format!("{}/t", a.uri())).send().await;
        assert!(refused.is_err(), "a redirect to another origin must fail");
        assert!(b.received_requests().await.unwrap().is_empty());

        Mock::given(method("POST"))
            .and(path("/r"))
            .respond_with(ResponseTemplate::new(307).insert_header("Location", "/ok"))
            .mount(&a)
            .await;
        Mock::given(method("POST"))
            .and(path("/ok"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&a)
            .await;
        let ok = client.post(format!("{}/r", a.uri())).send().await.unwrap();
        assert_eq!(ok.status(), 200);
    }
}
