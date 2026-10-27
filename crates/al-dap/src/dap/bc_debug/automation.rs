//! Automation API lookups the snapshot commands need: a BC user name to its
//! `userSecurityId`, and the installed Base Application version.
//!
//! Routes checked before writing this: the AL host's snapshot attach takes a
//! user GUID only and has no name lookup. The Automation API
//! (`api/microsoft/automation/v2.0`) is the standard OData route for
//! `users` and `extensions`, and it is the same service `bc-build-deploy`
//! uses, so no new client is added. The calls take `http`, `&BcDebugConfig`
//! and the access token like `rest.rs`. SaaS only: an on-premises server has
//! no fixed Automation API address in the launch configuration.

use reqwest::header::AUTHORIZATION;
use serde_json::Value;

use super::session_config::BcDebugConfig;
use super::wire::percent_encode_url;

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error(
        "looking up a user name or version needs a SaaS environment: pass the user's GUID (userSecurityId) instead"
    )]
    OnPrem,
    #[error("automation API request failed: {0}")]
    Request(String),
    #[error("automation API {what} returned HTTP {status}: {body}")]
    Http {
        what: &'static str,
        status: u16,
        body: String,
    },
    #[error("automation API {what} sent a reply that could not be read: {message}")]
    BadResponse { what: &'static str, message: String },
    #[error("no company found in the environment")]
    NoCompany,
    #[error(
        "no user named `{0}` in the environment, pass the user's GUID (userSecurityId) instead"
    )]
    UserNotFound(String),
    #[error("{0} users are named `{1}`, pass the user's GUID (userSecurityId) instead")]
    UserAmbiguous(usize, String),
    #[error("the Base Application extension was not found in the environment")]
    NoBaseApp,
}

pub type Result<T> = std::result::Result<T, AutomationError>;

/// `https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/api/microsoft/automation/v2.0`
fn automation_base(config: &BcDebugConfig) -> Result<String> {
    if config.environment_type.eq_ignore_ascii_case("OnPrem") {
        return Err(AutomationError::OnPrem);
    }
    let tenant = percent_encode_url(&config.tenant);
    let env = percent_encode_url(config.environment_name.as_deref().unwrap_or("sandbox"));
    Ok(format!(
        "https://api.businesscentral.dynamics.com/v2.0/{tenant}/{env}/api/microsoft/automation/v2.0"
    ))
}

/// Double single quotes and percent encode the whole filter text.
fn filter_param(field: &str, value: &str) -> String {
    let escaped = value.replace('\'', "''");
    percent_encode_url(&format!("{field} eq '{escaped}'"))
}

async fn get_json(
    http: &reqwest::Client,
    url: &str,
    access_token: &str,
    what: &'static str,
) -> Result<Value> {
    let response = http
        .get(url)
        .header(AUTHORIZATION, format!("Bearer {access_token}"))
        .send()
        .await
        .map_err(|e| AutomationError::Request(e.without_url().to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let body = al_bc::bc_client::read_error_body_capped(response).await;
        return Err(AutomationError::Http {
            what,
            status: status.as_u16(),
            body,
        });
    }
    let bytes = al_bc::bc_client::read_binary_body_capped(response)
        .await
        .map_err(|e| AutomationError::BadResponse {
            what,
            message: al_bc::bc_client::sanitize_error_body(&e.to_string()),
        })?;
    serde_json::from_slice(&bytes).map_err(|e| AutomationError::BadResponse {
        what,
        message: e.to_string(),
    })
}

fn first_string(value: &Value, key: &str) -> Option<String> {
    value
        .get("value")?
        .as_array()?
        .first()?
        .get(key)?
        .as_str()
        .map(str::to_string)
}

async fn first_company_id(http: &reqwest::Client, base: &str, token: &str) -> Result<String> {
    let companies = get_json(http, &format!("{base}/companies"), token, "companies").await?;
    first_string(&companies, "id").ok_or(AutomationError::NoCompany)
}

/// Same as the config based calls but against an explicit API base, for a proxy or a test server.
/// Resolve a BC user name to its `userSecurityId` GUID.
pub async fn resolve_user_security_id(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
    user_name: &str,
) -> Result<String> {
    resolve_user_at(http, &automation_base(config)?, access_token, user_name).await
}

pub async fn resolve_user_at(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    user_name: &str,
) -> Result<String> {
    let company = first_company_id(http, base, token).await?;
    let url = format!(
        "{base}/companies({company})/users?$filter={}",
        filter_param("userName", user_name)
    );
    let users = get_json(http, &url, token, "users").await?;
    let rows = users
        .get("value")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    match rows {
        [] => Err(AutomationError::UserNotFound(user_name.to_string())),
        [row] => row
            .get("userSecurityId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AutomationError::BadResponse {
                what: "users",
                message: "row has no userSecurityId".to_string(),
            }),
        many => Err(AutomationError::UserAmbiguous(
            many.len(),
            user_name.to_string(),
        )),
    }
}

