//! Business Central Dev API REST client.
//!
//! Wraps the two BC server REST endpoints this crate actually implements:
//! - `POST /dev/extensions` — upload + publish a `.app` file (see [`BcClient::publish_extension`])
//! - `PATCH /dev/applications/{appId}` — RAD incremental delta deploy (see [`BcClient::rad_publish`])
//!
//! Reference endpoint: `{server}:{port}/{serverInstance}/dev/...`
//!
//! This client does **not** implement extension install/uninstall
//! (`POST /dev/extensions/{id}/install`, `DELETE /dev/extensions/{id}`) or an
//! application-status query (`GET /dev/applications/{appId}`) — there is no
//! caller in this codebase for them today, and no live BC dev endpoint to
//! verify a guessed wire shape against. `PublishPhase` (in `al-publish`)
//! correspondingly has no `Install` variant.
//!
//! Authentication:
//! - `UserPassword` — HTTP Basic (username + password from env vars `BC_USERNAME` / `BC_PASSWORD`)
//! - `Windows` — **also plain HTTP Basic**, using the same `BC_USERNAME` / `BC_PASSWORD` env vars.
//!   This is *not* a real NTLM/Negotiate handshake — `reqwest` performs no such handshake — so a BC
//!   server that requires genuine Windows-integrated auth (and has no Basic-auth fallback enabled)
//!   will reject every request from this client with 401, regardless of which credentials are set.
//! - `AAD` — Bearer token from the `BC_ACCESS_TOKEN` (or legacy `BC_TOKEN`) env var only; there is no
//!   interactive device-code / OAuth sign-in flow. Without a pre-provisioned token, AAD publishes
//!   fail fast with [`BcClientError::MissingCredentials`].

use std::path::Path;
use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use thiserror::Error;
use tracing::{debug, warn};

use crate::launch::{is_safe_http_server, AuthMethod, BcServerConfig, EnvironmentType};

/// Maximum bytes of an HTTP error body kept in BcClientError messages.
/// Anything past this is replaced with a `... [N more bytes truncated]`
/// suffix so that a verbose BC error page (which can echo request URL
/// parameters or auth header context) doesn't propagate unbounded into
/// logs and JSON-RPC responses.
pub(crate) const ERROR_BODY_MAX: usize = 512;

/// Truncate `body` to ERROR_BODY_MAX bytes (UTF-8 boundary safe) and
/// blank out the values of common credential-bearing headers if they
/// happen to appear inline. Conservative — pattern-based, not parsing
/// — but better than passing the body through verbatim.
pub fn sanitize_error_body(body: &str) -> String {
    let mut out = if body.len() > ERROR_BODY_MAX {
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
    for needle in [
        "Authorization: Bearer ",
        "Authorization:Bearer ",
        "access_token=",
        "refresh_token=",
        "client_secret=",
        "password=",
    ] {
        // BC/IIS error pages commonly capitalize these differently
        // (`Password=`, `PASSWORD=`, `authorization: bearer …`), so matching
        // must be case-insensitive. `to_ascii_lowercase` is a 1:1,
        // length-preserving byte mapping for ASCII bytes and leaves
        // non-ASCII (UTF-8 continuation) bytes untouched, so byte offsets
        // found in the lowercased haystack are valid offsets into `out`.
        //
        // Advance the search start past each replacement so we never
        // re-scrub our own [REDACTED] sentinel — that bug would make the
        // loop run forever on bodies like client_secret=x&password=y where
        // replacing x with [REDACTED] still left a trailing & for the next
        // pattern.
        let needle_lower = needle.to_ascii_lowercase();
        const REDACTED: &str = "[REDACTED]";
        let mut search_from = 0;
        while let Some(rel_idx) = out[search_from..].to_ascii_lowercase().find(&needle_lower) {
            let idx = search_from + rel_idx;
            let value_start = idx + needle.len();
            let value_end = out[value_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '&')
                .map(|n| value_start + n)
                .unwrap_or(out.len());
            out.replace_range(value_start..value_end, REDACTED);
            search_from = value_start + REDACTED.len();
        }
    }
    out
}

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
    #[error(
        "Missing credentials: set BC_ACCESS_TOKEN (or BC_TOKEN) for AAD, or BC_USERNAME and BC_PASSWORD for UserPassword. \
         (AAD has no interactive sign-in flow here; a pre-provisioned token is required.)"
    )]
    MissingCredentials,
    #[error("Invalid bearer-token environment: {0}")]
    CredentialConfiguration(#[from] crate::http_auth::AccessTokenEnvError),
    #[error("Extension upload failed: compilation errors in the .app file")]
    CompilationErrors,
    #[error("Timeout after {secs}s waiting for BC server")]
    Timeout { secs: u64 },
    #[error(".app file too large to upload: {bytes} bytes exceeds {limit} byte limit")]
    AppFileTooLarge { bytes: u64, limit: u64 },
}

