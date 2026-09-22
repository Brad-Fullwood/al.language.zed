//! The OAuth flows themselves: auth code with PKCE, device code, and
//! refresh, plus the polling rules the token endpoint drives.

use super::cache::{load_cached_token, now_unix, save_cached_token, token_cache_path};
use super::encoding::percent_encode;
use super::pkce::{generate_code_verifier, generate_random_string, pkce_challenge};
use super::redirect::{open_browser, wait_for_auth_callback};
use super::validation::{is_valid_tenant, is_well_formed_guid};
use super::{OAuthError, TokenResponse};
use serde::Deserialize;
use std::time::{Duration, SystemTime};
use tracing::{debug, info};

/// Azure CLI public client ID — first-party Microsoft app that supports
/// auth code + PKCE with localhost redirect for any Microsoft API scope.
pub(super) const DEFAULT_CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";

const BC_SCOPE: &str = "https://api.businesscentral.dynamics.com/.default offline_access";

/// Upper bound on the device-code polling interval. Each `slow_down` response
/// from the token endpoint bumps the interval by 5s; without a cap a buggy or
/// hostile server could push the interval arbitrarily high (stalling sign-in
/// until the deadline). Capping at 60s respects the server's congestion signal
/// while keeping the worst-case poll cadence bounded.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Lower bound on the device-code polling interval. A server reporting
/// `interval: 0` would otherwise spin the token endpoint as fast as the network
/// allows.
const MIN_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Upper bound on the device-code lifetime. Entra ID issues 15 minutes; the cap
/// keeps a bogus `expires_in` from overflowing `SystemTime + Duration` (which
/// panics) and from pinning a daemon task open indefinitely.
const MAX_DEVICE_CODE_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// Clamp a polling interval into `[MIN_POLL_INTERVAL, MAX_POLL_INTERVAL]`.
/// Applied to every interval the server can influence — the initial `interval`,
/// `slow_down` bumps, and `Retry-After` — so the worst-case poll cadence is
/// bounded no matter what the token endpoint returns.
pub(super) fn clamp_poll_interval(interval: Duration) -> Duration {
    interval.clamp(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL)
}

/// Compute the next polling interval after a `slow_down` response: bump by 5s,
/// saturating, and clamp to [`MAX_POLL_INTERVAL`].
pub(super) fn next_slow_down_interval(current: Duration) -> Duration {
    clamp_poll_interval(current.saturating_add(Duration::from_secs(5)))
}

/// Acquire a BC access token for the given tenant.
///
/// 1. Check disk cache for a valid token
/// 2. If expired, try refresh token
/// 3. If no valid token, do interactive sign-in (browser → localhost redirect)
/// 4. Falls back to device code flow if browser can't be opened
///
/// `on_message` is called with status messages for the user.
pub async fn acquire_token(
    client: &reqwest::Client,
    tenant: &str,
    on_message: impl Fn(&str),
) -> Result<String, OAuthError> {
    // Reject obviously-malformed tenants before interpolating into the OAuth
    // URLs. A typical tenant is a GUID, an `*.onmicrosoft.com` domain, or one
    // of the well-known reserved names. Anything else — slashes, query
    // strings, embedded URLs — could redirect/poison the request even though
    // the host (login.microsoftonline.com) is hardcoded.
    if !is_valid_tenant(tenant) {
        return Err(OAuthError::Other(format!(
            "Invalid tenant '{tenant}': expected a GUID, domain, or one of \
             'common'/'organizations'/'consumers'"
        )));
    }
    let client_id = configured_client_id()?;
    let cache_path = token_cache_path(tenant);

    if let Some(cached) = load_cached_token(&cache_path, tenant) {
        let now = now_unix();
        if cached.expires_at > now + 60 {
            debug!(tenant, "Using cached BC access token");
            // Clone so `cached` drops intact and its secret fields are
            // zeroized; a partial move would forfeit the `Drop` scrub.
            return Ok(cached.access_token.clone());
        }

        if let Some(ref refresh) = cached.refresh_token {
            debug!(tenant, "Access token expired, refreshing");
            match refresh_token_flow(client, tenant, &client_id, refresh).await {
                Ok(tok) => {
                    save_cached_token(&cache_path, tenant, &tok);
                    info!(tenant, "Refreshed BC access token");
                    // Clone so `tok` drops intact (Drop scrubs the secrets).
                    return Ok(tok.access_token.clone());
                }
                Err(e) => {
                    debug!(error = %e, "Refresh failed, doing interactive sign-in");
                }
            }
        }
    }

    let tok = interactive_sign_in(client, tenant, &client_id, &on_message).await?;
    save_cached_token(&cache_path, tenant, &tok);
    info!(tenant, "Acquired BC access token");
    // Clone so `tok` drops intact (Drop scrubs the secrets).
    Ok(tok.access_token.clone())
}

