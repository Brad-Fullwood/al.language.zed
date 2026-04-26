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

use crate::nuget::AppDependency;
use crate::oauth;

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
    cached_token: tokio::sync::OnceCell<String>,
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
            cached_token: tokio::sync::OnceCell::new(),
        })
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
                let filename = format!(
                    "{}_{}.app",
                    dep.publisher.replace(' ', "_"),
                    dep.name.replace(' ', "_")
                );
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
            401 | 403 => Err(BcServerError::AuthenticationFailed {
                status,
                message: response.text().await.unwrap_or_default(),
            }),
            404 => Err(BcServerError::PackageNotFound {
                name: dep.name.clone(),
                version: dep.version.clone(),
            }),
            _ => Err(BcServerError::ServerError {
                status,
                message: response.text().await.unwrap_or_default(),
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

                let sink = self.message_sink.clone();
                let client = self.client.clone();
                let token = self
                    .cached_token
                    .get_or_try_init(|| async {
                        oauth::acquire_token(&client, tenant, &*sink)
                            .await
                            .map_err(BcServerError::OAuth)
                    })
                    .await?;

                Ok(request.bearer_auth(token))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_filename() {
        let dep = AppDependency {
            id: "63ca2034-0ab3-4d4b-be0b-52cd8f9e8e85".into(),
            name: "System Application".into(),
            publisher: "Microsoft".into(),
            version: "26.5.0.0".into(),
        };
        let filename = format!(
            "{}_{}.app",
            dep.publisher.replace(' ', "_"),
            dep.name.replace(' ', "_")
        );
        assert_eq!(filename, "Microsoft_System_Application.app");
    }

    #[test]
    fn test_auth_method_variants() {
        // Verify the local AuthMethod enum covers all three variants
        let _u = AuthMethod::UserPassword;
        let _w = AuthMethod::Windows;
        let _a = AuthMethod::AAD;
    }
}