/// Maximum size of a `.app` file we'll buffer into memory for upload.
///
/// 500 MB. Typical BC extensions are 1–50 MB, with the largest BaseApp
/// builds around 200 MB. Anything past 500 MB is almost certainly a
/// build mistake or a malformed input we shouldn't be loading into the
/// daemon's address space.
const MAX_UPLOADABLE_APP_BYTES: u64 = 500 * 1024 * 1024;

/// Read an `.app` file into memory after verifying its size doesn't
/// exceed `MAX_UPLOADABLE_APP_BYTES`. Returns `AppFileTooLarge` if the
/// file is bigger than the cap, without ever buffering the body.
async fn read_app_capped(app_path: &Path) -> Result<Vec<u8>, BcClientError> {
    let metadata = tokio::fs::metadata(app_path).await?;
    let size = metadata.len();
    if size > MAX_UPLOADABLE_APP_BYTES {
        return Err(BcClientError::AppFileTooLarge {
            bytes: size,
            limit: MAX_UPLOADABLE_APP_BYTES,
        });
    }
    Ok(tokio::fs::read(app_path).await?)
}

/// Upper bound on JSON response bodies from the BC dev API.
/// Snapshot lists, profile-start replies, test-run results — all tiny
/// in practice (under 1 MB). 16 MB is a generous defence-in-depth bound
/// that mirrors the NuGet metadata cap and stops a
/// misbehaving server from streaming gigabytes through `serde_json`.
pub(crate) const MAX_BC_JSON_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Read a JSON response body, refusing bodies larger than
/// `MAX_BC_JSON_RESPONSE_BYTES`. Requires a `Content-Length` header so
/// the cap is enforceable without first buffering the whole body —
/// responses without one are refused. Same hardening pattern as
/// `NuGetClient::fetch_metadata_json`.
pub async fn read_json_body_capped<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
) -> Result<T, BcClientError> {
    let content_length = response
        .content_length()
        .ok_or_else(|| BcClientError::ServerError {
            status: response.status().as_u16(),
            message: "JSON response missing Content-Length header — refusing".to_string(),
        })?;
    if content_length > MAX_BC_JSON_RESPONSE_BYTES {
        return Err(BcClientError::ServerError {
            status: response.status().as_u16(),
            message: format!(
                "JSON response body {content_length} bytes exceeds {MAX_BC_JSON_RESPONSE_BYTES} byte limit"
            ),
        });
    }
    // Capture the status before `bytes()` consumes the response, so the
    // error paths below can report the real HTTP status rather than 0.
    let status = response.status().as_u16();
    let bytes = response.bytes().await?;
    if bytes.len() as u64 > MAX_BC_JSON_RESPONSE_BYTES {
        return Err(BcClientError::ServerError {
            status,
            message: format!(
                "JSON response actual body {actual} bytes exceeds {MAX_BC_JSON_RESPONSE_BYTES} byte limit — server lied about Content-Length",
                actual = bytes.len(),
            ),
        });
    }
    serde_json::from_slice(&bytes).map_err(|e| BcClientError::ServerError {
        status,
        message: format!("Failed to parse JSON response: {e}"),
    })
}

/// Upper bound on binary download bodies (profile / snapshot files) from the
/// BC dev API. 500 MB mirrors `MAX_UPLOADABLE_APP_BYTES`: profiling
/// `.alcpuprofile` and snapshot `.alvsc` files are typically a few MB to tens
/// of MB; anything past 500 MB is almost certainly a misbehaving server and
/// we refuse to buffer it into the daemon's address space.
pub(crate) const MAX_BC_BINARY_RESPONSE_BYTES: u64 = 500 * 1024 * 1024;

/// Read a binary response body, refusing bodies larger than
/// `MAX_BC_BINARY_RESPONSE_BYTES`. A body whose advertised `Content-Length`
/// exceeds the cap is rejected before any bytes are buffered; a server that
/// lies about (or omits) `Content-Length` is still bounded by the post-read
/// size re-check. Same defensive shape as `read_json_body_capped` and
/// `bc_server::download_one`.
pub async fn read_binary_body_capped(
    response: reqwest::Response,
) -> Result<Vec<u8>, BcClientError> {
    let status = response.status().as_u16();
    if let Some(content_length) = response.content_length() {
        if content_length > MAX_BC_BINARY_RESPONSE_BYTES {
            return Err(BcClientError::ServerError {
                status,
                message: format!(
                    "binary response body {content_length} bytes exceeds {MAX_BC_BINARY_RESPONSE_BYTES} byte limit — refusing download"
                ),
            });
        }
    }
    let bytes = response.bytes().await?;
    if bytes.len() as u64 > MAX_BC_BINARY_RESPONSE_BYTES {
        return Err(BcClientError::ServerError {
            status,
            message: format!(
                "binary response actual body {actual} bytes exceeds {MAX_BC_BINARY_RESPONSE_BYTES} byte limit — server lied about Content-Length",
                actual = bytes.len(),
            ),
        });
    }
    Ok(bytes.to_vec())
}

