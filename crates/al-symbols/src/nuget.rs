//! NuGet v3 client for downloading AL symbol packages.
//!
//! BC NuGet packages follow the pattern:
//! - Package ID: `{publisher}.{name}.symbols.{app_id}` (lowercase, spaces→dots)
//! - The `.nupkg` is a ZIP containing the `.app` file

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

const MAX_NUPKG_BYTES: u64 = 200 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DOWNLOAD_ATTEMPTS: usize = 3;

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
    #[error("Downloaded package contains an invalid .app: {0}")]
    InvalidApp(#[from] super::app_reader::AppReaderError),
    #[error(
        "Downloaded .app identity/version mismatch: requested id {expected_id} version {expected_version}, got id {actual_id} version {actual_version}"
    )]
    PackageIdentityMismatch {
        expected_id: String,
        expected_version: String,
        actual_id: String,
        actual_version: String,
    },
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct PackageRef {
    /// NuGet package ID (e.g., "microsoft.application.symbols.437dbf0e-84ff-417a-965d-ed2bb9650972")
    pub id: String,
    /// Desired version (e.g., "24.0.12345.0"), or None for latest.
    pub version: Option<String>,
    pub display_name: String,
    pub app_id: String,
}

#[derive(Debug, Clone)]
pub struct NuGetFeed {
    pub index_url: String,
}

impl Default for NuGetFeed {
    fn default() -> Self {
        Self {
            index_url: "https://api.nuget.org/v3/index.json".to_string(),
        }
    }
}

pub use al_types::AppDependency;

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
    resolve_dependencies_for_country(deps, None)
}

