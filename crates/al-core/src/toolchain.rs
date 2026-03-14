//! AL toolchain discovery.
//!
//! Finds ALTool installation, validates components, and provides
//! paths to the compiler, analyzers, and .NET runtime.

use std::path::{Path, PathBuf};

use crate::errors::DiscoveryError;
use crate::project::home_dir;

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

    // Strategy 3: system PATH
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
            if s.chars().next().is_some_and(|c| c.is_ascii_digit()) && s.contains('.') {
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
    let entries = std::fs::read_dir(store).ok()?; // ok(): read_dir failure means store doesn't exist, non-fatal

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

/// Recursively search a directory tree for `alc.dll`.
fn search_dir_recursive(root: &Path) -> Option<AlToolchain> {
    if root.join(ALC_DLL).is_file() {
        // Non-critical search path: if build fails here, keep searching
        if let Ok(tc) = build_toolchain(root) {
            return Some(tc);
        }
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
        .ok()?; // ok(): `which` binary missing is non-fatal for toolchain search

    if !output.status.success() {
        return None;
    }

    let alc_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !alc_path.is_file() {
        return None;
    }

    let dir = alc_path.parent()?;

    if dir.join(ALC_DLL).is_file() {
        // Fallback search: if build fails, try recursive search instead
        if let Ok(tc) = build_toolchain(dir) {
            return Some(tc);
        }
    }

    search_dir_recursive(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
    fn test_extract_version_edge_cases() {
        assert_eq!(extract_version_from_path(Path::new("/")), "unknown");
        assert_eq!(extract_version_from_path(Path::new("/opt/tools/1.0")), "1.0");
        assert_eq!(extract_version_from_path(Path::new("/home/user123/tools")), "unknown");
    }

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("al-core-toolchain-test-{}-{}", std::process::id(), id));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
