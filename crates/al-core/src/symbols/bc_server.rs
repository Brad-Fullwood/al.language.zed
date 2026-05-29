//! BC Dev API client for downloading symbol packages from a running BC instance.
//!
//! This is the standard "Download Symbols" approach used in VS Code.
//! The BC server exposes a `/dev/packages` endpoint that returns `.app` files
//! when authenticated with appropriate credentials.
//!
//! The caller (al-core) is responsible for constructing per-package download URLs
//! using its own `BcServerConfig`. This client handles only HTTP transport and
//! authentication.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tracing::{debug, info, warn};

use super::nuget::AppDependency;
use super::oauth;

/// Authentication method for BC server connections.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthMethod {
    Windows,
    UserPassword,
    AAD,
}

#[derive(Debug, Error)]
pub enum BcServerError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Authentication failed (HTTP {status}): {message}")]
    AuthenticationFailed { status: u16, message: String },
    #[error("Package not found: {name} {version}")]
    PackageNotFound { name: String, version: String },
    #[error("Server error (HTTP {status}): {message}")]
    ServerError { status: u16, message: String },
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("No credentials available. Set BC_USERNAME and BC_PASSWORD environment variables.")]
    CredentialsRequired,
    #[error("OAuth error: {0}")]
    OAuth(#[from] oauth::OAuthError),
}

/// Callback for displaying authentication messages (device code URL, etc.) to the user.
pub type MessageSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Client for downloading symbol packages from a BC instance's Dev API.
///
/// The caller is responsible for constructing the per-dependency download URL
/// (e.g., using `BcServerConfig::dev_packages_url` in al-core) and passing it
/// to [`download_one`].
pub struct BcServerClient {
    client: reqwest::Client,
    auth: AuthMethod,
    tenant: Option<String>,
    message_sink: MessageSink,
    /// Cached access token for the session (avoids re-auth per package).
    ///
    /// Stored behind an `RwLock<Option<String>>` rather than a `OnceCell`
    /// so it can be cleared in-place when a 401/403 reveals the token is
    /// stale. A plain `OnceCell` permanently memoises the first value and
    /// has no way to forget it, which left concurrent downloads re-using a
    /// dead token after the disk cache had already been invalidated
    /// (F-OPEN-013).
    cached_token: tokio::sync::RwLock<Option<String>>,
}

impl BcServerClient {
    /// Create a new client with explicit auth method, TLS setting, and message sink.
    ///
    /// `insecure_tls` disables TLS certificate validation. Only set to `true` for
    /// on-prem BC servers using self-signed certificates. Defaults to `false` for
    /// cloud connections.
    ///
    /// Returns an error if the HTTP client cannot be built (e.g. missing TLS
    /// backend). The error is surfaced rather than silently falling back to a
    /// default client that may not support HTTPS.
    pub fn new(
        auth: AuthMethod,
        tenant: Option<String>,
        message_sink: MessageSink,
        insecure_tls: bool,
    ) -> Result<Self, BcServerError> {
        if insecure_tls {
            warn!(
                "TLS certificate verification DISABLED for BC server connection — \
                 this is unsafe and should only be used against trusted local servers \
                 with self-signed certificates."
            );
        }
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(insecure_tls)
            .timeout(std::time::Duration::from_secs(300)) // 5 min for large packages
            .build()?;

        Ok(Self {
            client,
            auth,
            tenant,
            message_sink,
            cached_token: tokio::sync::RwLock::new(None),
        })
    }

    /// Forget the in-memory cached access token so the next `add_auth` call
    /// re-runs the OAuth acquisition flow (refresh → interactive sign-in).
    ///
    /// Called when a 401/403 reveals the cached token is stale. Without this
    /// the session-level cache would keep handing out the dead token to
    /// concurrent downloads even after the on-disk cache was cleared
    /// (F-OPEN-013).
    async fn reset_cached_token(&self) {
        *self.cached_token.write().await = None;
    }