/// Country/region-aware variant (`al.symbolsCountryRegion` parity, BC 2026 W1).
///
/// Localized apps (Application, Base Application) ship country-specific
/// packages on the MSSymbols feed — e.g. `Microsoft.Application.DE.symbols`.
/// `"w1"` (worldwide) and `None` resolve to the unsuffixed W1 packages.
/// Platform/System packages are country-invariant.
pub fn resolve_dependencies_for_country(
    deps: &[AppDependency],
    country: Option<&str>,
) -> Vec<PackageRef> {
    let cc = country
        .map(str::trim)
        .filter(|c| !c.is_empty() && !c.eq_ignore_ascii_case("w1"))
        .map(str::to_uppercase);
    deps.iter()
        .map(|dep| {
            let id = resolve_package_id(dep, cc.as_deref());
            PackageRef {
                id,
                version: Some(dep.version.clone()),
                display_name: dep.name.clone(),
                app_id: dep.id.clone(),
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
fn resolve_package_id(dep: &AppDependency, country: Option<&str>) -> String {
    let id_lower = dep.id.to_lowercase();

    // These were found empirically — Microsoft is inconsistent about GUID inclusion.
    match id_lower.as_str() {
        APPLICATION_APP_ID => match country {
            Some(cc) => format!("Microsoft.Application.{cc}.symbols"),
            None => "Microsoft.Application.symbols".to_string(),
        },
        BASE_APPLICATION_APP_ID => match country {
            Some(cc) => format!(
                "Microsoft.BaseApplication.{cc}.symbols.{}",
                BASE_APPLICATION_APP_ID
            ),
            None => format!(
                "Microsoft.BaseApplication.symbols.{}",
                BASE_APPLICATION_APP_ID
            ),
        },
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

type DownloadCacheKey = (PathBuf, String, Option<String>);

pub struct NuGetClient {
    client: reqwest::Client,
    feeds: Vec<NuGetFeed>,
    base_address_cache: Mutex<HashMap<String, String>>,
    /// Per-package download mutexes. Two concurrent downloads of the SAME
    /// package id will serialise on the same `tokio::sync::Mutex`, so the
    /// second observer hits the on-disk artefact written by the first and
    /// skips the network round-trip. Downloads of DIFFERENT packages still
    /// run concurrently up to the `download_all` semaphore.
    package_locks: Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    /// Successful downloads keyed by destination + package identity/version.
    /// This is what turns the per-package mutex into true request de-duplication:
    /// waiters reuse the first completed artifact instead of downloading again.
    completed_downloads: Mutex<HashMap<DownloadCacheKey, PathBuf>>,
    /// `al.symbolsCountryRegion` — selects localized core packages
    /// (e.g. `Microsoft.Application.DE.symbols`). None/"w1" = worldwide.
    country: Option<String>,
}

impl NuGetClient {
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
            completed_downloads: Mutex::new(HashMap::new()),
            country: None,
        }
    }

    /// Select the symbols country/region (`al.symbolsCountryRegion` parity).
    pub fn with_country(mut self, country: Option<String>) -> Self {
        self.country = country;
        self
    }

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

        let completion_key = (
            dest.to_path_buf(),
            pkg.id.to_lowercase(),
            pkg.version.clone(),
        );
        let completed = self
            .completed_downloads
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&completion_key)
            .cloned();
        if let Some(path) = completed {
            if nuget_manifest_satisfies(&path, pkg) {
                debug!(package = %pkg.display_name, path = %path.display(), "Reusing completed NuGet package download");
                return Ok(path);
            }
            self.completed_downloads
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&completion_key);
        }

        let mut last_err = None;
        for feed in &self.feeds {
            match download(&self.client, &self.base_address_cache, feed, pkg, dest).await {
                Ok(path) => {
                    self.completed_downloads
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .insert(completion_key, path.clone());
                    return Ok(path);
                }
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
        let refs = resolve_dependencies_for_country(deps, self.country.as_deref());
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

    let version_url = format!("{}{}/index.json", base_url, id_lower);
    debug!(url = %version_url, "Fetching version index");
    let version_index: VersionIndex = fetch_metadata_json(client, &version_url).await?;

    if version_index.versions.is_empty() {
        return Err(NuGetError::NoVersions(pkg.id.clone()));
    }

    let version = select_version(&pkg.id, pkg.version.as_deref(), &version_index.versions)?;
    if pkg.version.as_deref() != Some(version.as_str()) {
        info!(
            requested = ?pkg.version,
            resolved = %version,
            "Resolved NuGet symbol package version"
        );
    }

    let nupkg_url = format!(
        "{}{}/{}/{}.{}.nupkg",
        base_url, id_lower, version, id_lower, version
    );
    info!(
        package = %pkg.display_name,
        version = %version,
        "Downloading package"
    );
    let mut response = get_with_retry(client, &nupkg_url).await?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_NUPKG_BYTES)
    {
        return Err(body_too_large_error(
            &pkg.display_name,
            response.content_length().unwrap_or_default(),
            MAX_NUPKG_BYTES,
        ));
    }

    tokio::fs::create_dir_all(dest).await?;
    let nupkg_tmp = download_temp_path(dest, &pkg.id);
    let stream_result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&nupkg_tmp)
            .await?;
        let mut received = 0u64;
        while let Some(chunk) = response.chunk().await? {
            received = received.checked_add(chunk.len() as u64).ok_or_else(|| {
                body_too_large_error(&pkg.display_name, u64::MAX, MAX_NUPKG_BYTES)
            })?;
            if received > MAX_NUPKG_BYTES {
                return Err(body_too_large_error(
                    &pkg.display_name,
                    received,
                    MAX_NUPKG_BYTES,
                ));
            }
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        Ok::<(), NuGetError>(())
    }
    .await;
    if let Err(error) = stream_result {
        let _ = tokio::fs::remove_file(&nupkg_tmp).await;
        return Err(error);
    }

    let app_result = extract_app_from_nupkg_file(
        &nupkg_tmp,
        dest,
        &pkg.display_name,
        &pkg.app_id,
        pkg.version.as_deref().unwrap_or(&version),
    );
    let _ = tokio::fs::remove_file(&nupkg_tmp).await;
    let app_path = app_result?;
    info!(
        package = %pkg.display_name,
        path = %app_path.display(),
        "Extracted .app file"
    );

    Ok(app_path)
}

fn select_version(
    package_id: &str,
    requested: Option<&str>,
    versions: &[String],
) -> Result<String, NuGetError> {
    if versions.is_empty() {
        return Err(NuGetError::NoVersions(package_id.to_string()));
    }
    if let Some(requested) = requested {
        if let Some(exact) = versions
            .iter()
            .find(|version| version.as_str() == requested)
        {
            return Ok(exact.clone());
        }
        let prefix = version_prefix(requested);
        return versions
            .iter()
            .filter(|version| version.starts_with(&prefix))
            .max_by_key(|version| parse_version(version))
            .cloned()
            .ok_or_else(|| NuGetError::VersionNotFound {
                id: package_id.to_string(),
                version: requested.to_string(),
            });
    }
    versions
        .iter()
        .max_by_key(|version| parse_version(version))
        .cloned()
        .ok_or_else(|| NuGetError::NoVersions(package_id.to_string()))
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

/// Fetch a JSON metadata response from `url`, refusing bodies larger than
/// `MAX_METADATA_BYTES`. Chunked responses are accepted and counted while
/// streaming; a missing or dishonest `Content-Length` cannot bypass the cap.
async fn fetch_metadata_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, NuGetError> {
    let mut response = get_with_retry(client, url).await?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_METADATA_BYTES)
    {
        return Err(NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Metadata response from {url} Content-Length exceeds \
                 {MAX_METADATA_BYTES} byte limit"
            ),
        )));
    }
    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(MAX_METADATA_BYTES) as usize,
    );
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_METADATA_BYTES as usize {
            return Err(NuGetError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Metadata response from {url} exceeds {MAX_METADATA_BYTES} byte limit"),
            )));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