pub(super) fn configured_client_id() -> Result<String, OAuthError> {
    match std::env::var("BC_CLIENT_ID") {
        Ok(v) if !v.trim().is_empty() => {
            let trimmed = v.trim();
            // Reject malformed IDs before building an authorization URL so a
            // configuration error does not become an opaque redirect failure.
            if !is_well_formed_guid(trimmed) {
                Err(OAuthError::Other(
                    "BC_CLIENT_ID must be a well-formed GUID".to_string(),
                ))
            } else {
                Ok(trimmed.to_string())
            }
        }
        Ok(_) => Err(OAuthError::Other("BC_CLIENT_ID is blank".to_string())),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_CLIENT_ID.into()),
        Err(std::env::VarError::NotUnicode(_)) => Err(OAuthError::Other(
            "BC_CLIENT_ID contains non-UTF-8 bytes".to_string(),
        )),
    }
}

pub(super) async fn interactive_sign_in(
    client: &reqwest::Client,
    tenant: &str,
    client_id: &str,
    on_message: &impl Fn(&str),
) -> Result<TokenResponse, OAuthError> {
    match browser_auth_flow(client, tenant, client_id, on_message).await {
        Ok(tok) => return Ok(tok),
        Err(e) => {
            debug!(error = %e, "Browser auth flow failed, trying device code");
            on_message("Browser sign-in failed, falling back to device code...");
        }
    }

    device_code_flow(client, tenant, client_id, on_message).await
}

pub(super) async fn browser_auth_flow(
    client: &reqwest::Client,
    tenant: &str,
    client_id: &str,
    on_message: &impl Fn(&str),
) -> Result<TokenResponse, OAuthError> {
    let verifier = generate_code_verifier()?;
    let challenge = pkce_challenge(&verifier);
    let state = generate_random_string(16)?;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}");

    let auth_url = format!(
        "https://login.microsoftonline.com/{}/oauth2/v2.0/authorize?\
         client_id={}&response_type=code&redirect_uri={}&scope={}&\
         code_challenge={}&code_challenge_method=S256&state={}&prompt=select_account",
        tenant,
        client_id,
        percent_encode(&redirect_uri),
        percent_encode(BC_SCOPE),
        challenge,
        state,
    );

    on_message("Opening browser for BC sign-in...");
    if !open_browser(&auth_url) {
        return Err(OAuthError::Protocol {
            error: "no_browser".into(),
            description: "Could not open browser".into(),
        });
    }

    let code = tokio::time::timeout(
        Duration::from_secs(300),
        wait_for_auth_callback(&listener, &state),
    )
    .await
    .map_err(|_| OAuthError::Expired)??;

    let token_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");

    let resp = client
        .post(&token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("code_verifier", &verifier),
            ("scope", BC_SCOPE),
        ])
        .send()
        .await?;

    handle_token_response(resp).await
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    #[serde(default)]
    user_code: String,
    #[serde(default)]
    verification_uri: String,
    /// URL with code embedded — user just clicks Continue instead of typing.
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(default = "default_device_code_lifetime_secs")]
    expires_in: u64,
    #[serde(default = "default_device_code_poll_secs")]
    interval: u64,
}

fn default_device_code_lifetime_secs() -> u64 {
    900
}

fn default_device_code_poll_secs() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[serde(default)]
    error_description: String,
}