/// The installed Base Application version as `Major.Minor.Build.Revision`.
pub async fn base_application_version(
    http: &reqwest::Client,
    config: &BcDebugConfig,
    access_token: &str,
) -> Result<String> {
    base_app_version_at(http, &automation_base(config)?, access_token).await
}

pub async fn base_app_version_at(
    http: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<String> {
    let company = first_company_id(http, base, token).await?;
    let url = format!(
        "{base}/companies({company})/extensions?$filter={}",
        filter_param("displayName", "Base Application")
    );
    let extensions = get_json(http, &url, token, "extensions").await?;
    let row = extensions
        .get("value")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or(AutomationError::NoBaseApp)?;
    let part = |key: &str| row.get(key).and_then(Value::as_u64);
    match (
        part("versionMajor"),
        part("versionMinor"),
        part("versionBuild"),
        part("versionRevision"),
    ) {
        (Some(a), Some(b), Some(c), Some(d)) => Ok(format!("{a}.{b}.{c}.{d}")),
        _ => Err(AutomationError::BadResponse {
            what: "extensions",
            message: "row has no version parts".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "FAKE-TOKEN";
    const COMPANY: &str = "11111111-aaaa-bbbb-cccc-000000000001";

    async fn with_companies() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/companies"))
            .and(header("authorization", "Bearer FAKE-TOKEN"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"value": [{"id": COMPANY, "name": "CRONUS"}]}),
                ),
            )
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn resolves_a_user_name_and_escapes_the_quote() {
        let server = with_companies().await;
        Mock::given(method("GET"))
            .and(path(format!("/companies({COMPANY})/users")))
            .and(query_param("$filter", "userName eq 'O''BRIEN'"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"value": [{"userSecurityId": "aaaaaaaa-0000-0000-0000-000000000001"}]}),
            ))
            .mount(&server)
            .await;
        let id = resolve_user_at(&reqwest::Client::new(), &server.uri(), TOKEN, "O'BRIEN")
            .await
            .unwrap();
        assert_eq!(id, "aaaaaaaa-0000-0000-0000-000000000001");
    }

    #[tokio::test]
    async fn unknown_and_ambiguous_users_are_errors() {
        let server = with_companies().await;
        Mock::given(method("GET"))
            .and(path(format!("/companies({COMPANY})/users")))
            .and(query_param("$filter", "userName eq 'NOBODY'"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": []})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/companies({COMPANY})/users")))
            .and(query_param("$filter", "userName eq 'TWIN'"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"value": [{"userSecurityId": "a"}, {"userSecurityId": "b"}]}),
            ))
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let err = resolve_user_at(&http, &server.uri(), TOKEN, "NOBODY")
            .await
            .unwrap_err();
        assert!(matches!(err, AutomationError::UserNotFound(_)));
        let err = resolve_user_at(&http, &server.uri(), TOKEN, "TWIN")
            .await
            .unwrap_err();
        assert!(matches!(err, AutomationError::UserAmbiguous(2, _)));
    }

    #[tokio::test]
    async fn reads_the_base_application_version() {
        let server = with_companies().await;
        Mock::given(method("GET"))
            .and(path(format!("/companies({COMPANY})/extensions")))
            .and(query_param("$filter", "displayName eq 'Base Application'"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"value": [
                    {"displayName": "Base Application", "versionMajor": 29, "versionMinor": 0,
                     "versionBuild": 12345, "versionRevision": 6}
                ]})),
            )
            .mount(&server)
            .await;
        let version = base_app_version_at(&reqwest::Client::new(), &server.uri(), TOKEN)
            .await
            .unwrap();
        assert_eq!(version, "29.0.12345.6");
    }

    #[tokio::test]
    async fn http_failure_carries_the_status() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_string("denied"))
            .mount(&server)
            .await;
        let err = base_app_version_at(&reqwest::Client::new(), &server.uri(), TOKEN)
            .await
            .unwrap_err();
        assert!(
            matches!(err, AutomationError::Http { status: 403, .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn on_prem_asks_for_the_guid() {
        let config = BcDebugConfig {
            environment_type: "OnPrem".into(),
            ..BcDebugConfig::default()
        };
        let http = reqwest::Client::new();
        let err = resolve_user_security_id(&http, &config, TOKEN, "X")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("GUID"));
        assert!(base_application_version(&http, &config, TOKEN)
            .await
            .is_err());
    }

    #[test]
    fn saas_base_escapes_tenant_and_environment() {
        let config = BcDebugConfig {
            environment_type: "Sandbox".into(),
            tenant: "t id".into(),
            environment_name: Some("Sand box".into()),
            ..BcDebugConfig::default()
        };
        assert_eq!(
            automation_base(&config).unwrap(),
            "https://api.businesscentral.dynamics.com/v2.0/t%20id/Sand%20box/api/microsoft/automation/v2.0"
        );
    }
}
