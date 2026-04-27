//! NuGet v3 client for downloading AL symbol packages.
//!
//! BC NuGet packages follow the pattern:
//! - Package ID: `{publisher}.{name}.symbols.{app_id}` (lowercase, spaces→dots)
//! - The `.nupkg` is a ZIP containing the `.app` file

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
///
/// Fields use camelCase for JSON serialization to match the app.json format.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDependency {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

/// Well-known BC core package GUIDs that use special naming on the MSSymbols feed.
const APPLICATION_APP_ID: &str = "c1335042-3002-4257-bf8a-75c898ccb1b8";
const BASE_APPLICATION_APP_ID: &str = "437dbf0e-84ff-417a-965d-ed2bb9650972";
const BUSINESS_FOUNDATION_APP_ID: &str = "f3552374-a1f2-4356-848e-196002525837";
const SYSTEM_APPLICATION_APP_ID: &str = "63ca2fa4-4f03-4f2b-a480-172fef340d3f";
const SYSTEM_APP_ID: &str = "8874ed3a-0643-4247-9ced-7a7002f7135d";

/// Resolve app.json dependencies into NuGet PackageRefs.
///
/// BC NuGet package IDs follow the pattern:
/// - Core Microsoft packages have fixed names (no GUID or special casing)
/// - Other packages: `{Publisher}.{AppName}.symbols.{AppId}` (spaces removed, lowercase)
pub fn resolve_dependencies(deps: &[AppDependency]) -> Vec<PackageRef> {
    deps.iter()
        .map(|dep| {
            let id = resolve_package_id(dep);
            PackageRef {
                id,
                version: Some(dep.version.clone()),
                display_name: dep.name.clone(),
            }
        })
        .collect()
}

/// Resolve a single dependency to its NuGet package ID.
///
/// The core BC packages have hardcoded names because Microsoft's MSSymbols feed
/// uses inconsistent naming:
/// - Application (Base App): `Microsoft.Application.symbols` (no GUID)
/// - System Application: `Microsoft.SystemApplication.symbols.{guid}` (no space, with GUID)
/// - System (Platform): `Microsoft.Platform.symbols` (no GUID, different name)
fn resolve_package_id(dep: &AppDependency) -> String {
    let id_lower = dep.id.to_lowercase();

    // Core Microsoft packages have special naming on the MSSymbols feed.
    // These were found empirically — Microsoft is inconsistent about GUID inclusion.
    match id_lower.as_str() {
        APPLICATION_APP_ID => "Microsoft.Application.symbols".to_string(),
        BASE_APPLICATION_APP_ID => format!(
            "Microsoft.BaseApplication.symbols.{}",
            BASE_APPLICATION_APP_ID
        ),
        BUSINESS_FOUNDATION_APP_ID => format!(
            "Microsoft.BusinessFoundation.symbols.{}",
            BUSINESS_FOUNDATION_APP_ID
        ),
        SYSTEM_APPLICATION_APP_ID => format!(
            "Microsoft.SystemApplication.symbols.{}",
            SYSTEM_APPLICATION_APP_ID
        ),
        SYSTEM_APP_ID => "Microsoft.Platform.symbols".to_string(),
        // General pattern: {Publisher}.{AppName}.symbols.{AppId}
        // Spaces are removed (not replaced with dots) to match ADO feed convention
        _ => format!(
            "{}.{}.symbols.{}",
            dep.publisher.replace(' ', ""),
            dep.name.replace(' ', ""),
            id_lower
        ),
    }
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
    /// Cache of service index base addresses keyed by feed index_url.
    base_address_cache: Mutex<HashMap<String, String>>,
}

