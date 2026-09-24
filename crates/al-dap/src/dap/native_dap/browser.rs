//! The URL that opens a debug session in the Business Central client.

use tracing::warn;

use crate::dap::bc_debug::{percent_encode_url, BcDebugConfig};
use crate::dap::{DapError, Result};

/// Build the browser URL that opens the BC debug context for a launch session.
///
/// For cloud sessions the tenant and environment name are percent-encoded so
/// values containing special characters (spaces, ampersands, slashes) produce
/// valid URLs — matching the encoding already applied in
/// [`bc_debug::BcDebugConfig::base_url`] and `debug_hub_url`.
pub fn build_debug_browser_url(
    config: &BcDebugConfig,
    conn_id: &str,
    onprem_web_base: Option<&str>,
) -> Result<String> {
    let object_parameter = match config.startup_object_type.to_ascii_lowercase().as_str() {
        "table" => "table",
        "report" => "report",
        "query" => "query",
        _ => "page",
    };
    let base = if config.environment_type.eq_ignore_ascii_case("OnPrem") {
        onprem_web_base
            .ok_or_else(|| {
                DapError::ConnectionFailed(
                    "on-premises browser launch requires the server-provided PublicWebBaseUrl"
                        .to_string(),
                )
            })?
            .to_string()
    } else {
        let tenant = percent_encode_url(&config.tenant);
        let env = percent_encode_url(config.environment_name.as_deref().unwrap_or("sandbox"));
        format!("https://businesscentral.dynamics.com/{tenant}/{env}/")
    };
    let mut url = url::Url::parse(&base).map_err(|error| {
        DapError::ConnectionFailed(format!("invalid Web client base URL `{base}`: {error}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(DapError::ConnectionFailed(format!(
            "unsupported Web client base URL `{base}`"
        )));
    }
    {
        let mut query = url.query_pairs_mut();
        query.append_pair(object_parameter, &config.startup_object_id.to_string());
        if let Some(company) = config
            .startup_company
            .as_deref()
            .filter(|company| !company.is_empty())
        {
            query.append_pair("company", company);
        }
        if !config.environment_type.eq_ignore_ascii_case("OnPrem") {
            query.append_pair("noSignUpCheck", "1");
        }
        query.append_pair("connectioncontext", conn_id);
        query.append_pair("debuggingcontext", conn_id);
        query.append_pair("sk", conn_id);
    }
    Ok(url.to_string())
}

/// Open the BC web client for a debug session. See [`al_types::browser`] for
/// why Windows does not go through `cmd /c start`.
pub(super) fn open_browser(url: &str) -> bool {
    match al_types::open_in_browser(url) {
        Ok(()) => true,
        Err(error) => {
            warn!("open_browser: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_browser_url_percent_encodes_tenant_and_env() {
        // A tenant/environment containing special characters must be encoded,
        // otherwise the resulting URL is malformed (mirrors the encoding already
        // applied in bc_debug::base_url / debug_hub_url).
        let config = BcDebugConfig {
            environment_type: "Cloud".to_string(),
            tenant: "acme & co".to_string(),
            environment_name: Some("prod/east".to_string()),
            startup_object_id: 22,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "abc123", None).expect("valid cloud URL");
        assert!(
            url.contains("acme%20%26%20co"),
            "tenant special chars must be encoded: {url}"
        );
        assert!(
            url.contains("prod%2Feast"),
            "environment special chars must be encoded: {url}"
        );
        assert!(
            !url.contains("acme & co"),
            "raw unencoded tenant must not leak into URL: {url}"
        );
        assert!(
            url.contains("page=22"),
            "startup object id must be present: {url}"
        );
    }

    #[test]
    fn cloud_browser_url_defaults_env_to_sandbox() {
        let config = BcDebugConfig {
            environment_type: "Cloud".to_string(),
            tenant: "tenant1".to_string(),
            environment_name: None,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "conn", None).expect("valid cloud URL");
        assert!(
            url.contains("/tenant1/sandbox/?"),
            "missing env should default to sandbox: {url}"
        );
    }

    #[test]
    fn browser_url_honors_every_advertised_startup_object_type_and_company() {
        for (object_type, parameter) in [
            ("Page", "page"),
            ("Table", "table"),
            ("Report", "report"),
            ("Query", "query"),
        ] {
            let config = BcDebugConfig {
                environment_type: "Cloud".to_string(),
                tenant: "tenant".to_string(),
                environment_name: Some("sandbox".to_string()),
                startup_object_type: object_type.to_string(),
                startup_object_id: 42,
                startup_company: Some("CRONUS UK Ltd.".to_string()),
                ..Default::default()
            };
            let url = build_debug_browser_url(&config, "conn", None).expect("valid cloud URL");
            assert!(
                url.contains(&format!("{parameter}=42")),
                "{object_type} must use its own URL parameter: {url}"
            );
            let parsed = url::Url::parse(&url).expect("browser URL parses");
            assert_eq!(
                parsed
                    .query_pairs()
                    .find(|(key, _)| key == "company")
                    .map(|(_, value)| value.into_owned()),
                Some("CRONUS UK Ltd.".to_string()),
                "startup company must survive URL encoding: {url}"
            );
        }
    }

    #[test]
    fn onprem_browser_url_requires_server_provided_public_web_base_url() {
        let config = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: None,
            port: 7049,
            startup_object_id: 42,
            ..Default::default()
        };
        let error = build_debug_browser_url(&config, "cid", None)
            .expect_err("developer-services URL must not stand in for the Web client URL");
        assert!(error.to_string().contains("PublicWebBaseUrl"), "{error}");
    }

    #[test]
    fn onprem_browser_url_is_case_insensitive_for_env_type() {
        // environment_type matching uses eq_ignore_ascii_case, so "onprem"
        // (lowercase) must still take the on-prem branch, not the cloud branch.
        let config = BcDebugConfig {
            environment_type: "onprem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: 8080,
            startup_object_id: 9,
            ..Default::default()
        };
        let url = build_debug_browser_url(&config, "conn", Some("http://localhost:8080/BC"))
            .expect("server-provided on-prem URL");
        assert!(
            url.contains(":8080/BC?page=9"),
            "lowercase onprem must use the on-prem URL branch: {url}"
        );
        assert!(
            !url.contains("businesscentral.dynamics.com"),
            "must not fall through to the cloud branch: {url}"
        );
    }

    #[test]
    fn onprem_browser_url_uses_authoritative_web_base_not_developer_services_port() {
        let config = BcDebugConfig {
            environment_type: "OnPrem".to_string(),
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: 7049,
            startup_object_id: 5,
            ..Default::default()
        };
        let url =
            build_debug_browser_url(&config, "conn", Some("https://web.example.test:8443/BC"))
                .expect("server-provided on-prem URL");
        assert!(
            url.starts_with("https://web.example.test:8443/BC?page=5"),
            "on-prem URL must use the authoritative Web endpoint: {url}"
        );
        assert!(
            !url.contains(":7049"),
            "developer-services port must not leak into Web client URL: {url}"
        );
    }

    #[test]
    fn open_browser_refuses_a_non_web_url() {
        assert!(!open_browser("file:///etc/passwd"));
    }
}
