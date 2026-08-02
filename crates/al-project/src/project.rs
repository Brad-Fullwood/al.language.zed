//! AL project discovery.
//!
//! Finds `app.json` manifests, locates `.alpackages`, and provides NuGet feed URLs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::DiscoveryError;

#[derive(Debug, Clone)]
pub struct AlProject {
    pub root: PathBuf,
    pub app_json: AppManifest,
    pub packages_dir: PathBuf,
    pub packages: Vec<PathBuf>,
    /// Server configs from launch.json for downloading symbols from a BC instance.
    pub server_configs: Vec<al_bc::launch::BcServerConfig>,
}

/// Fully resolved package-cache directory and deterministic `.app` selection
/// for one project's effective settings.
///
/// Package discovery does not require a valid `app.json`. Native verification
/// uses this before manifest validation so malformed manifests can still be
/// reported as structured compiler diagnostics instead of being short-circuited
/// by workspace discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolPackageSelection {
    pub packages_dir: PathBuf,
    pub packages: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    #[serde(default)]
    pub dependencies: Vec<AppDependency>,
    #[serde(default)]
    pub application: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
}

/// A dependency entry in app.json.
///
/// Canonical type from crate::symbols — unified so no field-for-field conversion is needed.
pub use al_types::AppDependency;

#[derive(Debug, Clone)]
pub struct NuGetFeed {
    pub name: String,
    pub index_url: String,
}

// Well-known BC package GUIDs for implicit dependencies.
const APPLICATION_APP_ID: &str = "c1335042-3002-4257-bf8a-75c898ccb1b8";
const BASE_APPLICATION_APP_ID: &str = "437dbf0e-84ff-417a-965d-ed2bb9650972";
const BUSINESS_FOUNDATION_APP_ID: &str = "f3552374-a1f2-4356-848e-196002525837";
const SYSTEM_APPLICATION_APP_ID: &str = "63ca2fa4-4f03-4f2b-a480-172fef340d3f";
const SYSTEM_APP_ID: &str = "8874ed3a-0643-4247-9ced-7a7002f7135d";

/// Derive the implicit System package's minimum version from project-owned
/// manifest data. `application` is the normal source because projects often
/// keep `platform = "1.0.0.0"` while targeting a current application release;
/// a platform-only/System-only project falls back to its own `platform`.
///
/// Never substitute a hard-coded "current BC" release here. That silently
/// changes the dependency graph as time passes and made older platform-only
/// projects request unrelated symbol packages.
fn implicit_system_version(manifest: &AppManifest) -> Option<String> {
    for version in [
        manifest.application.as_deref(),
        manifest.platform.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let major = version.split('.').next().unwrap_or_default();
        if !major.is_empty() && major.bytes().all(|byte| byte.is_ascii_digit()) {
            return Some(format!("{major}.0.0.0"));
        }
    }

    // Preserve malformed project input for the downstream validator/error
    // rather than inventing a dependency version unrelated to the manifest.
    manifest
        .platform
        .as_ref()
        .filter(|version| !version.trim().is_empty())
        .cloned()
}

impl AlProject {
    /// Compute the full dependency list including implicit BC dependencies.
    pub fn all_dependencies(&self) -> Vec<AppDependency> {
        let mut deps = self.app_json.dependencies.clone();

        if let Some(app_version) = &self.app_json.application {
            for (id, name) in [
                (APPLICATION_APP_ID, "Application"),
                (BASE_APPLICATION_APP_ID, "Base Application"),
                (BUSINESS_FOUNDATION_APP_ID, "Business Foundation"),
                (SYSTEM_APPLICATION_APP_ID, "System Application"),
            ] {
                if !deps.iter().any(|d| d.id.eq_ignore_ascii_case(id)) {
                    deps.push(AppDependency {
                        id: id.to_string(),
                        name: name.to_string(),
                        publisher: "Microsoft".to_string(),
                        version: app_version.clone(),
                    });
                }
            }
        }

        if self.app_json.platform.is_some()
            && !deps
                .iter()
                .any(|d| d.id.eq_ignore_ascii_case(SYSTEM_APP_ID))
        {
            if let Some(platform_version) = implicit_system_version(&self.app_json) {
                deps.push(AppDependency {
                    id: SYSTEM_APP_ID.to_string(),
                    name: "System".to_string(),
                    publisher: "Microsoft".to_string(),
                    version: platform_version,
                });
            }
        }

        deps
    }

