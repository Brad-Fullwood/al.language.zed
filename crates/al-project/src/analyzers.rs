//! Deterministic discovery of explicitly requested third-party AL analyzers.
//!
//! Built-in Microsoft analyzers are resolved by the selected AL toolchain.
//! This module handles custom analyzer names and paths across project-local,
//! configured probing, NuGet-global, and common editor-extension locations.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

const MAX_SCAN_ENTRIES: usize = 50_000;
const MAX_SCAN_DEPTH: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum AnalyzerDiscoveryError {
    #[error("configured analyzer path '{}' does not identify a file", .0.display())]
    MissingExplicitPath(PathBuf),
    #[error("failed to inspect analyzer search path '{}': {source}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "analyzer search below '{}' exceeded the safety limit of {MAX_SCAN_ENTRIES} entries",
        .0.display()
    )]
    ScanLimit(PathBuf),
}

/// Whether `name` denotes one of Microsoft's analyzer assemblies shipped with
/// the AL toolchain. The alias mirrors Microsoft's DLL name.
#[must_use]
pub fn is_builtin_analyzer(name: &str) -> bool {
    matches!(
        analyzer_name(name).to_ascii_lowercase().as_str(),
        "codecop" | "appsourcecop" | "uicop" | "pertenantcop" | "pertenantextensioncop"
    )
}

/// Return an analyzer entry without a case-insensitive trailing `.dll`.
#[must_use]
pub fn analyzer_name(name: &str) -> &str {
    name.get(..name.len().saturating_sub(4))
        .filter(|_| {
            name.get(name.len().saturating_sub(4)..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".dll"))
        })
        .unwrap_or(name)
}

/// Resolve one explicitly requested custom analyzer.
///
/// `Ok(None)` means no matching analyzer was installed in any supported
/// location. Explicit absolute or path-containing entries are different:
/// their absence is a configuration error and returns `Err`.
pub fn discover_custom_analyzer(
    entry: &str,
    project_root: &Path,
    assembly_probing_paths: &[PathBuf],
) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
    let entry = entry.trim();
    if entry.is_empty() || is_builtin_analyzer(entry) {
        return Ok(None);
    }

    let configured_path = Path::new(entry);
    let contains_separator =
        entry.contains(std::path::MAIN_SEPARATOR) || entry.contains('/') || entry.contains('\\');
    if configured_path.is_absolute() || contains_separator {
        let path = if configured_path.is_absolute() {
            configured_path.to_path_buf()
        } else {
            project_root.join(configured_path)
        };
        return canonical_file(&path)
            .map(Some)
            .ok_or(AnalyzerDiscoveryError::MissingExplicitPath(path));
    }

    let file_name = if entry
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("dll"))
    {
        entry.to_string()
    } else {
        format!("{entry}.dll")
    };

    // Explicit probing paths have highest priority and are searched in the
    // order configured by the user.
    for configured in assembly_probing_paths {
        let root = if configured.is_absolute() {
            configured.clone()
        } else {
            project_root.join(configured)
        };
        if let Some(path) = find_best_below(&root, &file_name, true)? {
            return Ok(Some(path));
        }
    }

    for root in [
        project_root.join(".netpackages"),
        project_root.join("packages"),
    ] {
        if let Some(path) = find_best_below(&root, &file_name, false)? {
            return Ok(Some(path));
        }
    }

    let package_name = analyzer_name(entry).to_ascii_lowercase();
    let mut nuget_roots = Vec::new();
    if let Some(path) = std::env::var_os("NUGET_PACKAGES") {
        nuget_roots.push(PathBuf::from(path));
    }
    if let Some(home) = crate::project::home_dir() {
        nuget_roots.push(home.join(".nuget/packages"));
    }
    dedup_paths(&mut nuget_roots);
    for root in nuget_roots {
        // NuGet's global-packages layout lowercases the package ID, so narrow
        // the scan to the requested package rather than traversing the entire
        // global cache.
        let package_root = root.join(&package_name);
        if let Some(path) = find_best_below(&package_root, &file_name, false)? {
            return Ok(Some(path));
        }
    }

    if let Some(home) = crate::project::home_dir() {
        for extension_root in [
            home.join(".vscode/extensions"),
            home.join(".vscode-insiders/extensions"),
        ] {
            for candidate_root in matching_immediate_directories(&extension_root, &package_name)? {
                if let Some(path) = find_best_below(&candidate_root, &file_name, false)? {
                    return Ok(Some(path));
                }
            }
        }
    }

    Ok(None)
}

fn canonical_file(path: &Path) -> Option<PathBuf> {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file())
        .and_then(|_| path.canonicalize().ok())
}

