//! OAuth 2.0 authentication for Microsoft Entra ID (Azure AD).
//!
//! Primary flow: Authorization Code + PKCE with local redirect server.
//! Browser opens → user signs in → redirect to localhost → token acquired.
//!
//! Fallback: Device code flow for headless environments.
//!
//! Tokens are cached to disk with refresh token support.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracing::{debug, info, warn};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Azure CLI public client ID — first-party Microsoft app that supports
/// auth code + PKCE with localhost redirect for any Microsoft API scope.
const DEFAULT_CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";

const BC_SCOPE: &str = "https://api.businesscentral.dynamics.com/.default offline_access";

/// Upper bound on the device-code polling interval. Each `slow_down` response
/// from the token endpoint bumps the interval by 5s; without a cap a buggy or
/// hostile server could push the interval arbitrarily high (stalling sign-in
/// until the deadline). Capping at 60s respects the server's congestion signal
/// while keeping the worst-case poll cadence bounded.
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Compute the next polling interval after a `slow_down` response: bump by 5s,
/// saturating, and clamp to [`MAX_POLL_INTERVAL`].
fn next_slow_down_interval(current: Duration) -> Duration {
    current
        .saturating_add(Duration::from_secs(5))
        .min(MAX_POLL_INTERVAL)
}

#[derive(Debug, Error)]
pub enum OAuthError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Sign-in timed out — user did not complete authentication")]
    Expired,
    #[error("Authorization denied by user")]
    Denied,
    #[error("OAuth error: {error} — {description}")]
    Protocol { error: String, description: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Caller-supplied input failed validation before any network I/O.
    #[error("{0}")]
    Other(String),
}

/// Successful token response from the token endpoint.
///
/// The `access_token`/`refresh_token` byte buffers are wiped from memory when
/// this value drops (F-OPEN-010): al-lsp runs as a long-lived daemon (30-min
/// idle window), so without an explicit scrub the bearer/refresh secrets would
/// linger in freed heap allocations for the life of the process and could be
/// recovered from a core dump or `/proc/<pid>/mem` read.
#[derive(Debug, Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    #[zeroize(skip)]
    expires_in: u64,
}

/// Secret fields are zeroized on drop for the same reason as [`TokenResponse`]
/// `expires_at` and `tenant` are non-secret and skipped.
#[derive(Debug, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct CachedToken {
    access_token: String,
    refresh_token: Option<String>,
    #[zeroize(skip)]
    expires_at: u64,
    #[zeroize(skip)]
    tenant: String,
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