    /// Apply the symbol-package paths from the merged workspace configuration.
    ///
    /// Relative paths are resolved from the directory containing `app.json`,
    /// matching how AL project settings are normally interpreted. The package
    /// cache is searched first, followed by `appLocalFolderPaths`; when the same
    /// package version appears in more than one folder, the earlier folder wins.
    pub fn apply_symbol_settings(
        &mut self,
        config: &crate::config::AlConfig,
    ) -> Result<(), DiscoveryError> {
        let selection = configured_symbol_packages(&self.root, config)?;
        self.packages_dir = selection.packages_dir;
        self.packages = selection.packages;
        Ok(())
    }
}

fn resolve_project_path(project_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    }
}

/// Resolve and scan every symbol-package folder configured for a project.
///
/// The primary cache wins exact filename collisions, followed by
/// `appLocalFolderPaths` in settings order. The scan itself is strict:
/// unreadable paths and non-directories are returned as errors, while missing
/// optional folders are treated as empty.
pub fn configured_symbol_packages(
    project_root: &Path,
    config: &crate::config::AlConfig,
) -> Result<SymbolPackageSelection, DiscoveryError> {
    let packages_dir = config
        .package_cache_path
        .as_deref()
        .map(|path| resolve_project_path(project_root, path))
        .unwrap_or_else(|| project_root.join(".alpackages"));

    let mut folders = Vec::with_capacity(1 + config.app_local_folder_paths.len());
    folders.push(packages_dir.clone());
    folders.extend(
        config
            .app_local_folder_paths
            .iter()
            .map(|path| resolve_project_path(project_root, path)),
    );
    let packages = scan_package_folders(&folders)?;
    Ok(SymbolPackageSelection {
        packages_dir,
        packages,
    })
}