impl NuGetClient {
    /// Create a new NuGet client with the given feeds.
    pub fn new(feeds: Vec<NuGetFeed>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            feeds,
            base_address_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Download a single package, trying each feed in order until one succeeds.
    ///
    /// Returns the path to the extracted .app file.
    pub async fn download(&self, pkg: &PackageRef, dest: &Path) -> Result<PathBuf, NuGetError> {
        let mut last_err = None;
        for feed in &self.feeds {
            match download(&self.client, &self.base_address_cache, feed, pkg, dest).await {
                Ok(path) => return Ok(path),
                Err(e) => {
                    tracing::debug!(feed = %feed.index_url, error = %e, "Feed failed, trying next");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(NuGetError::NoBaseAddress))
    }

    /// Download all dependencies concurrently, trying each feed in order for each package.
    ///
    /// Downloads run concurrently, limited to `MAX_CONCURRENT_DOWNLOADS`
    /// permits to avoid overwhelming the NuGet feed (and consuming hundreds of
    /// MB of RAM on workspaces with many BC dependencies). Returns one result
    /// per dependency in the same order as the input slice.
    pub async fn download_all(
        &self,
        deps: &[AppDependency],
        dest: &Path,
    ) -> Vec<Result<PathBuf, NuGetError>> {
        const MAX_CONCURRENT_DOWNLOADS: usize = 4;
        let refs = resolve_dependencies(deps);
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_DOWNLOADS));
        let futures = refs.iter().map(|pkg_ref| {
            let sem = std::sync::Arc::clone(&semaphore);
            async move {
                let _permit = sem
                    .acquire()
                    .await
                    .expect("download_all semaphore is never closed");
                self.download(pkg_ref, dest).await
            }
        });
        futures::future::join_all(futures).await
    }
}

/// Download a single package from a NuGet feed and extract the .app file.
///
/// Returns the path to the extracted .app file.
async fn download(
    client: &reqwest::Client,
    cache: &Mutex<HashMap<String, String>>,
    feed: &NuGetFeed,
    pkg: &PackageRef,
    dest: &Path,
) -> Result<PathBuf, NuGetError> {
    // 1. Get service index (cached per feed index_url)
    let cached = {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&feed.index_url).cloned()
    };
    let base_url = if let Some(url) = cached {
        debug!(feed = %feed.index_url, "Using cached PackageBaseAddress");
        url
    } else {
        let url = get_package_base_address(client, &feed.index_url).await?;
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(feed.index_url.clone(), url.clone());
        url
    };

    let id_lower = pkg.id.to_lowercase();

    // 2. Get version list
    let version_url = format!("{}{}/index.json", base_url, id_lower);
    debug!(url = %version_url, "Fetching version index");
    let version_index: VersionIndex = client
        .get(&version_url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if version_index.versions.is_empty() {
        return Err(NuGetError::NoVersions(pkg.id.clone()));
    }

    // 3. Determine version to download
    let version = if let Some(ref requested) = pkg.version {
        // Try exact match first
        if let Some(v) = version_index.versions.iter().find(|v| *v == requested) {
            v.clone()
        } else {
            // Find best prefix match: e.g. "26.5.0.0" → latest "26.5.*"
            let prefix = version_prefix(requested);
            let mut prefix_matches: Vec<&String> = version_index
                .versions
                .iter()
                .filter(|v| v.starts_with(&prefix))
                .collect();
            prefix_matches.sort_by_key(|a| parse_version(a));
            if let Some(v) = prefix_matches.last() {
                info!(
                    requested = %requested,
                    resolved = %v,
                    "Resolved version via prefix match"
                );
                (*v).clone()
            } else {
                // Fall back to latest available
                let latest = version_index
                    .versions
                    .last()
                    .ok_or_else(|| NuGetError::NoVersions(pkg.id.clone()))?;
                info!(
                    requested = %requested,
                    resolved = %latest,
                    "No prefix match, using latest version"
                );
                latest.clone()
            }
        }
    } else {
        // Use latest
        version_index
            .versions
            .last()
            .ok_or_else(|| NuGetError::NoVersions(pkg.id.clone()))?
            .clone()
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
    const MAX_NUPKG_BYTES: u64 = 200 * 1024 * 1024; // 200 MB
    let response = client.get(&nupkg_url).send().await?;
    // Require a Content-Length header so the cap below is enforceable. Without
    // a length header an attacker-controlled server could lie about the
    // content size and push arbitrary bytes through `response.bytes()` —
    // bytes() buffers without a cap. Refuse the download in that case.
    let content_length = response.content_length().ok_or_else(|| {
        NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Package '{}' download has no Content-Length header — refusing.",
                pkg.display_name
            ),
        ))
    })?;
    if content_length > MAX_NUPKG_BYTES {
        return Err(NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Package '{name}' Content-Length {content_length} exceeds {max} byte limit — refusing download",
                name = pkg.display_name,
                max = MAX_NUPKG_BYTES,
            ),
        )));
    }
    let nupkg_bytes = response.bytes().await?;
    if nupkg_bytes.len() as u64 > MAX_NUPKG_BYTES {
        return Err(NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Package '{name}' actual body {actual} exceeds {max} byte limit — server lied about Content-Length",
                name = pkg.display_name,
                actual = nupkg_bytes.len(),
                max = MAX_NUPKG_BYTES,
            ),
        )));
    }

    // 5. Extract .app from .nupkg
    let app_path = extract_app_from_nupkg(&nupkg_bytes, dest, &pkg.display_name)?;
    info!(
        package = %pkg.display_name,
        path = %app_path.display(),
        "Extracted .app file"
    );

    Ok(app_path)
}

