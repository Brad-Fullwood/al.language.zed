//! NuGet v3 client for downloading AL symbol packages.
//!
//! BC NuGet packages follow the pattern:
//! - Package ID: `{publisher}.{name}.symbols.{app_id}` (lowercase, spaces→dots)
//! - The `.nupkg` is a ZIP containing the `.app` file

use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{debug, info, warn};

#[derive(Debug, Error)]
pub enum NuGetError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("NuGet service index missing PackageBaseAddress resource")]
    NoBaseAddress,
    #[error("No versions found for package {0}")]
    NoVersions(String),
    #[error("Version {version} not found for package {id}")]
    VersionNotFound { id: String, version: String },
    #[error("No .app file found in nupkg")]
    NoAppInNupkg,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ZIP error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

/// A reference to a NuGet package to download.
#[derive(Debug, Clone)]
pub struct PackageRef {
    /// NuGet package ID (e.g., "microsoft.application.symbols.437dbf0e-84ff-417a-965d-ed2bb9650972")
    pub id: String,
    /// Desired version (e.g., "24.0.12345.0"), or None for latest.
    pub version: Option<String>,
    /// Original app name (for display purposes).
    pub display_name: String,
}

/// A NuGet feed configuration.
#[derive(Debug, Clone)]
pub struct NuGetFeed {
    /// The NuGet v3 service index URL.
    pub index_url: String,
}

impl Default for NuGetFeed {
    fn default() -> Self {
        Self {
            index_url: "https://api.nuget.org/v3/index.json".to_string(),
        }
    }
}

/// A dependency from app.json.
#[derive(Debug, Clone)]
pub struct AppDependency {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

/// Resolve app.json dependencies into NuGet PackageRefs.
///
/// BC NuGet package IDs follow the pattern:
/// `{publisher}.{name}.symbols.{id}` (all lowercase, spaces→dots)
pub fn resolve_dependencies(deps: &[AppDependency]) -> Vec<PackageRef> {
    deps.iter()
        .map(|dep| {
            let id = format!(
                "{}.{}.symbols.{}",
                dep.publisher.to_lowercase().replace(' ', "."),
                dep.name.to_lowercase().replace(' ', "."),
                dep.id.to_lowercase()
            );
            PackageRef {
                id,
                version: Some(dep.version.clone()),
                display_name: dep.name.clone(),
            }
        })
        .collect()
}

// -- NuGet v3 service index types --

#[derive(Debug, serde::Deserialize)]
struct ServiceIndex {
    resources: Vec<ServiceResource>,
}

#[derive(Debug, serde::Deserialize)]
struct ServiceResource {
    #[serde(rename = "@id")]
    id: String,
    #[serde(rename = "@type")]
    resource_type: String,
}

#[derive(Debug, serde::Deserialize)]
struct VersionIndex {
    versions: Vec<String>,
}

/// A NuGet client that manages feeds and downloads symbol packages.
pub struct NuGetClient {
    client: reqwest::Client,
    feeds: Vec<NuGetFeed>,
}

impl NuGetClient {
    /// Create a new NuGet client with the given feeds.
    pub fn new(feeds: Vec<NuGetFeed>) -> Self {
        Self {
            client: reqwest::Client::new(),
            feeds,
        }
    }

