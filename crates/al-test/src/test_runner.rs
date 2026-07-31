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

use crate::error::TestRunnerError;
use crate::result::{TestCodeunitResult, TestMethodResult, TestStatus};
use al_bc::launch::{AuthMethod, BcServerConfig, EnvironmentType};

/// Response from `GET /dev/tests/{codeunit}` — list of test methods.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestListResponse {
    pub value: Vec<DevTestMethod>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestMethod {
    pub name: String,
}

/// Response from `POST /dev/tests/{codeunit}/run` — test results.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestRunResponse {
    pub value: Vec<DevTestResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevTestResult {
    pub name: String,
    pub result: String,
    pub message: Option<String>,
    pub duration: Option<f64>,
}

/// Client for the BC test runner dev API.
///
/// Constructed from a `BcServerConfig` (parsed from launch.json).
pub struct TestRunnerClient {
    client: Client,
    /// Base URL: `{scheme}://{server}:{port}/{serverInstance}`
    base_url: String,
    auth: AuthMethod,
    tenant: Option<String>,
    /// Request-scoped OAuth token supplied by daemon/MCP authentication.
    /// Falls back to `BC_TOKEN` for existing standalone callers.
    bearer_token: Option<String>,
}

impl TestRunnerClient {
    pub fn new(config: &BcServerConfig) -> Result<Self, TestRunnerError> {
        Self::with_access_token(config, None)
    }

    pub fn with_access_token(
        config: &BcServerConfig,
        access_token: Option<&str>,
    ) -> Result<Self, TestRunnerError> {
        if config.accept_invalid_certs {
            // Parity with bc_server / bc_debug / native_dap / http_auth so
            // an operator watching daemon logs sees the same "TLS disabled"
            // warning regardless of which BC client path runs.
            al_bc::http_auth::warn_insecure_tls("test runner");
        }
        let client = Client::builder()
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .timeout(Duration::from_secs(300))
            .build()?;

        let base_url = build_base_url(config);

        Ok(Self {
            client,
            base_url,
            auth: config.authentication.clone(),
            tenant: config.tenant.clone(),
            bearer_token: access_token
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(str::to_string),
        })
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
            let text = al_bc::bc_client::read_error_body_capped(response).await;
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

        // Cap reads based on Content-Length. BC test-run responses
        // are typically a few KB; 16 MB is a defence-in-depth bound.
        let raw: DevTestRunResponse = al_bc::bc_client::read_json_body_capped(response)
            .await
            .map_err(|e| TestRunnerError::ServerError {
                status: 0,
                message: e.to_string(),
            })?;
        let methods = raw
            .value
            .into_iter()
            .map(map_dev_result)
            .collect::<Result<Vec<_>, _>>()?;
        if methods.is_empty() {
            return Err(TestRunnerError::InvalidResponse(format!(
                "test run for codeunit {codeunit_id} returned no method results"
            )));
        }
        if let Some(expected) = method {
            if methods.len() != 1 || !methods[0].name.eq_ignore_ascii_case(expected) {
                let returned = methods
                    .iter()
                    .map(|result| result.name.as_str())
                    .collect::<Vec<_>>();
                return Err(TestRunnerError::InvalidResponse(format!(
                    "requested test method '{expected}', but Business Central returned {returned:?}"
                )));
            }
        }

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
            let text = al_bc::bc_client::read_error_body_capped(response).await;
            return Err(TestRunnerError::ServerError {
                status: status.as_u16(),
                message: text,
            });
        }

