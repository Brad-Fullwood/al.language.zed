//! AL project discovery and manifest parsing.
//!
//! Finds `app.json` manifests, parses them, locates `.alpackages`,
//! and provides NuGet feed URLs for BC symbol packages.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::DiscoveryError;
use crate::launch;

/// A discovered AL project on disk.
#[derive(Debug, Clone)]
pub struct AlProject {
    pub root: PathBuf,
    pub app_json: AppManifest,
    pub packages_dir: PathBuf,
    pub packages: Vec<PathBuf>,
    /// Server configs from launch.json for downloading symbols from a BC instance.
    pub server_configs: Vec<launch::BcServerConfig>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDependency {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
}

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

impl AlProject {
    /// Compute the full dependency list including implicit BC dependencies.
    ///
    /// BC projects have implicit dependencies derived from `application` and `platform`
    /// properties in app.json:
    /// - `application` -> Application + System Application packages
    /// - `platform` -> System package (version derived from application major)
    pub fn all_dependencies(&self) -> Vec<AppDependency> {
        let mut deps = self.app_json.dependencies.clone();

        // Add implicit Application dependency chain.
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

        // Add implicit System (platform) dependency.
        if self.app_json.platform.is_some()
            && !deps.iter().any(|d| d.id == SYSTEM_APP_ID)
        {
            let platform_version = self
                .app_json
                .application
                .as_ref()
                .and_then(|v| v.split('.').next())
                .map(|major| format!("{}.0.0.0", major))
                .unwrap_or_else(|| "26.0.0.0".to_string());
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

/// Find an AL project starting from the given directory, searching upward.
pub fn find_project(start: &Path) -> Result<AlProject, DiscoveryError> {
    let start = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()?.join(start)
    };

    let mut searched = Vec::new();

    // Phase 1: Search upward from start directory
    let mut current = Some(start.as_path());
    while let Some(dir) = current {
        searched.push(dir.to_path_buf());
        if let Some(project) = try_load_project(dir)? {
            return Ok(project);
        }
        current = dir.parent();
    }

    // Phase 2: Search immediate subdirectories of the start directory.
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

/// Try to load an AL project from a specific directory (checks for app.json).
fn try_load_project(dir: &Path) -> Result<Option<AlProject>, DiscoveryError> {
    let app_json_path = dir.join("app.json");
    if !app_json_path.is_file() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&app_json_path)?;
    let manifest: AppManifest =
        serde_json::from_str(&content).map_err(|e| DiscoveryError::InvalidAppJson {
            path: app_json_path.clone(),
            error: e.to_string(),
        })?;

    let packages_dir = dir.join(".alpackages");
    let packages = scan_packages(&packages_dir);
    let server_configs = launch::find_launch_config(dir)
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

/// Scan a `.alpackages/` directory for `.app` files.
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
    packages
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
            name: "BC Public".into(),
            index_url: "https://dynamicssmb2.pkgs.visualstudio.com/DynamicsBCPublicFeeds/_packaging/BCPublic/nuget/v3/index.json".into(),
        },
    ]
}

/// Get the user's home directory.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or({
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
mod tests {
    use super::*;
    use std::fs;

    fn minimal_app_json() -> String {
        serde_json::json!({
            "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "name": "My AL App",
            "publisher": "Test Publisher",
            "version": "1.0.0.0",
            "dependencies": [
                {
                    "id": "63ca2fa4-4f03-4f2b-a480-172fef340d3f",
                    "name": "System Application",
                    "publisher": "Microsoft",
                    "version": "25.0.0.0"
                }
            ],
            "application": "25.0.0.0",
            "platform": "25.0.0.0",
            "runtime": "14.0"
        })
        .to_string()
    }