fn configured_client_id() -> Result<String, OAuthError> {
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

async fn interactive_sign_in(
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

async fn browser_auth_flow(
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

/// Read an HTTP request from an async stream, looping until the header
/// terminator `\r\n\r\n` is seen or the 8 KiB buffer limit is reached.
/// This handles TCP segmentation where a single `read()` may not deliver
/// the full request line containing the query string.
async fn read_http_request<R: tokio::io::AsyncRead + Unpin>(
    stream: R,
) -> Result<String, OAuthError> {
    use tokio::io::AsyncReadExt;
    let mut reader = tokio::io::BufReader::new(stream);
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 1024];
    loop {
        let n = reader
            .read(&mut tmp)
            .await
            .map_err(|e| OAuthError::Protocol {
                error: "read_failed".into(),
                description: format!("Failed to read HTTP request: {e}"),
            })?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 8192 {
            break;
        }
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8(buf).map_err(|e| OAuthError::Protocol {
        error: "invalid_utf8".into(),
        description: format!("HTTP request is not valid UTF-8: {e}"),
    })
}

async fn wait_for_auth_callback(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, OAuthError> {
    use tokio::io::AsyncWriteExt;

    let (stream, _) = listener.accept().await?;

    let (read_half, mut write_half) = tokio::io::split(stream);

    let request = read_http_request(read_half).await?;

    // Parse first line: GET /?code=...&state=... HTTP/1.1
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let query = path.split('?').nth(1).unwrap_or("");
    let params = parse_query_string(query);

    let (status_line, body) = if params.contains_key("error") {
        let err = params.get("error").map(|s| s.as_str()).unwrap_or("unknown");
        let desc = params
            .get("error_description")
            .map(|s| s.as_str())
            .unwrap_or("");
        (
            "HTTP/1.1 400 Bad Request",
            format!(
                "<html><body style=\"font-family:system-ui;display:flex;justify-content:center;\
                 align-items:center;height:100vh;margin:0\">\
                 <div style=\"text-align:center\">\
                 <h2 style=\"color:#c00\">Sign-in failed</h2>\
                 <p>{}: {}</p>\
                 <p style=\"color:#666\">You can close this tab.</p>\
                 </div></body></html>",
                html_escape(err),
                html_escape(desc)
            ),
        )
    } else {
        (
            "HTTP/1.1 200 OK",
            "<html><body style=\"font-family:system-ui;display:flex;justify-content:center;\
             align-items:center;height:100vh;margin:0\">\
             <div style=\"text-align:center\">\
             <h2>Signed in successfully</h2>\
             <p style=\"color:#666\">You can close this tab and return to your editor.</p>\
             </div></body></html>"
                .to_string(),
        )
    };

    let response = format!(
        "{status_line}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    if let Err(e) = write_half.write_all(response.as_bytes()).await {
        tracing::warn!("OAuth callback: failed to write HTTP response to browser: {e}");
    }
    if let Err(e) = write_half.shutdown().await {
        tracing::debug!("OAuth callback: shutdown of browser socket failed: {e}");
    }

    if let Some(err) = params.get("error") {
        let desc = params.get("error_description").cloned().unwrap_or_default();
        return Err(if err == "access_denied" {
            OAuthError::Denied
        } else {
            OAuthError::Protocol {
                error: err.clone(),
                description: percent_decode(&desc),
            }
        });
    }

    let state = params.get("state").map(|s| s.as_str()).unwrap_or("");
    if state != expected_state {
        return Err(OAuthError::Protocol {
            error: "state_mismatch".into(),
            description: "CSRF state parameter mismatch".into(),
        });
    }

    params
        .get("code")
        .cloned()
        .ok_or_else(|| OAuthError::Protocol {
            error: "missing_code".into(),
            description: "No authorization code in redirect".into(),
        })
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
    #[serde(default = "default_900")]
    expires_in: u64,
    #[serde(default = "default_5")]
    interval: u64,
}

fn default_900() -> u64 {
    900
}
fn default_5() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[serde(default)]
    error_description: String,
}

async fn device_code_flow(
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

    let deadline = SystemTime::now() + Duration::from_secs(dc.expires_in);
    let mut interval = Duration::from_secs(dc.interval);

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
                .unwrap_or_else(|| interval * 2);
            interval = retry_after;
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

async fn refresh_token_flow(
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

fn generate_code_verifier() -> Result<String, OAuthError> {
    let bytes = random_bytes(32)?;
    Ok(base64url_encode(&bytes))
}

fn pkce_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64url_encode(&hash)
}

fn generate_random_string(len: usize) -> Result<String, OAuthError> {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    // Rejection sampling threshold: largest multiple of CHARS.len() (62) that
    // fits in a u8. 256 - (256 % 62) = 248. Bytes >= 248 would skew the
    // distribution toward chars 0..7 if folded with %, so we discard them
    // and draw fresh bytes.  Worst case ratio is 248/256 ≈ 96.875% accept,
    // so the loop terminates in expected O(len) draws.
    const ACCEPT_LT: u8 = (u8::MAX as usize - (u8::MAX as usize % CHARS.len())) as u8;
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let need = len - out.len();
        // Draw a buffer larger than `need` to amortise the syscall cost when
        // ~3% of bytes will be rejected.
        let bytes = random_bytes(need + need / 16 + 1)?;
        for b in bytes {
            if b < ACCEPT_LT {
                out.push(CHARS[(b as usize) % CHARS.len()] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Generate `n` cryptographically random bytes using the OS entropy source.
/// Uses `getrandom` which works on Linux, macOS, Windows, and WASM.
fn random_bytes(n: usize) -> Result<Vec<u8>, OAuthError> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).map_err(|e| OAuthError::Protocol {
        error: "getrandom_failed".to_string(),
        description: format!("Failed to get random bytes: {e}"),
    })?;
    Ok(buf)
}

/// Base64url encoding without padding (RFC 7636).
fn base64url_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3F) as usize] as char);
        }
    }
    out
}

/// Escape HTML special characters to prevent XSS in the OAuth redirect page.
///
/// The OAuth redirect page renders server-returned values (`error`, `error_description`)
/// directly in HTML. These values come from the authorization server redirect URL and
/// could contain `<script>` or other HTML if the user was redirected to a malicious server.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            other => out.push(other),
        }
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        // Also decode '+' as space (form encoding)
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'A'..=b'F' => Some(b - b'A' + 10),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Parse a URL query string into key-value pairs.
/// Both keys and values are percent-decoded.
fn parse_query_string(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let val = parts.next().unwrap_or("");
            Some((percent_decode(key), percent_decode(val)))
        })
        .collect()
}

/// Spawn `cmd` with `args`, discarding all three standard streams. Returns
/// whether the spawn succeeded (the child is detached; we never wait on it).
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn suppress_stdio_and_spawn(cmd: &str, args: &[&str]) -> bool {
    use std::process::Stdio;
    std::process::Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        suppress_stdio_and_spawn("xdg-open", &[url])
    }
    #[cfg(target_os = "macos")]
    {
        suppress_stdio_and_spawn("open", &[url])
    }
    #[cfg(target_os = "windows")]
    {
        suppress_stdio_and_spawn("cmd", &["/c", "start", "", url])
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = url;
        false
    }
}

/// Validate a tenant identifier before interpolating it into Microsoft's
/// OAuth URLs. Accepts:
///   - A well-formed GUID (e.g. `12345678-1234-1234-1234-123456789012`)
///   - The well-known reserved names `common`, `organizations`, `consumers`
///   - A domain like `contoso.onmicrosoft.com` (letters/digits/`.`/`-`/`_`)
///
/// Rejects anything containing `/`, `?`, `#`, whitespace, or other URL
/// punctuation that could redirect / poison the request.
fn is_valid_tenant(s: &str) -> bool {
    if s.is_empty() || s.len() > 256 {
        return false;
    }
    if matches!(s, "common" | "organizations" | "consumers") {
        return true;
    }
    if is_well_formed_guid(s) {
        return true;
    }
    // Domain-shaped: alphanumeric segments separated by dots, optional
    // hyphens / underscores. No `/`, `?`, `#`, ':', '@', whitespace.
    let domain_chars = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_';
    s.chars().all(domain_chars) && s.contains('.')
}