pub fn find_project(start: &Path) -> Result<AlProject, DiscoveryError> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()?.join(start)
    };

    let mut searched = Vec::new();
    // A malformed `app.json` in the *start* directory is the user's own
    // project and stays a hard error. A malformed manifest anywhere else on
    // the walk (an unrelated `$HOME/app.json`, a broken sibling project) must
    // not hide a perfectly valid project further along the search; those are
    // recorded and only reported if nothing valid is found.
    let mut deferred_error: Option<DiscoveryError> = None;
    let try_dir = |dir: &Path,
                   deferred_error: &mut Option<DiscoveryError>|
     -> Result<Option<AlProject>, DiscoveryError> {
        match try_load_project(dir) {
            Ok(project) => Ok(project),
            Err(error) if dir == start => Err(error),
            Err(error) => {
                tracing::warn!(
                    path = %dir.display(),
                    %error,
                    "skipping directory with unloadable app.json during project discovery"
                );
                deferred_error.get_or_insert(error);
                Ok(None)
            }
        }
    };

    let mut current = Some(start.as_path());
    while let Some(dir) = current {
        searched.push(dir.to_path_buf());
        if let Some(project) = try_dir(dir, &mut deferred_error)? {
            return Ok(project);
        }
        current = dir.parent();
    }

    let entries =
        std::fs::read_dir(&start).map_err(|source| DiscoveryError::WorkspaceDirectory {
            path: start.clone(),
            source,
        })?;
    for entry in entries {
        let entry = entry.map_err(|source| DiscoveryError::WorkspaceDirectory {
            path: start.clone(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| DiscoveryError::WorkspaceDirectory {
                path: path.clone(),
                source,
            })?;
        if file_type.is_dir() {
            let dir_name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if dir_name.starts_with('.') || dir_name.eq_ignore_ascii_case("node_modules") {
                continue;
            }
            searched.push(path.clone());
            if let Some(project) = try_dir(&path, &mut deferred_error)? {
                return Ok(project);
            }
        }
    }

    // Nothing valid anywhere: a recorded manifest error explains the failure
    // better than a bare "no project found".
    if let Some(error) = deferred_error {
        return Err(error);
    }

    let searched_str = searched
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");

    Err(DiscoveryError::NoProjectFound {
        start: start.to_path_buf(),
        searched: searched_str,
    })
}

/// Maximum bytes accepted for an `app.json`. The largest legitimate manifest
/// we've observed across hundreds of AL projects is ~20 KB (heavy
/// `dependencies` + `idRanges` arrays). 1 MiB is two orders of magnitude past
/// that — more than enough headroom for any real project while still refusing
/// pathological inputs that would OOM the daemon on `read_to_string`.
const MAX_APP_JSON_BYTES: u64 = 1_048_576;

/// Load the exact `app.json` in `project_root` with the same size and schema
/// validation used by workspace discovery.
pub fn load_app_manifest(project_root: &Path) -> Result<AppManifest, DiscoveryError> {
    let app_json_path = project_root.join("app.json");
    let size = std::fs::metadata(&app_json_path)
        .map_err(|error| DiscoveryError::InvalidAppJson {
            path: app_json_path.clone(),
            error: format!("cannot inspect file: {error}"),
        })?
        .len();
    if size > MAX_APP_JSON_BYTES {
        return Err(DiscoveryError::InvalidAppJson {
            path: app_json_path.clone(),
            error: format!(
                "app.json is {} bytes — refusing to parse (cap = {} bytes)",
                size, MAX_APP_JSON_BYTES
            ),
        });
    }

    let content = std::fs::read_to_string(&app_json_path).map_err(|error| {
        DiscoveryError::InvalidAppJson {
            path: app_json_path.clone(),
            error: format!("cannot read file: {error}"),
        }
    })?;
    serde_json::from_str(&content).map_err(|error| DiscoveryError::InvalidAppJson {
        path: app_json_path,
        error: error.to_string(),
    })
}

fn try_load_project(dir: &Path) -> Result<Option<AlProject>, DiscoveryError> {
    let app_json_path = dir.join("app.json");
    if !app_json_path.is_file() {
        return Ok(None);
    }

    let manifest = load_app_manifest(dir)?;

    let packages_dir = dir.join(".alpackages");
    let packages = scan_packages(&packages_dir)?;
    let server_configs = al_bc::launch::find_launch_config(dir)?
        .map(|lf| lf.configs)
        .unwrap_or_default();

    Ok(Some(AlProject {
        root: dir.to_path_buf(),
        app_json: manifest,
        packages_dir,
        packages,
        server_configs,
    }))
}

fn scan_packages(packages_dir: &Path) -> Result<Vec<PathBuf>, DiscoveryError> {
    scan_package_folders(&[packages_dir.to_path_buf()])
}

/// Scan the configured package folders in priority order.
///
/// Directory iteration is sorted within each folder for deterministic startup.
/// Exact duplicate filenames are kept from the first (highest-priority) folder,
/// then versioned package filenames are collapsed to their newest version.
fn scan_package_folders(folders: &[PathBuf]) -> Result<Vec<PathBuf>, DiscoveryError> {
    let mut packages = Vec::new();
    let mut seen_filenames = std::collections::HashSet::new();

    for folder in folders {
        match std::fs::metadata(folder) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(DiscoveryError::PackageFolder {
                    path: folder.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "configured symbol package path is not a directory",
                    ),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %folder.display(), "symbol package folder does not exist; skipping");
                continue;
            }
            Err(source) => {
                return Err(DiscoveryError::PackageFolder {
                    path: folder.clone(),
                    source,
                });
            }
        }

        let entries =
            std::fs::read_dir(folder).map_err(|source| DiscoveryError::PackageFolder {
                path: folder.clone(),
                source,
            })?;
        let mut folder_packages = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| DiscoveryError::PackageFolder {
                path: folder.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| DiscoveryError::PackageFolder {
                    path: path.clone(),
                    source,
                })?;
            if !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
            {
                continue;
            }
            if file_type.is_file() {
                folder_packages.push(path);
            } else if file_type.is_symlink() {
                // Resolve the target: a dangling symlink or one pointing at a
                // directory is not a loadable package, and admitting it would
                // fail the whole atomic batch load later. Skip it with a
                // warning instead.
                match std::fs::metadata(&path) {
                    Ok(target) if target.is_file() => folder_packages.push(path),
                    Ok(_) => {
                        tracing::warn!(
                            path = %path.display(),
                            "ignoring .app symlink that resolves to a non-file"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            path = %path.display(),
                            %error,
                            "ignoring dangling .app symlink"
                        );
                    }
                }
            }
        }
        folder_packages.sort();

        for path in folder_packages {
            let filename = path
                .file_name()
                .map(|name| name.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if seen_filenames.insert(filename) {
                packages.push(path);
            }
        }
    }

    Ok(dedup_package_versions(packages))
}

