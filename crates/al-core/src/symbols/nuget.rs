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
    /// Per-package download mutexes. Two concurrent downloads of the SAME
    /// package id will serialise on the same `tokio::sync::Mutex`, so the
    /// second observer hits the on-disk artefact written by the first and
    /// skips the network round-trip. Downloads of DIFFERENT packages still
    /// run concurrently up to the `download_all` semaphore. F-OPEN-019.
    package_locks: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
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
            package_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Get or create the per-package serialisation mutex for `pkg_id`.
    fn lock_for(&self, pkg_id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
        let key = pkg_id.to_lowercase();
        let mut map = self.package_locks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = map.get(&key) {
            return existing.clone();
        }
        let new_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        map.insert(key, new_lock.clone());
        new_lock
    }

    /// Download a single package, trying each feed in order until one succeeds.
    ///
    /// Returns the path to the extracted .app file.
    pub async fn download(&self, pkg: &PackageRef, dest: &Path) -> Result<PathBuf, NuGetError> {
        // Serialise concurrent downloads of the SAME package id. If two
        // callers race on `Foo.symbols.<guid>`, only the first hits the
        // network; the second observes the on-disk artefact written by
        // the tempfile+rename below and short-circuits.
        let lock = self.lock_for(&pkg.id);
        let _serial = lock.lock().await;

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
    let version_index: VersionIndex = fetch_metadata_json(client, &version_url).await?;

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

/// Upper bound for NuGet metadata responses (service index + version list).
/// These should be a few hundred KB at most for normal feeds; the cap is a
/// defence against a hostile or misconfigured server streaming gigabytes of
/// JSON before parser-side truncation kicks in. F-OPEN-018.
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024; // 16 MB

/// Fetch a JSON metadata response from `url`, refusing bodies larger than
/// `MAX_METADATA_BYTES`. Requires a `Content-Length` header so the cap is
/// enforceable without buffering the whole response first; servers without
/// one are refused. Same hardening pattern as the package-download path.
async fn fetch_metadata_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, NuGetError> {
    let response = client.get(url).send().await?.error_for_status()?;
    let content_length = response.content_length().ok_or_else(|| {
        NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Metadata response from {url} has no Content-Length header — refusing"),
        ))
    })?;
    if content_length > MAX_METADATA_BYTES {
        return Err(NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Metadata response from {url} Content-Length {content_length} exceeds \
                 {MAX_METADATA_BYTES} byte limit — refusing"
            ),
        )));
    }
    let bytes = response.bytes().await?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Metadata response from {url} body {actual} exceeds {MAX_METADATA_BYTES} \
                 byte limit — server lied about Content-Length",
                actual = bytes.len(),
            ),
        )));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

