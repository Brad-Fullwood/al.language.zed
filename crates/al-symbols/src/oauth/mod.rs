//! OAuth 2.0 authentication for Microsoft Entra ID (Azure AD).
//!
//! Primary flow: Authorization Code + PKCE with local redirect server.
//! Browser opens → user signs in → redirect to localhost → token acquired.
//!
//! Fallback: Device code flow for headless environments.
//!
//! Tokens are cached to disk with refresh token support.

mod cache;
mod encoding;
mod flows;
mod pkce;
mod redirect;
mod validation;

pub use cache::{cached_token_expiry, invalidate_cached_token, token_cache_path};
pub use flows::acquire_token;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

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
/// this value drops: al-lsp runs as a long-lived daemon (30-min
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

#[cfg(test)]
mod zeroize_tests {
    //! OAuth secrets must be scrubbed from memory, not left in
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
