//! AL project discovery.
//!
//! Finds `app.json` manifests, locates `.alpackages`, and provides NuGet feed URLs.

use std::path::{Path, PathBuf};

use crate::errors::DiscoveryError;
use crate::{AlProject, AppManifest, NuGetFeed};

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