/// Keep only the highest version when `.alpackages` holds several versions
/// of the same package (`Publisher_Name_1.0.0.0.app` + `_1.0.2.0.app`).
///
/// Real project folders accumulate old versions (every symbol download or
/// vendor update adds one); loading all of them indexed every object once
/// PER VERSION — duplicate search results, doubled symbol counts, and
/// ambiguous go-to-definition. Files whose names don't end in a parseable
/// dotted version are kept unconditionally (e.g. `System.app`).
fn dedup_package_versions(packages: Vec<PathBuf>) -> Vec<PathBuf> {
    fn split_versioned(path: &Path) -> Option<(String, Vec<u64>)> {
        let stem = path.file_stem()?.to_str()?;
        let (prefix, version) = stem.rsplit_once('_')?;
        let parts: Vec<u64> = version
            .split('.')
            .map(|p| p.parse::<u64>())
            .collect::<std::result::Result<_, _>>()
            .ok()?;
        if parts.is_empty() {
            return None;
        }
        Some((prefix.to_lowercase(), parts))
    }

    let mut best: std::collections::HashMap<String, (Vec<u64>, PathBuf)> =
        std::collections::HashMap::new();
    let mut unversioned: Vec<PathBuf> = Vec::new();

    for path in packages {
        match split_versioned(&path) {
            Some((key, version)) => match best.get(&key) {
                Some((existing, _)) if *existing >= version => {}
                _ => {
                    best.insert(key, (version, path));
                }
            },
            None => unversioned.push(path),
        }
    }

    let mut result: Vec<PathBuf> = best
        .into_values()
        .map(|(_, p)| p)
        .chain(unversioned)
        .collect();
    result.sort();
    result
}

pub fn nuget_feeds() -> Vec<NuGetFeed> {
    vec![
        NuGetFeed {
            name: "BC Symbols".into(),
            index_url: "https://dynamicssmb2.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/MSSymbols/nuget/v3/index.json".into(),
        },
        NuGetFeed {
            name: "AppSource Symbols".into(),
            index_url: "https://dynamicssmb2.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/AppSourceSymbols/nuget/v3/index.json".into(),
        },
        NuGetFeed {
            // Microsoft's full-apps feed (runtime packages, country
            // localizations). NOTE: an earlier revision pointed at
            // "BCPublic", which does not exist on Azure DevOps (404 —
            // TF1600011) and added a noisy failure to every download run.
            name: "MS Apps".into(),
            index_url: "https://dynamicssmb2.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/MSApps/nuget/v3/index.json".into(),
        },
    ]
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from).or({
        #[cfg(target_os = "windows")]
        {
            std::env::var("USERPROFILE").ok().map(PathBuf::from)
        }
        #[cfg(not(target_os = "windows"))]
        {
            None
        }
    })
}