    /// Download a single dependency from the BC Dev API.
    ///
    /// `url` is the fully-constructed `/dev/packages` URL for this dependency.
    /// The caller (al-core) constructs this URL using `BcServerConfig::dev_packages_url`.
    ///
    /// Returns the path to the saved `.app` file.
    pub async fn download_one(
        &self,
        url: &str,
        dep: &AppDependency,
        dest: &Path,
    ) -> Result<PathBuf, BcServerError> {
        debug!(url = %url, package = %dep.name, "Downloading from BC server");

        let mut request = self.client.get(url);

        // Add authentication
        request = self.add_auth(request).await?;

        let response = request.send().await?;
        let status = response.status().as_u16();

        const MAX_PACKAGE_BYTES: u64 = 200 * 1024 * 1024; // 200 MB
        match status {
            200 => {
                if let Some(content_length) = response.content_length() {
                    if content_length > MAX_PACKAGE_BYTES {
                        return Err(BcServerError::ServerError {
                            status,
                            message: format!(
                                "Package '{name}' Content-Length {content_length} exceeds {max} byte limit — refusing download",
                                name = dep.name,
                                max = MAX_PACKAGE_BYTES,
                            ),
                        });
                    }
                }
                let bytes = response.bytes().await?;

                // Re-check actual size: a server may omit Content-Length or lie
                // about it; the cap also has to apply to the buffered response.
                if bytes.len() as u64 > MAX_PACKAGE_BYTES {
                    return Err(BcServerError::ServerError {
                        status,
                        message: format!(
                            "Package '{name}' body {got} bytes exceeds {max} byte limit",
                            name = dep.name,
                            got = bytes.len(),
                            max = MAX_PACKAGE_BYTES,
                        ),
                    });
                }

                // Save to .alpackages/
                std::fs::create_dir_all(dest)?;
                let filename = package_filename(&dep.publisher, &dep.name);
                let out_path = dest.join(&filename);

                std::fs::write(&out_path, &bytes)?;
                info!(
                    package = %dep.name,
                    path = %out_path.display(),
                    size = bytes.len(),
                    "Downloaded symbol package from BC server"
                );
                Ok(out_path)
            }
            401 | 403 => {
                // The cached OAuth token (if any) is now known-stale —
                // either expired or its grant was revoked. Invalidate it
                // so the next acquire_token call falls through to refresh
                // → interactive sign-in instead of re-presenting the same
                // dead token. F-OPEN-012.
                if let Some(t) = self.tenant.as_deref() {
                    let _ = crate::symbols::oauth::invalidate_cached_token(t);
                }
                // Also clear the session-level in-memory token so concurrent
                // downloads in the same batch don't keep re-using the dead
                // token; the disk-cache invalidation above does not touch it
                // (F-OPEN-013).
                self.reset_cached_token().await;
                Err(BcServerError::AuthenticationFailed {
                    status,
                    // Truncate + scrub: never propagate the full BC error body
                    // (T008 / sec-002). Same helper as bc_client::map_error_response.
                    message: read_error_body_capped(response).await,
                })
            }
            404 => Err(BcServerError::PackageNotFound {
                name: dep.name.clone(),
                version: dep.version.clone(),
            }),
            _ => Err(BcServerError::ServerError {
                status,
                message: read_error_body_capped(response).await,
            }),
        }
    }

    /// Download all dependencies concurrently, given pre-computed URLs for each.
    ///
    /// `url_deps` is a slice of `(url, dep)` pairs. The caller (al-core) is
    /// responsible for pairing each dependency with its corresponding download URL.
    /// All downloads are launched in parallel using `futures::future::join_all`.
    /// Returns one result per entry in the same order as the input slice.
    pub async fn download_all(
        &self,
        url_deps: &[(String, AppDependency)],
        dest: &Path,
    ) -> Vec<Result<PathBuf, BcServerError>> {
        let futures: Vec<_> = url_deps
            .iter()
            .map(|(url, dep)| self.download_one(url, dep, dest))
            .collect();
        futures::future::join_all(futures).await
    }

