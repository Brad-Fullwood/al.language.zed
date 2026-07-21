//! Workspace health checks used by `al doctor` and `al setup`.

use serde::Serialize;

use al_project::toolchain::{validate_toolchain, ToolchainValidation};

use crate::Workspace;

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

    // This diagnostic is synchronous and invoked infrequently.
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
        workspace_files: workspace.file_index.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
