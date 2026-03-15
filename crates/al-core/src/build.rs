//! AL project compilation via `dotnet alc`.
//!
//! Provides `compile_project()` which invokes the AL compiler and returns
//! structured results including diagnostics. Used by:
//! - `al package` CLI command
//! - `al.package` LSP execute command
//! - `al debug start` (via al-dap-client, which has its own simpler version)

use std::path::{Path, PathBuf};

use crate::toolchain::AlToolchain;
use serde::Serialize;
use tokio::process::Command;

use crate::errors::AlError;

/// Result of a compilation attempt.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileResult {
    /// Whether compilation succeeded (exit code 0).
    pub success: bool,
    /// Path to the produced .app file (if successful).
    pub app_path: Option<PathBuf>,
    /// Compiler diagnostics (errors and warnings).
    pub diagnostics: Vec<CompileDiagnostic>,
    /// Raw compiler output (stdout + stderr).
    pub output: String,
}

/// A single compiler diagnostic parsed from alc output.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileDiagnostic {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
}

/// Severity level for compiler diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

/// Compile an AL project using `dotnet alc`.
///
/// Returns a structured `CompileResult` with success/failure, the .app path,
/// and parsed diagnostics. Does NOT fail on compilation errors — those are
/// returned as diagnostics in the result.
///
/// Uses `tokio::process::Command` to avoid blocking the tokio worker thread
/// during what can be a 30+ second compilation.
/// Optional list of analyzer names to enable (e.g., ["CodeCop", "AppSourceCop"]).
/// If None, all available analyzers are used.
pub async fn compile_project(
    toolchain: &AlToolchain,
    project_root: &Path,
    package_cache: Option<&Path>,
) -> Result<CompileResult, AlError> {
    compile_project_with_analyzers(toolchain, project_root, package_cache, None).await
}