    #[test]
    fn test_app_manifest_deserialization() {
        let json = minimal_app_json();
        let manifest: AppManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.id, "a1b2c3d4-e5f6-7890-abcd-ef1234567890");
        assert_eq!(manifest.name, "My AL App");
        assert_eq!(manifest.publisher, "Test Publisher");
        assert_eq!(manifest.version, "1.0.0.0");
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].name, "System Application");
        assert_eq!(manifest.application, Some("25.0.0.0".to_string()));
    }

    #[test]
    fn test_app_manifest_minimal_fields() {
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "Minimal",
            "publisher": "Nobody",
            "version": "0.0.0.0"
        })
        .to_string();
        let manifest: AppManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.name, "Minimal");
        assert!(manifest.dependencies.is_empty());
        assert!(manifest.application.is_none());
    }

    #[test]
    fn test_find_project_with_temp_dir() {
        let tmp = tempdir();
        let project_dir = tmp.join("my-project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("app.json"), minimal_app_json()).unwrap();
        let packages_dir = project_dir.join(".alpackages");
        fs::create_dir_all(&packages_dir).unwrap();
        fs::write(packages_dir.join("Base.app"), b"fake").unwrap();
        fs::write(packages_dir.join("System.app"), b"fake").unwrap();
        fs::write(packages_dir.join("readme.txt"), b"not a package").unwrap();

        let project = find_project(&project_dir).unwrap();
        assert_eq!(project.root, project_dir);
        assert_eq!(project.app_json.name, "My AL App");
        assert_eq!(project.packages.len(), 2);
    }

    #[test]
    fn test_find_project_searches_upward() {
        let tmp = tempdir();
        let project_dir = tmp.join("workspace");
        let subdir = project_dir.join("src").join("deep").join("nested");
        fs::create_dir_all(&subdir).unwrap();
        fs::write(project_dir.join("app.json"), minimal_app_json()).unwrap();

        let project = find_project(&subdir).unwrap();
        assert_eq!(project.root, project_dir);
    }

    #[test]
    fn test_find_project_no_app_json() {
        let tmp = tempdir();
        let empty_dir = tmp.join("empty");
        fs::create_dir_all(&empty_dir).unwrap();
        let result = find_project(&empty_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No AL project found"));
    }

    #[test]
    fn test_find_project_invalid_app_json() {
        let tmp = tempdir();
        let project_dir = tmp.join("bad-project");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("app.json"), "{ not valid json }").unwrap();
        let result = find_project(&project_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid app.json"));
    }

    #[test]
    fn test_nuget_feeds_returns_three() {
        let feeds = nuget_feeds();
        assert_eq!(feeds.len(), 3);
        let names: Vec<&str> = feeds.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"AppSource Symbols"));
        assert!(names.contains(&"BC Symbols"));
        assert!(names.contains(&"BC Public"));
    }

    #[test]
    fn test_all_dependencies_includes_implicit() {
        let project = AlProject {
            root: PathBuf::from("/tmp/fake"),
            app_json: AppManifest {
                id: "00000000-0000-0000-0000-000000000000".to_string(),
                name: "Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: vec![],
                application: Some("25.0.0.0".to_string()),
                platform: Some("25.0.0.0".to_string()),
                runtime: Some("14.0".to_string()),
            },
            packages_dir: PathBuf::from("/tmp/fake/.alpackages"),
            packages: vec![],
            server_configs: vec![],
        };
        let all = project.all_dependencies();
        assert!(all.len() >= 5, "Expected at least 5 implicit deps, got {}", all.len());
        let names: Vec<&str> = all.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"Application"));
        assert!(names.contains(&"Base Application"));
        assert!(names.contains(&"Business Foundation"));
        assert!(names.contains(&"System Application"));
        assert!(names.contains(&"System"));
    }

    #[test]
    fn test_all_dependencies_no_implicit_without_application() {
        let project = AlProject {
            root: PathBuf::from("/tmp/fake"),
            app_json: AppManifest {
                id: "00000000-0000-0000-0000-000000000000".to_string(),
                name: "Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: vec![],
                application: None,
                platform: None,
                runtime: None,
            },
            packages_dir: PathBuf::from("/tmp/fake/.alpackages"),
            packages: vec![],
            server_configs: vec![],
        };
        let all = project.all_dependencies();
        assert!(all.is_empty(), "No implicit deps when application/platform are None");
    }

    #[test]
    fn test_all_dependencies_no_duplicate_system_app() {
        let project = AlProject {
            root: PathBuf::from("/tmp/fake"),
            app_json: AppManifest {
                id: "00000000-0000-0000-0000-000000000000".to_string(),
                name: "Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: vec![
                    AppDependency {
                        id: SYSTEM_APPLICATION_APP_ID.to_string(),
                        name: "System Application".to_string(),
                        publisher: "Microsoft".to_string(),
                        version: "25.0.0.0".to_string(),
                    },
                ],
                application: Some("25.0.0.0".to_string()),
                platform: Some("1.0.0.0".to_string()),
                runtime: None,
            },
            packages_dir: PathBuf::from("/tmp/fake/.alpackages"),
            packages: vec![],
            server_configs: vec![],
        };
        let all = project.all_dependencies();
        let sys_app_count = all.iter().filter(|d| d.id == SYSTEM_APPLICATION_APP_ID).count();
        assert_eq!(sys_app_count, 1, "System Application should not be duplicated");
    }

    #[test]
    fn test_platform_version_derived_from_application_major() {
        let project = AlProject {
            root: PathBuf::from("/tmp/fake"),
            app_json: AppManifest {
                id: "00000000-0000-0000-0000-000000000000".to_string(),
                name: "Test".to_string(),
                publisher: "Test".to_string(),
                version: "1.0.0.0".to_string(),
                dependencies: vec![],
                application: Some("26.1.2.3".to_string()),
                platform: Some("1.0.0.0".to_string()),
                runtime: None,
            },
            packages_dir: PathBuf::from("/tmp/fake/.alpackages"),
            packages: vec![],
            server_configs: vec![],
        };
        let all = project.all_dependencies();
        let system = all.iter().find(|d| d.name == "System").expect("Should have System dep");
        assert_eq!(system.version, "26.0.0.0", "Platform version should derive from application major");
    }

    #[test]
    fn test_scan_packages_filters_app_only() {
        let tmp = tempdir();
        let pkg_dir = tmp.join("mixed");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("good.app"), b"data").unwrap();
        fs::write(pkg_dir.join("also.APP"), b"data").unwrap();
        fs::write(pkg_dir.join("skip.txt"), b"data").unwrap();
        let packages = scan_packages(&pkg_dir);
        assert_eq!(packages.len(), 2);
    }

    #[test]
    fn test_scan_packages_empty_dir() {
        let tmp = tempdir();
        let pkg_dir = tmp.join("empty-packages");
        fs::create_dir_all(&pkg_dir).unwrap();
        let packages = scan_packages(&pkg_dir);
        assert!(packages.is_empty());
    }

    #[test]
    fn test_scan_packages_nonexistent_dir() {
        let packages = scan_packages(Path::new("/nonexistent/path/packages"));
        assert!(packages.is_empty());
    }

    #[test]
    fn test_app_manifest_with_extra_fields() {
        let json = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "Test",
            "publisher": "Publisher",
            "version": "1.0.0.0",
            "target": "Cloud",
            "idRanges": [{ "from": 50100, "to": 50199 }],
            "extraUnknownField": true
        })
        .to_string();
        let manifest: Result<AppManifest, _> = serde_json::from_str(&json);
        assert!(manifest.is_ok(), "Should deserialize with unknown fields");
    }

    #[test]
    fn test_nuget_feeds_urls_are_valid() {
        let feeds = nuget_feeds();
        for feed in &feeds {
            assert!(feed.index_url.starts_with("https://"), "Feed URL should use HTTPS: {}", feed.index_url);
            assert!(feed.index_url.ends_with("index.json"), "Feed URL should end with index.json: {}", feed.index_url);
            assert!(feed.index_url.contains("dynamicssmb2"), "Feed URL should use dynamicssmb2 domain: {}", feed.index_url);
        }
    }

    #[test]
    fn test_error_messages_are_actionable() {
        let err = DiscoveryError::NoProjectFound { start: PathBuf::from("/tmp/foo"), searched: "/tmp/foo, /tmp, /".to_string() };
        assert!(err.to_string().contains("app.json"));
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("al-core-project-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