/// True iff `s` is a 36-character hyphenated GUID
/// (8-4-4-4-12, hex elsewhere). Used by acquire_token to validate
/// `BC_CLIENT_ID` before interpolating it into the authorization URL.
fn is_well_formed_guid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (i, &b) in bytes.iter().enumerate() {
        let is_hyphen_pos = matches!(i, 8 | 13 | 18 | 23);
        if is_hyphen_pos {
            if b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod guid_tests {
    use super::{configured_client_id, is_well_formed_guid, DEFAULT_CLIENT_ID};
    use serial_test::serial;
    #[test]
    fn accepts_canonical_aad_app_id() {
        assert!(is_well_formed_guid("ef72a0a7-b59c-4f97-99c8-5b9a2cd3a1b6"));
        // Real BC default client id (uppercase hex)
        assert!(is_well_formed_guid("ABCDEF12-3456-7890-ABCD-EF1234567890"));
    }
    #[test]
    fn rejects_obvious_bad_shapes() {
        assert!(!is_well_formed_guid(""));
        assert!(!is_well_formed_guid("not-a-guid"));
        // 35 chars
        assert!(!is_well_formed_guid("ef72a0a7-b59c-4f97-99c8-5b9a2cd3a1b"));
        // 37 chars
        assert!(!is_well_formed_guid(
            "ef72a0a7-b59c-4f97-99c8-5b9a2cd3a1b66"
        ));
        // wrong hyphen position
        assert!(!is_well_formed_guid(
            "ef72a0a-7b59c-4f97-99c8-5b9a2cd3a1b66"
        ));
        // non-hex
        assert!(!is_well_formed_guid("zf72a0a7-b59c-4f97-99c8-5b9a2cd3a1b6"));
    }

    #[test]
    #[serial]
    fn missing_client_id_uses_default() {
        let previous = std::env::var_os("BC_CLIENT_ID");
        std::env::remove_var("BC_CLIENT_ID");
        assert_eq!(configured_client_id().unwrap(), DEFAULT_CLIENT_ID);
        if let Some(value) = previous {
            std::env::set_var("BC_CLIENT_ID", value);
        }
    }

    #[test]
    #[serial]
    fn invalid_client_id_is_rejected() {
        let previous = std::env::var_os("BC_CLIENT_ID");
        std::env::set_var("BC_CLIENT_ID", "not-a-guid");
        assert!(configured_client_id().is_err());
        match previous {
            Some(value) => std::env::set_var("BC_CLIENT_ID", value),
            None => std::env::remove_var("BC_CLIENT_ID"),
        }
    }
}

#[cfg(test)]
mod tenant_tests {
    use super::is_valid_tenant;

    #[test]
    fn accepts_guid_tenant() {
        assert!(is_valid_tenant("12345678-1234-1234-1234-123456789012"));
    }

    #[test]
    fn accepts_reserved_names() {
        assert!(is_valid_tenant("common"));
        assert!(is_valid_tenant("organizations"));
        assert!(is_valid_tenant("consumers"));
    }

    #[test]
    fn accepts_domain_tenants() {
        assert!(is_valid_tenant("contoso.onmicrosoft.com"));
        assert!(is_valid_tenant("my-org.example.com"));
        assert!(is_valid_tenant("a.b.c.d"));
    }

    #[test]
    fn rejects_empty_and_oversize() {
        assert!(!is_valid_tenant(""));
        let long = "a".repeat(257);
        assert!(!is_valid_tenant(&long));
    }

    #[test]
    fn rejects_url_punctuation() {
        // Negative: anything that could redirect or poison the URL must be rejected.
        assert!(!is_valid_tenant("contoso.com/extra"));
        assert!(!is_valid_tenant("contoso.com?query=evil"));
        assert!(!is_valid_tenant("contoso.com#frag"));
        assert!(!is_valid_tenant("evil@contoso.com"));
        assert!(!is_valid_tenant("contoso .com")); // whitespace
        assert!(!is_valid_tenant("contoso\ncom")); // newline
        assert!(!is_valid_tenant("http://contoso.com"));
        assert!(!is_valid_tenant("///pwned"));
    }

    #[test]
    fn rejects_bare_word_without_dot() {
        // Looks like it could be a single-segment domain but isn't one of the
        // reserved names — reject so a typo of "common" doesn't sneak through.
        assert!(!is_valid_tenant("randomword"));
    }
}

#[cfg(test)]
mod cache_io_tests {
    use super::*;

    fn temp_cache_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("token.json");
        (dir, path)
    }

    fn sample_token() -> TokenResponse {
        TokenResponse {
            access_token: "ACCESS".to_string(),
            refresh_token: Some("REFRESH".to_string()),
            expires_in: 3600,
        }
    }

    #[test]
    fn save_cached_token_writes_complete_json() {
        let (_dir, path) = temp_cache_path();
        save_cached_token(&path, "common", &sample_token());
        let content = std::fs::read_to_string(&path).expect("cache file must exist");
        let parsed: CachedToken = serde_json::from_str(&content).expect("must be valid JSON");
        assert_eq!(parsed.tenant, "common");
        assert_eq!(parsed.access_token, "ACCESS");
        assert_eq!(parsed.refresh_token.as_deref(), Some("REFRESH"));
    }

    #[test]
    fn save_cached_token_cleans_up_tempfile() {
        let (_dir, path) = temp_cache_path();
        save_cached_token(&path, "common", &sample_token());
        let pid = std::process::id();
        let tmp = path.with_file_name(format!("token.json.{pid}.tmp"));
        assert!(
            !tmp.exists(),
            "tempfile {tmp:?} should have been renamed away"
        );
    }

    #[test]
    fn save_cached_token_is_atomic_against_concurrent_readers() {
        // Concurrency: hammer save_cached_token from one thread while a
        // reader thread repeatedly loads the file. The reader must NEVER
        // observe an empty / partial / unparseable file — every successful
        // read must yield a complete CachedToken. F-OPEN-011 regression.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let (dir, path) = temp_cache_path();
        // Prime with one valid token so the reader sees something to parse.
        save_cached_token(&path, "common", &sample_token());

        let stop = Arc::new(AtomicBool::new(false));
        let writer_stop = stop.clone();
        let writer_path = path.clone();
        let writer = thread::spawn(move || {
            for i in 0..200 {
                let tok = TokenResponse {
                    access_token: format!("A{i}"),
                    refresh_token: Some(format!("R{i}")),
                    expires_in: 3600,
                };
                save_cached_token(&writer_path, "common", &tok);
                if writer_stop.load(Ordering::Acquire) {
                    break;
                }
            }
        });

        let reader_path = path.clone();
        let mut partial_reads = 0u32;
        for _ in 0..200 {
            // File transiently missing during rename is fine; ignore Err.
            let content = std::fs::read_to_string(&reader_path).ok();
            if let Some(c) = content {
                if serde_json::from_str::<CachedToken>(&c).is_err() {
                    partial_reads += 1;
                }
            }
            thread::sleep(Duration::from_micros(50));
        }
        stop.store(true, Ordering::Release);
        writer.join().expect("writer panicked");
        drop(dir); // keep dir alive across the spawn

        assert_eq!(
            partial_reads, 0,
            "atomic rename means readers must never see an unparseable file"
        );
    }

    #[test]
    fn invalidate_cached_token_removes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Tenant maps deterministically to a path via token_cache_path,
        // but that path is in ~/.cache. To keep the test hermetic, point
        // at a file we control and exercise the same logic.
        let path = dir.path().join("scratch.json");
        save_cached_token(&path, "contoso.onmicrosoft.com", &sample_token());
        assert!(path.exists());
        std::fs::remove_file(&path).expect("remove ok");
        assert!(!path.exists());
    }

    #[test]
    fn invalidate_cached_token_missing_file_returns_false() {
        // Negative: calling invalidate when no cache exists must not panic
        // and must return false (no work done).
        // We can't easily synthesize a unique tenant that's guaranteed-absent
        // from ~/.cache, but a freshly-tempdir'd path with no save is one.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("never-existed.json");
        assert!(!path.exists());
        match std::fs::remove_file(&path) {
            Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            Ok(_) => panic!("file shouldn't have existed"),
        }
    }
}