async fn get_with_retry(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        match client.get(url).send().await {
            Ok(response)
                if is_retryable_status(response.status()) && attempt < MAX_DOWNLOAD_ATTEMPTS =>
            {
                let delay = retry_delay(&response, attempt);
                warn!(url, status = %response.status(), attempt, ?delay, "Transient NuGet response; retrying");
                tokio::time::sleep(delay).await;
            }
            Ok(response) => return response.error_for_status(),
            Err(error) if attempt < MAX_DOWNLOAD_ATTEMPTS => {
                let delay = std::time::Duration::from_millis(200 * (1 << (attempt - 1)));
                warn!(url, %error, attempt, ?delay, "Transient NuGet request failure; retrying");
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("retry loop always returns on its final attempt")
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

fn retry_delay(response: &reqwest::Response, attempt: usize) -> std::time::Duration {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| std::time::Duration::from_secs(seconds.min(30)))
        .unwrap_or_else(|| std::time::Duration::from_millis(200 * (1 << (attempt - 1))))
}

fn body_too_large_error(name: &str, actual: u64, limit: u64) -> NuGetError {
    NuGetError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("Package '{name}' body {actual} exceeds {limit} byte limit"),
    ))
}

fn download_temp_path(dest: &Path, package_id: &str) -> PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let safe_id: String = package_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect();
    dest.join(format!(
        ".{safe_id}.{}.{}.nupkg.tmp",
        std::process::id(),
        sequence
    ))
}

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

#[cfg(test)]
fn extract_app_from_nupkg(
    nupkg_bytes: &[u8],
    dest: &Path,
    display_name: &str,
) -> Result<PathBuf, NuGetError> {
    let cursor = std::io::Cursor::new(nupkg_bytes);
    extract_app_from_nupkg_reader(cursor, dest, display_name, None)
}