/// Get the PackageBaseAddress URL from the NuGet v3 service index.
async fn get_package_base_address(
    client: &reqwest::Client,
    index_url: &str,
) -> Result<String, NuGetError> {
    debug!(url = %index_url, "Fetching NuGet service index");
    let index: ServiceIndex = fetch_metadata_json(client, index_url).await?;

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
    fn resolve_core_base_application() {
        // Base Application uses special naming: with GUID embedded in the name.
        let deps = vec![AppDependency {
            id: "437dbf0e-84ff-417a-965d-ed2bb9650972".to_string(),
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "26.0.0.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].id,
            "Microsoft.BaseApplication.symbols.437dbf0e-84ff-417a-965d-ed2bb9650972"
        );
    }

    #[test]
    fn resolve_core_business_foundation() {
        // Business Foundation uses special naming: with GUID embedded in the name.
        let deps = vec![AppDependency {
            id: "f3552374-a1f2-4356-848e-196002525837".to_string(),
            name: "Business Foundation".to_string(),
            publisher: "Microsoft".to_string(),
            version: "26.0.0.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 1);
        assert_eq!(
            refs[0].id,
            "Microsoft.BusinessFoundation.symbols.f3552374-a1f2-4356-848e-196002525837"
        );
    }

    #[test]
    fn resolve_core_id_is_case_insensitive() {
        // The match arms lowercase the id first, so an UPPERCASE GUID from
        // app.json must still hit the special-naming branch (no general fallback).
        let deps = vec![AppDependency {
            id: "C1335042-3002-4257-BF8A-75C898CCB1B8".to_string(),
            name: "Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "26.5.0.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(refs[0].id, "Microsoft.Application.symbols");
    }

    #[test]
    fn resolve_third_party_lowercases_guid_and_strips_spaces() {
        // General pattern: spaces removed from publisher+name, GUID lowercased.
        let deps = vec![AppDependency {
            id: "AB12CD34-0000-0000-0000-000000000000".to_string(),
            name: "Cool Tool".to_string(),
            publisher: "Acme Corp".to_string(),
            version: "1.0.0.0".to_string(),
        }];

        let refs = resolve_dependencies(&deps);
        assert_eq!(
            refs[0].id,
            "AcmeCorp.CoolTool.symbols.ab12cd34-0000-0000-0000-000000000000"
        );
        assert_eq!(refs[0].display_name, "Cool Tool");
    }

    #[test]
    fn resolve_dependencies_preserves_order_and_count() {
        let deps = vec![
            AppDependency {
                id: "id-a".to_string(),
                name: "A".to_string(),
                publisher: "P".to_string(),
                version: "1.0.0.0".to_string(),
            },
            AppDependency {
                id: "id-b".to_string(),
                name: "B".to_string(),
                publisher: "P".to_string(),
                version: "2.0.0.0".to_string(),
            },
        ];
        let refs = resolve_dependencies(&deps);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].display_name, "A");
        assert_eq!(refs[1].display_name, "B");
        assert_eq!(refs[0].version.as_deref(), Some("1.0.0.0"));
        assert_eq!(refs[1].version.as_deref(), Some("2.0.0.0"));
    }

    #[test]
    fn nuget_feed_default_points_at_public_feed() {
        let feed = NuGetFeed::default();
        assert_eq!(feed.index_url, "https://api.nuget.org/v3/index.json");
    }

    #[test]
    fn version_prefix_extracts_major_minor() {
        // Multi-component versions yield the major.minor prefix with trailing dot.
        assert_eq!(version_prefix("26.5.0.0"), "26.5.");
        assert_eq!(version_prefix("26.0.40469"), "26.0.");
        // Exactly two components still works.
        assert_eq!(version_prefix("12.3"), "12.3.");
    }

    #[test]
    fn version_prefix_single_component_falls_back() {
        // Fewer than two components: the whole string plus a trailing dot.
        assert_eq!(version_prefix("26"), "26.");
        assert_eq!(version_prefix(""), ".");
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
    fn extract_app_strips_subfolder_path() {
        // A .app nested in a subfolder must be extracted to dest using only the
        // bare filename — the path prefix is stripped (ZIP-slip defence).
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut nupkg_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut nupkg_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default();
            zip.start_file("lib/net/Nested.app", options).unwrap();
            zip.write_all(b"NAVX").unwrap();
            zip.finish().unwrap();
        }

        let dest = std::env::temp_dir().join("al-symbols-test-subfolder");
        let _ = std::fs::remove_dir_all(&dest);
        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        let path = result.expect("nested .app should extract");
        // The file lands directly under dest, NOT under dest/lib/net/.
        assert_eq!(path, dest.join("Nested.app"));
        assert!(dest.join("Nested.app").exists());
        assert!(!dest.join("lib").exists(), "subfolder must not be created");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_app_rejects_dotdot_basename() {
        // The guard rejects any .app whose *basename* (after stripping path
        // separators) still contains "..". Such an entry is skipped, and with
        // no other safe .app the result is NoAppInNupkg — nothing is written.
        use std::io::Write;
        use zip::write::SimpleFileOptions;

        let mut nupkg_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut nupkg_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default();
            // Basename survives splitting and still contains "..".
            zip.start_file("lib/evil..payload.app", options).unwrap();
            zip.write_all(b"NAVX").unwrap();
            zip.finish().unwrap();
        }

        let dest = std::env::temp_dir().join("al-symbols-test-dotdot");
        let _ = std::fs::remove_dir_all(&dest);
        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        assert!(
            matches!(result, Err(NuGetError::NoAppInNupkg)),
            "entry with '..' in basename must be skipped, got {result:?}"
        );
        // The unsafe basename must never be materialised under dest.
        assert!(!dest.join("evil..payload.app").exists());
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

    // --- fetch_metadata_json (F-OPEN-018) -----------------------------------

    #[derive(Debug, serde::Deserialize)]
    struct DummyJson {
        ok: bool,
    }

    #[tokio::test]
    async fn fetch_metadata_json_accepts_small_response() {
        // Positive: a small valid JSON response under the cap deserialises.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", "11")
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let v: DummyJson = fetch_metadata_json(&client, &server.uri()).await.unwrap();
        assert!(v.ok);
    }

    #[tokio::test]
    async fn fetch_metadata_json_refuses_missing_content_length() {
        // Negative: a response with no Content-Length header is refused, so
        // we never start buffering an unbounded body.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_string(r#"{"ok":true}"#)
                    .insert_header("Transfer-Encoding", "chunked"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let res: Result<DummyJson, NuGetError> = fetch_metadata_json(&client, &server.uri()).await;
        assert!(
            res.is_err(),
            "missing Content-Length must be refused, got Ok"
        );
    }

    #[tokio::test]
    async fn fetch_metadata_json_refuses_oversized_content_length() {
        // Negative: a hostile feed claims an enormous Content-Length — we
        // refuse before reading the body.
        let oversize = (MAX_METADATA_BYTES + 1).to_string();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", oversize.as_str())
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let res: Result<DummyJson, NuGetError> = fetch_metadata_json(&client, &server.uri()).await;
        // Either we caught the lie ourselves (NuGetError::Io with "exceeds")
        // or reqwest noticed the body didn't match the claimed length and
        // returned a transport error. Both outcomes mean: bogus oversize
        // Content-Length does NOT result in successfully deserialised JSON.
        assert!(res.is_err(), "oversized Content-Length must not yield Ok");
    }

    // --- per-package serialisation lock (F-OPEN-019) -----------------------

    #[test]
    fn lock_for_same_id_returns_same_arc() {
        // Positive: two calls with the same (case-insensitive) id share one
        // mutex, so concurrent downloads will serialise.
        let client = NuGetClient::new(vec![]);
        let a = client.lock_for("Microsoft.Foo.symbols.abc-123");
        let b = client.lock_for("microsoft.foo.symbols.abc-123");
        assert!(
            std::sync::Arc::ptr_eq(&a, &b),
            "case-insensitive same id should yield same mutex"
        );
    }

    #[test]
    fn lock_for_different_ids_returns_different_arcs() {
        // Negative: different package ids must NOT share a mutex, or
        // unrelated downloads would block each other for no reason.
        let client = NuGetClient::new(vec![]);
        let a = client.lock_for("Microsoft.Foo");
        let b = client.lock_for("Microsoft.Bar");
        assert!(
            !std::sync::Arc::ptr_eq(&a, &b),
            "different ids must yield independent mutexes"
        );
    }

    #[tokio::test]
    async fn concurrent_downloads_of_same_package_serialise() {
        // Hammer: spawn 10 tasks that all hold the lock for the same id
        // for 5ms each. If the per-package mutex works, they run strictly
        // sequentially (total >= 50ms) instead of in parallel.
        let client = std::sync::Arc::new(NuGetClient::new(vec![]));
        let start = std::time::Instant::now();
        let mut handles = Vec::new();
        for _ in 0..10 {
            let c = client.clone();
            handles.push(tokio::spawn(async move {
                let lock = c.lock_for("Pkg.X");
                let _g = lock.lock().await;
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }));
        }
        for h in handles {
            h.await.expect("task panicked");
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(45),
            "10 × 5ms serial waits should take >= 45ms (got {elapsed:?}) — \
             if they ran concurrently the mutex isn't serialising"
        );
    }

    // --- get_package_base_address -------------------------------------------

    /// Mount a service index that advertises `base_id` as a PackageBaseAddress
    /// resource (alongside an unrelated resource to prove selection works).
    async fn mount_service_index(server: &wiremock::MockServer, base_id: &str) {
        let body = format!(
            r#"{{"resources":[
                {{"@id":"https://example/search","@type":"SearchQueryService"}},
                {{"@id":"{base_id}","@type":"PackageBaseAddress/3.0.0"}}
            ]}}"#
        );
        let len = body.len().to_string();
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", len.as_str())
                    .set_body_string(body),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn base_address_selected_and_trailing_slash_appended() {
        // The PackageBaseAddress resource is picked out by @type prefix, and a
        // trailing slash is appended when the advertised id lacks one.
        let server = wiremock::MockServer::start().await;
        mount_service_index(&server, "https://feed.example/base").await;

        let client = reqwest::Client::new();
        let url = get_package_base_address(&client, &server.uri())
            .await
            .expect("https base address should resolve");
        assert_eq!(url, "https://feed.example/base/");
    }

    #[tokio::test]
    async fn base_address_keeps_existing_trailing_slash() {
        let server = wiremock::MockServer::start().await;
        mount_service_index(&server, "https://feed.example/base/").await;

        let client = reqwest::Client::new();
        let url = get_package_base_address(&client, &server.uri())
            .await
            .unwrap();
        assert_eq!(url, "https://feed.example/base/");
    }

    #[tokio::test]
    async fn base_address_missing_resource_errors() {
        // A service index with no PackageBaseAddress resource → NoBaseAddress.
        let server = wiremock::MockServer::start().await;
        let body = r#"{"resources":[{"@id":"https://x/s","@type":"SearchQueryService"}]}"#;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("Content-Length", body.len().to_string().as_str())
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let res = get_package_base_address(&client, &server.uri()).await;
        assert!(
            matches!(res, Err(NuGetError::NoBaseAddress)),
            "expected NoBaseAddress, got {res:?}"
        );
    }

    #[tokio::test]
    #[serial_test::serial(al_lsp_http_feed_env)]
    async fn base_address_refuses_plain_http_by_default() {
        // An HTTP PackageBaseAddress must be refused unless explicitly opted in.
        std::env::remove_var("AL_LSP_ALLOW_HTTP_FEED");
        let server = wiremock::MockServer::start().await;
        mount_service_index(&server, "http://insecure.example/base").await;

        let client = reqwest::Client::new();
        let res = get_package_base_address(&client, &server.uri()).await;
        match res {
            Err(NuGetError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
                assert!(e.to_string().contains("not HTTPS"));
            }
            other => panic!("expected Io(InvalidInput) refusing HTTP, got {other:?}"),
        }
    }

    #[tokio::test]
    #[serial_test::serial(al_lsp_http_feed_env)]
    async fn base_address_allows_http_when_opted_in() {
        // With AL_LSP_ALLOW_HTTP_FEED=1 the HTTP base address is accepted.
        std::env::set_var("AL_LSP_ALLOW_HTTP_FEED", "1");
        let server = wiremock::MockServer::start().await;
        mount_service_index(&server, "http://insecure.example/base").await;

        let client = reqwest::Client::new();
        let url = get_package_base_address(&client, &server.uri()).await;
        std::env::remove_var("AL_LSP_ALLOW_HTTP_FEED");
        assert_eq!(url.unwrap(), "http://insecure.example/base/");
    }

    #[tokio::test]
    async fn fetch_metadata_json_propagates_http_error_status() {
        // A 500 from the feed surfaces as an error via error_for_status(),
        // never as a successful deserialisation.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(500)
                    .insert_header("Content-Length", "2")
                    .set_body_string("{}"),
            )
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let res: Result<DummyJson, NuGetError> = fetch_metadata_json(&client, &server.uri()).await;
        assert!(res.is_err(), "HTTP 500 must yield an error");
    }
}
