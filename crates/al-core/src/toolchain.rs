//! AL toolchain discovery, validation, and diagnostics.
//!
//! Contains `AlToolchain`, `AnalyzerPaths`, `find_toolchain()` and all discovery
//! helpers (formerly in al-protocol). Also provides `validate_toolchain()` and
//! `doctor()` for health-check operations.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::errors::DiscoveryError;
use crate::project::home_dir;
use crate::workspace::Workspace;

// ---------------------------------------------------------------------------
// Toolchain types
// ---------------------------------------------------------------------------

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
    /// Custom analyzer DLL paths (e.g. BusinessCentral.LinterCop.dll).
    /// Populated from `al.codeAnalyzers` entries that are absolute DLL paths.
    pub custom: Vec<PathBuf>,
}

// ---------------------------------------------------------------------------
// Discovery constants
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
// Discovery
// ---------------------------------------------------------------------------

/// Discover the AL toolchain (ALTool installation).
///
/// Search order:
/// 1. `$AL_TOOL_PATH` environment variable
/// 2. `~/.dotnet/tools/.store/microsoft.dynamics.businesscentral.development.tools*/`
/// 3. System PATH (`which alc`)
pub fn find_toolchain() -> Result<AlToolchain, DiscoveryError> {
    if let Ok(tool_path) = std::env::var("AL_TOOL_PATH") {
        let dir = PathBuf::from(&tool_path);
        if dir.join(ALC_DLL).is_file() {
            return build_toolchain(&dir);
        }
        if let Some(tc) = search_dir_recursive(&dir) {
            return Ok(tc);
        }
    }

    if let Some(home) = home_dir() {
        let store = home.join(".dotnet/tools/.store");
        if store.is_dir() {
            if let Some(tc) = search_dotnet_tool_store(&store) {
                return Ok(tc);
            }
        }
    }

    if let Some(tc) = search_system_path() {
        return Ok(tc);
    }

    Err(DiscoveryError::AlToolNotInstalled {
        install_cmd: INSTALL_CMD.to_string(),
    })
}

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
        custom: Vec::new(),
    }
}

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