    /// Add authentication headers to the request based on the auth method.
    async fn add_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, BcServerError> {
        match self.auth {
            AuthMethod::UserPassword => {
                let username =
                    std::env::var("BC_USERNAME").map_err(|_| BcServerError::CredentialsRequired)?;
                let password =
                    std::env::var("BC_PASSWORD").map_err(|_| BcServerError::CredentialsRequired)?;
                Ok(request.basic_auth(username, Some(password)))
            }
            AuthMethod::Windows => {
                // Windows auth (NTLM/Negotiate) — works on Windows, limited on Linux
                warn!("Windows authentication may not work from Linux; set BC_USERNAME/BC_PASSWORD for UserPassword auth");
                Ok(request)
            }
            AuthMethod::AAD => {
                // Azure AD / Microsoft Entra ID — device code flow with token caching
                // Check for explicit env var first (manual override)
                if let Ok(token) = std::env::var("BC_ACCESS_TOKEN") {
                    return Ok(request.bearer_auth(token));
                }

                let tenant = self.tenant.as_deref().unwrap_or("common");

                // Fast path: return the cached token under a read lock.
                if let Some(token) = self.cached_token.read().await.as_ref() {
                    return Ok(request.bearer_auth(token));
                }

                // Slow path: acquire a fresh token under the write lock so
                // concurrent callers serialise on a single sign-in. Re-check
                // after taking the write lock in case another task filled it
                // while we waited.
                let mut guard = self.cached_token.write().await;
                if guard.is_none() {
                    let sink = self.message_sink.clone();
                    let token = oauth::acquire_token(&self.client, tenant, &*sink)
                        .await
                        .map_err(BcServerError::OAuth)?;
                    *guard = Some(token);
                }
                let token = guard.as_ref().expect("token populated above");
                Ok(request.bearer_auth(token))
            }
        }
    }
}

/// Maximum number of bytes to buffer from a non-200 (error) response body
/// before truncating. `sanitize_error_body` ultimately trims to 512 bytes for
/// display, but without an up-front cap a malicious or misbehaving BC server
/// could stream a multi-gigabyte error body that `response.text()` would
/// buffer entirely into memory first (F-OPEN-014).
const MAX_ERROR_BODY_BYTES: u64 = 64 * 1024; // 64 KiB — far more than any real error page

/// Read an error response body, refusing to buffer more than
/// `MAX_ERROR_BODY_BYTES`, then scrub/truncate it via `sanitize_error_body`.
///
/// Mirrors the `read_json_body_capped` hardening pattern in `bc_client`:
/// a body whose advertised `Content-Length` exceeds the cap (or that omits
/// the header entirely) is not buffered at all, since `reqwest`'s default
/// `text()`/`bytes()` would otherwise pull the whole body into memory. A
/// server that lies about a small `Content-Length` and then streams a huge
/// body is still bounded by the post-read size re-check (F-OPEN-014).
async fn read_error_body_capped(response: reqwest::Response) -> String {
    match response.content_length() {
        Some(len) if len <= MAX_ERROR_BODY_BYTES => {
            let bytes = match response.bytes().await {
                Ok(b) => b,
                Err(_) => return String::new(),
            };
            // Defend against a lied Content-Length: only retain the cap.
            let end = (MAX_ERROR_BODY_BYTES as usize).min(bytes.len());
            crate::bc_client::sanitize_error_body(&String::from_utf8_lossy(&bytes[..end]))
        }
        Some(len) => crate::bc_client::sanitize_error_body(&format!(
            "<error body {len} bytes exceeds {MAX_ERROR_BODY_BYTES} byte cap — not read>"
        )),
        None => {
            crate::bc_client::sanitize_error_body("<error body has no Content-Length — not read>")
        }
    }
}

