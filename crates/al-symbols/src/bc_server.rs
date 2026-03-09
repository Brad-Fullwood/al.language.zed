//! BC Dev API client for downloading symbol packages from a running BC instance.
//!
//! This is the standard "Download Symbols" approach used in VS Code.
//! The BC server exposes a `/dev/packages` endpoint that returns `.app` files
//! when authenticated with appropriate credentials.

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{debug, info, warn};

use al_discovery::launch::BcServerConfig;
use al_discovery::AppDependency;

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
    #[error("Cannot construct download URL for this configuration")]
    InvalidConfig,
}

/// Client for downloading symbol packages from a BC instance's Dev API.
pub struct BcServerClient {
    client: reqwest::Client,
    config: BcServerConfig,
}

impl BcServerClient {
    /// Create a new client for the given server configuration.
    pub fn new(config: BcServerConfig) -> Self {
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // On-prem often uses self-signed certs
            .timeout(std::time::Duration::from_secs(300)) // 5 min for large packages
            .build()
            .unwrap_or_default();

        Self { client, config }
    }

    /// Download a single dependency from the BC Dev API.
    ///
    /// Returns the path to the saved `.app` file.
    pub async fn download_one(
        &self,
        dep: &AppDependency,
        dest: &Path,
    ) -> Result<PathBuf, BcServerError> {
        let url = self
            .config
            .dev_packages_url(dep)
            .ok_or(BcServerError::InvalidConfig)?;

        debug!(url = %url, package = %dep.name, "Downloading from BC server");

        let mut request = self.client.get(&url);

        // Add authentication
        request = self.add_auth(request)?;

        let response = request.send().await?;
        let status = response.status().as_u16();

        match status {
            200 => {
                let bytes = response.bytes().await?;

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

    /// Download all dependencies, returning one result per dependency.
    pub async fn download_all(
        &self,
        deps: &[AppDependency],
        dest: &Path,
    ) -> Vec<Result<PathBuf, BcServerError>> {
        let mut results = Vec::new();
        for dep in deps {
            results.push(self.download_one(dep, dest).await);
        }
        results
    }

    /// Add authentication headers to the request based on the server config.
    fn add_auth(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, BcServerError> {
        use al_discovery::launch::AuthMethod;

        match self.config.authentication {
            AuthMethod::UserPassword => {
                let username =
                    std::env::var("BC_USERNAME").map_err(|_| BcServerError::CredentialsRequired)?;
                let password =
                    std::env::var("BC_PASSWORD").map_err(|_| BcServerError::CredentialsRequired)?;
                Ok(request.basic_auth(username, Some(password)))
            }
            AuthMethod::Windows => {
                // Windows auth (NTLM/Negotiate) — works on Windows, limited on Linux
                // For now, try without explicit credentials (system default)
                warn!("Windows authentication may not work from Linux; set BC_USERNAME/BC_PASSWORD for UserPassword auth");
                Ok(request)
            }
            AuthMethod::AAD => {
                // Azure AD / Microsoft Entra ID — requires device code flow
                // Check for a bearer token in env
                if let Ok(token) = std::env::var("BC_ACCESS_TOKEN") {
                    Ok(request.bearer_auth(token))
                } else {
                    warn!("AAD authentication requires BC_ACCESS_TOKEN environment variable (device code flow not yet implemented)");
                    Err(BcServerError::CredentialsRequired)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use al_discovery::launch::{AuthMethod, BcServerConfig, EnvironmentType};

    fn onprem_config() -> BcServerConfig {
        BcServerConfig {
            name: "Test".into(),
            environment_type: EnvironmentType::OnPrem,
            server: Some("https://erp.example.com".into()),
            server_instance: Some("BC".into()),
            port: Some(7049),
            environment_name: None,
            tenant: Some("default".into()),
            authentication: AuthMethod::UserPassword,
        }
    }

    #[test]
    fn test_download_url_construction() {
        let config = onprem_config();
        let dep = AppDependency {
            id: "xxx".into(),
            name: "Base Application".into(),
            publisher: "Microsoft".into(),
            version: "26.5.0.0".into(),
        };

        let url = config.dev_packages_url(&dep).unwrap();
        assert_eq!(
            url,
            "https://erp.example.com:7049/BC/dev/packages?publisher=Microsoft&appName=Base%20Application&versionText=26.5.0.0&tenant=default"
        );
    }

    #[test]
    fn test_app_filename() {
        let dep = AppDependency {
            id: "xxx".into(),
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
}