/// Maximum number of bytes to buffer from a non-2xx (error) response body
/// before truncating. `sanitize_error_body` ultimately trims to 512 bytes for
/// display, but without an up-front cap a misbehaving BC server could stream a
/// multi-gigabyte error body that `response.text()` would buffer entirely into
/// memory first.
pub(crate) const MAX_ERROR_BODY_BYTES: u64 = 64 * 1024; // 64 KiB — far more than any real error page

/// Read an error response body, refusing to buffer more than
/// `MAX_ERROR_BODY_BYTES`, then scrub/truncate it via `sanitize_error_body`.
///
/// Mirrors the `read_json_body_capped` hardening pattern: a body whose
/// advertised `Content-Length` exceeds the cap (or that omits the header
/// entirely) is not buffered at all, since `reqwest`'s `text()`/`bytes()`
/// would otherwise pull the whole body into memory. A server that lies about a
/// small `Content-Length` and then streams a huge body is still bounded by the
/// post-read size re-check.
pub async fn read_error_body_capped(response: reqwest::Response) -> String {
    match response.content_length() {
        Some(len) if len <= MAX_ERROR_BODY_BYTES => {
            let bytes = match response.bytes().await {
                Ok(b) => b,
                Err(_) => return String::new(),
            };
            // Defend against a lied Content-Length: only retain the cap.
            let end = (MAX_ERROR_BODY_BYTES as usize).min(bytes.len());
            sanitize_error_body(&String::from_utf8_lossy(&bytes[..end]))
        }
        Some(len) => sanitize_error_body(&format!(
            "<error body {len} bytes exceeds {MAX_ERROR_BODY_BYTES} byte cap — not read>"
        )),
        None => sanitize_error_body("<error body has no Content-Length — not read>"),
    }
}

/// Response from `POST /dev/extensions` — extension publish result.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPublishResponse {
    pub app_id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    /// "Completed", "InProgress", etc.
    pub status: Option<String>,
    pub operation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationStateResponse {
    pub app_id: Option<String>,
    pub status: Option<String>,
    pub version: Option<String>,
}

/// HTTP client for the BC Dev API.
pub struct BcClient {
    client: Client,
    base_url: String,
    auth: AuthMethod,
    tenant: Option<String>,
}

impl BcClient {
    pub fn new(config: &BcServerConfig) -> Self {
        if config.accept_invalid_certs {
            crate::http_auth::warn_insecure_tls("BcClient");
        }
        let client = Client::builder()
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .timeout(Duration::from_secs(300)) // 5 min for large uploads
            .build()
            .expect("reqwest client must support the configured TLS backend");

        let base_url = build_base_url(config);

        BcClient {
            client,
            base_url,
            auth: config.authentication.clone(),
            tenant: config.tenant.clone(),
        }
    }

    /// Returns the publish response (may be async — check `status`).
    pub async fn publish_extension(
        &self,
        app_path: &Path,
    ) -> Result<ExtensionPublishResponse, BcClientError> {
        let url = format!("{}/dev/extensions", self.base_url);
        let app_bytes = read_app_capped(app_path).await?;
        let file_name = app_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("extension.app")
            .to_string();

        debug!(url = %url, file = %file_name, bytes = app_bytes.len(), "Publishing extension");

        let mut req = self
            .client
            .post(&url)
            .body(app_bytes)
            .header("Content-Type", "application/octet-stream");
        req = self.apply_auth(req)?;
        req = self.apply_tenant_query(req);

        let response = req.send().await?;
        self.handle_response(response).await
    }

