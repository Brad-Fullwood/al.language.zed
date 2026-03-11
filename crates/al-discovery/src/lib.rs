//! AL toolchain and project discovery.
//!
//! Finds ALTool installation, parses app.json manifests,
//! locates .alpackages, and provides NuGet feed URLs.

pub mod jsonrpc;
pub mod launch;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Paths to the AL toolchain components.
#[derive(Debug, Clone)]
pub struct AlToolchain {
    pub alc: PathBuf,
    pub aldoc: Option<PathBuf>,
    pub code_analysis: PathBuf,
    pub analyzers: AnalyzerPaths,
    pub dotnet_root: PathBuf,
    pub version: String,
}

/// Paths to the official Microsoft analyzers.
#[derive(Debug, Clone)]
pub struct AnalyzerPaths {
    pub code_cop: PathBuf,
    pub app_source_cop: PathBuf,
    pub ui_cop: PathBuf,
    pub per_tenant_cop: PathBuf,
    pub common: PathBuf,
}

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
    /// - `application` → Application + System Application packages
    /// - `platform` → System package (version derived from application major)
    pub fn all_dependencies(&self) -> Vec<AppDependency> {
        let mut deps = self.app_json.dependencies.clone();

        // Add implicit Application dependency chain.
        // "Application" is a stub that depends on "Base Application" + "Business Foundation".
        // We need all of them for complete symbol coverage.
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
        // The platform version in app.json is a minimum (often "1.0.0.0"),
        // so derive actual version from the application major version.
        if self.app_json.platform.is_some() {
            if !deps.iter().any(|d| d.id == SYSTEM_APP_ID) {
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
        }

        deps
    }
}

/// Errors with actionable messages.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("ALTool is not installed. Install it with: {install_cmd}")]
    AlToolNotInstalled { install_cmd: String },

    #[error(".NET SDK is not installed")]
    DotNetNotInstalled,

    #[error("No AL project found (no app.json). Searched from {start} upward through: {searched}")]
    NoProjectFound { start: PathBuf, searched: String },

    #[error("Invalid app.json at {path}: {error}")]
    InvalidAppJson { path: PathBuf, error: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const ALC_DLL: &str = "alc.dll";
const ALDOC_DLL: &str = "aldoc.dll";
const CODE_ANALYSIS_DLL: &str = "Microsoft.Dynamics.Nav.CodeAnalysis.dll";

const ANALYZER_DLLS: [(&str, &str); 5] = [
    ("code_cop", "Microsoft.Dynamics.Nav.CodeCop.dll"),
    ("app_source_cop", "Microsoft.Dynamics.Nav.AppSourceCop.dll"),
    ("ui_cop", "Microsoft.Dynamics.Nav.UICop.dll"),
    (
        "per_tenant_cop",
        "Microsoft.Dynamics.Nav.PerTenantExtensionCop.dll",
    ),
    ("common", "Microsoft.Dynamics.Nav.Analyzers.Common.dll"),
];

const DOTNET_TOOL_PACKAGE_PREFIX: &str = "microsoft.dynamics.businesscentral.development.tools";

const INSTALL_CMD: &str =
    "dotnet tool install --global Microsoft.Dynamics.BusinessCentral.Development.Tools";

// ---------------------------------------------------------------------------
// find_toolchain
// ---------------------------------------------------------------------------

/// Discover the AL toolchain (ALTool installation).
///
/// Search order:
/// 1. `$AL_TOOL_PATH` environment variable (points to the directory containing alc.dll)
/// 2. `~/.dotnet/tools/.store/microsoft.dynamics.businesscentral.development.tools*/`
/// 3. System PATH (`which alc`)
pub fn find_toolchain() -> Result<AlToolchain, DiscoveryError> {
    // Strategy 1: explicit env var
    if let Ok(tool_path) = std::env::var("AL_TOOL_PATH") {
        let dir = PathBuf::from(&tool_path);
        if dir.join(ALC_DLL).is_file() {
            return build_toolchain(&dir);
        }
        // The env var might point to a parent; search children
        if let Some(tc) = search_dir_recursive(&dir) {
            return Ok(tc);
        }
    }

    // Strategy 2: dotnet tool store
    if let Some(home) = home_dir() {
        let store = home.join(".dotnet/tools/.store");
        if store.is_dir() {
            if let Some(tc) = search_dotnet_tool_store(&store) {
                return Ok(tc);
            }
        }
    }

    // Strategy 3: system PATH — look for `alc` binary
    if let Some(tc) = search_system_path() {
        return Ok(tc);
    }

    Err(DiscoveryError::AlToolNotInstalled {
        install_cmd: INSTALL_CMD.to_string(),
    })
}

/// Build an `AlToolchain` from a directory known to contain `alc.dll`.
fn build_toolchain(dir: &Path) -> Result<AlToolchain, DiscoveryError> {
    let alc = dir.join(ALC_DLL);
    if !alc.is_file() {
        return Err(DiscoveryError::AlToolNotInstalled {
            install_cmd: INSTALL_CMD.to_string(),
        });
    }

    let aldoc = {
        let p = dir.join(ALDOC_DLL);
        if p.is_file() { Some(p) } else { None }
    };

    let code_analysis = dir.join(CODE_ANALYSIS_DLL);
    if !code_analysis.is_file() {
        return Err(DiscoveryError::AlToolNotInstalled {
            install_cmd: format!(
                "{INSTALL_CMD} (found alc.dll but missing {CODE_ANALYSIS_DLL} in {})",
                dir.display()
            ),
        });
    }

    let analyzers = find_analyzers(dir);
    let version = extract_version_from_path(dir);

    Ok(AlToolchain {
        alc,
        aldoc,
        code_analysis,
        analyzers,
        dotnet_root: dir.to_path_buf(),
        version,
    })
}

/// Find analyzer DLLs, looking in the given directory and common subdirectories.
fn find_analyzers(dir: &Path) -> AnalyzerPaths {
    let find_dll = |name: &str| -> PathBuf {
        let p = dir.join(name);
        if p.is_file() {
            return p;
        }
        for subdir in &["Analyzers", "analyzers"] {
            let p = dir.join(subdir).join(name);
            if p.is_file() {
                return p;
            }
        }
        dir.join(name)
    };

    AnalyzerPaths {
        code_cop: find_dll(ANALYZER_DLLS[0].1),
        app_source_cop: find_dll(ANALYZER_DLLS[1].1),
        ui_cop: find_dll(ANALYZER_DLLS[2].1),
        per_tenant_cop: find_dll(ANALYZER_DLLS[3].1),
        common: find_dll(ANALYZER_DLLS[4].1),
    }
}

/// Try to extract a version string from the directory path.
fn extract_version_from_path(dir: &Path) -> String {
    for component in dir.components().rev() {
        if let std::path::Component::Normal(s) = component {
            let s = s.to_string_lossy();
            if s.chars().next().map_or(false, |c| c.is_ascii_digit()) && s.contains('.') {
                let parts: Vec<&str> = s.split('.').collect();
                if parts.len() >= 2 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
                {
                    return s.to_string();
                }
            }
        }
    }
    "unknown".to_string()
}

/// Search the dotnet tool store for an ALTool installation.
fn search_dotnet_tool_store(store: &Path) -> Option<AlToolchain> {
    let entries = std::fs::read_dir(store).ok()?;

    let mut package_dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .to_lowercase()
                .starts_with(DOTNET_TOOL_PACKAGE_PREFIX)
        })
        .map(|e| e.path())
        .collect();

    package_dirs.sort();
    package_dirs.reverse();

    for pkg_dir in package_dirs {
        if let Some(tc) = search_dir_recursive(&pkg_dir) {
            return Some(tc);
        }
    }

    None
}