fn find_best_below(
    root: &Path,
    file_name: &str,
    required_root: bool,
) -> Result<Option<PathBuf>, AnalyzerDiscoveryError> {
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !required_root => {
            return Ok(None);
        }
        Err(source) => {
            return Err(AnalyzerDiscoveryError::Inspect {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    if metadata.is_file() {
        if root
            .file_name()
            .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case(file_name))
        {
            return Ok(canonical_file(root));
        }
        return Ok(None);
    }
    if !metadata.is_dir() {
        return Ok(None);
    }

    let mut inspected = 0usize;
    let mut candidates = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = stack.pop() {
        let entries =
            std::fs::read_dir(&directory).map_err(|source| AnalyzerDiscoveryError::Inspect {
                path: directory.clone(),
                source,
            })?;
        for entry in entries {
            let entry = entry.map_err(|source| AnalyzerDiscoveryError::Inspect {
                path: directory.clone(),
                source,
            })?;
            inspected += 1;
            if inspected > MAX_SCAN_ENTRIES {
                return Err(AnalyzerDiscoveryError::ScanLimit(root.to_path_buf()));
            }
            let path = entry.path();
            let file_type =
                entry
                    .file_type()
                    .map_err(|source| AnalyzerDiscoveryError::Inspect {
                        path: path.clone(),
                        source,
                    })?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(file_name)
            {
                if let Some(path) = canonical_file(&path) {
                    candidates.push(path);
                }
            } else if file_type.is_dir() && depth < MAX_SCAN_DEPTH {
                stack.push((path, depth + 1));
            }
        }
    }

    candidates.sort_by(compare_versioned_paths);
    Ok(candidates.pop())
}

fn compare_versioned_paths(left: &PathBuf, right: &PathBuf) -> Ordering {
    version_key(left)
        .cmp(&version_key(right))
        .then_with(|| left.cmp(right))
}

fn version_key(path: &Path) -> Vec<u64> {
    path.components()
        .filter_map(|component| {
            let text = component.as_os_str().to_string_lossy();
            let numbers: Vec<u64> = text
                .split(['.', '-', '+'])
                .map(str::parse)
                .collect::<Result<_, _>>()
                .ok()?;
            (!numbers.is_empty()).then_some(numbers)
        })
        .max()
        .unwrap_or_default()
}

fn matching_immediate_directories(
    root: &Path,
    package_name: &str,
) -> Result<Vec<PathBuf>, AnalyzerDiscoveryError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(AnalyzerDiscoveryError::Inspect {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let compact_name = package_name.replace(['.', '-', '_'], "");
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| AnalyzerDiscoveryError::Inspect {
            path: root.to_path_buf(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| AnalyzerDiscoveryError::Inspect {
                path: entry.path(),
                source,
            })?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let compact = name.replace(['.', '-', '_'], "");
        if name.contains(package_name) || compact.contains(&compact_name) {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
}

fn dedup_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probing_path_finds_nested_analyzer_case_insensitively() {
        let project = tempfile::tempdir().unwrap();
        let probing = project.path().join("tools/analyzers/net8.0");
        std::fs::create_dir_all(&probing).unwrap();
        let dll = probing.join("BusinessCentral.LinterCop.DLL");
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover_custom_analyzer(
            "BusinessCentral.LinterCop",
            project.path(),
            &[PathBuf::from("tools")],
        )
        .unwrap()
        .expect("analyzer");
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    #[test]
    fn project_package_discovery_selects_highest_numeric_version() {
        let project = tempfile::tempdir().unwrap();
        let package = project
            .path()
            .join(".netpackages/businesscentral.lintercop");
        let old = package.join("1.9.0/lib/net8.0");
        let new = package.join("1.10.0/lib/net8.0");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("BusinessCentral.LinterCop.dll"), b"old").unwrap();
        let expected = new.join("BusinessCentral.LinterCop.dll");
        std::fs::write(&expected, b"new").unwrap();

        let found = discover_custom_analyzer("BusinessCentral.LinterCop", project.path(), &[])
            .unwrap()
            .expect("analyzer");
        assert_eq!(found, expected.canonicalize().unwrap());
    }

    #[test]
    fn explicit_relative_path_is_resolved_against_project() {
        let project = tempfile::tempdir().unwrap();
        let dll = project.path().join("analyzers/Custom.dll");
        std::fs::create_dir_all(dll.parent().unwrap()).unwrap();
        std::fs::write(&dll, b"analyzer").unwrap();

        let found = discover_custom_analyzer("analyzers/Custom.dll", project.path(), &[])
            .unwrap()
            .expect("analyzer");
        assert_eq!(found, dll.canonicalize().unwrap());
    }

    #[test]
    fn missing_explicit_path_is_an_error() {
        let project = tempfile::tempdir().unwrap();
        let result = discover_custom_analyzer("analyzers/Missing.dll", project.path(), &[]);
        assert!(matches!(
            result,
            Err(AnalyzerDiscoveryError::MissingExplicitPath(_))
        ));
    }

    #[test]
    fn builtin_dll_suffix_is_case_insensitive() {
        assert!(is_builtin_analyzer("CodeCop.DLL"));
        assert!(is_builtin_analyzer("PerTenantExtensionCop.dLl"));
        assert_eq!(
            analyzer_name("BusinessCentral.LinterCop.DLL"),
            "BusinessCentral.LinterCop"
        );
    }
}
