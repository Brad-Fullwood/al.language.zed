//! Test execution via BC REST API — `al test run`.
//!
//! Calls the OData test runner endpoint exposed by Business Central:
//! `GET /ODataV4/TestRunner_RunTests` or the standard test codeunit runner.
//!
//! BC exposes two test-runner flavours:
//! 1. **OData TestRunner** — a special BC page/codeunit exposed as OData.
//!    Endpoint: `{base}/ODataV4/TestRunner_RunTests?company={company}`
//!    Trigger by POSTing to the bound action, or by calling the page directly.
//!
//! 2. **Dev API tests endpoint** — `GET /dev/tests/{codeunit}` (on-prem only).
//!    Returns test method names. `POST /dev/tests/{codeunit}/run` executes.
//!
//! This implementation targets the dev API (more reliable across BC versions).
//!
//! Authentication re-uses the `BcClient` infrastructure from `bc_client.rs`.

use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::launch::{AuthMethod, BcServerConfig, EnvironmentType};
use crate::test_engine::error::TestRunnerError;
use crate::test_engine::result::{TestCodeunitResult, TestMethodResult, TestStatus};

// ---------------------------------------------------------------------------
// BC Dev API raw response shapes
// ---------------------------------------------------------------------------

/// Response from `GET /dev/tests/{codeunit}` — list of test methods.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestListResponse {
    pub value: Option<Vec<DevTestMethod>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestMethod {
    pub name: Option<String>,
}

/// Response from `POST /dev/tests/{codeunit}/run` — test results.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestRunResponse {
    pub value: Option<Vec<DevTestResult>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestResult {
    pub name: Option<String>,
    pub result: Option<String>,
    pub message: Option<String>,
    pub duration: Option<f64>,
}

// ---------------------------------------------------------------------------
// Test runner client
// ---------------------------------------------------------------------------

/// Client for the BC test runner dev API.
///
/// Constructed from a `BcServerConfig` (parsed from launch.json).
pub struct TestRunnerClient {
    client: Client,
    /// Base URL: `{scheme}://{server}:{port}/{serverInstance}`
    base_url: String,
    auth: AuthMethod,
    tenant: Option<String>,
}

impl TestRunnerClient {
    /// Build a test runner client from server configuration.
    pub fn new(config: &BcServerConfig) -> Self {
        let client = Client::builder()
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .timeout(Duration::from_secs(300))
            .build()
            .unwrap_or_else(|e| {
                warn!(error = %e, "TLS client build failed; falling back to default");
                Client::default()
            });

        let base_url = build_base_url(config);

        Self {
            client,
            base_url,
            auth: config.authentication.clone(),
            tenant: config.tenant.clone(),
        }
    }

    /// Run all [Test] procedures in the specified codeunit.
    ///
    /// Calls `POST /dev/tests/{codeunit}/run`.
    /// Returns results per test method (pass/fail/skip).
    ///
    /// If `method` is Some, only that test method is run.
    pub async fn run_codeunit(
        &self,
        codeunit_id: i32,
        codeunit_name: &str,
        method: Option<&str>,
    ) -> Result<TestCodeunitResult, TestRunnerError> {
        let url = if let Some(m) = method {
            format!(
                "{}/dev/tests/{}/run?method={}",
                self.base_url,
                codeunit_id,
                urlencoding::encode(m)
            )
        } else {
            format!("{}/dev/tests/{}/run", self.base_url, codeunit_id)
        };

        debug!(url = %url, codeunit = %codeunit_name, "Running BC test codeunit");

        let mut req = self.client.post(&url).json(&serde_json::json!({}));
        req = self.apply_auth(req)?;
        req = self.apply_tenant(req);

        let response = req.send().await?;
        let status = response.status();

        if !status.is_success() {
            let text = response.text().await.unwrap_or_else(|_| status.to_string());
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(TestRunnerError::AuthenticationFailed {
                    status: status.as_u16(),
                    message: text,
                });
            }
            return Err(TestRunnerError::ServerError {
                status: status.as_u16(),
                message: text,
            });
        }

        let raw: DevTestRunResponse = response.json().await?;
        let methods = raw
            .value
            .unwrap_or_default()
            .into_iter()
            .map(map_dev_result)
            .collect();