/// Recursively search a directory tree for `alc.dll`, returning the first valid toolchain found.
fn search_dir_recursive(root: &Path) -> Option<AlToolchain> {
    if root.join(ALC_DLL).is_file() {
        return build_toolchain(root).ok();
    }

    let mut queue: Vec<(PathBuf, u8)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = queue.pop() {
        if depth > 8 {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join(ALC_DLL).is_file() {
                    if let Ok(tc) = build_toolchain(&path) {
                        return Some(tc);
                    }
                }
                queue.push((path, depth + 1));
            }
        }
    }
    None
}

/// Search the system PATH for `alc` and derive the toolchain directory from it.
fn search_system_path() -> Option<AlToolchain> {
    let output = std::process::Command::new("which")
        .arg("alc")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let alc_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !alc_path.is_file() {
        return None;
    }

    let dir = alc_path.parent()?;

    if dir.join(ALC_DLL).is_file() {
        return build_toolchain(dir).ok();
    }

    search_dir_recursive(dir)
}

/// Get the user's home directory.
fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
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

// ---------------------------------------------------------------------------
// find_project
// ---------------------------------------------------------------------------

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
    // Handles multi-app workspaces where app.json lives in a child folder
    // (e.g., workspace root contains Core/, Implementation/, etc.).
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
                .map_or(false, |ext| ext.eq_ignore_ascii_case("app"))
        })
        .collect();

    packages.sort();
    packages
}