#[cfg(test)]
mod zeroize_tests {
    //! F-OPEN-010 — OAuth secrets must be scrubbed from memory, not left in
    //! freed heap allocations for the life of the (long-lived) daemon.
    use super::*;

    #[test]
    fn zeroize_wipes_token_response_secrets() {
        let mut tok = TokenResponse {
            access_token: "super-secret-bearer".to_string(),
            refresh_token: Some("super-secret-refresh".to_string()),
            expires_in: 3600,
        };
        tok.zeroize();
        assert!(
            tok.access_token.is_empty(),
            "access_token must be wiped by zeroize()"
        );
        assert!(
            tok.refresh_token.is_none() || tok.refresh_token.as_deref() == Some(""),
            "refresh_token must be wiped by zeroize()"
        );
        assert_eq!(tok.expires_in, 3600, "expires_in is #[zeroize(skip)]");
    }

    #[test]
    fn zeroize_wipes_cached_token_secrets_and_keeps_metadata() {
        let mut cached = CachedToken {
            access_token: "secret-access".to_string(),
            refresh_token: Some("secret-refresh".to_string()),
            expires_at: 1_700_000_000,
            tenant: "contoso.onmicrosoft.com".to_string(),
        };
        cached.zeroize();
        assert!(cached.access_token.is_empty(), "access_token must be wiped");
        assert!(
            cached.refresh_token.is_none() || cached.refresh_token.as_deref() == Some(""),
            "refresh_token must be wiped"
        );
        // #[zeroize(skip)] fields: tenant/expires_at are non-secret and kept.
        assert_eq!(cached.expires_at, 1_700_000_000);
        assert_eq!(cached.tenant, "contoso.onmicrosoft.com");
    }

    #[test]
    fn token_response_with_no_refresh_token_zeroizes_cleanly() {
        // Device-code / client-credential responses may omit refresh_token;
        // zeroize must not panic on the None variant.
        let mut tok = TokenResponse {
            access_token: "only-access".to_string(),
            refresh_token: None,
            expires_in: 60,
        };
        tok.zeroize();
        assert!(tok.access_token.is_empty());
        assert!(tok.refresh_token.is_none());
    }
}

