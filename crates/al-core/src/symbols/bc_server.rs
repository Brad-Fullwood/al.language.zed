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
    /// Set once a 401/403 is seen while the `BC_ACCESS_TOKEN` env var was the
    /// auth source. The env var is a manual one-shot override; if it is stale
    /// there is no way to refresh it in-process, and re-presenting it on every
    /// retry just burns requests against the same dead credential. Once this
    /// flag is set, `add_auth` stops honouring the env var and falls through to
    /// the OAuth acquisition flow, which *can* recover (F-OPEN-129).
    stale_env_token: std::sync::atomic::AtomicBool,
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
            crate::http_auth::warn_insecure_tls("BC server connection");
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
            stale_env_token: std::sync::atomic::AtomicBool::new(false),
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
                // If the auth came from the BC_ACCESS_TOKEN env var, mark it
                // stale so subsequent retries fall through to the OAuth flow
                // instead of re-presenting the same dead token on every call
                // (F-OPEN-129).
                if matches!(self.auth, AuthMethod::AAD) && std::env::var("BC_ACCESS_TOKEN").is_ok()
                {
                    self.stale_env_token
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
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

    /// Whether the `BC_ACCESS_TOKEN` env-var override should still be honoured.
    ///
    /// Returns `false` once a 401/403 has flagged the env token as stale, so
    /// `add_auth` falls through to the OAuth acquisition flow instead of
    /// re-presenting a dead credential on every retry (F-OPEN-129).
    fn env_token_active(&self) -> bool {
        !self
            .stale_env_token
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Mark the `BC_ACCESS_TOKEN` env-var override as stale (test seam mirror of
    /// the 401/403 handler).
    #[cfg(test)]
    fn mark_env_token_stale(&self) {
        self.stale_env_token
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

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
                // Check for explicit env var first (manual override). Skip it
                // once a 401/403 has flagged that env token as stale, so we can
                // recover via the OAuth flow instead of re-presenting a dead
                // credential on every retry (F-OPEN-129).
                if self.env_token_active() {
                    if let Ok(token) = std::env::var("BC_ACCESS_TOKEN") {
                        return Ok(request.bearer_auth(token));
                    }
                }

                let tenant = self.tenant.as_deref().unwrap_or("common");

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

/// Read an error response body with a Content-Length cap before buffering,
/// then scrub/truncate it via `sanitize_error_body`.
///
/// Thin wrapper over the shared `bc_client::read_error_body_capped` helper so
/// every BC client path (bc_server, profiling, snapshot, test_runner) enforces
/// the same 64 KiB pre-read cap (F-OPEN-014).
async fn read_error_body_capped(response: reqwest::Response) -> String {
    crate::bc_client::read_error_body_capped(response).await
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

        let dest = Path::new("/tmp/alpackages");
        let joined = dest.join(&filename);
        assert!(
            joined.starts_with(dest),
            "filename escaped dest: {}",
            joined.display()
        );
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
    fn test_stale_env_token_disables_env_override() {
        // A fresh client honours the BC_ACCESS_TOKEN env override; once a
        // 401/403 marks it stale, the override is skipped so add_auth can fall
        // through to the OAuth flow and recover (F-OPEN-129).
        let sink: MessageSink = Arc::new(|_: &str| {});
        let client =
            BcServerClient::new(AuthMethod::AAD, Some("tenant".to_string()), sink, false).unwrap();

        assert!(
            client.env_token_active(),
            "env override should be active on a fresh client"
        );

        client.mark_env_token_stale();

        assert!(
            !client.env_token_active(),
            "env override must be skipped once flagged stale"
        );
    }

    #[test]
    fn test_auth_method_variants() {
        // Verify the local AuthMethod enum covers all three variants
        let _u = AuthMethod::UserPassword;
        let _w = AuthMethod::Windows;
        let _a = AuthMethod::AAD;
    }

    fn dep(name: &str, publisher: &str, version: &str) -> AppDependency {
        AppDependency {
            id: "00000000-0000-0000-0000-000000000000".into(),
            name: name.into(),
            publisher: publisher.into(),
            version: version.into(),
        }
    }

    /// A client that never authenticates (Windows auth adds no headers), so
    /// `download_one` can be driven against a mock server without touching the
    /// OAuth flow or env vars.
    fn no_auth_client() -> BcServerClient {
        BcServerClient::new(AuthMethod::Windows, None, Arc::new(|_| {}), false)
            .expect("client builds")
    }

    #[tokio::test]
    async fn download_one_200_writes_app_file_to_dest() {
        let body = b"AL-PACKAGE-BYTES";
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_bytes(body.to_vec()),
            )
            .mount(&server)
            .await;

        let tmp = std::env::temp_dir().join(format!("bc_server_dl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dep = dep("System Application", "Microsoft", "1.0.0.0");

        let out = no_auth_client()
            .download_one(&server.uri(), &dep, &tmp)
            .await
            .expect("download succeeds");

        assert_eq!(out, tmp.join("Microsoft_System_Application.app"));
        assert_eq!(std::fs::read(&out).unwrap(), body);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn download_one_404_maps_to_package_not_found() {
        // A 404 must surface as PackageNotFound carrying the dep name+version,
        // not a generic ServerError.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let dep = dep("Missing", "Pub", "2.3.4.5");
        let err = no_auth_client()
            .download_one(&server.uri(), &dep, Path::new("/tmp/nope"))
            .await
            .expect_err("404 must error");

        match err {
            BcServerError::PackageNotFound { name, version } => {
                assert_eq!(name, "Missing");
                assert_eq!(version, "2.3.4.5");
            }
            other => panic!("expected PackageNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn download_one_503_maps_to_server_error_with_status() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(503).set_body_string("boom"))
            .mount(&server)
            .await;

        let dep = dep("Pkg", "Pub", "1.0.0.0");
        let err = no_auth_client()
            .download_one(&server.uri(), &dep, Path::new("/tmp/nope"))
            .await
            .expect_err("503 must error");

        match err {
            BcServerError::ServerError { status, .. } => assert_eq!(status, 503),
            other => panic!("expected ServerError 503, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn download_one_401_maps_to_authentication_failed() {
        // A 401 is mapped to AuthenticationFailed carrying the status. (Windows
        // auth here means no env-token side effects fire.)
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(401).set_body_string("denied"))
            .mount(&server)
            .await;

        let dep = dep("Pkg", "Pub", "1.0.0.0");
        let err = no_auth_client()
            .download_one(&server.uri(), &dep, Path::new("/tmp/nope"))
            .await
            .expect_err("401 must error");

        match err {
            BcServerError::AuthenticationFailed { status, .. } => assert_eq!(status, 401),
            other => panic!("expected AuthenticationFailed 401, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn download_all_preserves_order_and_per_entry_results() {
        let server = wiremock::MockServer::start().await;
        let body = b"ok-bytes";
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/ok"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_bytes(body.to_vec()),
            )
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/missing"))
            .respond_with(wiremock::ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let tmp = std::env::temp_dir().join(format!("bc_server_all_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let url_deps = vec![
            (
                format!("{}/ok", server.uri()),
                dep("Good", "Pub", "1.0.0.0"),
            ),
            (
                format!("{}/missing", server.uri()),
                dep("Bad", "Pub", "9.9.9.9"),
            ),
        ];

        let results = no_auth_client().download_all(&url_deps, &tmp).await;

        assert_eq!(results.len(), 2);
        assert!(results[0].is_ok(), "first entry should succeed");
        assert!(
            matches!(results[1], Err(BcServerError::PackageNotFound { .. })),
            "second entry should be PackageNotFound, got {:?}",
            results[1]
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn add_auth_userpassword_missing_creds_errors() {
        std::env::remove_var("BC_USERNAME");
        std::env::remove_var("BC_PASSWORD");
        let client = BcServerClient::new(AuthMethod::UserPassword, None, Arc::new(|_| {}), false)
            .expect("client builds");
        let req = client.client.get("http://example.invalid/dev/packages");

        let err = client
            .add_auth(req)
            .await
            .expect_err("missing creds must error");
        assert!(matches!(err, BcServerError::CredentialsRequired));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn add_auth_userpassword_with_creds_succeeds() {
        std::env::set_var("BC_USERNAME", "alice");
        std::env::set_var("BC_PASSWORD", "secret");
        let client = BcServerClient::new(AuthMethod::UserPassword, None, Arc::new(|_| {}), false)
            .expect("client builds");
        let req = client.client.get("http://example.invalid/dev/packages");

        let result = client.add_auth(req).await;
        assert!(result.is_ok(), "creds present should yield Ok");

        std::env::remove_var("BC_USERNAME");
        std::env::remove_var("BC_PASSWORD");
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn add_auth_aad_uses_env_access_token() {
        std::env::set_var("BC_ACCESS_TOKEN", "env-token-123");
        let client = BcServerClient::new(
            AuthMethod::AAD,
            Some("contoso".into()),
            Arc::new(|_| {}),
            false,
        )
        .expect("client builds");
        let req = client.client.get("http://example.invalid/dev/packages");
        assert!(client.add_auth(req).await.is_ok());

        client.mark_env_token_stale();
        assert!(!client.env_token_active());
        std::env::remove_var("BC_ACCESS_TOKEN");
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