// ---------------------------------------------------------------------------
// nuget_feeds
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
    fn test_extract_version_from_path() {
        assert_eq!(
            extract_version_from_path(Path::new("/home/user/.dotnet/tools/.store/pkg/16.3.2065053/tools/net8.0/any")),
            "16.3.2065053"
        );
        assert_eq!(extract_version_from_path(Path::new("/opt/altool/25.1.0")), "25.1.0");
        assert_eq!(extract_version_from_path(Path::new("/opt/altool/bin")), "unknown");
    }

    #[test]
    fn test_error_messages_are_actionable() {
        let err = DiscoveryError::AlToolNotInstalled { install_cmd: INSTALL_CMD.to_string() };
        assert!(err.to_string().contains("dotnet tool install"));

        let err = DiscoveryError::NoProjectFound { start: PathBuf::from("/tmp/foo"), searched: "/tmp/foo, /tmp, /".to_string() };
        assert!(err.to_string().contains("app.json"));
    }

    #[test]
    fn test_build_toolchain_flat_dir() {
        let tmp = tempdir();
        let tool_dir = tmp.join("altool");
        fs::create_dir_all(&tool_dir).unwrap();
        fs::write(tool_dir.join(ALC_DLL), b"fake alc").unwrap();
        fs::write(tool_dir.join(CODE_ANALYSIS_DLL), b"fake").unwrap();
        for (_, dll) in &ANALYZER_DLLS {
            fs::write(tool_dir.join(dll), b"fake").unwrap();
        }

        let tc = build_toolchain(&tool_dir).unwrap();
        assert_eq!(tc.alc, tool_dir.join(ALC_DLL));
        assert_eq!(tc.code_analysis, tool_dir.join(CODE_ANALYSIS_DLL));
        assert!(tc.aldoc.is_none());
    }

    #[test]
    fn test_build_toolchain_with_aldoc() {
        let tmp = tempdir();
        let tool_dir = tmp.join("altool-with-aldoc");
        fs::create_dir_all(&tool_dir).unwrap();
        fs::write(tool_dir.join(ALC_DLL), b"fake").unwrap();
        fs::write(tool_dir.join(ALDOC_DLL), b"fake").unwrap();
        fs::write(tool_dir.join(CODE_ANALYSIS_DLL), b"fake").unwrap();
        for (_, dll) in &ANALYZER_DLLS { fs::write(tool_dir.join(dll), b"fake").unwrap(); }

        let tc = build_toolchain(&tool_dir).unwrap();
        assert_eq!(tc.aldoc, Some(tool_dir.join(ALDOC_DLL)));
    }

    #[test]
    fn test_build_toolchain_missing_code_analysis() {
        let tmp = tempdir();
        let tool_dir = tmp.join("missing-ca");
        fs::create_dir_all(&tool_dir).unwrap();
        fs::write(tool_dir.join(ALC_DLL), b"fake").unwrap();
        let err = build_toolchain(&tool_dir).unwrap_err();
        assert!(err.to_string().contains(CODE_ANALYSIS_DLL));
    }

    #[test]
    fn test_search_dir_recursive_nested() {
        let tmp = tempdir();
        let nested = tmp.join("dotnet-store/pkg/16.3.2065053/tools/net8.0/any");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join(ALC_DLL), b"fake").unwrap();
        fs::write(nested.join(CODE_ANALYSIS_DLL), b"fake").unwrap();
        for (_, dll) in &ANALYZER_DLLS { fs::write(nested.join(dll), b"fake").unwrap(); }

        let tc = search_dir_recursive(&tmp.join("dotnet-store")).expect("Should find toolchain");
        assert_eq!(tc.alc, nested.join(ALC_DLL));
        assert_eq!(tc.version, "16.3.2065053");
    }

    #[test]
    fn test_find_analyzers_in_subdirectory() {
        let tmp = tempdir();
        let tool_dir = tmp.join("altool-sub");
        let analyzers_dir = tool_dir.join("Analyzers");
        fs::create_dir_all(&analyzers_dir).unwrap();
        fs::write(tool_dir.join(ALC_DLL), b"fake").unwrap();
        fs::write(tool_dir.join(CODE_ANALYSIS_DLL), b"fake").unwrap();
        for (_, dll) in &ANALYZER_DLLS { fs::write(analyzers_dir.join(dll), b"fake").unwrap(); }

        let tc = build_toolchain(&tool_dir).unwrap();
        assert_eq!(tc.analyzers.code_cop, analyzers_dir.join(ANALYZER_DLLS[0].1));
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
        // Should include Application, Base Application, Business Foundation, System Application, System
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
        // If System Application is already in dependencies, it should not be duplicated
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
    fn test_extract_version_edge_cases() {
        // No version components
        assert_eq!(extract_version_from_path(Path::new("/")), "unknown");
        // Single digit version
        assert_eq!(extract_version_from_path(Path::new("/opt/tools/1.0")), "1.0");
        // Path with non-version numbers
        assert_eq!(extract_version_from_path(Path::new("/home/user123/tools")), "unknown");
    }

    #[test]
    fn test_app_manifest_with_extra_fields() {
        // app.json may contain additional fields not in our struct — serde should ignore them
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
        // This should not fail — serde default is to ignore unknown fields unless deny_unknown_fields
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

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("al-discovery-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
