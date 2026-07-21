//! BC Dev API client for downloading symbol packages from a running BC instance.
//!
//! This is the standard "Download Symbols" approach used in VS Code.
//! The BC server exposes a `/dev/packages` endpoint that returns `.app` files
//! when authenticated with appropriate credentials.
//!
//! The caller (al-core) is responsible for constructing per-package download URLs
//! using its own `BcServerConfig`. This client handles only HTTP transport and
//! authentication.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;
use tracing::{debug, info, warn};

use super::nuget::AppDependency;
use super::oauth;

pub use al_types::AuthMethod;

const MAX_PACKAGE_BYTES: u64 = 200 * 1024 * 1024;

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
    /// dead token after the disk cache had already been invalidated.
    cached_token: tokio::sync::RwLock<Option<String>>,
    /// Set once a 401/403 is seen while the `BC_ACCESS_TOKEN` env var was the
    /// auth source. The env var is a manual one-shot override; if it is stale
    /// there is no way to refresh it in-process, and re-presenting it on every
    /// retry just burns requests against the same dead credential. Once this
    /// flag is set, `add_auth` stops honouring the env var and falls through to
    /// the OAuth acquisition flow, which *can* recover.
    stale_env_token: std::sync::atomic::AtomicBool,
    /// Per-output-path locks prevent duplicate concurrent requests from racing
    /// writes to the same package file.
    package_locks: std::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    /// Successful downloads in this client session. Checked again after taking
    /// the per-package lock so concurrent duplicates reuse the first result.
    completed_downloads: std::sync::Mutex<HashMap<PathBuf, PathBuf>>,
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
            al_bc::http_auth::warn_insecure_tls("BC server connection");
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
            package_locks: std::sync::Mutex::new(HashMap::new()),
            completed_downloads: std::sync::Mutex::new(HashMap::new()),
        })
    }

    fn lock_for(&self, output_path: &Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .package_locks
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Arc::clone(
            locks
                .entry(output_path.to_path_buf())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Forget the in-memory cached access token so the next `add_auth` call
    /// re-runs the OAuth acquisition flow (refresh → interactive sign-in).
    ///
    /// Called when a 401/403 reveals the cached token is stale. Without this
    /// the session-level cache would keep handing out the dead token to
    /// concurrent downloads even after the on-disk cache was cleared.
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
        let output_path = dest.join(package_filename(&dep.publisher, &dep.name, &dep.version));
        let lock = self.lock_for(&output_path);
        let _guard = lock.lock().await;

        if let Some(completed) = self
            .completed_downloads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&output_path)
            .filter(|path| path.is_file())
            .cloned()
        {
            debug!(package = %dep.name, path = %completed.display(), "Reusing completed BC package download");
            return Ok(completed);
        }

        let downloaded = self
            .download_one_locked(url, dep, dest, &output_path)
            .await?;
        self.completed_downloads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(output_path, downloaded.clone());
        Ok(downloaded)
    }

    async fn download_one_locked(
        &self,
        url: &str,
        dep: &AppDependency,
        dest: &Path,
        output_path: &Path,
    ) -> Result<PathBuf, BcServerError> {
        debug!(url = %url, package = %dep.name, "Downloading from BC server");
        let response = self.send_with_retry(url, dep).await?;
        let status = response.status().as_u16();

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
                let size = stream_package_to_file(response, dep, dest, output_path).await?;
                info!(
                    package = %dep.name,
                    path = %output_path.display(),
                    size,
                    "Downloaded symbol package from BC server"
                );
                Ok(output_path.to_path_buf())
            }
            401 | 403 => {
                // The cached OAuth token (if any) is now known-stale —
                // either expired or its grant was revoked. Invalidate it
                // so the next acquire_token call falls through to refresh
                // → interactive sign-in instead of re-presenting the same
                // dead token.
                if let Some(t) = self.tenant.as_deref() {
                    let _ = crate::oauth::invalidate_cached_token(t);
                }
                // Also clear the session-level in-memory token so concurrent
                // downloads in the same batch don't keep re-using the dead
                // token; the disk-cache invalidation above does not touch it.
                self.reset_cached_token().await;
                // If the auth came from the BC_ACCESS_TOKEN env var, mark it
                // stale so subsequent retries fall through to the OAuth flow
                // instead of re-presenting the same dead token on every call.
                if matches!(self.auth, AuthMethod::AAD) && std::env::var("BC_ACCESS_TOKEN").is_ok()
                {
                    self.stale_env_token
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
                Err(BcServerError::AuthenticationFailed {
                    status,
                    // Truncate and scrub: never propagate the full BC error body.
                    // This is the same helper used by `bc_client::map_error_response`.
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

    async fn send_with_retry(
        &self,
        url: &str,
        dep: &AppDependency,
    ) -> Result<reqwest::Response, BcServerError> {
        const MAX_ATTEMPTS: usize = 3;
        for attempt in 0..MAX_ATTEMPTS {
            let request = self.add_auth(self.client.get(url)).await?;
            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    let transient = matches!(status, 429 | 502 | 503 | 504);
                    if transient && attempt + 1 < MAX_ATTEMPTS {
                        let retry_after = response
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.parse::<u64>().ok())
                            .map(std::time::Duration::from_secs)
                            .unwrap_or_else(|| {
                                std::time::Duration::from_millis(200 * (1u64 << attempt))
                            })
                            .min(std::time::Duration::from_secs(30));
                        warn!(
                            package = %dep.name,
                            status,
                            attempt = attempt + 1,
                            retry_ms = retry_after.as_millis() as u64,
                            "Transient BC symbol download failure; retrying"
                        );
                        drop(response);
                        tokio::time::sleep(retry_after).await;
                        continue;
                    }
                    return Ok(response);
                }
                Err(error) if attempt + 1 < MAX_ATTEMPTS => {
                    let retry_after = std::time::Duration::from_millis(200 * (1u64 << attempt));
                    warn!(
                        package = %dep.name,
                        %error,
                        attempt = attempt + 1,
                        retry_ms = retry_after.as_millis() as u64,
                        "BC symbol download transport failure; retrying"
                    );
                    tokio::time::sleep(retry_after).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!("retry loop always returns on its final attempt")
    }

    /// Download all dependencies concurrently, given pre-computed URLs for each.
    ///
    /// `url_deps` is a slice of `(url, dep)` pairs. The caller (al-core) is
    /// responsible for pairing each dependency with its corresponding download URL.
    /// Downloads run concurrently behind a bounded semaphore.
    /// Returns one result per entry in the same order as the input slice.
    pub async fn download_all(
        &self,
        url_deps: &[(String, AppDependency)],
        dest: &Path,
    ) -> Vec<Result<PathBuf, BcServerError>> {
        const MAX_CONCURRENT_DOWNLOADS: usize = 4;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS));
        let futures: Vec<_> = url_deps
            .iter()
            .map(|(url, dep)| {
                let semaphore = Arc::clone(&semaphore);
                async move {
                    let _permit = semaphore
                        .acquire()
                        .await
                        .expect("download semaphore is never closed");
                    self.download_one(url, dep, dest).await
                }
            })
            .collect();
        futures::future::join_all(futures).await
    }

    /// Whether the `BC_ACCESS_TOKEN` env-var override should still be honoured.
    ///
    /// Returns `false` once a 401/403 has flagged the env token as stale, so
    /// `add_auth` falls through to the OAuth acquisition flow instead of
    /// re-presenting a dead credential on every retry.
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
                // credential on every retry.
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

async fn stream_package_to_file(
    mut response: reqwest::Response,
    dep: &AppDependency,
    dest: &Path,
    output_path: &Path,
) -> Result<u64, BcServerError> {
    use tokio::io::AsyncWriteExt;

    tokio::fs::create_dir_all(dest).await?;
    static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let filename = output_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package.app");
    let temp_path = dest.join(format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));

    let write_result: Result<u64, BcServerError> = async {
        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut received = 0u64;
        let mut magic = [0u8; 4];
        let mut magic_len = 0usize;

        while let Some(chunk) = response.chunk().await? {
            received = received.checked_add(chunk.len() as u64).ok_or_else(|| {
                BcServerError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "package response size overflow",
                ))
            })?;
            if received > MAX_PACKAGE_BYTES {
                return Err(BcServerError::ServerError {
                    status: 200,
                    message: format!(
                        "Package '{name}' body exceeds {MAX_PACKAGE_BYTES} byte limit",
                        name = dep.name
                    ),
                });
            }
            if magic_len < magic.len() {
                let take = (magic.len() - magic_len).min(chunk.len());
                magic[magic_len..magic_len + take].copy_from_slice(&chunk[..take]);
                magic_len += take;
            }
            file.write_all(&chunk).await?;
        }

        if magic_len != magic.len() || &magic != b"NAVX" {
            return Err(BcServerError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "BC server returned non-NAVX content for package '{}'",
                    dep.name
                ),
            )));
        }
        file.flush().await?;
        file.sync_all().await?;
        Ok(received)
    }
    .await;

    let received = match write_result {
        Ok(received) => received,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error);
        }
    };

    if let Err(error) = crate::app_reader::read_app_manifest_file(&temp_path) {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(BcServerError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "BC server returned an invalid .app for package '{}': {error}",
                dep.name
            ),
        )));
    }

    if let Err(error) = tokio::fs::rename(&temp_path, output_path).await {
        // Windows cannot atomically replace an existing destination. The full
        // temp file is durable at this point, so fall back to remove+rename.
        if tokio::fs::try_exists(output_path).await.unwrap_or(false) {
            tokio::fs::remove_file(output_path).await?;
            tokio::fs::rename(&temp_path, output_path).await?;
        } else {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error.into());
        }
    }

    Ok(received)
}