/// Extract the major.minor prefix from a version string.
///
/// "26.5.0.0" → "26.5."
/// "26.0.40469" → "26.0."
fn version_prefix(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.", parts[..2].join("."))
    } else {
        format!("{}.", version)
    }
}

/// Parse a version string into a tuple of integer components for correct numeric comparison.
///
/// "2.0.999.0"   → (2, 0, 999, 0)
/// "2.0.12345.0" → (2, 0, 12345, 0)
///
/// Components that fail to parse as `u64` are treated as 0 so that malformed
/// versions sort consistently rather than panicking.
fn parse_version(version: &str) -> (u64, u64, u64, u64) {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    let rev = parts.next().unwrap_or(0);
    (major, minor, patch, rev)
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

    // Refuse non-HTTPS PackageBaseAddress URLs by default — package downloads
    // over plain HTTP are vulnerable to MITM substitution and we have no
    // checksum verification path. Setting `AL_LSP_ALLOW_HTTP_FEED=1` opts in
    // for local-dev / loopback feeds.
    if !url.starts_with("https://") {
        let allow_http = std::env::var("AL_LSP_ALLOW_HTTP_FEED")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !allow_http {
            return Err(NuGetError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "PackageBaseAddress {url} is not HTTPS. Refusing to download \
                     over plain HTTP. Set AL_LSP_ALLOW_HTTP_FEED=1 to opt in for \
                     local-dev or loopback feeds."
                ),
            )));
        }
        warn!(
            url = %url,
            "PackageBaseAddress is HTTP — explicitly allowed via AL_LSP_ALLOW_HTTP_FEED"
        );
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
        let file = archive.by_index(i)?;
        let name = file.name().to_string();

        if name.to_lowercase().ends_with(".app") {
            // Extract the bare filename, stripping both Unix and Windows path
            // separators to prevent ZIP-slip attacks.
            let raw_filename = name.rsplit(['/', '\\']).next().unwrap_or(&name);

            // Reject filenames that are empty, traverse directories, or contain
            // embedded separators that survived splitting.
            if raw_filename.is_empty()
                || raw_filename.contains("..")
                || raw_filename.contains('/')
                || raw_filename.contains('\\')
            {
                warn!(entry = %name, "Skipping unsafe ZIP entry (potential ZIP-slip)");
                continue;
            }
            let out_path = dest.join(raw_filename);
            let tmp_path = dest.join(format!("{}.tmp", raw_filename));

            std::fs::create_dir_all(dest)?;
            {
                let mut out_file = std::fs::File::create(&tmp_path)?;
                // Limit extraction to 512 MB to guard against decompression bombs.
                // Use Read::take explicitly to avoid ambiguity with Iterator::take.
                const MAX_APP_SIZE: u64 = 536_870_912;
                let mut limited = std::io::Read::take(file, MAX_APP_SIZE);
                let bytes_copied = std::io::copy(&mut limited, &mut out_file)?;
                if bytes_copied >= MAX_APP_SIZE {
                    // Remove the partial temp file before returning the error.
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(NuGetError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "Extracted .app file exceeds {:.0} MB limit — possible decompression bomb or oversized package",
                            MAX_APP_SIZE as f64 / 1_048_576.0
                        ),
                    )));
                }
            }
            // Atomic rename: only the complete file is ever visible at the final path.
            std::fs::rename(&tmp_path, &out_path)?;

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
    fn resolve_core_system_application() {
        // System Application uses special naming: with GUID, spaces removed
        let deps = vec![AppDependency {
            id: "63ca2fa4-4f03-4f2b-a480-172fef340d3f".to_string(),
            name: "System Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "24.0.12345.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].id,
            "Microsoft.SystemApplication.symbols.63ca2fa4-4f03-4f2b-a480-172fef340d3f"
        );
        assert_eq!(refs[0].version.as_deref(), Some("24.0.12345.0"));
    }

    #[test]
    fn resolve_core_application() {
        // Application (Base App) uses special naming: no GUID
        let deps = vec![AppDependency {
            id: "c1335042-3002-4257-bf8a-75c898ccb1b8".to_string(),
            name: "Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "26.5.0.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "Microsoft.Application.symbols");
    }

    #[test]
    fn resolve_core_platform() {
        // System/Platform uses special naming: no GUID, different name
        let deps = vec![AppDependency {
            id: "8874ed3a-0643-4247-9ced-7a7002f7135d".to_string(),
            name: "System".to_string(),
            publisher: "Microsoft".to_string(),
            version: "1.0.0.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "Microsoft.Platform.symbols");
    }

    #[test]
    fn resolve_third_party_dependency() {
        let deps = vec![AppDependency {
            id: "id-2".to_string(),
            name: "My App".to_string(),
            publisher: "Contoso Ltd".to_string(),
            version: "1.0.0.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "ContosoLtd.MyApp.symbols.id-2");
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

    #[test]
    fn parse_version_numeric_comparison() {
        // "2.0.999.0" must sort BELOW "2.0.12345.0" — lexicographic sort gets this wrong
        assert!(parse_version("2.0.999.0") < parse_version("2.0.12345.0"));
        assert_eq!(parse_version("26.5.0.0"), (26, 5, 0, 0));
        assert_eq!(parse_version("2.0.999.0"), (2, 0, 999, 0));
        assert_eq!(parse_version("2.0.12345.0"), (2, 0, 12345, 0));
        // Malformed components fall back to 0
        assert_eq!(parse_version("1.x.0.0"), (1, 0, 0, 0));
        // Fewer than 4 components are padded with 0
        assert_eq!(parse_version("26.0.40469"), (26, 0, 40469, 0));
    }

    #[test]
    fn parse_version_invalid_empty() {
        // Empty string should not panic
        assert_eq!(parse_version(""), (0, 0, 0, 0));
    }

    #[test]
    fn extract_app_from_nupkg_atomic_no_tmp_left_on_success() {
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut nupkg_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut nupkg_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default();
            zip.start_file("Test.app", options).expect("add zip entry");
            zip.write_all(b"NAVX").expect("write zip data");
            zip.finish().expect("finalize zip");
        }

        let dest = std::env::temp_dir().join("al-symbols-test-atomic");
        let _ = std::fs::create_dir_all(&dest);

        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        assert!(result.is_ok());

        // The .tmp file must not remain after a successful extraction.
        let tmp = dest.join("Test.app.tmp");
        assert!(!tmp.exists(), ".tmp file should have been renamed away");

        // The final .app file must exist.
        let app = dest.join("Test.app");
        assert!(app.exists(), ".app file should exist at final path");

        let _ = std::fs::remove_dir_all(&dest);
    }
}