pub fn token_cache_path(tenant: &str) -> PathBuf {
    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("al-lsp")
        .join("oauth");
    let safe: String = tenant
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    cache_dir.join(format!("{safe}.json"))
}

/// Create a directory with owner-only permissions (0o700 on Unix).
#[cfg(unix)]
fn create_secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
}

#[cfg(not(unix))]
fn create_secure_dir(path: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

// OS secret store (S1)
//
// Persist the OAuth bundle in the platform keyring (Secret Service / Keychain /
// Credential Manager) so the 90-day refresh token is not stored as plaintext on
// disk. Everything here degrades safely: any failure (no backend, locked
// keychain, even a panic in the backend) falls through to the hardened 0o600
// file, so authentication never breaks because a keyring is unavailable.

/// Keyring service name (the `account`/`user` is the tenant).
const KEYRING_SERVICE: &str = "al-lsp-oauth";

/// Whether to use the OS keyring. Disabled under `cfg!(test)` (unit tests assert
/// on file behavior and run without a backend) and via `AL_OAUTH_DISABLE_KEYRING`
/// (lets a user — e.g. on a shared CI account — force the file path).
fn keyring_enabled() -> bool {
    !cfg!(test) && std::env::var_os("AL_OAUTH_DISABLE_KEYRING").is_none()
}

/// Run a keyring operation on a dedicated OS thread.
///
/// keyring's `async-secret-service` backend blocks on an internal async runtime;
/// al-lsp calls token save/load from within tokio, where that nested block-on
/// would panic. A fresh `std::thread` has no ambient runtime, so the backend can
/// manage its own; `join()` additionally turns any panic into `None`, so a
/// misbehaving backend degrades to the file fallback rather than crashing al-lsp.
fn keyring_op<T, F>(f: F) -> Option<T>
where
    F: FnOnce() -> Option<T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name("al-keyring".to_string())
        .spawn(f)
        .ok()?
        .join()
        .ok()
        .flatten()
}

/// Read the token JSON from the OS keyring. `None` = absent / no backend.
fn keyring_get(tenant: &str) -> Option<String> {
    if !keyring_enabled() {
        return None;
    }
    let tenant = tenant.to_string();
    keyring_op(move || {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &tenant).ok()?;
        entry.get_password().ok()
    })
}

/// Store the token JSON in the OS keyring. Returns `true` only on success.
fn keyring_set(tenant: &str, json: &str) -> bool {
    if !keyring_enabled() {
        return false;
    }
    let tenant = tenant.to_string();
    let json = json.to_string();
    keyring_op(move || {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &tenant).ok()?;
        entry.set_password(&json).ok()
    })
    .is_some()
}

/// Delete the OS keyring entry for `tenant`. Returns `true` if one was removed.
fn keyring_delete(tenant: &str) -> bool {
    if !keyring_enabled() {
        return false;
    }
    let tenant = tenant.to_string();
    keyring_op(move || {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &tenant).ok()?;
        match entry.delete_credential() {
            Ok(()) => Some(true),
            Err(keyring::Error::NoEntry) => Some(false),
            Err(_) => None,
        }
    })
    .unwrap_or(false)
}

fn load_cached_token(path: &PathBuf, tenant: &str) -> Option<CachedToken> {
    if let Some(json) = keyring_get(tenant) {
        let json = zeroize::Zeroizing::new(json);
        match serde_json::from_str::<CachedToken>(&json) {
            Ok(tok) => return Some(tok),
            Err(e) => {
                tracing::warn!(tenant, error = %e, "OAuth keyring token corrupt — ignoring");
            }
        }
    }
    // 2. Legacy plaintext file. Distinguish the two failure modes:
    // - file missing / unreadable: expected on first run, debug-level only
    // - file readable but JSON deserialise fails: corrupt or schema drift,
    //   warn so the user knows why their cached token isn't being honoured
    let content = match std::fs::read_to_string(path) {
        Ok(c) => zeroize::Zeroizing::new(c),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "OAuth token cache: read failed");
            return None;
        }
    };
    let tok: CachedToken = match serde_json::from_str(&content) {
        Ok(tok) => tok,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "OAuth token cache: JSON deserialize failed — re-authentication will be required"
            );
            return None;
        }
    };
    // Migrate-on-read: move the secret into the OS keyring and delete the
    // plaintext file, so a token cached by an earlier version stops lingering on
    // disk after the first load. Best-effort — if the keyring is unavailable the
    // file simply stays as the fallback store.
    if keyring_set(tenant, &content) {
        let _ = std::fs::remove_file(path);
        tracing::info!(
            tenant,
            "Migrated OAuth token from plaintext cache to OS keyring"
        );
    }
    Some(tok)
}