fn extract_app_from_nupkg_file(
    nupkg_path: &Path,
    dest: &Path,
    display_name: &str,
    expected_app_id: &str,
    expected_version: &str,
) -> Result<PathBuf, NuGetError> {
    extract_app_from_nupkg_reader(
        std::fs::File::open(nupkg_path)?,
        dest,
        display_name,
        Some((expected_app_id, expected_version)),
    )
}

fn extract_app_from_nupkg_reader<R: std::io::Read + std::io::Seek>(
    reader: R,
    dest: &Path,
    display_name: &str,
    expected: Option<(&str, &str)>,
) -> Result<PathBuf, NuGetError> {
    let mut archive = zip::ZipArchive::new(reader)?;
    const MAX_ARCHIVE_ENTRIES: usize = 200_000;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(NuGetError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "nupkg contains {} entries; limit is {MAX_ARCHIVE_ENTRIES}",
                archive.len()
            ),
        )));
    }

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
            static EXTRACT_SEQUENCE: std::sync::atomic::AtomicU64 =
                std::sync::atomic::AtomicU64::new(0);
            let sequence = EXTRACT_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let tmp_path = dest.join(format!(
                ".{raw_filename}.{}.{}.tmp",
                std::process::id(),
                sequence
            ));

            std::fs::create_dir_all(dest)?;
            {
                let mut out_file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp_path)?;
                // Match the .app reader's 200 MB cap so downloads cannot
                // publish an artifact the symbol engine will immediately reject.
                // Use Read::take explicitly to avoid ambiguity with Iterator::take.
                const MAX_APP_SIZE: u64 = super::app_reader::MAX_APP_FILE_SIZE;
                let mut limited = std::io::Read::take(file, MAX_APP_SIZE + 1);
                let bytes_copied = std::io::copy(&mut limited, &mut out_file)?;
                if bytes_copied > MAX_APP_SIZE {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(NuGetError::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "Extracted .app file exceeds {:.0} MB limit — possible decompression bomb or oversized package",
                            MAX_APP_SIZE as f64 / 1_048_576.0
                        ),
                    )));
                }
                out_file.flush()?;
                out_file.sync_all()?;
            }
            // Reject corrupt/truncated payloads before they become visible in
            // the package folder. Manifest-only validation avoids the much
            // larger SymbolReference parse that normal indexing performs next.
            let manifest = match super::app_reader::read_app_manifest_file(&tmp_path) {
                Ok(manifest) => manifest,
                Err(error) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(error.into());
                }
            };
            if let Some((expected_id, expected_version)) = expected {
                if !manifest.app_id.eq_ignore_ascii_case(expected_id)
                    || !crate::model::version_at_least(&manifest.version, expected_version)
                {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(NuGetError::PackageIdentityMismatch {
                        expected_id: expected_id.to_string(),
                        expected_version: expected_version.to_string(),
                        actual_id: manifest.app_id,
                        actual_version: manifest.version,
                    });
                }
            }
            // Atomic rename: only the complete file is ever visible at the final path.
            if let Err(error) = std::fs::rename(&tmp_path, &out_path) {
                if out_path.exists() {
                    std::fs::remove_file(&out_path)?;
                    std::fs::rename(&tmp_path, &out_path)?;
                } else {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(error.into());
                }
            }

            return Ok(out_path);
        }
    }

    warn!(
        package = %display_name,
        "No safe .app file found in nupkg"
    );
    Err(NuGetError::NoAppInNupkg)
}

