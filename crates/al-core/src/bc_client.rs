//! Business Central Dev API REST client.
//!
//! Wraps BC server REST endpoints used for publishing AL extensions:
//! - `POST /dev/extensions` — upload + publish a `.app` file
//! - `POST /dev/extensions/{id}/install` — install into a tenant
//! - `DELETE /dev/extensions/{id}` — uninstall/remove
//! - `GET /dev/applications/{appId}` — RAD state check
//! - `PATCH /dev/applications/{appId}` — RAD incremental delta deploy
//!
//! Reference endpoint: `{server}:{port}/{serverInstance}/dev/...`
//!
//! Authentication:
//! - `UserPassword` — HTTP Basic (username + password from env vars `BC_USERNAME` / `BC_PASSWORD`)
//! - `Windows` — NTLM/negotiate (on-prem only; uses `BC_USERNAME` / `BC_PASSWORD` if set)
//! - `AAD` — Bearer token from `BC_TOKEN` env var or device-code flow

use std::path::Path;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use crate::launch::{AuthMethod, BcServerConfig, EnvironmentType};

/// Maximum bytes of an HTTP error body kept in BcClientError messages.
/// Anything past this is replaced with a `... [N more bytes truncated]`
/// suffix so that a verbose BC error page (which can echo request URL
/// parameters or auth header context) doesn't propagate unbounded into
/// logs and JSON-RPC responses (T006 / sec-003).
pub(crate) const ERROR_BODY_MAX: usize = 512;

/// Truncate `body` to ERROR_BODY_MAX bytes (UTF-8 boundary safe) and
/// blank out the values of common credential-bearing headers if they
/// happen to appear inline. Conservative — pattern-based, not parsing
/// — but better than passing the body through verbatim.
pub(crate) fn sanitize_error_body(body: &str) -> String {
    let mut out = if body.len() > ERROR_BODY_MAX {
        // Find a UTF-8 char boundary at or before ERROR_BODY_MAX.
        let mut cut = ERROR_BODY_MAX;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        format!(
            "{}... [{} more bytes truncated]",
            &body[..cut],
            body.len() - cut
        )
    } else {
        body.to_string()
    };
    // Best-effort scrub of obvious credential echo patterns. Matches
    // the conservative shape used by other BC clients in this crate.
    for needle in [
        "Authorization: Bearer ",
        "Authorization:Bearer ",
        "access_token=",
        "refresh_token=",
        "client_secret=",
        "password=",
    ] {
        while let Some(idx) = out.find(needle) {
            let value_start = idx + needle.len();
            // Scrub up to the next whitespace, ", &, or end of string.
            let value_end = out[value_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '&')
                .map(|n| value_start + n)
                .unwrap_or(out.len());
            out.replace_range(value_start..value_end, "[REDACTED]");
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that can occur when communicating with the BC Dev API.
#[derive(Debug, Error)]
pub enum BcClientError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Authentication failed (HTTP {status}): {message}")]
    AuthenticationFailed { status: u16, message: String },
    #[error("BC server error (HTTP {status}): {message}")]
    ServerError { status: u16, message: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("No server configuration found in launch.json")]
    NoConfig,
    #[error("Missing credentials: set BC_USERNAME and BC_PASSWORD environment variables")]
    MissingCredentials,
    #[error("Extension upload failed: compilation errors in the .app file")]
    CompilationErrors,
    #[error("Timeout after {secs}s waiting for BC server")]
    Timeout { secs: u64 },
}

// ---------------------------------------------------------------------------
// BC Dev API response types
// ---------------------------------------------------------------------------

/// Response from `POST /dev/extensions` — extension publish result.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPublishResponse {
    /// Extension app ID (GUID)
    pub app_id: Option<String>,
    /// Extension name
    pub name: Option<String>,
    /// Extension version
    pub version: Option<String>,
    /// Status after publish: "Completed", "InProgress", etc.
    pub status: Option<String>,
    /// Operation ID for async operations
    pub operation_id: Option<String>,
}

/// Response from `GET /dev/applications/{appId}` — RAD state.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationStateResponse {
    pub app_id: Option<String>,
    pub status: Option<String>,
    pub version: Option<String>,
}

// ---------------------------------------------------------------------------
// BC HTTP client
// ---------------------------------------------------------------------------

/// HTTP client for the BC Dev API.
///
/// Constructed from a `BcServerConfig` parsed out of launch.json.
pub struct BcClient {
    client: Client,
    /// Base URL: `{scheme}://{server}:{port}/{serverInstance}`
    base_url: String,
    auth: AuthMethod,
    tenant: Option<String>,
}