/// Read an error response body with a Content-Length cap before buffering,
/// then scrub/truncate it via `sanitize_error_body`.
///
/// Thin wrapper over the shared `bc_client::read_error_body_capped` helper so
/// every BC client path (bc_server, profiling, snapshot, test_runner) enforces
/// the same 64 KiB pre-read cap.
async fn read_error_body_capped(response: reqwest::Response) -> String {
    al_bc::bc_client::read_error_body_capped(response).await
}

/// Build a safe `.app` filename from a dependency's publisher and name.
///
/// Publisher/name come from `app.json`, which can be authored or corrupted by
/// third parties. Path separators and parent-directory components in those
/// fields would otherwise let `dest.join(filename)` escape the destination
/// directory (`../../evil`, `..\\pwned`, absolute paths, drive letters). We
/// replace every character that isn't ASCII-alphanumeric, `.`, `-` or `_`
/// with `_`, and additionally collapse any `..` sequence so no parent-dir
/// component can survive.
fn package_filename(publisher: &str, name: &str, version: &str) -> String {
    format!(
        "{}_{}_{}.app",
        sanitize_path_component(publisher),
        sanitize_path_component(name),
        sanitize_path_component(version)
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
        let filename = package_filename("Microsoft", "System Application", "27.4.0.0");
        assert_eq!(filename, "Microsoft_System_Application_27.4.0.0.app");
    }

    #[test]
    fn test_filename_rejects_path_traversal() {
        // Parent-directory components and path separators in publisher/name
        // must not survive into the filename.
        let filename = package_filename("../../evil", "..\\pwned", "1.0/../../bad");
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
        // through to the OAuth flow and recover.
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

    fn valid_app_bytes(name: &str) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let manifest = format!(
            r#"<?xml version="1.0"?><Package><App Id="00000000-0000-0000-0000-000000000001" Name="{name}" Publisher="Test" Version="1.0.0.0" /></Package>"#
        );
        let mut data = Vec::from(&b"NAVX"[..]);
        data.resize(40, 0);
        let mut zip_data = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut zip_data));
            let options = SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(b"{}").unwrap();
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_data);
        data
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
        let body = valid_app_bytes("System Application");
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_bytes(body.clone()),
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

        assert_eq!(out, tmp.join("Microsoft_System_Application_1.0.0.0.app"));
        assert_eq!(std::fs::read(&out).unwrap(), body);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn chunked_download_is_streamed_without_content_length() {
        let body = valid_app_bytes("Chunked");
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Transfer-Encoding", "chunked")
                    .set_body_bytes(body.clone()),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();

        let out = no_auth_client()
            .download_one(&server.uri(), &dep("Chunked", "Pub", "1.0.0.0"), tmp.path())
            .await
            .expect("chunked package should stream successfully");

        assert_eq!(std::fs::read(out).unwrap(), body);
    }

    #[tokio::test]
    async fn non_navx_response_is_rejected_without_publishing_partial_file() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("login page"))
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let dependency = dep("Bad", "Pub", "1.0.0.0");

        let error = no_auth_client()
            .download_one(&server.uri(), &dependency, tmp.path())
            .await
            .expect_err("HTML/error content must not be published as an .app");

        assert!(error.to_string().contains("non-NAVX"));
        assert!(
            std::fs::read_dir(tmp.path()).unwrap().next().is_none(),
            "failed download must leave neither final nor temp files"
        );
    }

    #[tokio::test]
    async fn concurrent_same_package_downloads_share_one_request() {
        let body = valid_app_bytes("Shared");
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(50))
                    .set_body_bytes(body),
            )
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().unwrap();
        let client = Arc::new(no_auth_client());
        let dependency = dep("Shared", "Pub", "1.0.0.0");
        let url = server.uri();

        let first = {
            let client = Arc::clone(&client);
            let dependency = dependency.clone();
            let dest = tmp.path().to_path_buf();
            let url = url.clone();
            tokio::spawn(async move { client.download_one(&url, &dependency, &dest).await })
        };
        let second = {
            let client = Arc::clone(&client);
            let dependency = dependency.clone();
            let dest = tmp.path().to_path_buf();
            tokio::spawn(async move { client.download_one(&url, &dependency, &dest).await })
        };

        let first_path = first.await.unwrap().unwrap();
        let second_path = second.await.unwrap().unwrap();
        assert_eq!(first_path, second_path);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
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
        let body = valid_app_bytes("Good");
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/ok"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_bytes(body),
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
        // a 401/403 must be able to forget the session-level
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