fn nuget_manifest_satisfies(path: &Path, package: &PackageRef) -> bool {
    super::app_reader::read_app_manifest_file(path).is_ok_and(|manifest| {
        manifest.app_id.eq_ignore_ascii_case(&package.app_id)
            && package
                .version
                .as_deref()
                .is_none_or(|version| crate::model::version_at_least(&manifest.version, version))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_app_bytes() -> Vec<u8> {
        app_bytes("test-id", "1.0.0.0")
    }

    fn app_bytes(app_id: &str, version: &str) -> Vec<u8> {
        let mut app = Vec::new();
        app.extend_from_slice(b"NAVX");
        app.extend_from_slice(&1u32.to_le_bytes());
        app.extend_from_slice(&[0u8; 32]);
        let mut zip_bytes = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options)
                .expect("manifest entry");
            write!(
                zip,
                r#"<Package><App Id="{app_id}" Name="Test" Publisher="Test" Version="{version}" /></Package>"#
            )
            .expect("manifest body");
            zip.start_file("SymbolReference.json", options)
                .expect("symbols entry");
            zip.write_all(br#"{"Tables":[]}"#).expect("symbols body");
            zip.finish().expect("finish app archive");
        }
        app.extend_from_slice(&zip_bytes);
        app
    }

    #[test]
    fn resolve_core_system_application() {
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

    /// (`al.symbolsCountryRegion` parity): localized core apps
    /// resolve to country-specific packages; platform packages and the "w1"
    /// worldwide marker stay unsuffixed.
    #[test]
    fn resolve_dependencies_applies_country_region_to_localized_apps() {
        let deps = vec![
            AppDependency {
                id: "c1335042-3002-4257-bf8a-75c898ccb1b8".to_string(),
                name: "Application".to_string(),
                publisher: "Microsoft".to_string(),
                version: "26.5.0.0".to_string(),
            },
            AppDependency {
                id: "437dbf0e-84ff-417a-965d-ed2bb9650972".to_string(),
                name: "Base Application".to_string(),
                publisher: "Microsoft".to_string(),
                version: "26.5.0.0".to_string(),
            },
            AppDependency {
                id: "8874ed3a-0643-4247-9ced-7a7002f7135d".to_string(),
                name: "System".to_string(),
                publisher: "Microsoft".to_string(),
                version: "26.0.0.0".to_string(),
            },
        ];
        let refs = resolve_dependencies_for_country(&deps, Some("de"));
        assert_eq!(refs[0].id, "Microsoft.Application.DE.symbols");
        assert_eq!(
            refs[1].id,
            "Microsoft.BaseApplication.DE.symbols.437dbf0e-84ff-417a-965d-ed2bb9650972"
        );
        // Platform is country-invariant.
        assert_eq!(refs[2].id, "Microsoft.Platform.symbols");

        // "w1" (and case variants) means worldwide — identical to None.
        let w1 = resolve_dependencies_for_country(&deps, Some("W1"));
        assert_eq!(w1[0].id, "Microsoft.Application.symbols");
        let none = resolve_dependencies_for_country(&deps, None);
        assert_eq!(none[0].id, "Microsoft.Application.symbols");
    }

    #[test]
    fn resolve_core_application() {
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
        assert_eq!(version_prefix("26.5.0.0"), "26.5.");
        assert_eq!(version_prefix("26.0.40469"), "26.0.");
        assert_eq!(version_prefix("12.3"), "12.3.");
    }

    #[test]
    fn version_prefix_single_component_falls_back() {
        // Fewer than two components: the whole string plus a trailing dot.
        assert_eq!(version_prefix("26"), "26.");
        assert_eq!(version_prefix(""), ".");
    }

    #[test]
    fn version_selection_is_numeric_and_does_not_depend_on_feed_order() {
        let versions = vec![
            "26.5.999.0".to_string(),
            "25.9.99999.0".to_string(),
            "26.5.12345.0".to_string(),
            "26.4.99999.0".to_string(),
        ];
        assert_eq!(
            select_version("pkg", Some("26.5.0.0"), &versions).unwrap(),
            "26.5.12345.0"
        );
        assert_eq!(
            select_version("pkg", None, &versions).unwrap(),
            "26.5.12345.0"
        );
    }

    #[test]
    fn version_selection_never_silently_crosses_requested_release_line() {
        let versions = vec!["25.5.0.0".to_string(), "27.0.0.0".to_string()];
        assert!(matches!(
            select_version("pkg", Some("26.5.0.0"), &versions),
            Err(NuGetError::VersionNotFound { .. })
        ));
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

        let mut nupkg_buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut nupkg_buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let options = SimpleFileOptions::default();

            zip.start_file("[Content_Types].xml", options).unwrap();
            zip.write_all(b"<xml/>").unwrap();

            zip.start_file("Microsoft.Application.symbols.app", options)
                .unwrap();
            zip.write_all(&valid_app_bytes()).unwrap();

            zip.finish().unwrap();
        }

        let dest = std::env::temp_dir().join("al-symbols-test-nupkg");
        let _ = std::fs::create_dir_all(&dest);

        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.to_str().unwrap().ends_with(".app"));

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
            zip.write_all(&valid_app_bytes()).unwrap();
            zip.finish().unwrap();
        }

        let dest = std::env::temp_dir().join("al-symbols-test-subfolder");
        let _ = std::fs::remove_dir_all(&dest);
        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        let path = result.expect("nested .app should extract");
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
            zip.write_all(&valid_app_bytes()).expect("write zip data");
            zip.finish().expect("finalize zip");
        }

        let dest = std::env::temp_dir().join("al-symbols-test-atomic");
        let _ = std::fs::create_dir_all(&dest);

        let result = extract_app_from_nupkg(&nupkg_buf, &dest, "Test");
        assert!(result.is_ok());

        // The .tmp file must not remain after a successful extraction.
        let tmp = dest.join("Test.app.tmp");
        assert!(!tmp.exists(), ".tmp file should have been renamed away");

        let app = dest.join("Test.app");
        assert!(app.exists(), ".app file should exist at final path");

        let _ = std::fs::remove_dir_all(&dest);
    }

    #[derive(Debug, serde::Deserialize)]
    struct DummyJson {
        ok: bool,
    }

    #[tokio::test]
    async fn fetch_metadata_json_accepts_small_response() {
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
    async fn fetch_metadata_json_accepts_bounded_chunked_response() {
        // Valid chunked feeds are useful and safe: the streaming reader applies
        // the same cap even when no Content-Length is present.
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
        let result: DummyJson = fetch_metadata_json(&client, &server.uri())
            .await
            .expect("bounded chunked response should be accepted");
        assert!(result.ok);
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

    #[tokio::test]
    async fn completed_download_is_reused_without_another_feed_request() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = tmp.path().join("Cached.app");
        std::fs::write(&artifact, valid_app_bytes()).unwrap();
        let client = NuGetClient::new(vec![]);
        let package = PackageRef {
            id: "Microsoft.Cached.symbols".into(),
            version: Some("1.0.0.0".into()),
            display_name: "Cached".into(),
            app_id: "test-id".into(),
        };
        client.completed_downloads.lock().unwrap().insert(
            (
                tmp.path().to_path_buf(),
                package.id.to_lowercase(),
                package.version.clone(),
            ),
            artifact.clone(),
        );

        let resolved = client
            .download(&package, tmp.path())
            .await
            .expect("completed artifact should be reused even with no feeds");
        assert_eq!(resolved, artifact);
    }

    #[test]
    fn completed_download_requires_matching_identity_and_minimum_version() {
        let tmp = tempfile::tempdir().unwrap();
        let artifact = tmp.path().join("Cached.app");
        std::fs::write(&artifact, app_bytes("expected-id", "2.1.0.0")).unwrap();

        let mut package = PackageRef {
            id: "Microsoft.Cached.symbols".into(),
            version: Some("2.0.0.0".into()),
            display_name: "Cached".into(),
            app_id: "EXPECTED-ID".into(),
        };
        assert!(nuget_manifest_satisfies(&artifact, &package));

        package.app_id = "different-id".into();
        assert!(!nuget_manifest_satisfies(&artifact, &package));

        package.app_id = "expected-id".into();
        package.version = Some("2.2.0.0".into());
        assert!(!nuget_manifest_satisfies(&artifact, &package));
    }

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