pub(super) async fn device_code_flow(
    client: &reqwest::Client,
    tenant: &str,
    client_id: &str,
    on_message: &impl Fn(&str),
) -> Result<TokenResponse, OAuthError> {
    let device_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/devicecode");
    let token_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");

    let dc: DeviceCodeResponse = client
        .post(&device_url)
        .form(&[("client_id", client_id), ("scope", BC_SCOPE)])
        .send()
        .await?
        .json()
        .await?;

    if let Some(ref uri) = dc.verification_uri_complete {
        open_browser(uri);
        on_message(&format!(
            "Browser opened for sign-in. Code {} is pre-filled — just click Continue and sign in.",
            dc.user_code
        ));
    } else {
        open_browser(&dc.verification_uri);
        on_message(&format!(
            "Open {} and enter code: {}",
            dc.verification_uri, dc.user_code
        ));
    }

    // `expires_in` / `interval` are server-controlled. Clamp both before they
    // reach `SystemTime::add` / `tokio::time::sleep`: `SystemTime + Duration`
    // panics on overflow, and an unclamped interval would park the sign-in for
    // as long as the server asks (up to ~584 billion years for `u64::MAX`).
    let lifetime = Duration::from_secs(dc.expires_in.min(MAX_DEVICE_CODE_LIFETIME.as_secs()));
    // If even the clamped lifetime cannot be represented, fall back to an
    // already-elapsed deadline. `SystemTime::now() + anything` would be a second
    // unchecked add that can panic on exactly the boundary we are guarding.
    let deadline = SystemTime::now()
        .checked_add(lifetime)
        .unwrap_or_else(SystemTime::now);
    let mut interval = clamp_poll_interval(Duration::from_secs(dc.interval));

    loop {
        tokio::time::sleep(interval).await;

        if SystemTime::now() >= deadline {
            return Err(OAuthError::Expired);
        }

        let resp = client
            .post(&token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", client_id),
                ("device_code", &dc.device_code),
            ])
            .send()
            .await?;

        if resp.status().is_success() {
            return Ok(resp.json().await?);
        }

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(Duration::from_secs)
                // `interval * 2` panics on overflow; saturate instead. Both
                // branches go through the same clamp as `slow_down`, so a
                // hostile `Retry-After` can't stall sign-in past the cap.
                .unwrap_or_else(|| interval.saturating_mul(2));
            interval = clamp_poll_interval(retry_after);
            continue;
        }

        let body = resp.text().await.unwrap_or_default();
        let err: TokenErrorResponse = match serde_json::from_str(&body) {
            Ok(e) => e,
            Err(_) => return parse_token_error(&body),
        };

        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval = next_slow_down_interval(interval);
                continue;
            }
            "authorization_declined" => return Err(OAuthError::Denied),
            "expired_token" => return Err(OAuthError::Expired),
            _ => {
                return Err(OAuthError::Protocol {
                    error: err.error,
                    description: err.error_description,
                })
            }
        }
    }
}

pub(super) async fn refresh_token_flow(
    client: &reqwest::Client,
    tenant: &str,
    client_id: &str,
    refresh: &str,
) -> Result<TokenResponse, OAuthError> {
    let token_url = format!("https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token");

    let resp = client
        .post(&token_url)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", refresh),
            ("scope", BC_SCOPE),
        ])
        .send()
        .await?;

    handle_token_response(resp).await
}

/// Parse a token endpoint response: deserialize the body as a `TokenResponse`
/// on success, otherwise surface the OAuth error from the body.
pub(super) async fn handle_token_response(
    resp: reqwest::Response,
) -> Result<TokenResponse, OAuthError> {
    if resp.status().is_success() {
        Ok(resp.json().await?)
    } else {
        let body = resp.text().await.unwrap_or_default();
        parse_token_error(&body)
    }
}