fn search_dotnet_tool_store(store: &Path) -> Option<AlToolchain> {
    let entries = std::fs::read_dir(store).ok()?; // ok(): store unreadable is non-fatal

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

fn search_dir_recursive(root: &Path) -> Option<AlToolchain> {
    if root.join(ALC_DLL).is_file() {
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

fn search_system_path() -> Option<AlToolchain> {
    // Use `where` on Windows, `which` on Unix — both are non-fatal if missing.
    #[cfg(target_os = "windows")]
    let which_cmd = "where";
    #[cfg(not(target_os = "windows"))]
    let which_cmd = "which";

    let output = std::process::Command::new(which_cmd)
        .arg("alc")
        .output()
        .ok()?; // ok(): command missing is non-fatal

    if !output.status.success() {
        return None;
    }

    // Take only the first line — `where` (Windows) can return multiple matches.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("").trim();
    let alc_path = PathBuf::from(first_line);
    if !alc_path.is_file() {
        return None;
    }

    let dir = alc_path.parent()?;

    if dir.join(ALC_DLL).is_file() {
        if let Ok(tc) = build_toolchain(dir) {
            return Some(tc);
        }
    }

    search_dir_recursive(dir)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Result of validating an AlToolchain's components.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainValidation {
    pub alc_exists: bool,
    pub code_analysis_exists: bool,
    pub aldoc_exists: bool,
    pub analyzers_found: u32,
    pub analyzers_total: u32,
    pub version: String,
    pub issues: Vec<String>,
}

impl ToolchainValidation {
    pub fn is_healthy(&self) -> bool {
        self.alc_exists && self.code_analysis_exists && self.issues.is_empty()
    }
}

/// Validate that an AlToolchain's referenced files actually exist.
pub fn validate_toolchain(tc: &AlToolchain) -> ToolchainValidation {
    let mut issues = Vec::new();
    let alc_exists = tc.alc.is_file();
    if !alc_exists {
        issues.push(format!("alc not found at {}", tc.alc.display()));
    }

    let code_analysis_exists = tc.code_analysis.is_file();
    if !code_analysis_exists {
        issues.push(format!(
            "CodeAnalysis.dll not found at {}",
            tc.code_analysis.display()
        ));
    }

    let aldoc_exists = tc.aldoc.as_ref().is_some_and(|p| p.is_file());

    let analyzer_paths = [
        &tc.analyzers.code_cop,
        &tc.analyzers.app_source_cop,
        &tc.analyzers.ui_cop,
        &tc.analyzers.per_tenant_cop,
        &tc.analyzers.common,
    ];
    let analyzers_found = analyzer_paths.iter().filter(|p| p.is_file()).count() as u32;
    let analyzers_total = analyzer_paths.len() as u32;

    ToolchainValidation {
        alc_exists,
        code_analysis_exists,
        aldoc_exists,
        analyzers_found,
        analyzers_total,
        version: tc.version.clone(),
        issues,
    }
}

// ---------------------------------------------------------------------------
// Doctor
// ---------------------------------------------------------------------------

/// Full workspace health report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub altool_installed: bool,
    pub toolchain: Option<ToolchainInfo>,
    pub toolchain_validation: Option<ToolchainValidation>,
    pub project: Option<ProjectInfo>,
    pub dotnet_version: Option<String>,
    pub indexed_symbols: usize,
    pub workspace_files: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolchainInfo {
    pub version: String,
    pub alc: String,
    pub code_analysis: String,
    pub dotnet_root: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub root: String,
    pub packages: usize,
}

/// Run a full health check on the workspace.
///
/// Uses `try_read()` on async locks — returns `None` for fields that are
/// currently locked (e.g., during initialization).
pub fn doctor(workspace: &Workspace) -> DoctorReport {
    let tc = workspace.toolchain.try_read().ok(); // SILENT: avoid RwLock poison panic per CLAUDE.md
    let project = workspace.project.try_read().ok(); // SILENT: avoid RwLock poison panic per CLAUDE.md

    let tc_ref = tc.as_ref().and_then(|guard| guard.as_ref());

    let toolchain_info = tc_ref.map(|t| ToolchainInfo {
        version: t.version.clone(),
        alc: t.alc.display().to_string(),
        code_analysis: t.code_analysis.display().to_string(),
        dotnet_root: t.dotnet_root.display().to_string(),
    });

    let toolchain_validation = tc_ref.map(validate_toolchain);

    let project_info = project.as_ref().and_then(|guard| {
        guard.as_ref().map(|p| ProjectInfo {
            name: p.app_json.name.clone(),
            publisher: p.app_json.publisher.clone(),
            version: p.app_json.version.clone(),
            root: p.root.display().to_string(),
            packages: p.packages.len(),
        })
    });

    // Note: This blocks the current thread for ~50ms to run `dotnet --version`.
    // Acceptable for a diagnostic command called rarely (al doctor / al setup).
    let dotnet_version = std::process::Command::new("dotnet")
        .arg("--version")
        .output()
        // SILENT: dotnet may not be installed; missing version is handled by returning None
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    DoctorReport {
        altool_installed: tc_ref.is_some(),
        toolchain: toolchain_info,
        toolchain_validation,
        project: project_info,
        dotnet_version,
        indexed_symbols: workspace.symbols.len(),
        workspace_files: workspace.file_index.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_toolchain(dir: &std::path::Path) -> AlToolchain {
        AlToolchain {
            alc: dir.join("alc.dll"),
            aldoc: Some(dir.join("aldoc.dll")),
            code_analysis: dir.join("Microsoft.Dynamics.Nav.CodeAnalysis.dll"),
            analyzers: AnalyzerPaths {
                code_cop: dir.join("Microsoft.Dynamics.Nav.CodeCop.dll"),
                app_source_cop: dir.join("Microsoft.Dynamics.Nav.AppSourceCop.dll"),
                ui_cop: dir.join("Microsoft.Dynamics.Nav.UICop.dll"),
                per_tenant_cop: dir.join("Microsoft.Dynamics.Nav.PerTenantExtensionCop.dll"),
                common: dir.join("Microsoft.Dynamics.Nav.Analyzers.Common.dll"),
                custom: Vec::new(),
            },
            dotnet_root: dir.to_path_buf(),
            version: "26.0.12345.0".to_string(),
        }
    }

    #[test]
    fn validate_toolchain_all_present() {
        let dir = std::env::temp_dir().join("al-tc-test-valid");
        std::fs::create_dir_all(&dir).unwrap();

        let tc = fake_toolchain(&dir);
        // Create all required files
        std::fs::write(&tc.alc, b"").unwrap();
        std::fs::write(&tc.code_analysis, b"").unwrap();
        std::fs::write(tc.aldoc.as_ref().unwrap(), b"").unwrap();
        std::fs::write(&tc.analyzers.code_cop, b"").unwrap();
        std::fs::write(&tc.analyzers.app_source_cop, b"").unwrap();
        std::fs::write(&tc.analyzers.ui_cop, b"").unwrap();
        std::fs::write(&tc.analyzers.per_tenant_cop, b"").unwrap();
        std::fs::write(&tc.analyzers.common, b"").unwrap();

        let result = validate_toolchain(&tc);
        assert!(result.is_healthy());
        assert!(result.alc_exists);
        assert!(result.code_analysis_exists);
        assert!(result.aldoc_exists);
        assert_eq!(result.analyzers_found, 5);
        assert!(result.issues.is_empty());
        assert_eq!(result.version, "26.0.12345.0");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_toolchain_missing_alc() {
        let dir = std::env::temp_dir().join("al-tc-test-missing");
        std::fs::create_dir_all(&dir).unwrap();

        let tc = fake_toolchain(&dir);
        // Don't create alc.dll — leave it missing
        std::fs::write(&tc.code_analysis, b"").unwrap();

        let result = validate_toolchain(&tc);
        assert!(!result.is_healthy());
        assert!(!result.alc_exists);
        assert!(result.code_analysis_exists);
        assert_eq!(result.issues.len(), 1);
        assert!(result.issues[0].contains("alc"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_toolchain_nonexistent_dir() {
        let tc = fake_toolchain(&PathBuf::from("/nonexistent/toolchain/path"));

        let result = validate_toolchain(&tc);
        assert!(!result.is_healthy());
        assert!(!result.alc_exists);
        assert!(!result.code_analysis_exists);
        assert!(!result.aldoc_exists);
        assert_eq!(result.analyzers_found, 0);
        assert_eq!(result.issues.len(), 2); // alc + code_analysis
    }

    #[test]
    fn doctor_empty_workspace() {
        let ws = Workspace::new();
        let report = doctor(&ws);

        assert!(!report.altool_installed);
        assert!(report.toolchain.is_none());
        assert!(report.toolchain_validation.is_none());
        assert!(report.project.is_none());
        assert_eq!(report.indexed_symbols, 0);
        assert_eq!(report.workspace_files, 0);
    }

    #[test]
    fn doctor_report_serializes_to_json() {
        let ws = Workspace::new();
        let report = doctor(&ws);

        let json = serde_json::to_value(&report).unwrap();
        assert!(json["altoolInstalled"].is_boolean());
        assert!(json["indexedSymbols"].is_number());
        assert!(json["workspaceFiles"].is_number());
    }
}