    /// Download a single package, trying each feed in order until one succeeds.
    ///
    /// Returns the path to the extracted .app file.
    pub async fn download(&self, pkg: &PackageRef, dest: &Path) -> Result<PathBuf, NuGetError> {
        let mut last_err = None;
        for feed in &self.feeds {
            match download(&self.client, feed, pkg, dest).await {
                Ok(path) => return Ok(path),
                Err(e) => {
                    tracing::debug!(feed = %feed.index_url, error = %e, "Feed failed, trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(NuGetError::NoBaseAddress))
    }

    /// Download all dependencies, trying each feed in order for each package.
    ///
    /// Returns one result per dependency.
    pub async fn download_all(
        &self,
        deps: &[AppDependency],
        dest: &Path,
    ) -> Vec<Result<PathBuf, NuGetError>> {
        let refs = resolve_dependencies(deps);
        let mut results = Vec::new();
        for pkg_ref in &refs {
            results.push(self.download(pkg_ref, dest).await);
        }
        results
    }
}

/// Download a single package from a NuGet feed and extract the .app file.
///
/// Returns the path to the extracted .app file.
async fn download(
    client: &reqwest::Client,
    feed: &NuGetFeed,
    pkg: &PackageRef,
    dest: &Path,
) -> Result<PathBuf, NuGetError> {
    // 1. Get service index
    let base_url = get_package_base_address(client, &feed.index_url).await?;

    let id_lower = pkg.id.to_lowercase();

    // 2. Get version list
    let version_url = format!("{}{}/index.json", base_url, id_lower);
    debug!(url = %version_url, "Fetching version index");
    let version_index: VersionIndex = client.get(&version_url).send().await?.json().await?;

    if version_index.versions.is_empty() {
        return Err(NuGetError::NoVersions(pkg.id.clone()));
    }

    // 3. Determine version to download
    let version = if let Some(ref requested) = pkg.version {
        // Find exact or best match
        version_index
            .versions
            .iter()
            .find(|v| v == &requested)
            .or_else(|| version_index.versions.last())
            .ok_or_else(|| NuGetError::VersionNotFound {
                id: pkg.id.clone(),
                version: requested.clone(),
            })?
            .clone()
    } else {
        // Use latest
        version_index.versions.last().unwrap().clone()
    };

    // 4. Download .nupkg
    let nupkg_url = format!(
        "{}{}/{}/{}.{}.nupkg",
        base_url, id_lower, version, id_lower, version
    );
    info!(
        package = %pkg.display_name,
        version = %version,
        "Downloading package"
    );
    let nupkg_bytes = client.get(&nupkg_url).send().await?.bytes().await?;

    // 5. Extract .app from .nupkg
    let app_path = extract_app_from_nupkg(&nupkg_bytes, dest, &pkg.display_name)?;
    info!(
        package = %pkg.display_name,
        path = %app_path.display(),
        "Extracted .app file"
    );

    Ok(app_path)
}

/// Download all dependencies from a NuGet feed.
///
/// Returns paths to all extracted .app files.
async fn download_all(
    client: &reqwest::Client,
    feed: &NuGetFeed,
    deps: &[AppDependency],
    dest: &Path,
) -> Vec<Result<PathBuf, NuGetError>> {
    let refs = resolve_dependencies(deps);
    let mut results = Vec::new();

    for pkg_ref in &refs {
        let result = download(client, feed, pkg_ref, dest).await;
        results.push(result);
    }

    results
}

/// Get the PackageBaseAddress URL from the NuGet v3 service index.
async fn get_package_base_address(
    client: &reqwest::Client,
    index_url: &str,
) -> Result<String, NuGetError> {
    debug!(url = %index_url, "Fetching NuGet service index");
    let index: ServiceIndex = client.get(index_url).send().await?.json().await?;

    let base = index
        .resources
        .iter()
        .find(|r| r.resource_type.starts_with("PackageBaseAddress"))
        .ok_or(NuGetError::NoBaseAddress)?;

    let mut url = base.id.clone();
    if !url.ends_with('/') {
        url.push('/');
    }

    Ok(url)
}

/// Extract the first .app file from a .nupkg (ZIP) archive.
fn extract_app_from_nupkg(
    nupkg_bytes: &[u8],
    dest: &Path,
    display_name: &str,
) -> Result<PathBuf, NuGetError> {
    let cursor = std::io::Cursor::new(nupkg_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();

        if name.to_lowercase().ends_with(".app") {
            // Determine output filename
            let filename = name
                .rsplit('/')
                .next()
                .unwrap_or(&name);
            let out_path = dest.join(filename);

            std::fs::create_dir_all(dest)?;
            let mut out_file = std::fs::File::create(&out_path)?;
            std::io::copy(&mut file, &mut out_file)?;

            return Ok(out_path);
        }
    }

    // If no .app file found directly, some packages put it in a subfolder
    warn!(
        package = %display_name,
        "No .app file found at root of nupkg, trying any path"
    );
    Err(NuGetError::NoAppInNupkg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_single_dependency() {
        let deps = vec![AppDependency {
            id: "63ca2fa4-4f03-4f2b-a480-172fef340d3f".to_string(),
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "24.0.12345.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].id,
            "microsoft.base.application.symbols.63ca2fa4-4f03-4f2b-a480-172fef340d3f"
        );
        assert_eq!(refs[0].version.as_deref(), Some("24.0.12345.0"));
        assert_eq!(refs[0].display_name, "Base Application");
    }

    #[test]
    fn resolve_multiple_dependencies() {
        let deps = vec![
            AppDependency {
                id: "id-1".to_string(),
                name: "System Application".to_string(),
                publisher: "Microsoft".to_string(),
                version: "24.0.0.0".to_string(),
            },
            AppDependency {
                id: "id-2".to_string(),
                name: "My App".to_string(),
                publisher: "Contoso Ltd".to_string(),
                version: "1.0.0.0".to_string(),
            },
        ];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].id, "microsoft.system.application.symbols.id-1");
        assert_eq!(refs[1].id, "contoso.ltd.my.app.symbols.id-2");
    }

    #[test]
    fn extract_app_from_nupkg_test() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        // Create a fake .nupkg with a .app file inside
        let mut nupkg_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut nupkg_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default();

            // Add some nupkg metadata files
            zip.start_file("[Content_Types].xml", options).unwrap();
            zip.write_all(b"<xml/>").unwrap();

            // Add the .app file
            zip.start_file("Microsoft.Application.symbols.app", options)
                .unwrap();
            // Write NAVX header + minimal content (won't be a valid .app but tests extraction)
            zip.write_all(b"NAVX_APP_CONTENT").unwrap();

            zip.finish().unwrap();
        }

        let dest = std::env::temp_dir().join("al-symbols-test-nupkg");
        let _ = std::fs::create_dir_all(&dest);

        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_str().unwrap().ends_with(".app"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn no_app_in_nupkg() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut nupkg_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut nupkg_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default();

            zip.start_file("readme.txt", options).unwrap();
            zip.write_all(b"no app here").unwrap();

            zip.finish().unwrap();
        }

        let dest = std::env::temp_dir().join("al-symbols-test-no-app");
        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        assert!(matches!(result.unwrap_err(), NuGetError::NoAppInNupkg));
    }
}