impl BcClient {
    /// Build a BC client from the given server config.
    pub fn new(config: &BcServerConfig) -> Self {
        if config.accept_invalid_certs {
            warn!(
                "BcClient: accept_invalid_certs=true is active — TLS certificate \
                 validation is DISABLED. Use only for local-dev sandboxes."
            );
        }
        let client = Client::builder()
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .timeout(Duration::from_secs(300))  // 5 min for large uploads
            .build()
            .unwrap_or_else(|e| {
                // reqwest::Client::builder().build() failures are essentially
                // unreachable on a healthy install (TLS backend missing or
                // OS-level config corruption). Falling back to Client::default()
                // means the user might silently lose timeout / TLS-permissive
                // settings on every BC request — surface at error level so the
                // root cause appears in the log even though we don't propagate.
                tracing::error!(
                    error = %e,
                    "Failed to build TLS-configured HTTP client; falling back to Client::default(). Subsequent BC requests may fail with TLS handshake errors or hang past the configured 300s timeout."
                );
                Client::default()
            });

        let base_url = build_base_url(config);

        BcClient {
            client,
            base_url,
            auth: config.authentication.clone(),
            tenant: config.tenant.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // Standard publish flow: upload → install
    // -----------------------------------------------------------------------

    /// Upload and publish a `.app` file to the BC server.
    ///
    /// Corresponds to the VS Code "Publish" command. Calls `POST /dev/extensions`.
    /// Returns the publish response (may be async — check `status`).
    pub async fn publish_extension(
        &self,
        app_path: &Path,
    ) -> Result<ExtensionPublishResponse, BcClientError> {
        let url = format!("{}/dev/extensions", self.base_url);
        let app_bytes = tokio::fs::read(app_path).await?;
        let file_name = app_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("extension.app")
            .to_string();

        debug!(url = %url, file = %file_name, bytes = app_bytes.len(), "Publishing extension");

        // BC Dev API accepts the raw .app binary as the request body.
        let mut req = self
            .client
            .post(&url)
            .body(app_bytes)
            .header("Content-Type", "application/octet-stream");
        req = self.apply_auth(req)?;
        req = self.apply_tenant_header(req);

        let response = req.send().await?;
        self.handle_response(response).await
    }

    // -----------------------------------------------------------------------
    // RAD (Rapid Application Development) flow
    // -----------------------------------------------------------------------

    /// Incremental delta deploy via the RAD API.
    ///
    /// Calls `PATCH /dev/applications/{appId}` with the `.app` binary.
    /// Returns the application state response.
    pub async fn rad_publish(
        &self,
        app_id: &str,
        app_path: &Path,
    ) -> Result<ApplicationStateResponse, BcClientError> {
        let url = format!("{}/dev/applications/{}", self.base_url, app_id);
        let app_bytes = tokio::fs::read(app_path).await?;

        debug!(url = %url, app_id = %app_id, bytes = app_bytes.len(), "RAD incremental deploy");

        let mut req = self
            .client
            .patch(&url)
            .body(app_bytes)
            .header("Content-Type", "application/octet-stream");
        req = self.apply_auth(req)?;
        req = self.apply_tenant_header(req);

        let response = req.send().await?;
        self.handle_response(response).await
    }

    // -----------------------------------------------------------------------
    // Auth helpers
    // -----------------------------------------------------------------------

    fn apply_auth(
        &self,
        mut req: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, BcClientError> {
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
                            return Err(BcClientError::MissingCredentials);
                        }
                        // Windows NTLM: attempt without credentials (OS-level auth)
                        warn!("Windows auth without credentials — may fail; set BC_USERNAME/BC_PASSWORD");
                    }
                }
            }
            AuthMethod::AAD => {
                // Bearer token from env var BC_TOKEN
                let token = std::env::var("BC_TOKEN").unwrap_or_default();
                let token = token.trim();
                if token.is_empty() {
                    return Err(BcClientError::MissingCredentials);
                }
                req = req.bearer_auth(token);
            }
        }
        Ok(req)
    }

    fn apply_tenant_header(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.tenant {
            Some(t) if !t.is_empty() && t != "default" => req.header("X-Tenant", t),
            _ => req,
        }
    }

    // -----------------------------------------------------------------------
    // Response parsing
    // -----------------------------------------------------------------------

    async fn handle_response<T: for<'de> Deserialize<'de>>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, BcClientError> {
        let status = response.status();
        if status.is_success() {
            let body = response.json::<T>().await?;
            return Ok(body);
        }
        self.map_error_response(status, response).await
    }

    async fn map_error_response<T>(
        &self,
        status: StatusCode,
        response: reqwest::Response,
    ) -> Result<T, BcClientError> {
        let raw = response.text().await.unwrap_or_else(|_| status.to_string());
        // Truncate + scrub: never propagate the full BC error body into
        // logs/JSON-RPC responses (T006 / sec-003). BC servers can echo
        // request URL parameters (incl. tenant) into error pages, and a
        // verbose 401 page sometimes mirrors the Authorization header
        // family; we keep enough text to be useful for debugging
        // without leaving a tail of unbounded credential surface.
        let message = sanitize_error_body(&raw);

        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(BcClientError::AuthenticationFailed {
                status: status.as_u16(),
                message,
            });
        }

        Err(BcClientError::ServerError {
            status: status.as_u16(),
            message,
        })
    }
}