/// Compile with specific analyzer selection.
pub async fn compile_project_with_analyzers(
    toolchain: &AlToolchain,
    project_root: &Path,
    package_cache: Option<&Path>,
    analyzer_filter: Option<&[String]>,
) -> Result<CompileResult, AlError> {
    if !project_root.join("app.json").is_file() {
        return Err(AlError::DocumentNotOpen(format!(
            "No app.json found in {}",
            project_root.display()
        )));
    }

    let mut cmd = Command::new("dotnet");
    cmd.arg(toolchain.alc.display().to_string());
    cmd.arg(format!("/project:{}", project_root.display()));
    cmd.arg(format!("/out:{}", project_root.display()));

    // Use explicit package cache path, or fall back to .alpackages
    let pkg_dir = package_cache
        .map(PathBuf::from)
        .unwrap_or_else(|| project_root.join(".alpackages"));
    if pkg_dir.is_dir() {
        cmd.arg(format!("/packagecachepath:{}", pkg_dir.display()));
    }

    // Add analyzers — filtered if a specific list is requested
    let all_analyzers = [
        ("CodeCop", &toolchain.analyzers.code_cop),
        ("AppSourceCop", &toolchain.analyzers.app_source_cop),
        ("UICop", &toolchain.analyzers.ui_cop),
        ("PerTenantCop", &toolchain.analyzers.per_tenant_cop),
    ];
    let mut analyzer_paths = Vec::new();
    for (name, path) in &all_analyzers {
        if !path.is_file() {
            continue;
        }
        if let Some(filter) = analyzer_filter {
            if !filter.iter().any(|f| f.eq_ignore_ascii_case(name)) {
                continue;
            }
        }
        analyzer_paths.push(path.display().to_string());
    }
    if !analyzer_paths.is_empty() {
        cmd.arg(format!("/analyzer:{}", analyzer_paths.join(",")));
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.output().await?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");

    let diagnostics = parse_alc_output(&combined);

    // Find .app file in project root
    let app_path = if output.status.success() {
        find_app_file(project_root)
    } else {
        None
    };

    Ok(CompileResult {
        success: output.status.success(),
        app_path,
        diagnostics,
        output: combined,
    })
}

/// Parse alc compiler output into structured diagnostics.
///
/// alc output format: `file(line,col): error CODE: message`
fn parse_alc_output(output: &str) -> Vec<CompileDiagnostic> {
    let mut diagnostics = Vec::new();

    for line in output.lines() {
        if let Some(diag) = parse_diagnostic_line(line) {
            diagnostics.push(diag);
        }
    }

    diagnostics
}

/// Parse a single alc diagnostic line.
///
/// Format: `path/file.al(10,5): error AL0001: Some message`
fn parse_diagnostic_line(line: &str) -> Option<CompileDiagnostic> {
    // Find the (line,col) pattern
    let paren_open = line.find('(')?;
    let paren_close = line[paren_open..].find(')')? + paren_open;
    let coords = &line[paren_open + 1..paren_close];
    let mut parts = coords.split(',');
    let line_num: u32 = parts.next()?.trim().parse().ok()?; // SILENT: non-numeric coords skipped
    let col_num: u32 = parts.next()?.trim().parse().ok()?; // SILENT: non-numeric coords skipped

    let file = line[..paren_open].to_string();

    // After ): find severity and code
    let rest = line[paren_close + 1..].trim();
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim();

    let (severity, rest) = if let Some(r) = rest.strip_prefix("error") {
        (DiagnosticSeverity::Error, r.trim())
    } else if let Some(r) = rest.strip_prefix("warning") {
        (DiagnosticSeverity::Warning, r.trim())
    } else if let Some(r) = rest.strip_prefix("info") {
        (DiagnosticSeverity::Info, r.trim())
    } else {
        return None;
    };

    // Code: message
    let (code, message) = if let Some(colon_pos) = rest.find(':') {
        let code = rest[..colon_pos].trim().to_string();
        let message = rest[colon_pos + 1..].trim().to_string();
        (code, message)
    } else {
        (String::new(), rest.to_string())
    };

    Some(CompileDiagnostic {
        file,
        line: line_num,
        column: col_num,
        severity,
        code,
        message,
    })
}

/// Find the .app file produced by compilation.
///
/// First tries to construct the expected filename from app.json
/// (`{publisher}_{name}_{version}.app`) to avoid returning a stale artifact
/// when multiple .app files from old builds are present in the project root.
/// Falls back to the most-recently-modified .app file if the manifest cannot
/// be read or the expected path does not exist.
fn find_app_file(project_root: &Path) -> Option<PathBuf> {
    // Try the deterministic path derived from app.json
    if let Some(path) = find_app_file_from_manifest(project_root) {
        return Some(path);
    }

    // Fallback: pick the most recently modified .app in the project root
    let entries = std::fs::read_dir(project_root).ok()?; // SILENT: dir read failure means no .app
    let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_some_and(|ext| ext == "app") {
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((mtime, path))
            } else {
                None
            }
        })
        .collect();

    candidates.sort_by(|a, b| b.0.cmp(&a.0)); // most-recent first
    candidates.into_iter().next().map(|(_, path)| path)
}