/// Build a safe `.app` filename from a dependency's publisher and name.
///
/// Publisher/name come from `app.json`, which can be authored or corrupted by
/// third parties. Path separators and parent-directory components in those
/// fields would otherwise let `dest.join(filename)` escape the destination
/// directory (`../../evil`, `..\\pwned`, absolute paths, drive letters). We
/// replace every character that isn't ASCII-alphanumeric, `.`, `-` or `_`
/// with `_`, and additionally collapse any `..` sequence so no parent-dir
/// component can survive (F-OPEN-015).
fn package_filename(publisher: &str, name: &str) -> String {
    format!(
        "{}_{}.app",
        sanitize_path_component(publisher),
        sanitize_path_component(name)
    )
}

/// Sanitize a single filename component: keep only ASCII alphanumerics and
/// `.`, `-`, `_`; map everything else (including `/`, `\\`, `:`) to `_`; then
/// neutralise any remaining `..` so the result can never be a parent-dir ref.
fn sanitize_path_component(input: &str) -> String {
    let mut out: String = input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // `..` can only appear via retained dots; collapse it so no component is a
    // parent-directory reference even after the char-class filter above.
    while out.contains("..") {
        out = out.replace("..", "_");
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_filename() {
        let filename = package_filename("Microsoft", "System Application");
        assert_eq!(filename, "Microsoft_System_Application.app");
    }

    #[test]
    fn test_filename_rejects_path_traversal() {
        // Parent-directory components and path separators in publisher/name
        // must not survive into the filename (F-OPEN-015).
        let filename = package_filename("../../evil", "..\\pwned");
        assert!(!filename.contains(".."), "got {filename}");
        assert!(!filename.contains('/'), "got {filename}");
        assert!(!filename.contains('\\'), "got {filename}");
        assert!(filename.ends_with(".app"));

        // The joined path stays inside the destination directory.
        let dest = Path::new("/tmp/alpackages");
        let joined = dest.join(&filename);
        assert!(
            joined.starts_with(dest),
            "filename escaped dest: {}",
            joined.display()
        );
        // No `..` component should appear in the joined path.
        assert!(
            !joined
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir)),
            "joined path has a ParentDir component: {}",
            joined.display()
        );
    }

    #[test]
    fn test_sanitize_path_component_keeps_safe_chars() {
        assert_eq!(sanitize_path_component("Foo.Bar-Baz_1"), "Foo.Bar-Baz_1");
        assert_eq!(sanitize_path_component("a/b\\c:d"), "a_b_c_d");
        assert_eq!(sanitize_path_component(".."), "_");
        assert_eq!(sanitize_path_component(""), "_");
        // A drive-letter style prefix is neutralised.
        assert_eq!(sanitize_path_component("C:\\x"), "C__x");
    }

    #[test]
    fn test_auth_method_variants() {
        // Verify the local AuthMethod enum covers all three variants
        let _u = AuthMethod::UserPassword;
        let _w = AuthMethod::Windows;
        let _a = AuthMethod::AAD;
    }

    #[tokio::test]
    async fn test_reset_cached_token_clears_in_memory_token() {
        // F-OPEN-013: a 401/403 must be able to forget the session-level
        // in-memory token so concurrent downloads re-authenticate instead of
        // re-using the dead token. With the old `OnceCell` this was
        // impossible. Here we seed the cache and verify the reset clears it.
        let client = BcServerClient::new(
            AuthMethod::AAD,
            Some("contoso".into()),
            Arc::new(|_| {}),
            false,
        )
        .expect("client builds");

        *client.cached_token.write().await = Some("stale-token".into());
        assert!(client.cached_token.read().await.is_some());

        client.reset_cached_token().await;
        assert!(
            client.cached_token.read().await.is_none(),
            "reset_cached_token must clear the in-memory token"
        );
    }
}