pub(super) fn parse_token_error<T>(body: &str) -> Result<T, OAuthError> {
    let err: TokenErrorResponse = serde_json::from_str(body).unwrap_or(TokenErrorResponse {
        error: "unknown".into(),
        error_description: body.to_string(),
    });
    Err(OAuthError::Protocol {
        error: err.error,
        description: err.error_description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_down_interval_bumps_by_five_seconds() {
        let next = next_slow_down_interval(Duration::from_secs(5));
        assert_eq!(next, Duration::from_secs(10));
    }

    #[test]
    fn slow_down_interval_is_capped_at_max() {
        // Once at the cap, further slow_down responses must not exceed it.
        let at_cap = next_slow_down_interval(MAX_POLL_INTERVAL);
        assert_eq!(at_cap, MAX_POLL_INTERVAL);

        // Approaching the cap from just below clamps to exactly MAX_POLL_INTERVAL.
        let near_cap = next_slow_down_interval(MAX_POLL_INTERVAL - Duration::from_secs(1));
        assert_eq!(near_cap, MAX_POLL_INTERVAL);

        // A pathologically large current value saturates and clamps, never panicking.
        let huge = next_slow_down_interval(Duration::from_secs(u64::MAX));
        assert_eq!(huge, MAX_POLL_INTERVAL);
    }

    #[test]
    fn repeated_slow_down_converges_to_cap() {
        // Simulate many consecutive slow_down responses; the interval must
        // monotonically grow but never exceed the cap.
        let mut interval = Duration::from_secs(5);
        for _ in 0..200 {
            interval = next_slow_down_interval(interval);
            assert!(interval <= MAX_POLL_INTERVAL);
        }
        assert_eq!(interval, MAX_POLL_INTERVAL);
    }

    #[test]
    fn poll_interval_is_clamped_from_every_server_controlled_path() {
        // Initial `interval`, `slow_down` bumps and `Retry-After` all funnel
        // through the same clamp, so no server value can stall or spin sign-in.
        assert_eq!(clamp_poll_interval(Duration::ZERO), MIN_POLL_INTERVAL);
        assert_eq!(clamp_poll_interval(Duration::MAX), MAX_POLL_INTERVAL);
        assert_eq!(
            clamp_poll_interval(Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        // saturating_mul is what keeps the Retry-After fallback from panicking.
        assert_eq!(
            clamp_poll_interval(Duration::MAX.saturating_mul(2)),
            MAX_POLL_INTERVAL
        );
        assert_eq!(next_slow_down_interval(Duration::MAX), MAX_POLL_INTERVAL);
        assert_eq!(
            next_slow_down_interval(Duration::from_secs(5)),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn device_code_lifetime_cannot_overflow_systemtime() {
        // `SystemTime + Duration` panics on overflow; the lifetime clamp is what
        // stops a bogus `expires_in` from taking the process down. Mirror the
        // clamp `device_code_flow` applies, for each shape a server can send.
        for expires_in in [u64::MAX, u64::MAX / 2, 900, 0] {
            let lifetime = Duration::from_secs(expires_in.min(MAX_DEVICE_CODE_LIFETIME.as_secs()));
            assert!(lifetime <= MAX_DEVICE_CODE_LIFETIME);
            assert!(
                SystemTime::now().checked_add(lifetime).is_some(),
                "expires_in={expires_in} must not overflow SystemTime"
            );
        }
    }

    #[test]
    fn parse_token_error_extracts_oauth_fields() {
        let body = r#"{"error":"invalid_grant","error_description":"AADSTS70008: expired"}"#;
        let err: OAuthError = parse_token_error::<TokenResponse>(body).unwrap_err();
        match err {
            OAuthError::Protocol { error, description } => {
                assert_eq!(error, "invalid_grant");
                assert!(description.contains("AADSTS70008"));
            }
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    #[test]
    fn parse_token_error_falls_back_for_non_json_body() {
        // Boundary: an HTML/plain-text error page (not JSON) must still yield a
        // Protocol error carrying the raw body as the description.
        let body = "<html>503 upstream down</html>";
        let err: OAuthError = parse_token_error::<TokenResponse>(body).unwrap_err();
        match err {
            OAuthError::Protocol { error, description } => {
                assert_eq!(error, "unknown");
                assert_eq!(description, body);
            }
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_token_response_parses_success_body() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"access_token":"TOK","refresh_token":"REF","expires_in":3599}"#,
            ))
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .post(server.uri())
            .send()
            .await
            .unwrap();
        let tok = handle_token_response(resp).await.expect("success body");
        assert_eq!(tok.access_token, "TOK");
        assert_eq!(tok.refresh_token.as_deref(), Some("REF"));
        assert_eq!(tok.expires_in, 3599);
    }

    #[tokio::test]
    async fn handle_token_response_surfaces_error_body() {
        // A non-2xx status must be turned into an OAuthError::Protocol carrying
        // the parsed error fields — NOT silently swallowed as success.
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(
                    r#"{"error":"invalid_request","error_description":"bad code"}"#,
                ),
            )
            .mount(&server)
            .await;

        let resp = reqwest::Client::new()
            .post(server.uri())
            .send()
            .await
            .unwrap();
        match handle_token_response(resp).await {
            Err(OAuthError::Protocol { error, description }) => {
                assert_eq!(error, "invalid_request");
                assert_eq!(description, "bad code");
            }
            other => panic!("expected Protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn acquire_token_rejects_malformed_tenant_before_network() {
        // A tenant containing URL punctuation must be rejected up-front with
        // OAuthError::Other and never touch the network. The wiremock client
        // is unreachable host, so any network attempt would surface a different
        // error; getting Other proves the guard short-circuits first.
        let client = reqwest::Client::new();
        let err = acquire_token(&client, "evil.com/redirect?to=phish", |_| {})
            .await
            .unwrap_err();
        match err {
            OAuthError::Other(msg) => assert!(msg.contains("Invalid tenant"), "got {msg}"),
            other => panic!("expected Other(Invalid tenant), got {other:?}"),
        }
    }
}