/// Derive the expected .app filename from `app.json` fields.
///
/// alc names the output `{publisher}_{name}_{version}.app` in the directory
/// passed to `/out:` (the project root in our case).
fn find_app_file_from_manifest(project_root: &Path) -> Option<PathBuf> {
    let manifest_bytes = std::fs::read(project_root.join("app.json")).ok()?; // SILENT: missing manifest handled by caller
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).ok()?; // SILENT: malformed JSON handled by caller

    let publisher = manifest.get("publisher")?.as_str()?;
    let name = manifest.get("name")?.as_str()?;
    let version = manifest.get("version")?.as_str()?;

    let filename = format!("{publisher}_{name}_{version}.app");
    let path = project_root.join(&filename);
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_diagnostic() {
        let line = r#"src/MyTable.al(10,5): error AL0001: Variable 'x' is not defined"#;
        let diag = parse_diagnostic_line(line).unwrap();
        assert_eq!(diag.file, "src/MyTable.al");
        assert_eq!(diag.line, 10);
        assert_eq!(diag.column, 5);
        assert_eq!(diag.severity, DiagnosticSeverity::Error);
        assert_eq!(diag.code, "AL0001");
        assert_eq!(diag.message, "Variable 'x' is not defined");
    }

    #[test]
    fn parse_warning_diagnostic() {
        let line = r#"src/Page.al(25,1): warning AL0432: The type 'Record' is not fully qualified"#;
        let diag = parse_diagnostic_line(line).unwrap();
        assert_eq!(diag.severity, DiagnosticSeverity::Warning);
        assert_eq!(diag.code, "AL0432");
    }

    #[test]
    fn parse_info_diagnostic() {
        let line = r#"src/Cod.al(1,1): info AL0999: Consider using 'var' parameter"#;
        let diag = parse_diagnostic_line(line).unwrap();
        assert_eq!(diag.severity, DiagnosticSeverity::Info);
    }

    #[test]
    fn parse_non_diagnostic_line_returns_none() {
        assert!(parse_diagnostic_line("Compiling project...").is_none());
        assert!(parse_diagnostic_line("").is_none());
        assert!(parse_diagnostic_line("Build succeeded.").is_none());
    }

    #[test]
    fn parse_multiple_diagnostics() {
        let output = "\
src/A.al(1,1): error AL0001: Error one
Compiling...
src/B.al(5,10): warning AL0002: Warning two
Build failed.";
        let diags = parse_alc_output(output);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].code, "AL0001");
        assert_eq!(diags[1].code, "AL0002");
    }

    #[test]
    fn compile_result_serializes_camel_case() {
        let result = CompileResult {
            success: false,
            app_path: None,
            diagnostics: vec![CompileDiagnostic {
                file: "test.al".to_string(),
                line: 1,
                column: 1,
                severity: DiagnosticSeverity::Error,
                code: "AL0001".to_string(),
                message: "test error".to_string(),
            }],
            output: "error output".to_string(),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"appPath\""));
        assert!(json.contains("\"diagnostics\""));
    }

    #[tokio::test]
    async fn compile_no_app_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let tc = crate::toolchain::AlToolchain {
            version: "1.0.0".to_string(),
            dotnet_root: PathBuf::from("/nonexistent"),
            alc: PathBuf::from("/nonexistent/alc.dll"),
            aldoc: None,
            code_analysis: PathBuf::new(),
            analyzers: crate::toolchain::AnalyzerPaths {
                code_cop: PathBuf::new(),
                app_source_cop: PathBuf::new(),
                ui_cop: PathBuf::new(),
                per_tenant_cop: PathBuf::new(),
                common: PathBuf::new(),
            },
        };
        let result = compile_project(&tc, dir.path(), None).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("app.json"));
    }

    #[test]
    fn find_app_file_prefers_manifest_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Write app.json
        std::fs::write(
            root.join("app.json"),
            r#"{"publisher":"MyPub","name":"MyApp","version":"2.0.0.0"}"#,
        )
        .unwrap();

        // Create a stale .app with a different name (old build artifact)
        std::fs::write(root.join("OldPub_OldApp_1.0.0.0.app"), b"stale").unwrap();

        // Create the expected .app from the manifest
        std::fs::write(root.join("MyPub_MyApp_2.0.0.0.app"), b"fresh").unwrap();

        let result = find_app_file(root).unwrap();
        assert_eq!(result.file_name().unwrap(), "MyPub_MyApp_2.0.0.0.app");
    }

    #[test]
    fn find_app_file_falls_back_to_most_recent_when_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // No app.json — manifest lookup will fail gracefully
        std::fs::write(root.join("Some_1.0.0.0.app"), b"only one").unwrap();

        let result = find_app_file(root).unwrap();
        assert_eq!(result.file_name().unwrap(), "Some_1.0.0.0.app");
    }
}
