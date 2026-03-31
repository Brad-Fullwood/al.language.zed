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

/// Azure CLI public client ID — first-party Microsoft app that supports
/// auth code + PKCE with localhost redirect for any Microsoft API scope.
const DEFAULT_CLIENT_ID: &str = "04b07795-8ddb-461a-bbee-02f9e1bf7b46";

const BC_SCOPE: &str = "https://api.businesscentral.dynamics.com/.default offline_access";

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
}

/// Successful token response from the token endpoint.
#[derive(Debug, Deserialize, Serialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: u64,
}

/// Cached token on disk.
#[derive(Debug, Serialize, Deserialize)]
struct CachedToken {
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
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
    let client_id = std::env::var("BC_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.into());
    let cache_path = token_cache_path(tenant);

    // 1. Try cached token
    if let Some(cached) = load_cached_token(&cache_path) {
        let now = now_unix();
        if cached.expires_at > now + 60 {
            debug!(tenant, "Using cached BC access token");
            return Ok(cached.access_token);
        }

        // 2. Try refresh
        if let Some(ref refresh) = cached.refresh_token {
            debug!(tenant, "Access token expired, refreshing");
            match refresh_token_flow(client, tenant, &client_id, refresh).await {
                Ok(tok) => {
                    save_cached_token(&cache_path, tenant, &tok);
                    info!(tenant, "Refreshed BC access token");
                    return Ok(tok.access_token);
                }
                Err(e) => {
                    debug!(error = %e, "Refresh failed, doing interactive sign-in");
                }
            }
        }
    }

    // 3. Interactive sign-in
    let tok = interactive_sign_in(client, tenant, &client_id, &on_message).await?;
    save_cached_token(&cache_path, tenant, &tok);
    info!(tenant, "Acquired BC access token");
    Ok(tok.access_token)
}

/// Try browser-based auth code + PKCE flow first, fall back to device code.
async fn interactive_sign_in(
    client: &reqwest::Client,
    tenant: &str,
    client_id: &str,
    on_message: &impl Fn(&str),
) -> Result<TokenResponse, OAuthError> {
    // Try auth code + PKCE with local redirect (opens browser, no code entry)
    match browser_auth_flow(client, tenant, client_id, on_message).await {
        Ok(tok) => return Ok(tok),
        Err(e) => {
            debug!(error = %e, "Browser auth flow failed, trying device code");
            on_message("Browser sign-in failed, falling back to device code...");
        }
    }

    // Fallback: device code flow
    device_code_flow(client, tenant, client_id, on_message).await
}

// ---------------------------------------------------------------------------
// Authorization Code + PKCE flow
// ---------------------------------------------------------------------------

async fn browser_auth_flow(
    client: &reqwest::Client,
    tenant: &str,
    client_id: &str,
    on_message: &impl Fn(&str),
) -> Result<TokenResponse, OAuthError> {
    // Generate PKCE
    let verifier = generate_code_verifier();
    let challenge = pkce_challenge(&verifier);
    let state = generate_random_string(16);

    // Start local server on random port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}");

    // Build authorization URL
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

    // Open browser
    on_message("Opening browser for BC sign-in...");
    if !open_browser(&auth_url) {
        return Err(OAuthError::Protocol {
            error: "no_browser".into(),
            description: "Could not open browser".into(),
        });
    }

    // Wait for redirect (5 minute timeout)
    let code = tokio::time::timeout(
        Duration::from_secs(300),
        wait_for_auth_callback(&listener, &state),
    )
    .await
    .map_err(|_| OAuthError::Expired)??;

    // Exchange auth code for token
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

    if resp.status().is_success() {
        Ok(resp.json().await?)
    } else {
        let body = resp.text().await.unwrap_or_default();
        parse_token_error(&body)
    }
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

/// Wait for the browser redirect to our local server, extract the auth code.
async fn wait_for_auth_callback(
    listener: &tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, OAuthError> {
    use tokio::io::AsyncWriteExt;

    let (stream, _) = listener.accept().await?;

    // Split the stream so read_http_request can consume the read half while we
    // keep the write half for sending the HTTP response back to the browser.
    let (read_half, mut write_half) = tokio::io::split(stream);

    // Read HTTP request — loop until we have the full header (handles TCP segmentation).
    let request = read_http_request(read_half).await?;

    // Parse first line: GET /?code=...&state=... HTTP/1.1
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");

    let query = path.split('?').nth(1).unwrap_or("");
    let params = parse_query_string(query);

    // Send response page
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
    let _ = write_half.write_all(response.as_bytes()).await;
    let _ = write_half.shutdown().await;

    // Check for error
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

    // Verify state
    let state = params.get("state").map(|s| s.as_str()).unwrap_or("");
    if state != expected_state {
        return Err(OAuthError::Protocol {
            error: "state_mismatch".into(),
            description: "CSRF state parameter mismatch".into(),
        });
    }

    // Extract code
    params
        .get("code")
        .cloned()
        .ok_or_else(|| OAuthError::Protocol {
            error: "missing_code".into(),
            description: "No authorization code in redirect".into(),
        })
}

// ---------------------------------------------------------------------------
// Device Code flow (fallback for headless environments)
// ---------------------------------------------------------------------------

/// Response from the `/devicecode` endpoint.
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

/// Token endpoint error response.
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

    // Try to open browser with verification_uri_complete (code pre-filled)
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

    // Poll token endpoint
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

        let body = resp.text().await.unwrap_or_default();
        let err: TokenErrorResponse = match serde_json::from_str(&body) {
            Ok(e) => e,
            Err(_) => return parse_token_error(&body),
        };

        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval += Duration::from_secs(5);
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

// ---------------------------------------------------------------------------
// Refresh token flow
// ---------------------------------------------------------------------------

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

    if resp.status().is_success() {
        Ok(resp.json().await?)
    } else {
        let body = resp.text().await.unwrap_or_default();
        parse_token_error(&body)
    }
}

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

fn generate_code_verifier() -> String {
    let bytes = random_bytes(32);
    base64url_encode(&bytes)
}

fn pkce_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64url_encode(&hash)
}

fn generate_random_string(len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let bytes = random_bytes(len);
    bytes
        .iter()
        .map(|b| CHARS[(*b as usize) % CHARS.len()] as char)
        .collect()
}

/// Generate `n` cryptographically random bytes using the OS entropy source.
/// Uses `getrandom` which works on Linux, macOS, Windows, and WASM.
fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).expect("failed to get random bytes");
    buf
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