#[cfg(test)]
mod dedup_tests {
    use super::dedup_package_versions;
    use std::path::PathBuf;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn keeps_only_highest_version_per_package() {
        let result = dedup_package_versions(paths(&[
            "/p/Microsoft_Application_27.3.44313.45677.app",
            "/p/Microsoft_Application_27.4.45366.45675.app",
            "/p/Insight Works_Product Configurator_4.0.9405.1.app",
            "/p/Insight Works_Product Configurator_4.0.9447.1.app",
        ]));
        let names: Vec<String> = result
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "Insight Works_Product Configurator_4.0.9447.1.app",
                "Microsoft_Application_27.4.45366.45675.app",
            ],
            "only the newest version of each package may remain"
        );
    }

    /// Files without a parseable version suffix are kept unconditionally,
    /// and different packages never collapse into each other.
    #[test]
    fn unversioned_and_distinct_packages_survive() {
        let result = dedup_package_versions(paths(&[
            "/p/System.app",
            "/p/Microsoft_Application_27.4.45366.45675.app",
            "/p/Microsoft_Base Application_27.4.45366.45675.app",
        ]));
        assert_eq!(result.len(), 3, "got: {result:?}");
    }

    /// Version comparison is numeric, not lexicographic: 10.0 > 9.0.
    #[test]
    fn version_compare_is_numeric() {
        let result = dedup_package_versions(paths(&[
            "/p/Vendor_App_9.0.0.0.app",
            "/p/Vendor_App_10.0.0.0.app",
        ]));
        assert_eq!(
            result[0].file_name().unwrap().to_string_lossy(),
            "Vendor_App_10.0.0.0.app"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AlConfig;
    use std::path::PathBuf;

    #[test]
    fn test_find_project_with_temp_dir() {
        let tmp = tempdir();
        let project_dir = tmp.join("my-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(
            project_dir.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "Test", "publisher": "Test", "version": "1.0.0.0"
            })
            .to_string(),
        )
        .unwrap();

        let project = find_project(&project_dir).unwrap();
        assert_eq!(project.root, project_dir);
        assert_eq!(project.app_json.name, "Test");
    }

    fn write_valid_manifest(dir: &Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "name": name, "publisher": "Test", "version": "1.0.0.0"
            })
            .to_string(),
        )
        .unwrap();
    }

    /// A malformed `app.json` in an *ancestor* directory (e.g. junk in
    /// `$HOME`) must not abort discovery before the child scan finds the
    /// user's valid project one level below the start.
    #[test]
    fn find_project_survives_malformed_ancestor_app_json() {
        let tmp = tempdir();
        std::fs::write(tmp.join("app.json"), "{ not json").unwrap();
        let start = tmp.join("workspace");
        std::fs::create_dir_all(&start).unwrap();
        let child = start.join("my-project");
        write_valid_manifest(&child, "Child Project");

        let project = find_project(&start).expect("valid child project must be discovered");
        assert_eq!(project.app_json.name, "Child Project");
        assert_eq!(project.root, child);
    }

    /// A malformed sibling subdirectory must not stop the scan from reaching
    /// a valid subdirectory project.
    #[test]
    fn find_project_skips_malformed_sibling_subdirectory() {
        let tmp = tempdir();
        let start = tmp.join("workspace");
        let broken = start.join("a-broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("app.json"), "{ definitely not json").unwrap();
        let valid = start.join("b-valid");
        write_valid_manifest(&valid, "Valid Project");

        let project = find_project(&start).expect("valid sibling project must be discovered");
        assert_eq!(project.app_json.name, "Valid Project");
    }

    /// The start directory's own malformed manifest is the user's project and
    /// stays a hard error; when nothing valid exists anywhere, a recorded
    /// manifest error is reported instead of a bare "no project found".
    #[test]
    fn find_project_reports_manifest_errors_when_nothing_valid_exists() {
        let tmp = tempdir();
        let start = tmp.join("direct");
        std::fs::create_dir_all(&start).unwrap();
        std::fs::write(start.join("app.json"), "{ nope").unwrap();
        assert!(matches!(
            find_project(&start),
            Err(DiscoveryError::InvalidAppJson { .. })
        ));

        let tmp2 = tempdir();
        let start2 = tmp2.join("workspace");
        std::fs::create_dir_all(&start2).unwrap();
        let broken = start2.join("only-broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join("app.json"), "{ nope").unwrap();
        assert!(matches!(
            find_project(&start2),
            Err(DiscoveryError::InvalidAppJson { .. })
        ));
    }

    /// Dangling `.app` symlinks (or symlinks to directories) must be skipped
    /// with a warning instead of entering the package list, where the atomic
    /// batch load would fail on them.
    #[cfg(unix)]
    #[test]
    fn scan_package_folders_skips_dangling_and_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let tmp = tempdir();
        let folder = tmp.join(".alpackages");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("Real.app"), b"NAVX").unwrap();
        symlink(tmp.join("missing-target.app"), folder.join("Dangling.app")).unwrap();
        let dir_target = tmp.join("a-directory.app");
        std::fs::create_dir_all(&dir_target).unwrap();
        symlink(&dir_target, folder.join("DirLink.app")).unwrap();
        // A symlink to a real file is still accepted.
        symlink(folder.join("Real.app"), folder.join("GoodLink.app")).unwrap();

        let packages =
            scan_package_folders(std::slice::from_ref(&folder)).expect("scan must not fail");
        let names: Vec<String> = packages
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"Real.app".to_string()));
        assert!(names.contains(&"GoodLink.app".to_string()));
        assert!(!names.contains(&"Dangling.app".to_string()));
        assert!(!names.contains(&"DirLink.app".to_string()));
    }

    #[test]
    fn test_app_json_oversize_is_rejected() {
        // Regression: pathological app.json should be refused before
        // `read_to_string` allocates. A 1 MiB cap is two orders of magnitude
        // past any legitimate manifest we've seen.
        let tmp = tempdir();
        let project_dir = tmp.join("oversize-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        // 2 MiB of valid JSON wrapping: well past the 1 MiB cap.
        let huge = format!(
            r#"{{"id":"00000000-0000-0000-0000-000000000000","name":"Test","publisher":"Test","version":"1.0.0.0","_pad":"{}"}}"#,
            "x".repeat(2 * 1024 * 1024)
        );
        std::fs::write(project_dir.join("app.json"), huge).unwrap();

        let result = try_load_project(&project_dir);
        assert!(result.is_err(), "oversize app.json must be rejected");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("refusing to parse"),
            "error must mention size refusal: {err}"
        );
    }

    #[test]
    fn test_app_json_at_cap_is_accepted() {
        // Manifests just under the cap should still load — we don't want the
        // size guard to clip legitimate (if unusual) projects.
        let tmp = tempdir();
        let project_dir = tmp.join("just-under-cap-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        // ~512 KiB of padding well within the 1 MiB cap.
        let manifest = format!(
            r#"{{"id":"00000000-0000-0000-0000-000000000000","name":"Test","publisher":"Test","version":"1.0.0.0","_pad":"{}"}}"#,
            "x".repeat(512 * 1024)
        );
        std::fs::write(project_dir.join("app.json"), manifest).unwrap();

        let result = try_load_project(&project_dir);
        assert!(
            result.is_ok(),
            "manifest under the cap must load: {result:?}"
        );
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_nuget_feeds_returns_three() {
        assert_eq!(nuget_feeds().len(), 3);
    }

    #[test]
    fn test_all_dependencies_includes_implicit() {
        let project = AlProject {
            root: PathBuf::from("/tmp/fake"),
            app_json: AppManifest {
                id: "00000000-0000-0000-0000-000000000000".into(),
                name: "Test".into(),
                publisher: "Test".into(),
                version: "1.0.0.0".into(),
                dependencies: vec![],
                application: Some("25.0.0.0".into()),
                platform: Some("25.0.0.0".into()),
                runtime: None,
            },
            packages_dir: PathBuf::from("/tmp/fake/.alpackages"),
            packages: vec![],
            server_configs: vec![],
        };
        assert!(project.all_dependencies().len() >= 5);
    }

    #[test]
    fn implicit_system_version_uses_application_release_not_platform_floor() {
        let manifest = AppManifest {
            id: String::new(),
            name: "Test".into(),
            publisher: "Test".into(),
            version: "1.0.0.0".into(),
            dependencies: vec![],
            application: Some("28.1.49838.0".into()),
            platform: Some("1.0.0.0".into()),
            runtime: None,
        };

        assert_eq!(
            implicit_system_version(&manifest).as_deref(),
            Some("28.0.0.0")
        );
    }

    #[test]
    fn implicit_system_version_uses_platform_for_system_only_project() {
        let manifest = AppManifest {
            id: String::new(),
            name: "Test".into(),
            publisher: "Test".into(),
            version: "1.0.0.0".into(),
            dependencies: vec![],
            application: None,
            platform: Some("24.3.0.0".into()),
            runtime: None,
        };

        assert_eq!(
            implicit_system_version(&manifest).as_deref(),
            Some("24.0.0.0")
        );
    }

    #[test]
    fn implicit_system_version_does_not_invent_current_release_for_bad_input() {
        let manifest = AppManifest {
            id: String::new(),
            name: "Test".into(),
            publisher: "Test".into(),
            version: "1.0.0.0".into(),
            dependencies: vec![],
            application: Some("not-a-version".into()),
            platform: Some("also-invalid".into()),
            runtime: None,
        };

        assert_eq!(
            implicit_system_version(&manifest).as_deref(),
            Some("also-invalid")
        );
    }

    #[test]
    fn symbol_settings_resolve_relative_paths_and_scan_all_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        let cache = root.join("custom-cache");
        let local = root.join("vendor-symbols");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(cache.join("Microsoft_System_27.0.0.0.app"), b"cache").unwrap();
        std::fs::write(local.join("Vendor_Library_1.0.0.0.APP"), b"local").unwrap();

        let mut project = AlProject {
            root: root.clone(),
            app_json: AppManifest {
                id: "00000000-0000-0000-0000-000000000000".into(),
                name: "Test".into(),
                publisher: "Test".into(),
                version: "1.0.0.0".into(),
                dependencies: vec![],
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: root.join(".alpackages"),
            packages: vec![],
            server_configs: vec![],
        };
        let config = AlConfig {
            package_cache_path: Some(PathBuf::from("custom-cache")),
            app_local_folder_paths: vec![PathBuf::from("vendor-symbols")],
            ..AlConfig::default()
        };

        project
            .apply_symbol_settings(&config)
            .expect("configured package folders");

        assert_eq!(project.packages_dir, cache);
        assert_eq!(project.packages.len(), 2);
        assert!(project.packages.iter().any(|path| path.starts_with(&local)));
    }

    #[test]
    fn configured_symbol_packages_do_not_require_a_parseable_manifest() {
        let root = tempdir();
        let cache = root.join(".alpackages");
        std::fs::create_dir_all(&cache).unwrap();
        let package = cache.join("Vendor_Library_1.0.0.0.app");
        std::fs::write(&package, b"package").unwrap();
        std::fs::write(root.join("app.json"), "{ definitely not json").unwrap();

        let selection = configured_symbol_packages(&root, &AlConfig::default())
            .expect("package discovery is independent of manifest validation");

        assert_eq!(selection.packages_dir, cache);
        assert_eq!(selection.packages, vec![package]);
    }

    #[test]
    fn symbol_settings_prefer_newest_version_across_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let primary = root.join(".alpackages");
        let local = root.join("local");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(primary.join("Vendor_App_1.0.0.0.app"), b"old").unwrap();
        let newest = local.join("Vendor_App_2.0.0.0.app");
        std::fs::write(&newest, b"new").unwrap();

        let mut project = AlProject {
            root: root.to_path_buf(),
            app_json: AppManifest {
                id: String::new(),
                name: "Test".into(),
                publisher: "Test".into(),
                version: "1.0.0.0".into(),
                dependencies: vec![],
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: primary,
            packages: vec![],
            server_configs: vec![],
        };
        project
            .apply_symbol_settings(&AlConfig {
                app_local_folder_paths: vec![local],
                ..AlConfig::default()
            })
            .expect("configured package folders");

        assert_eq!(project.packages, vec![newest]);
    }

    #[test]
    fn package_cache_wins_for_identical_filename() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let primary = root.join(".alpackages");
        let local = root.join("local");
        std::fs::create_dir_all(&primary).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        let primary_app = primary.join("Vendor_App_1.0.0.0.app");
        std::fs::write(&primary_app, b"primary").unwrap();
        std::fs::write(local.join("Vendor_App_1.0.0.0.app"), b"duplicate").unwrap();

        let result = scan_package_folders(&[primary, local]).unwrap();
        assert_eq!(result, vec![primary_app]);
    }

    #[test]
    fn malformed_launch_config_blocks_project_loading() {
        let root = tempdir();
        std::fs::write(
            root.join("app.json"),
            r#"{
                "id":"00000000-0000-0000-0000-000000000000",
                "name":"Test",
                "publisher":"Test",
                "version":"1.0.0.0"
            }"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".zed")).unwrap();
        std::fs::write(root.join(".zed/debug.json"), "{not json").unwrap();

        let error = try_load_project(&root).expect_err("invalid launch file must fail");
        assert!(matches!(error, DiscoveryError::LaunchConfiguration(_)));
    }

    #[test]
    fn failed_symbol_settings_update_retains_previous_generation() {
        let root = tempdir();
        let previous_dir = root.join(".alpackages");
        std::fs::create_dir_all(&previous_dir).unwrap();
        let previous_package = previous_dir.join("Previous_App_1.0.0.0.app");
        std::fs::write(&previous_package, b"previous").unwrap();
        let invalid_dir = root.join("not-a-directory");
        std::fs::write(&invalid_dir, b"file").unwrap();

        let mut project = AlProject {
            root: root.clone(),
            app_json: AppManifest {
                id: String::new(),
                name: "Test".into(),
                publisher: "Test".into(),
                version: "1.0.0.0".into(),
                dependencies: vec![],
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: previous_dir.clone(),
            packages: vec![previous_package.clone()],
            server_configs: vec![],
        };

        let error = project
            .apply_symbol_settings(&AlConfig {
                package_cache_path: Some(invalid_dir),
                ..AlConfig::default()
            })
            .expect_err("non-directory package path must fail");
        assert!(matches!(error, DiscoveryError::PackageFolder { .. }));
        assert_eq!(project.packages_dir, previous_dir);
        assert_eq!(project.packages, vec![previous_package]);
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "al-core-project-test-{}-{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