/// Persist the OAuth token bundle so subsequent al-lsp invocations don't
/// have to re-run the device-code or browser flow until the refresh token
/// expires.
///
/// **Storage (S1).** Prefers the OS secret store (Secret Service / Keychain /
/// Credential Manager) via the `keyring` crate, so the refresh token (90-day AAD
/// default) does not sit in plaintext on disk. When no backend is available
/// (headless Linux without a Secret Service, CI, or `AL_OAUTH_DISABLE_KEYRING`)
/// it falls back to a hardened file (parent dir 0o700, file 0o600 on Unix;
/// default ACL on Windows). The file fallback has the same trust model as
/// `~/.aws/credentials`; the short-lived access token (≤1h) only matters until
/// the next refresh.
fn save_cached_token(path: &PathBuf, tenant: &str, tok: &TokenResponse) {
    use std::fs::OpenOptions;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let cached = CachedToken {
        access_token: tok.access_token.clone(),
        refresh_token: tok.refresh_token.clone(),
        expires_at: now_unix() + tok.expires_in,
        tenant: tenant.to_string(),
    };
    let json = match serde_json::to_string_pretty(&cached) {
        Ok(j) => zeroize::Zeroizing::new(j),
        Err(e) => {
            warn!(error = %e, "Failed to serialize OAuth token");
            return;
        }
    };

    // 1. Prefer the OS secret store; on success the refresh token never touches
    //    plaintext disk. Remove any legacy file left by an earlier version.
    if keyring_set(tenant, &json) {
        let _ = std::fs::remove_file(path);
        debug!(tenant, "Stored OAuth token in OS keyring");
        return;
    }

    debug!(
        tenant,
        "OS keyring unavailable; caching OAuth token to a 0o600 file"
    );
    if let Some(parent) = path.parent() {
        if let Err(e) = create_secure_dir(parent) {
            warn!(error = %e, path = %parent.display(), "Failed to create secure OAuth cache directory — token will not be cached");
            return;
        }
    }

    // Atomic write: open a per-pid temp file in the same directory, write the
    // full JSON, fsync, then rename into place. rename(2) is atomic on POSIX
    // when source and dest are on the same filesystem, so concurrent
    // `acquire_token` callers for the same tenant can't observe a half-
    // written file *and* can't race on truncate — last-writer-wins still
    // applies but every observer sees a complete, valid token. F-OPEN-011.
    let pid = std::process::id();
    let mut tmp_path = path.clone();
    let tmp_name = match path.file_name() {
        Some(n) => format!("{}.{pid}.tmp", n.to_string_lossy()),
        None => format!("oauth_token.{pid}.tmp"),
    };
    tmp_path.set_file_name(tmp_name);

    #[cfg(unix)]
    let open_result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp_path);
    #[cfg(not(unix))]
    let open_result = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&tmp_path);

    let mut file = match open_result {
        Ok(f) => f,
        Err(e) => {
            warn!(error = %e, path = %tmp_path.display(), "Failed to open OAuth token tempfile");
            return;
        }
    };
    if let Err(e) = file.write_all(json.as_bytes()) {
        warn!(error = %e, "Failed to write OAuth token cache tempfile");
        let _ = std::fs::remove_file(&tmp_path);
        return;
    }
    if let Err(e) = file.sync_all() {
        warn!(error = %e, "Failed to fsync OAuth token cache tempfile");
        // Continue — rename will still complete; the durability guarantee
        // is best-effort and a fresh refresh-flow will recover on next boot.
    }
    drop(file);
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        warn!(error = %e, "Failed to rename OAuth token tempfile into place");
        let _ = std::fs::remove_file(&tmp_path);
    }
}

/// Delete the cached OAuth token for `tenant`, if any. Call this when an
/// upstream API returns 401/403 against a cached access token so the next
/// `acquire_token` call falls through to refresh-then-interactive sign-in
/// instead of re-using the same stale token (F-OPEN-012).
///
/// Returns `true` if a token existed (in the OS keyring or the file) and was
/// removed, `false` if none was present or removal failed (logged at warn level).
pub fn invalidate_cached_token(tenant: &str) -> bool {
    // Clear both stores so a token can't survive in one after the other is wiped.
    let keyring_cleared = keyring_delete(tenant);
    let path = token_cache_path(tenant);
    let file_cleared = match std::fs::remove_file(&path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            warn!(tenant, error = %e, "Failed to delete OAuth token cache file");
            false
        }
    };
    if keyring_cleared || file_cleared {
        info!(tenant, "OAuth token invalidated");
    }
    keyring_cleared || file_cleared
}