// ---------------------------------------------------------------------------
// URL helpers
// ---------------------------------------------------------------------------

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

/// Minimal percent-encoding for URL query parameter values.
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

/// Decode percent-encoded strings from query parameters.
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

// ---------------------------------------------------------------------------
// Browser opening
// ---------------------------------------------------------------------------

fn open_browser(url: &str) -> bool {
    use std::process::Stdio;
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = url;
        false
    }
}

// ---------------------------------------------------------------------------
// Token cache
// ---------------------------------------------------------------------------

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

fn load_cached_token(path: &PathBuf) -> Option<CachedToken> {
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_cached_token(path: &PathBuf, tenant: &str, tok: &TokenResponse) {
    use std::fs::OpenOptions;
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        if let Err(e) = create_secure_dir(parent) {
            warn!(error = %e, path = %parent.display(), "Failed to create secure OAuth cache directory — token will not be cached");
            return;
        }
    }
    let cached = CachedToken {
        access_token: tok.access_token.clone(),
        refresh_token: tok.refresh_token.clone(),
        expires_at: now_unix() + tok.expires_in,
        tenant: tenant.to_string(),
    };
    match serde_json::to_string_pretty(&cached) {
        Ok(json) => {
            // Open/create the file with owner-only read+write (0o600) to protect
            // the OAuth token. Using OpenOptions instead of fs::write() so we can
            // set the mode atomically on creation (Unix only).
            #[cfg(unix)]
            let open_result = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path);
            #[cfg(not(unix))]
            let open_result = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path);

            match open_result {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(json.as_bytes()) {
                        warn!(error = %e, "Failed to write OAuth token cache");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to open OAuth token cache file for writing"),
            }
        }
        Err(e) => warn!(error = %e, "Failed to serialize OAuth token"),
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

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
        let verifier = generate_code_verifier();
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
}