        // Cap reads based on Content-Length.
        let raw: DevTestListResponse = al_bc::bc_client::read_json_body_capped(response)
            .await
            .map_err(|e| TestRunnerError::ServerError {
                status: 0,
                message: e.to_string(),
            })?;
        let names = raw
            .value
            .into_iter()
            .map(|method| {
                let name = method.name.trim();
                if name.is_empty() {
                    Err(TestRunnerError::InvalidResponse(
                        "test method name is empty".to_string(),
                    ))
                } else {
                    Ok(name.to_string())
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(names)
    }

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
                let environment_token = if self.bearer_token.is_none() {
                    al_bc::http_auth::access_token_from_env()?
                } else {
                    None
                };
                let token = self
                    .bearer_token
                    .as_deref()
                    .or(environment_token.as_deref())
                    .unwrap_or_default();
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

fn map_dev_result(r: DevTestResult) -> Result<TestMethodResult, TestRunnerError> {
    let name = r.name.trim();
    if name.is_empty() {
        return Err(TestRunnerError::InvalidResponse(
            "test result method name is empty".to_string(),
        ));
    }
    let raw_status = r.result.trim();
    let status = match raw_status {
        s if s.eq_ignore_ascii_case("pass") || s.eq_ignore_ascii_case("success") => {
            TestStatus::Pass
        }
        s if s.eq_ignore_ascii_case("fail") || s.eq_ignore_ascii_case("failure") => {
            TestStatus::Fail
        }
        s if s.eq_ignore_ascii_case("skip") || s.eq_ignore_ascii_case("skipped") => {
            TestStatus::Skip
        }
        _ => {
            return Err(TestRunnerError::InvalidResponse(format!(
                "test result for '{name}' has unknown status '{raw_status}'"
            )));
        }
    };
    let duration_ms = r
        .duration
        .map(|seconds| {
            let duration = Duration::try_from_secs_f64(seconds).map_err(|_| {
                TestRunnerError::InvalidResponse(format!(
                    "test result for '{name}' has invalid duration {seconds}"
                ))
            })?;
            u64::try_from(duration.as_millis()).map_err(|_| {
                TestRunnerError::InvalidResponse(format!(
                    "test result for '{name}' duration exceeds the supported range"
                ))
            })
        })
        .transpose()?;
    Ok(TestMethodResult {
        name: name.to_string(),
        status,
        error: r.message.filter(|m| !m.is_empty()),
        duration_ms,
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use al_bc::launch::{AuthMethod, BcServerConfig, EnvironmentType};

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
            debug_args: serde_json::json!({}),
        }
    }

    #[test]
    fn test_runner_constructs_from_config() {
        let config = on_prem_config();
        let _client = TestRunnerClient::new(&config).expect("valid HTTP client");
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
            debug_args: serde_json::json!({}),
        };
        let url = build_base_url(&config);
        assert!(url.contains("MySandbox"), "URL should contain env: {url}");
    }

    #[test]
    fn request_scoped_oauth_token_is_used_without_bc_token_environment() {
        let config = BcServerConfig {
            name: "cloud".to_string(),
            environment_type: EnvironmentType::Sandbox,
            server: None,
            server_instance: None,
            port: None,
            environment_name: Some("Sandbox".to_string()),
            tenant: Some("tenant".to_string()),
            authentication: AuthMethod::AAD,
            accept_invalid_certs: false,
            debug_args: serde_json::json!({}),
        };
        let client =
            TestRunnerClient::with_access_token(&config, Some("request-token")).expect("client");
        let request = client
            .apply_auth(client.client.get("https://example.test"))
            .expect("auth")
            .build()
            .expect("request");
        assert_eq!(
            request
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer request-token")
        );
    }

    #[test]
    fn map_dev_result_pass() {
        let r = DevTestResult {
            name: "TestSomething".to_string(),
            result: "pass".to_string(),
            message: None,
            duration: Some(0.123),
        };
        let result = map_dev_result(r).unwrap();
        assert_eq!(result.name, "TestSomething");
        assert_eq!(result.status, TestStatus::Pass);
        assert_eq!(result.duration_ms, Some(123));
        assert!(result.error.is_none());
    }

    #[test]
    fn map_dev_result_fail_with_message() {
        let r = DevTestResult {
            name: "TestFailing".to_string(),
            result: "fail".to_string(),
            message: Some("Assert.AreEqual failed: expected 1, got 2".to_string()),
            duration: Some(0.05),
        };
        let result = map_dev_result(r).unwrap();
        assert_eq!(result.status, TestStatus::Fail);
        assert_eq!(
            result.error,
            Some("Assert.AreEqual failed: expected 1, got 2".to_string())
        );
    }

    #[test]
    fn map_dev_result_rejects_unknown_status() {
        let r = DevTestResult {
            name: "TestSkipped".to_string(),
            result: "unknown".to_string(),
            message: None,
            duration: None,
        };
        assert!(matches!(
            map_dev_result(r),
            Err(TestRunnerError::InvalidResponse(message))
                if message.contains("unknown status")
        ));
    }

    #[test]
    fn map_dev_result_rejects_empty_name() {
        let r = DevTestResult {
            name: "  ".to_string(),
            result: "pass".to_string(),
            message: None,
            duration: None,
        };
        assert!(matches!(
            map_dev_result(r),
            Err(TestRunnerError::InvalidResponse(message))
                if message.contains("name is empty")
        ));
    }

    #[test]
    fn map_dev_result_rejects_invalid_duration() {
        for duration in [f64::NAN, f64::INFINITY, -0.1] {
            let r = DevTestResult {
                name: "TestDuration".to_string(),
                result: "pass".to_string(),
                message: None,
                duration: Some(duration),
            };
            assert!(matches!(
                map_dev_result(r),
                Err(TestRunnerError::InvalidResponse(message))
                    if message.contains("invalid duration")
            ));
        }
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
mod url_tests {
    use super::*;
    use al_bc::launch::{AuthMethod, BcServerConfig, EnvironmentType};

    /// A bare hostname receives an `http://` scheme so reqwest accepts the URL.
    #[test]
    fn on_prem_url_without_scheme_adds_http() {
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
            debug_args: serde_json::json!({}),
        };
        let url = build_base_url(&config);
        assert!(
            url.starts_with("http://") || url.starts_with("https://"),
            "build_base_url must include a scheme; got: {url}"
        );
        assert!(url.contains("7049"), "URL should contain port: {url}");
        assert!(url.contains("/BC"), "URL should contain instance: {url}");
    }

    /// An existing scheme is preserved.
    #[test]
    fn on_prem_url_with_scheme_is_not_doubled() {
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
            debug_args: serde_json::json!({}),
        };
        let url = build_base_url(&config);
        assert!(
            !url.contains("http://http://"),
            "Scheme must not be doubled: {url}"
        );
        assert!(url.starts_with("http://"), "Must retain scheme: {url}");
    }
}
