//! AL toolchain discovery, validation, and diagnostics.
//!
//! Re-exports `find_toolchain` from `al_protocol`. Adds `validate_toolchain()`
//! for checking component health and `doctor()` for full workspace health reports.

use serde::Serialize;

pub use al_protocol::toolchain::find_toolchain;
pub use al_protocol::{AlToolchain, AnalyzerPaths};
pub use al_protocol::errors::DiscoveryError;

use crate::workspace::Workspace;

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
    let tc = workspace.toolchain.try_read().ok();
    let project = workspace.project.try_read().ok();

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

    let dotnet_version = std::process::Command::new("dotnet")
        .arg("--version")
        .output()
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
        workspace_files: workspace.workspace_files.len(),
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
