//! AL project discovery.
//!
//! Finds `app.json` manifests, locates `.alpackages`, and provides NuGet feed URLs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::DiscoveryError;

// ---------------------------------------------------------------------------
// Project types
// ---------------------------------------------------------------------------

/// A discovered AL project on disk.
#[derive(Debug, Clone)]
pub struct AlProject {
    pub root: PathBuf,
    pub app_json: AppManifest,
    pub packages_dir: PathBuf,
    pub packages: Vec<PathBuf>,
    /// Server configs from launch.json for downloading symbols from a BC instance.
    pub server_configs: Vec<crate::launch::BcServerConfig>,
}

/// Parsed app.json manifest.
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
pub use crate::symbols::nuget::AppDependency;

/// A NuGet feed for BC symbol packages.
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

/// Fallback major version for implicit System package when an `app.json` has
/// `platform` set but no `application` to extract the major from. Tracks the
/// "current shipping" major BC release — bump on each major BC milestone.
/// Used only as a last resort; the typical happy path derives the major from
/// `app.json.application` (e.g. "26.0.0.0" → "26").
const CURRENT_BC_MAJOR_FALLBACK: &str = "26.0.0.0";

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
                if !deps.iter().any(|d| d.id == id) {
                    deps.push(AppDependency {
                        id: id.to_string(),
                        name: name.to_string(),
                        publisher: "Microsoft".to_string(),
                        version: app_version.clone(),
                    });
                }
            }
        }

        if self.app_json.platform.is_some() && !deps.iter().any(|d| d.id == SYSTEM_APP_ID) {
            let platform_version = self
                .app_json
                .application
                .as_ref()
                .and_then(|v| v.split('.').next())
                .map(|major| format!("{}.0.0.0", major))
                .unwrap_or_else(|| CURRENT_BC_MAJOR_FALLBACK.to_string());
            deps.push(AppDependency {
                id: SYSTEM_APP_ID.to_string(),
                name: "System".to_string(),
                publisher: "Microsoft".to_string(),
                version: platform_version,
            });
        }

        deps
    }
}

// ---------------------------------------------------------------------------
// Discovery functions
// ---------------------------------------------------------------------------

/// Find an AL project starting from the given directory, searching upward.
pub fn find_project(start: &Path) -> Result<AlProject, DiscoveryError> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()?.join(start)
    };

    let mut searched = Vec::new();

    let mut current = Some(start.as_path());
    while let Some(dir) = current {
        searched.push(dir.to_path_buf());
        if let Some(project) = try_load_project(dir)? {
            return Ok(project);
        }
        current = dir.parent();
    }

    if let Ok(entries) = std::fs::read_dir(&start) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let dir_name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if dir_name.starts_with('.') || dir_name == "node_modules" {
                    continue;
                }
                searched.push(path.clone());
                if let Some(project) = try_load_project(&path)? {
                    return Ok(project);
                }
            }
        }
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

fn try_load_project(dir: &Path) -> Result<Option<AlProject>, DiscoveryError> {
    let app_json_path = dir.join("app.json");
    if !app_json_path.is_file() {
        return Ok(None);
    }

    // Refuse pathological inputs before allocating. A 100 GB `app.json` on a
    // sparse filesystem would have OOM'd the daemon in `read_to_string`.
    let size = std::fs::metadata(&app_json_path)?.len();
    if size > MAX_APP_JSON_BYTES {
        return Err(DiscoveryError::InvalidAppJson {
            path: app_json_path.clone(),
            error: format!(
                "app.json is {} bytes — refusing to parse (cap = {} bytes)",
                size, MAX_APP_JSON_BYTES
            ),
        });
    }

    let content = std::fs::read_to_string(&app_json_path)?;
    let manifest: AppManifest =
        serde_json::from_str(&content).map_err(|e| DiscoveryError::InvalidAppJson {
            path: app_json_path.clone(),
            error: e.to_string(),
        })?;

    let packages_dir = dir.join(".alpackages");
    let packages = scan_packages(&packages_dir);
    let server_configs = crate::launch::find_launch_config(dir)
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

fn scan_packages(packages_dir: &Path) -> Vec<PathBuf> {
    if !packages_dir.is_dir() {
        return Vec::new();
    }

    let mut packages: Vec<PathBuf> = std::fs::read_dir(packages_dir)
        .into_iter()
        .flat_map(|entries| entries.into_iter())
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        })
        .collect();

    packages.sort();
    dedup_package_versions(packages)
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

/// Returns the 3 public BC NuGet feeds (Azure DevOps hosted).
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

/// Get the user's home directory.
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

    /// Audit 2026-06-12: .alpackages folders accumulate old versions; loading
    /// all of them indexed every object once per version (duplicate search
    /// rows, doubled counts). Only the highest version may survive.
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