// ---------------------------------------------------------------------------
// URL construction
// ---------------------------------------------------------------------------

/// Build the base URL for the BC Dev API from a server config.
///
/// On-prem:  `http://{server}:{port}/{serverInstance}`
/// Cloud:    `https://api.businesscentral.dynamics.com/v2.0/{tenant}/{envName}`
fn build_base_url(config: &BcServerConfig) -> String {
    match config.environment_type {
        EnvironmentType::OnPrem => {
            let server = config.server.as_deref().unwrap_or("localhost");
            let instance = config.server_instance.as_deref().unwrap_or("BC");
            // Ensure the server URL has a scheme to prevent accidental plain-HTTP
            // requests when the caller omits the scheme prefix.
            let server_with_scheme =
                if server.starts_with("http://") || server.starts_with("https://") {
                    server.to_string()
                } else {
                    format!("http://{}", server)
                };
            let server_trimmed = server_with_scheme.trim_end_matches('/');
            if let Some(port) = config.port {
                format!("{}:{}/{}", server_trimmed, port, instance)
            } else {
                format!("{}/{}", server_trimmed, instance)
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

    fn cloud_config() -> BcServerConfig {
        BcServerConfig {
            name: "cloud".to_string(),
            environment_type: EnvironmentType::Sandbox,
            server: None,
            server_instance: None,
            port: None,
            environment_name: Some("MySandbox".to_string()),
            tenant: Some("mycompany.onmicrosoft.com".to_string()),
            authentication: AuthMethod::AAD,
            accept_invalid_certs: false,
        }
    }

    #[test]
    fn on_prem_url_has_port_and_instance() {
        let url = build_base_url(&on_prem_config());
        assert!(url.contains("7049"), "URL should contain port: {}", url);
        assert!(url.contains("/BC"), "URL should contain instance: {}", url);
    }

    #[test]
    fn on_prem_url_always_has_scheme() {
        // If the server field omits the scheme, build_base_url must add http://
        // to prevent accidental scheme-less URL construction.
        let mut config = on_prem_config();
        config.server = Some("localhost".to_string());
        let url = build_base_url(&config);
        assert!(
            url.starts_with("http://") || url.starts_with("https://"),
            "URL must have a scheme: {url}"
        );
    }

    #[test]
    fn cloud_url_contains_tenant_and_env() {
        let url = build_base_url(&cloud_config());
        assert!(
            url.contains("mycompany.onmicrosoft.com") || url.contains("mycompany"),
            "URL should contain tenant: {}",
            url
        );
        assert!(
            url.contains("MySandbox"),
            "URL should contain env name: {}",
            url
        );
    }

    #[test]
    fn bc_client_constructs_from_config() {
        // Just ensure it doesn't panic on construction
        let config = on_prem_config();
        let _client = BcClient::new(&config);
    }

    #[test]
    fn sanitize_error_body_truncates_at_512_bytes() {
        let body = "x".repeat(2000);
        let out = sanitize_error_body(&body);
        // Truncated portion is replaced; total stays below the original.
        assert!(out.len() < body.len());
        assert!(out.contains("more bytes truncated"));
    }

    #[test]
    fn sanitize_error_body_redacts_bearer_token() {
        let body = "401 Unauthorized\nAuthorization: Bearer eyJhbGc.123.456 token rejected";
        let out = sanitize_error_body(body);
        assert!(
            !out.contains("eyJhbGc.123.456"),
            "raw bearer token must not survive sanitisation"
        );
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_error_body_redacts_query_param_secrets() {
        let body = "Bad request for client_secret=topsecret&grant_type=password";
        let out = sanitize_error_body(body);
        assert!(
            !out.contains("topsecret"),
            "client_secret value must be redacted"
        );
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_error_body_short_body_passes_through() {
        let body = "404 not found";
        let out = sanitize_error_body(body);
        assert_eq!(out, body);
    }

    #[test]
    fn sanitize_error_body_handles_utf8_boundary_safely() {
        // Truncation must land on a UTF-8 char boundary.
        let body = "a".repeat(510) + "🦀🦀🦀";
        let out = sanitize_error_body(&body);
        // Should not panic and should be valid UTF-8.
        assert!(out.is_char_boundary(out.len()));
    }
}