/// Return the cached token's `expires_at` (unix secs) for `tenant` if one is
/// cached in either the OS keyring or the legacy file. Read-only — unlike
/// [`load_cached_token`] it never migrates or deletes — for the auth `status`
/// command. Keyring-aware so status is correct after a token migrates off disk.
pub fn cached_token_expiry(tenant: &str) -> Option<u64> {
    if let Some(json) = keyring_get(tenant) {
        let json = zeroize::Zeroizing::new(json);
        if let Ok(tok) = serde_json::from_str::<CachedToken>(&json) {
            return Some(tok.expires_at);
        }
    }
    let path = token_cache_path(tenant);
    let content = zeroize::Zeroizing::new(std::fs::read_to_string(&path).ok()?);
    serde_json::from_str::<CachedToken>(&content)
        .ok()
        .map(|t| t.expires_at)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Parse a token endpoint response: deserialize the body as a `TokenResponse`
/// on success, otherwise surface the OAuth error from the body.
async fn handle_token_response(resp: reqwest::Response) -> Result<TokenResponse, OAuthError> {
    if resp.status().is_success() {
        Ok(resp.json().await?)
    } else {
        let body = resp.text().await.unwrap_or_default();
        parse_token_error(&body)
    }
}

fn parse_token_error<T>(body: &str) -> Result<T, OAuthError> {
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
    fn pkce_verifier_and_challenge() {
        let verifier = generate_code_verifier().expect("random bytes available in test");
        assert!(verifier.len() >= 43); // 32 bytes → 43 base64url chars
        let challenge = pkce_challenge(&verifier);
        assert!(challenge.len() >= 43);
        // Challenge should differ from verifier (it's a hash)
        assert_ne!(verifier, challenge);
    }

    #[test]
    fn base64url_known_value() {
        // SHA256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = Sha256::digest(b"");
        let encoded = base64url_encode(&hash);
        assert_eq!(encoded, "47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU");
    }

    #[test]
    fn percent_encode_roundtrip() {
        let input = "https://api.businesscentral.dynamics.com/.default offline_access";
        let encoded = percent_encode(input);
        assert!(encoded.contains("%3A"));
        assert!(encoded.contains("%20"));
        let decoded = percent_decode(&encoded);
        assert_eq!(decoded, input);
    }

    #[test]
    fn query_string_parsing() {
        let params = parse_query_string("code=abc123&state=xyz&session_state=foo");
        assert_eq!(params.get("code").unwrap(), "abc123");
        assert_eq!(params.get("state").unwrap(), "xyz");
    }

    #[test]
    fn query_string_percent_decodes_values() {
        // Simulate a real OAuth redirect where error_description is percent-encoded
        let params = parse_query_string(
            "error=access_denied&error_description=The%20user%20denied%20access%2E",
        );
        assert_eq!(params.get("error").unwrap(), "access_denied");
        assert_eq!(
            params.get("error_description").unwrap(),
            "The user denied access."
        );
    }

    #[test]
    fn query_string_decodes_plus_as_space() {
        let params = parse_query_string("msg=hello+world");
        assert_eq!(params.get("msg").unwrap(), "hello world");
    }

    #[tokio::test]
    async fn read_http_request_handles_complete_request() {
        use tokio::io::AsyncWriteExt;

        // Set up a loopback pair: write a full HTTP request, read it back
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let write_task = tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = b"GET /?code=AUTH_CODE&state=STATE HTTP/1.1\r\nHost: localhost\r\n\r\n";
            client.write_all(req).await.unwrap();
        });

        let (stream, _) = listener.accept().await.unwrap();
        let result = read_http_request(stream).await.unwrap();

        write_task.await.unwrap();

        assert!(result.contains("GET /?code=AUTH_CODE"));
        assert!(result.contains("\r\n\r\n"));
    }

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

    #[cfg(unix)]
    #[test]
    fn oauth_directory_has_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("oauth");
        create_secure_dir(&cache_dir).unwrap();
        let perms = std::fs::metadata(&cache_dir).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o700,
            "OAuth cache dir must be owner-only"
        );
    }

    #[test]
    fn html_escape_neutralizes_script_injection() {
        // The redirect page renders server-supplied error/description verbatim;
        // a malicious authorization server could inject markup. Every HTML
        // metacharacter must be entity-encoded.
        let out = html_escape("<script>alert('x&y')</script>\"q\"");
        assert!(!out.contains('<'), "raw '<' must not survive: {out}");
        assert!(!out.contains('>'), "raw '>' must not survive: {out}");
        assert_eq!(
            out,
            "&lt;script&gt;alert(&#x27;x&amp;y&#x27;)&lt;/script&gt;&quot;q&quot;"
        );
    }

    #[test]
    fn html_escape_leaves_safe_text_untouched() {
        // Boundary: ordinary text (incl. non-ASCII) passes through verbatim.
        let s = "Signed in: café 123";
        assert_eq!(html_escape(s), s);
    }

    #[test]
    fn percent_decode_handles_truncated_and_invalid_escapes() {
        // A trailing '%' with no following hex digits must be preserved, not
        // panic or eat past the end of the string.
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("a%2"), "a%2");
        // A '%' followed by non-hex is left as a literal '%'.
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("a%2Bb+c"), "a+b c");
    }

    #[test]
    fn generate_random_string_respects_length_and_charset() {
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
        for len in [0usize, 1, 16, 100] {
            let s = generate_random_string(len).expect("entropy available in test");
            assert_eq!(s.len(), len, "exact requested length");
            assert!(
                s.bytes().all(|b| CHARS.contains(&b)),
                "only the URL-safe alphabet may appear: {s:?}"
            );
        }
    }

    #[test]
    fn token_cache_path_sanitizes_unsafe_chars() {
        // A domain tenant keeps its dots replaced by underscores; path
        // separators and other punctuation must never reach the filename.
        let p = token_cache_path("contoso.onmicrosoft.com");
        let name = p.file_name().unwrap().to_string_lossy();
        assert_eq!(name, "contoso_onmicrosoft_com.json");

        // A hostile tenant with slashes/dots must not escape the cache dir.
        let evil = token_cache_path("../../etc/passwd");
        let evil_name = evil.file_name().unwrap().to_string_lossy();
        assert!(
            !evil_name.contains('/') && !evil_name.contains('.') || evil_name.ends_with(".json"),
            "unsafe chars must be folded to '_': {evil_name}"
        );
        assert_eq!(evil_name, "______etc_passwd.json");
        // GUID/hyphen/underscore chars are preserved.
        let guid = token_cache_path("12345678-1234-1234-1234-123456789012");
        assert_eq!(
            guid.file_name().unwrap().to_string_lossy(),
            "12345678-1234-1234-1234-123456789012.json"
        );
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

    #[test]
    fn load_cached_token_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.json");
        assert!(load_cached_token(&path, "test-tenant").is_none());
    }

    #[test]
    fn load_cached_token_returns_none_for_corrupt_json() {
        // A truncated / corrupt cache file must be ignored (forces re-auth),
        // not propagate a parse error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, b"{not valid json").unwrap();
        assert!(load_cached_token(&path, "test-tenant").is_none());
    }

    #[test]
    fn load_cached_token_roundtrips_saved_token() {
        // Positive: a token written by save_cached_token loads back with all
        // fields intact and a future expiry.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.json");
        let tok = TokenResponse {
            access_token: "AAA".into(),
            refresh_token: Some("RRR".into()),
            expires_in: 3600,
        };
        save_cached_token(&path, "common", &tok);
        let loaded = load_cached_token(&path, "test-tenant").expect("must load");
        assert_eq!(loaded.access_token, "AAA");
        assert_eq!(loaded.refresh_token.as_deref(), Some("RRR"));
        assert_eq!(loaded.tenant, "common");
        assert!(
            loaded.expires_at > now_unix(),
            "expiry must be in the future"
        );
    }

    #[test]
    fn keyring_disabled_uses_file_fallback_and_keeps_it() {
        // Under cfg!(test) keyring_enabled() is false, so save/load use the file
        // and migrate-on-read (which only fires on a successful keyring_set) does
        // not run — the hardened-file fallback must stay intact when no backend
        // is available (the exact path headless Linux / CI takes).
        assert!(!keyring_enabled(), "keyring must be disabled in unit tests");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contoso.json");
        let tok = TokenResponse {
            access_token: "AT".into(),
            refresh_token: Some("RT".into()),
            expires_in: 3600,
        };
        save_cached_token(&path, "contoso", &tok);
        assert!(
            path.exists(),
            "keyring disabled -> token must be written to the 0o600 file"
        );
        let loaded = load_cached_token(&path, "contoso").expect("must load from file fallback");
        assert_eq!(loaded.access_token, "AT");
        assert!(
            path.exists(),
            "file must NOT be deleted when no keyring backend is available"
        );
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

    /// Drive `wait_for_auth_callback` against a loopback listener: a client
    /// connects and sends a single GET line carrying `query`, then the callback
    /// processes it. Returns the callback's result.
    async fn run_callback_with_query(
        query: &str,
        expected_state: &str,
    ) -> Result<String, OAuthError> {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let q = query.to_string();
        let writer = tokio::spawn(async move {
            let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
            let req = format!("GET /?{q} HTTP/1.1\r\nHost: localhost\r\n\r\n");
            client.write_all(req.as_bytes()).await.unwrap();
            // Drain the HTTP response so the callback's write_all succeeds.
            use tokio::io::AsyncReadExt;
            let mut sink = Vec::new();
            let _ = client.read_to_end(&mut sink).await;
        });
        let res = wait_for_auth_callback(&listener, expected_state).await;
        writer.await.unwrap();
        res
    }

    #[tokio::test]
    async fn callback_extracts_code_when_state_matches() {
        let code = run_callback_with_query("code=AUTH123&state=GOOD", "GOOD")
            .await
            .expect("happy path yields the code");
        assert_eq!(code, "AUTH123");
    }

    #[tokio::test]
    async fn callback_rejects_state_mismatch() {
        // CSRF defence: a code arriving with the wrong state must be refused.
        let err = run_callback_with_query("code=AUTH123&state=ATTACKER", "EXPECTED")
            .await
            .unwrap_err();
        match err {
            OAuthError::Protocol { error, .. } => assert_eq!(error, "state_mismatch"),
            other => panic!("expected state_mismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn callback_maps_access_denied_to_denied() {
        let err = run_callback_with_query("error=access_denied&error_description=nope", "GOOD")
            .await
            .unwrap_err();
        assert!(
            matches!(err, OAuthError::Denied),
            "access_denied must map to Denied, got {err:?}"
        );
    }

    #[tokio::test]
    async fn callback_surfaces_other_oauth_errors() {
        let err =
            run_callback_with_query("error=invalid_scope&error_description=Bad%20scope", "GOOD")
                .await
                .unwrap_err();
        match err {
            OAuthError::Protocol { error, description } => {
                assert_eq!(error, "invalid_scope");
                assert_eq!(description, "Bad scope", "description is percent-decoded");
            }
            other => panic!("expected Protocol, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn callback_errors_when_code_missing() {
        // No error, state matches, but no code present → missing_code.
        let err = run_callback_with_query("state=GOOD&session_state=x", "GOOD")
            .await
            .unwrap_err();
        match err {
            OAuthError::Protocol { error, .. } => assert_eq!(error, "missing_code"),
            other => panic!("expected missing_code, got {other:?}"),
        }
    }
}