    pub async fn rad_publish(
        &self,
        app_id: &str,
        app_path: &Path,
    ) -> Result<ApplicationStateResponse, BcClientError> {
        let url = format!("{}/dev/applications/{}", self.base_url, app_id);
        let app_bytes = read_app_capped(app_path).await?;

        debug!(url = %url, app_id = %app_id, bytes = app_bytes.len(), "RAD incremental deploy");

        let mut req = self
            .client
            .patch(&url)
            .body(app_bytes)
            .header("Content-Type", "application/octet-stream");
        req = self.apply_auth(req)?;
        req = self.apply_tenant_query(req);

        let response = req.send().await?;
        self.handle_response(response).await
    }

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
                        // NOTE: this is the ONLY authentication mechanism this
                        // client implements for `AuthMethod::Windows` — plain
                        // HTTP Basic, identical to `UserPassword`. It is not a
                        // real NTLM/Negotiate handshake (see the module doc
                        // comment), so it will not satisfy a BC server that
                        // requires genuine Windows-integrated auth without a
                        // Basic-auth fallback.
                        req = req.basic_auth(u, Some(p));
                    }
                    _ => {
                        if matches!(&self.auth, AuthMethod::UserPassword) {
                            return Err(BcClientError::MissingCredentials);
                        }
                        // `AuthMethod::Windows` without BC_USERNAME/BC_PASSWORD:
                        // this client implements no NTLM/Negotiate handshake
                        // and has no OS-level Windows-integrated-auth fallback
                        // (unlike a browser or WinHTTP client), so the request
                        // is sent with no Authorization header at all. Warn
                        // honestly instead of implying NTLM might still work —
                        // a genuinely Windows-auth-only BC server will 401 this.
                        warn!(
                            "AuthMethod::Windows has no BC_USERNAME/BC_PASSWORD set — this client \
                             does not implement NTLM/Negotiate, so the request carries no \
                             Authorization header and a Windows-auth-only BC server will reject it \
                             with 401. Set BC_USERNAME/BC_PASSWORD (sent as HTTP Basic, not NTLM)."
                        );
                    }
                }
            }
            AuthMethod::AAD => {
                let token = crate::http_auth::access_token_from_env()?
                    .ok_or(BcClientError::MissingCredentials)?;
                req = req.bearer_auth(token);
            }
        }
        Ok(req)
    }

    /// Attach the tenant as the `?tenant=` query parameter BC's dev endpoints
    /// actually read (see `launch.rs::dev_packages_url`, which the
    /// symbol-download path uses) — NOT a custom header. An `X-Tenant`
    /// header is not recognised by BC and multitenant on-prem publishes would
    /// silently target the default tenant instead.
    fn apply_tenant_query(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.tenant {
            Some(t) if !t.is_empty() && t != "default" => req.query(&[("tenant", t.as_str())]),
            _ => req,
        }
    }

    async fn handle_response<T: for<'de> Deserialize<'de>>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, BcClientError> {
        let status = response.status();
        if status.is_success() {
            // Use the capped reader so a hostile BC server cannot stream an
            // arbitrarily large JSON body into the daemon's address space
            // (hardening — enforces MAX_BC_JSON_RESPONSE_BYTES via
            // a Content-Length pre-check plus a post-read size re-check).
            let body = read_json_body_capped::<T>(response).await?;
            return Ok(body);
        }
        self.map_error_response(status, response).await
    }

    async fn map_error_response<T>(
        &self,
        status: StatusCode,
        response: reqwest::Response,
    ) -> Result<T, BcClientError> {
        // Read + scrub via the capped reader so a hostile BC server cannot
        // stream a multi-gigabyte error body that `response.text()` would
        // buffer entirely into memory before truncation. The
        // capped reader rejects oversize/absent Content-Length without
        // buffering and re-checks the actual size post-read, then runs
        // `sanitize_error_body` itself — never propagating the full BC error
        // body into logs/JSON-RPC responses. BC servers can
        // echo request URL parameters (incl. tenant) into error pages, and a
        // verbose 401 page sometimes mirrors the Authorization header family;
        // we keep enough text to be useful for debugging without leaving a
        // tail of unbounded credential surface.
        let message = read_error_body_capped(response).await;

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

/// Fallback base URL used when `config.server` fails the [`is_safe_http_server`]
/// allowlist. `.invalid` is the reserved TLD from RFC 2606 — guaranteed never
/// to resolve — so a rejected `file://`/`gopher://`/etc. server config turns
/// into an obvious connection failure instead of ever reaching the URL a
/// hostile or misconfigured launch config supplied.
const REJECTED_SERVER_BASE_URL: &str = "http://bc-client-rejected-unsafe-server.invalid";

/// BC's documented on-premises dev-services port when a launch config omits
/// `port` and the server string itself carries no explicit port. Applying
/// this only when the host has no port of its own avoids double-porting a
/// `server` value that already embeds one (e.g. a full `http://host:1234`
/// URL, as used by every mock-server test in this crate).
const DEFAULT_ONPREM_DEV_PORT: u16 = 7049;

/// Whether `host` (the part of `server` after any `scheme://`) already
/// carries an explicit `:<port>` suffix.
fn host_has_explicit_port(host: &str) -> bool {
    host.rsplit_once(':')
        .map(|(_, maybe_port)| {
            !maybe_port.is_empty() && maybe_port.bytes().all(|b| b.is_ascii_digit())
        })
        .unwrap_or(false)
}

/// Build the base URL for the BC Dev API from a server config.
///
/// On-prem:  `http://{server}:{port}/{serverInstance}`
/// Cloud:    `https://api.businesscentral.dynamics.com/v2.0/{tenant}/{envName}`
fn build_base_url(config: &BcServerConfig) -> String {
    match config.environment_type {
        EnvironmentType::OnPrem => {
            let server = config.server.as_deref().unwrap_or("localhost");
            let instance = config.server_instance.as_deref().unwrap_or("BC");

            // Same http(s)-or-bare-host allowlist the symbol-download path
            // (`launch.rs::dev_packages_url`) enforces on this same field —
            // refuse to build a request URL from a `file://`/`gopher://`/etc.
            // server value instead of silently embedding it.
            if !is_safe_http_server(server) {
                warn!(
                    server = %server,
                    "BC server URL failed the http(s)-or-bare-host safety allowlist; refusing to \
                     build a request URL from it"
                );
                return REJECTED_SERVER_BASE_URL.to_string();
            }

            // Ensure the server URL has a scheme to prevent accidental plain-HTTP
            // requests when the caller omits the scheme prefix.
            let server_with_scheme =
                if server.starts_with("http://") || server.starts_with("https://") {
                    server.to_string()
                } else {
                    // Not silent: defaulting to http:// here means Basic
                    // (UserPassword/Windows) credentials go out
                    // Base64-in-cleartext. Say so, so an operator who wanted
                    // TLS notices a plain hostname was misread as http.
                    warn!(
                        server = %server,
                        "BC server URL has no scheme — defaulting to http:// (cleartext); Basic/\
                         Windows credentials will be sent unencrypted. Use an explicit https:// \
                         URL to avoid this."
                    );
                    format!("http://{}", server)
                };
            let server_trimmed = server_with_scheme.trim_end_matches('/');
            let host = server_trimmed
                .split_once("://")
                .map_or(server_trimmed, |(_, rest)| rest);

            match config.port {
                Some(port) => format!("{}:{}/{}", server_trimmed, port, instance),
                // `server` already carries its own port (e.g. a full
                // `http://host:1234` URL, as every mock-server test in this
                // crate uses) — do not double it up.
                None if host_has_explicit_port(host) => {
                    format!("{}/{}", server_trimmed, instance)
                }
                // No port anywhere: apply BC's documented on-prem dev
                // default instead of silently falling through to whatever
                // the scheme's default port is (80 for http).
                None => format!(
                    "{}:{}/{}",
                    server_trimmed, DEFAULT_ONPREM_DEV_PORT, instance
                ),
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
            debug_args: serde_json::json!({}),
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
            debug_args: serde_json::json!({}),
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
        let mut config = on_prem_config();
        config.server = Some("localhost".to_string());
        let url = build_base_url(&config);
        assert!(
            url.starts_with("http://") || url.starts_with("https://"),
            "URL must have a scheme: {url}"
        );
    }

    #[test]
    fn on_prem_url_applies_default_dev_port_when_absent() {
        // The `port` field is documented as "default 7049" (launch.rs); a
        // bare host with no port anywhere must get that default rather than
        // silently falling through to the scheme's default (80 for http).
        let mut config = on_prem_config();
        config.server = Some("bc.example.com".to_string());
        config.port = None;
        let url = build_base_url(&config);
        assert_eq!(url, "http://bc.example.com:7049/BC");
    }

    #[test]
    fn on_prem_url_does_not_double_port_when_server_already_has_one() {
        // A `server` value that already embeds a port (as every mock-server
        // test in this module does) must not get `:7049` appended on top.
        let mut config = on_prem_config();
        config.server = Some("http://bc.example.com:8080".to_string());
        config.port = None;
        let url = build_base_url(&config);
        assert_eq!(url, "http://bc.example.com:8080/BC");
    }

    #[test]
    fn on_prem_url_rejects_unsafe_scheme() {
        // Same allowlist the symbol-download path enforces
        // (`launch::is_safe_http_server`) — a `file://`/`gopher://`/etc.
        // server value must never be embedded in the request URL.
        let mut config = on_prem_config();
        config.server = Some("file:///etc/passwd".to_string());
        let url = build_base_url(&config);
        assert_eq!(url, REJECTED_SERVER_BASE_URL);
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
        let config = on_prem_config();
        let _client = BcClient::new(&config);
    }

    #[test]
    fn sanitize_error_body_truncates_at_512_bytes() {
        let body = "x".repeat(2000);
        let out = sanitize_error_body(&body);
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
        assert!(out.is_char_boundary(out.len()));
    }

    #[tokio::test]
    async fn read_app_capped_accepts_normal_sized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("small.app");
        let payload = vec![0u8; 1024];
        tokio::fs::write(&path, &payload).await.unwrap();

        let bytes = read_app_capped(&path).await.unwrap();
        assert_eq!(bytes.len(), 1024);
    }

    #[tokio::test]
    async fn read_app_capped_rejects_oversize_without_reading() {
        // Negative: a `.app` larger than `MAX_UPLOADABLE_APP_BYTES` must
        // be refused with `AppFileTooLarge` BEFORE the body is read into
        // memory. Producing a 500+ MB file in the test would be wasteful;
        // we use a sparse file via `set_len` so the metadata reports a
        // huge size but the actual disk usage is one block.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.app");
        let file = std::fs::File::create(&path).unwrap();
        // 600 MB virtual size — past the 500 MB cap.
        file.set_len(600 * 1024 * 1024).unwrap();
        drop(file);

        let err = read_app_capped(&path).await.unwrap_err();
        match err {
            BcClientError::AppFileTooLarge { bytes, limit } => {
                assert_eq!(bytes, 600 * 1024 * 1024);
                assert_eq!(limit, MAX_UPLOADABLE_APP_BYTES);
            }
            other => panic!("expected AppFileTooLarge, got {other:?}"),
        }
    }

    #[derive(Debug, serde::Deserialize)]
    struct DummyResp {
        ok: bool,
    }

    #[tokio::test]
    async fn read_json_body_capped_accepts_small_response() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", "11")
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();
        let v: DummyResp = read_json_body_capped(resp).await.unwrap();
        assert!(v.ok);
    }

    #[tokio::test]
    async fn read_json_body_capped_refuses_missing_content_length() {
        // Negative: chunked / no-length responses are refused — the cap
        // would be unenforceable.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Transfer-Encoding", "chunked")
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();
        let res: Result<DummyResp, _> = read_json_body_capped(resp).await;
        assert!(res.is_err(), "missing Content-Length must be refused");
    }

    #[tokio::test]
    async fn read_json_body_capped_refuses_oversize_content_length() {
        // Negative: a Content-Length above the cap is rejected before
        // the body is read. Wiremock can't actually serve a body shorter
        // than the claimed length without hyper aborting on the way in;
        // either reqwest's transport error OR our cap-rejection means
        // "bogus oversize length doesn't yield Ok JSON".
        let oversize = (MAX_BC_JSON_RESPONSE_BYTES + 1).to_string();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", oversize.as_str())
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new().get(server.uri()).send().await;
        match resp {
            Ok(r) => {
                let res: Result<DummyResp, _> = read_json_body_capped(r).await;
                assert!(res.is_err(), "oversized Content-Length must not yield Ok");
            }
            Err(_) => {
                // reqwest aborted the response — also acceptable: the
                // mismatch was caught one layer below.
            }
        }
    }

    #[tokio::test]
    async fn read_json_body_capped_preserves_status_on_parse_failure() {
        // When the body is well-sized but not valid JSON, the resulting
        // error must carry the real HTTP status, not a hardcoded 0, so
        // callers can distinguish a 4xx/5xx error body from malformed
        // JSON in a 200 OK.
        let body = "not json";
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(503)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();
        let res: Result<DummyResp, _> = read_json_body_capped(resp).await;
        match res {
            Err(BcClientError::ServerError { status, .. }) => {
                assert_eq!(status, 503, "parse-failure error must keep the HTTP status");
            }
            other => panic!("expected ServerError with status 503, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_binary_body_capped_accepts_small_response() {
        let body = b"\x00\x01\x02profile-data";
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_bytes(body.to_vec()),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();
        let bytes = read_binary_body_capped(resp).await.unwrap();
        assert_eq!(bytes, body);
    }

    #[tokio::test]
    async fn read_binary_body_capped_refuses_oversize_content_length() {
        // Negative: an advertised Content-Length above the cap is rejected
        // before the body is buffered.
        let oversize = (MAX_BC_BINARY_RESPONSE_BYTES + 1).to_string();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", oversize.as_str())
                    .set_body_bytes(b"small".to_vec()),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new().get(server.uri()).send().await;
        match resp {
            Ok(r) => {
                let res = read_binary_body_capped(r).await;
                assert!(
                    res.is_err(),
                    "oversized Content-Length must not yield Ok bytes"
                );
            }
            Err(_) => {
                // reqwest aborted on the length mismatch — also acceptable.
            }
        }
    }

    #[tokio::test]
    async fn read_binary_body_capped_allows_missing_content_length() {
        // A chunked binary download (no Content-Length) is still allowed —
        // unlike JSON, file downloads are commonly chunked — but the
        // post-read size re-check still bounds it. Here the body is tiny.
        let body = b"chunked-binary";
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Transfer-Encoding", "chunked")
                    .set_body_bytes(body.to_vec()),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();
        let bytes = read_binary_body_capped(resp).await.unwrap();
        assert_eq!(bytes, body);
    }

    #[tokio::test]
    async fn read_error_body_capped_reads_small_error_body() {
        let body = "boom: something went wrong";
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(500)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .get(server.uri())
            .send()
            .await
            .unwrap();
        let msg = read_error_body_capped(resp).await;
        assert!(
            msg.contains("boom"),
            "small error body must be returned: {msg}"
        );
    }

    #[tokio::test]
    async fn read_error_body_capped_refuses_oversize_content_length() {
        // An error body whose advertised length exceeds the 64 KiB cap is
        // not buffered at all — the helper returns a sentinel instead.
        let oversize = (MAX_ERROR_BODY_BYTES + 1).to_string();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(500)
                    .insert_header("Content-Length", oversize.as_str())
                    .set_body_string("x"),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new().get(server.uri()).send().await;
        if let Ok(r) = resp {
            let msg = read_error_body_capped(r).await;
            assert!(
                msg.contains("exceeds") || msg.contains("not read"),
                "oversize error body must not be buffered: {msg}"
            );
        }
    }

    /// Point a `BcClient` at a wiremock server. The on-prem `build_base_url`
    /// yields `{server}/{instance}`; passing the full mock URI as `server`
    /// (scheme included) and `BC` as the instance makes the BC Dev API paths
    /// resolve under `{mock_uri}/BC/...`.
    fn client_for(uri: &str) -> BcClient {
        BcClient::new(&BcServerConfig {
            name: "mock".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some(uri.to_string()),
            server_instance: Some("BC".to_string()),
            port: None,
            environment_name: None,
            tenant: None,
            authentication: AuthMethod::Windows, // no creds required
            accept_invalid_certs: false,
            debug_args: serde_json::json!({}),
        })
    }

    #[tokio::test]
    async fn handle_response_success_path_uses_capped_json_reader() {
        // The success branch must route through `read_json_body_capped`, which requires a
        // Content-Length so the 16 MB cap is enforceable. A chunked (no
        // Content-Length) 200 JSON body is therefore refused.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/BC/dev/extensions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Transfer-Encoding", "chunked")
                    .set_body_string(r#"{"appId":"abc","status":"Completed"}"#),
            )
            .mount(&server)
            .await;

        let client = client_for(&server.uri());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        let res = client.publish_extension(&app).await;
        match res {
            Err(BcClientError::ServerError { message, .. }) => {
                assert!(
                    message.contains("Content-Length"),
                    "success path must enforce the capped JSON reader: {message}"
                );
            }
            other => panic!("expected capped-reader ServerError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_response_error_path_uses_capped_error_reader() {
        // The non-2xx branch routes through `map_error_response`, which must
        // use `read_error_body_capped`. An
        // error body advertising a Content-Length above the 64 KiB cap must
        // NOT be buffered — the returned message carries the sentinel instead
        // of the body.
        let oversize = (MAX_ERROR_BODY_BYTES + 1).to_string();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/BC/dev/extensions"))
            .respond_with(
                wiremock::ResponseTemplate::new(500)
                    .insert_header("Content-Length", oversize.as_str())
                    .set_body_string("x"),
            )
            .mount(&server)
            .await;

        let client = client_for(&server.uri());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        let res = client.publish_extension(&app).await;
        match res {
            Err(BcClientError::ServerError { message, .. }) => {
                assert!(
                    message.contains("exceeds") || message.contains("not read"),
                    "error path must not buffer an oversize body: {message}"
                );
            }
            // A transport-level abort is also acceptable: the oversize
            // mismatch was caught below us and the body still wasn't buffered.
            Err(_) => {}
            Ok(_) => panic!("oversize 500 error body must not yield Ok"),
        }
    }

    #[tokio::test]
    async fn handle_response_success_path_accepts_capped_json_with_length() {
        let body = r#"{"appId":"abc","status":"Completed"}"#;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/BC/dev/extensions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let client = client_for(&server.uri());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        let resp = client.publish_extension(&app).await.expect("should parse");
        assert_eq!(resp.app_id.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn publish_extension_sends_tenant_as_query_param_not_header() {
        // BC's on-prem dev endpoints read `?tenant=` (see
        // `launch::dev_packages_url`, used by the symbol-download path) —
        // not a custom header. A mock that only matches the query param
        // proves the tenant is sent that way: if the client regressed to an
        // `X-Tenant` header, this mock would not match and the request would
        // 404 against wiremock's default "no matching mock" response.
        let server = wiremock::MockServer::start().await;
        let body = r#"{"status":"Completed"}"#;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/BC/dev/extensions"))
            .and(wiremock::matchers::query_param("tenant", "contoso"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let config = BcServerConfig {
            name: "mock".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some(server.uri()),
            server_instance: Some("BC".to_string()),
            port: None,
            environment_name: None,
            tenant: Some("contoso".to_string()),
            authentication: AuthMethod::Windows,
            accept_invalid_certs: false,
            debug_args: serde_json::json!({}),
        };
        let client = BcClient::new(&config);
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        client
            .publish_extension(&app)
            .await
            .expect("tenant must be sent as a query param the mock matches on");
    }

    #[tokio::test]
    async fn tenant_default_and_empty_are_never_sent() {
        let server = wiremock::MockServer::start().await;
        let body = r#"{"status":"Completed"}"#;
        // The mock has NO query_param matcher: it must match regardless of
        // whether wiremock decides to inspect the query string, but the real
        // assertion is that request-building itself doesn't panic/error for
        // the "default" and "" tenant sentinels, mirroring `apply_tenant_query`'s
        // skip condition.
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/BC/dev/extensions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        for tenant in [Some("default".to_string()), Some(String::new()), None] {
            let config = BcServerConfig {
                name: "mock".to_string(),
                environment_type: EnvironmentType::OnPrem,
                server: Some(server.uri()),
                server_instance: Some("BC".to_string()),
                port: None,
                environment_name: None,
                tenant,
                authentication: AuthMethod::Windows,
                accept_invalid_certs: false,
                debug_args: serde_json::json!({}),
            };
            let client = BcClient::new(&config);
            let dir = tempfile::tempdir().unwrap();
            let app = dir.path().join("ext.app");
            tokio::fs::write(&app, b"app-bytes").await.unwrap();
            client
                .publish_extension(&app)
                .await
                .expect("default/empty tenant must not break the request");
        }
    }

    #[tokio::test]
    #[serial_test::serial(bc_creds_env)]
    async fn windows_auth_uses_http_basic_when_credentials_present() {
        // `AuthMethod::Windows` is documented as HTTP Basic, not a real
        // NTLM/Negotiate handshake — with BC_USERNAME/BC_PASSWORD set it must
        // send exactly the same `Authorization: Basic …` header UserPassword
        // would.
        // SAFETY: serialised via `#[serial_test::serial]` on this test name group.
        unsafe {
            std::env::set_var("BC_USERNAME", "alice");
            std::env::set_var("BC_PASSWORD", "wonderland");
        }

        let server = wiremock::MockServer::start().await;
        let body = r#"{"status":"Completed"}"#;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/BC/dev/extensions"))
            .and(wiremock::matchers::header(
                "Authorization",
                // base64("alice:wonderland")
                "Basic YWxpY2U6d29uZGVybGFuZA==",
            ))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server.uri());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();
        let result = client.publish_extension(&app).await;

        // SAFETY: serialised via `#[serial_test::serial]` on this test name group.
        unsafe {
            std::env::remove_var("BC_USERNAME");
            std::env::remove_var("BC_PASSWORD");
        }
        result.expect("Basic-auth request must match the mock");
    }

    #[tokio::test]
    #[serial_test::serial(bc_creds_env)]
    async fn windows_auth_without_credentials_sends_no_authorization_header() {
        // Without BC_USERNAME/BC_PASSWORD, `AuthMethod::Windows` must NOT
        // silently claim NTLM/Negotiate worked — it sends the request with no
        // Authorization header at all (this crate implements no such
        // handshake). A mock that rejects any Authorization header proves
        // none was attached.
        // SAFETY: serialised via `#[serial_test::serial]` on this test name group.
        unsafe {
            std::env::remove_var("BC_USERNAME");
            std::env::remove_var("BC_PASSWORD");
        }

        let server = wiremock::MockServer::start().await;
        let body = r#"{"status":"Completed"}"#;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/BC/dev/extensions"))
            .and(|req: &wiremock::Request| !req.headers.contains_key("Authorization"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server.uri());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        client
            .publish_extension(&app)
            .await
            .expect("no-Authorization request must match the mock");
    }

    #[test]
    fn sanitize_error_body_redacts_case_insensitively() {
        // IIS/BC error pages commonly capitalize these differently than the
        // lowercase needles this function matches against.
        let body = "Boom: Password=hunter2 and PASSWORD=hunter3 and \
                     authorization: bearer sekrit-token rejected";
        let out = sanitize_error_body(body);
        assert!(
            !out.contains("hunter2"),
            "Password= must be redacted: {out}"
        );
        assert!(
            !out.contains("hunter3"),
            "PASSWORD= must be redacted: {out}"
        );
        assert!(
            !out.contains("sekrit-token"),
            "lowercase 'authorization: bearer' must be redacted: {out}"
        );
        assert_eq!(out.matches("[REDACTED]").count(), 3);
    }

    // `rad_publish` (PATCH /dev/applications/{id}) previously had zero test
    // coverage: every wiremock test in this module exercised only
    // `publish_extension`. These pin the RAD URL shape (method + path +
    // tenant query param) and `ApplicationStateResponse` handling.

    #[tokio::test]
    async fn rad_publish_uses_patch_and_application_id_path() {
        let body = r#"{"appId":"guid-42","status":"Completed","version":"3.0.0.0"}"#;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path("/BC/dev/applications/guid-42"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for(&server.uri());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        let resp = client
            .rad_publish("guid-42", &app)
            .await
            .expect("PATCH to the app-id path must match the mock");
        assert_eq!(resp.app_id.as_deref(), Some("guid-42"));
        assert_eq!(resp.version.as_deref(), Some("3.0.0.0"));
        assert_eq!(resp.status.as_deref(), Some("Completed"));
    }

    #[tokio::test]
    async fn rad_publish_sends_tenant_as_query_param() {
        let body = r#"{"appId":"guid-7","status":"Completed"}"#;
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path("/BC/dev/applications/guid-7"))
            .and(wiremock::matchers::query_param("tenant", "contoso"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let config = BcServerConfig {
            name: "mock".to_string(),
            environment_type: EnvironmentType::OnPrem,
            server: Some(server.uri()),
            server_instance: Some("BC".to_string()),
            port: None,
            environment_name: None,
            tenant: Some("contoso".to_string()),
            authentication: AuthMethod::Windows,
            accept_invalid_certs: false,
            debug_args: serde_json::json!({}),
        };
        let client = BcClient::new(&config);
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        client
            .rad_publish("guid-7", &app)
            .await
            .expect("tenant must be sent as a query param the mock matches on");
    }

    #[tokio::test]
    async fn rad_publish_server_error_maps_to_server_error() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("PATCH"))
            .and(wiremock::matchers::path("/BC/dev/applications/guid-9"))
            .respond_with(wiremock::ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let client = client_for(&server.uri());
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("ext.app");
        tokio::fs::write(&app, b"app-bytes").await.unwrap();

        let err = client
            .rad_publish("guid-9", &app)
            .await
            .expect_err("500 must error");
        match err {
            BcClientError::ServerError { status, message } => {
                assert_eq!(status, 500);
                assert!(message.contains("boom"), "got: {message}");
            }
            other => panic!("expected ServerError, got {other:?}"),
        }
    }
}