        Ok(TestCodeunitResult::from_methods(
            codeunit_name.to_string(),
            codeunit_id,
            methods,
        ))
    }

    /// List test methods in a codeunit without executing them.
    ///
    /// Calls `GET /dev/tests/{codeunit}`.
    pub async fn list_methods(&self, codeunit_id: i32) -> Result<Vec<String>, TestRunnerError> {
        let url = format!("{}/dev/tests/{}", self.base_url, codeunit_id);
        debug!(url = %url, codeunit = %codeunit_id, "Listing BC test methods");

        let mut req = self.client.get(&url);
        req = self.apply_auth(req)?;
        req = self.apply_tenant(req);

        let response = req.send().await?;
        let status = response.status();

        if !status.is_success() {
            let text = response.text().await.unwrap_or_else(|_| status.to_string());
            return Err(TestRunnerError::ServerError {
                status: status.as_u16(),
                message: text,
            });
        }

        let raw: DevTestListResponse = response.json().await?;
        let names = raw
            .value
            .unwrap_or_default()
            .into_iter()
            .filter_map(|m| {
                // Only include actual test methods, not fixtures/setup
                let name = m.name?;
                Some(name)
            })
            .collect();

        Ok(names)
    }

    // -----------------------------------------------------------------------
    // Auth helpers
    // -----------------------------------------------------------------------

    fn apply_auth(
        &self,
        mut req: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, TestRunnerError> {
        match &self.auth {
            AuthMethod::UserPassword | AuthMethod::Windows => {
                let username = std::env::var("BC_USERNAME").ok();
                let password = std::env::var("BC_PASSWORD").ok();
                match (username, password) {
                    (Some(u), Some(p)) => {
                        req = req.basic_auth(u, Some(p));
                    }
                    _ => {
                        if matches!(&self.auth, AuthMethod::UserPassword) {
                            return Err(TestRunnerError::MissingCredentials);
                        }
                        warn!("Windows auth without credentials");
                    }
                }
            }
            AuthMethod::AAD => {
                let token = std::env::var("BC_TOKEN").unwrap_or_default();
                let token = token.trim();
                if token.is_empty() {
                    return Err(TestRunnerError::MissingCredentials);
                }
                req = req.bearer_auth(token);
            }
        }
        Ok(req)
    }

    fn apply_tenant(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.tenant {
            Some(t) if !t.is_empty() && t != "default" => req.header("X-Tenant", t),
            _ => req,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn map_dev_result(r: DevTestResult) -> TestMethodResult {
    let name = r.name.unwrap_or_default();
    let status = match r.result.as_deref() {
        Some(s) if s.eq_ignore_ascii_case("pass") || s.eq_ignore_ascii_case("success") => {
            TestStatus::Pass
        }
        Some(s) if s.eq_ignore_ascii_case("fail") || s.eq_ignore_ascii_case("failure") => {
            TestStatus::Fail
        }
        Some(s) if s.eq_ignore_ascii_case("skip") || s.eq_ignore_ascii_case("skipped") => {
            TestStatus::Skip
        }
        _ => TestStatus::Skip,
    };
    let duration_ms = r.duration.map(|d| (d * 1000.0) as u64);
    TestMethodResult {
        name,
        status,
        error: r.message.filter(|m| !m.is_empty()),
        duration_ms,
    }
}

/// Build the base URL for the BC dev API test endpoints.
fn build_base_url(config: &BcServerConfig) -> String {
    match config.environment_type {
        EnvironmentType::OnPrem => {
            let server = config.server.as_deref().unwrap_or("localhost");
            let instance = config.server_instance.as_deref().unwrap_or("BC");
            let server_trimmed = server.trim_end_matches('/');
            let server_with_scheme = if server_trimmed.to_lowercase().starts_with("http://")
                || server_trimmed.to_lowercase().starts_with("https://")
            {
                server_trimmed.to_string()
            } else {
                format!("http://{}", server_trimmed)
            };
            if let Some(port) = config.port {
                format!("{}:{}/{}", server_with_scheme, port, instance)
            } else {
                format!("{}/{}", server_with_scheme, instance)
            }
        }
        EnvironmentType::Sandbox | EnvironmentType::Production => {
            let tenant = config.tenant.as_deref().unwrap_or("common");
            let env_name = config.environment_name.as_deref().unwrap_or("Sandbox");
            format!(
                "https://api.businesscentral.dynamics.com/v2.0/{}/{}",
                urlencoding::encode(tenant),
                urlencoding::encode(env_name)
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{AuthMethod, BcServerConfig, EnvironmentType};

    fn on_prem_config() -> BcServerConfig {
        BcServerConfig {
            name: "local".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some("http://localhost".to_string()),
            server_instance: Some("BC".to_string()),
            port: Some(7049),
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::UserPassword,
            accept_invalid_certs: false,
        }
    }

    #[test]
    fn test_runner_constructs_from_config() {
        let config = on_prem_config();
        let _client = TestRunnerClient::new(&config);
    }

    #[test]
    fn on_prem_base_url_has_port_and_instance() {
        let config = on_prem_config();
        let url = build_base_url(&config);
        assert!(url.contains("7049"), "URL should contain port: {url}");
        assert!(url.contains("/BC"), "URL should contain instance: {url}");
    }

    #[test]
    fn cloud_base_url_contains_tenant_and_env() {
        let config = BcServerConfig {
            name: "cloud".to_string(),
            environment_type: EnvironmentType::Sandbox,
            server: None,
            server_instance: None,
            port: None,
            environment_name: Some("MySandbox".to_string()),
            tenant: Some("mycompany.onmicrosoft.com".to_string()),
            authentication: AuthMethod::AAD,
            accept_invalid_certs: false,
        };
        let url = build_base_url(&config);
        assert!(url.contains("MySandbox"), "URL should contain env: {url}");
    }

    #[test]
    fn map_dev_result_pass() {
        let r = DevTestResult {
            name: Some("TestSomething".to_string()),
            result: Some("pass".to_string()),
            message: None,
            duration: Some(0.123),
        };
        let result = map_dev_result(r);
        assert_eq!(result.name, "TestSomething");
        assert_eq!(result.status, TestStatus::Pass);
        assert_eq!(result.duration_ms, Some(123));
        assert!(result.error.is_none());
    }

    #[test]
    fn map_dev_result_fail_with_message() {
        let r = DevTestResult {
            name: Some("TestFailing".to_string()),
            result: Some("fail".to_string()),
            message: Some("Assert.AreEqual failed: expected 1, got 2".to_string()),
            duration: Some(0.05),
        };
        let result = map_dev_result(r);
        assert_eq!(result.status, TestStatus::Fail);
        assert_eq!(
            result.error,
            Some("Assert.AreEqual failed: expected 1, got 2".to_string())
        );
    }

    #[test]
    fn map_dev_result_skip_on_unknown_status() {
        let r = DevTestResult {
            name: Some("TestSkipped".to_string()),
            result: Some("unknown".to_string()),
            message: None,
            duration: None,
        };
        let result = map_dev_result(r);
        assert_eq!(result.status, TestStatus::Skip);
    }

    #[test]
    fn test_codeunit_result_computes_summary() {
        let methods = vec![
            TestMethodResult {
                name: "A".into(),
                status: TestStatus::Pass,
                error: None,
                duration_ms: None,
            },
            TestMethodResult {
                name: "B".into(),
                status: TestStatus::Fail,
                error: Some("err".into()),
                duration_ms: None,
            },
            TestMethodResult {
                name: "C".into(),
                status: TestStatus::Skip,
                error: None,
                duration_ms: None,
            },
        ];
        let result = TestCodeunitResult::from_methods("MyTests".into(), 50100, methods);
        assert_eq!(result.total, 3);
        assert_eq!(result.passed, 1);
        assert_eq!(result.failed, 1);
        assert_eq!(result.skipped, 1);
    }
}

#[cfg(test)]
mod adversarial_j_tests {
    use super::*;
    use crate::launch::{AuthMethod, BcServerConfig, EnvironmentType};

    // -------------------------------------------------------------------------
    // Adversarial-j findings
    // -------------------------------------------------------------------------

    /// Finding adversarial_j_1: build_base_url must include an http:// scheme even
    /// when config.server omits it.  Without a scheme, reqwest cannot parse the URL.
    /// This test currently FAILS (RED) because build_base_url does not inject the scheme.
    #[test]
    fn test_on_prem_url_without_scheme_adds_http_adversarial_j_1() {
        let config = BcServerConfig {
            name: "plain-host".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some("myserver.company.com".to_string()), // no scheme
            server_instance: Some("BC".to_string()),
            port: Some(7049),
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::UserPassword,
            accept_invalid_certs: false,
        };
        let url = build_base_url(&config);
        // The URL must be parseable by reqwest — must start with http:// or https://
        assert!(
            url.starts_with("http://") || url.starts_with("https://"),
            "build_base_url must include a scheme; got: {url}"
        );
        // Must still contain port and instance
        assert!(url.contains("7049"), "URL should contain port: {url}");
        assert!(url.contains("/BC"), "URL should contain instance: {url}");
    }

    /// Finding adversarial_j_1 (negative companion): server already has http:// — must not double it.
    #[test]
    fn test_on_prem_url_with_scheme_not_doubled_adversarial_j_1() {
        let config = BcServerConfig {
            name: "with-scheme".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some("http://myserver.company.com".to_string()),
            server_instance: Some("BC".to_string()),
            port: Some(7049),
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::UserPassword,
            accept_invalid_certs: false,
        };
        let url = build_base_url(&config);
        assert!(
            !url.contains("http://http://"),
            "Scheme must not be doubled: {url}"
        );
        assert!(url.starts_with("http://"), "Must retain scheme: {url}");
    }
}
