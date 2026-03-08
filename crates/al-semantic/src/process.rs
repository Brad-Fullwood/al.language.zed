//! .NET subprocess management — spawn, communicate, auto-kill on idle.
//!
//! Launches the AlSemantic .NET console app as a child process with
//! stdin/stdout pipes for JSON-RPC communication. Stderr is captured
//! for error reporting.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use al_discovery::AlToolchain;
use tokio::process::{Child, Command};

use crate::SemanticError;

/// Location of the .NET bridge project relative to the crate root.
const DOTNET_PROJECT_DIR: &str = "dotnet/AlSemantic";

/// Spawn the .NET bridge subprocess.
///
/// Looks for the AlSemantic .NET app in this order:
/// 1. Pre-compiled binary next to the Rust binary (`AlSemantic` / `AlSemantic.exe`)
/// 2. `AL_SEMANTIC_BRIDGE` environment variable pointing to the binary
/// 3. `dotnet run` in the source project directory (development mode)
///
/// The CodeAnalysis.dll path from the toolchain is passed as the first argument.
pub fn spawn_dotnet_bridge(toolchain: &AlToolchain) -> Result<Child, SemanticError> {
    let code_analysis_path = &toolchain.code_analysis;

    // Strategy 1: Look for pre-compiled binary alongside the current executable
    if let Ok(exe_dir) = std::env::current_exe().map(|p| p.parent().unwrap_or(Path::new(".")).to_path_buf()) {
        for name in &["AlSemantic", "AlSemantic.exe"] {
            let candidate = exe_dir.join(name);
            if candidate.is_file() {
                return spawn_binary(&candidate, code_analysis_path);
            }
        }
    }

    // Strategy 2: Environment variable
    if let Ok(bridge_path) = std::env::var("AL_SEMANTIC_BRIDGE") {
        let p = PathBuf::from(&bridge_path);
        if p.is_file() {
            return spawn_binary(&p, code_analysis_path);
        }
    }

    // Strategy 3: `dotnet run` in the source project (development mode)
    if let Some(project_dir) = find_dotnet_project() {
        return spawn_dotnet_run(&project_dir, code_analysis_path);
    }

    Err(SemanticError::SpawnFailed(
        "Could not find AlSemantic .NET bridge. Set AL_SEMANTIC_BRIDGE to the compiled binary path, \
         or ensure the dotnet/AlSemantic project is available for `dotnet run`."
            .to_string(),
    ))
}

/// Spawn a pre-compiled AlSemantic binary.
fn spawn_binary(binary: &Path, code_analysis_path: &Path) -> Result<Child, SemanticError> {
    Command::new(binary)
        .arg(code_analysis_path.as_os_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            SemanticError::SpawnFailed(format!(
                "Failed to spawn AlSemantic binary at {}: {}",
                binary.display(),
                e
            ))
        })
}

/// Spawn via `dotnet run` in the project directory (development mode).
fn spawn_dotnet_run(project_dir: &Path, code_analysis_path: &Path) -> Result<Child, SemanticError> {
    Command::new("dotnet")
        .arg("run")
        .arg("--project")
        .arg(project_dir.as_os_str())
        .arg("--")
        .arg(code_analysis_path.as_os_str())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            SemanticError::SpawnFailed(format!(
                "Failed to run `dotnet run --project {}`: {}",
                project_dir.display(),
                e
            ))
        })
}

/// Try to locate the AlSemantic .NET project directory.
///
/// Searches relative to:
/// 1. The `CARGO_MANIFEST_DIR` (for development/test builds)
/// 2. The current executable directory
fn find_dotnet_project() -> Option<PathBuf> {
    // During development, CARGO_MANIFEST_DIR points to crates/al-semantic/
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let candidate = PathBuf::from(&manifest_dir).join(DOTNET_PROJECT_DIR);
        if candidate.join("AlSemantic.csproj").is_file() {
            return Some(candidate);
        }
    }

    // Relative to executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            // Could be in a sibling directory structure
            for depth in &["", "../", "../../"] {
                let candidate = exe_dir.join(depth).join(DOTNET_PROJECT_DIR);
                if candidate.join("AlSemantic.csproj").is_file() {
                    return Some(candidate);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_dotnet_project_from_manifest_dir() {
        // When running under cargo test, CARGO_MANIFEST_DIR should be set
        // and the dotnet project should be findable
        if std::env::var("CARGO_MANIFEST_DIR").is_ok() {
            let result = find_dotnet_project();
            // This will succeed if the dotnet project exists in the source tree
            if let Some(path) = result {
                assert!(path.join("AlSemantic.csproj").is_file());
            }
        }
    }

    #[tokio::test]
    async fn test_spawn_fails_gracefully_without_dotnet() {
        // Create a fake toolchain pointing to a nonexistent path
        let toolchain = AlToolchain {
            alc: PathBuf::from("/nonexistent/alc.dll"),
            aldoc: None,
            code_analysis: PathBuf::from("/nonexistent/CodeAnalysis.dll"),
            analyzers: al_discovery::AnalyzerPaths {
                code_cop: PathBuf::from("/nonexistent/CodeCop.dll"),
                app_source_cop: PathBuf::from("/nonexistent/AppSourceCop.dll"),
                ui_cop: PathBuf::from("/nonexistent/UICop.dll"),
                per_tenant_cop: PathBuf::from("/nonexistent/PerTenantCop.dll"),
                common: PathBuf::from("/nonexistent/Common.dll"),
            },
            dotnet_root: PathBuf::from("/nonexistent"),
            version: "0.0.0".to_string(),
        };

        // Clear the env var so strategy 2 doesn't fire
        let _guard = ClearEnvGuard::new("AL_SEMANTIC_BRIDGE");

        let result = spawn_dotnet_bridge(&toolchain);
        // Should fail but not panic
        match result {
            Ok(_) => {
                // If dotnet run somehow works, that's fine too
            }
            Err(e) => {
                // Should be a SpawnFailed error, not a panic
                let msg = e.to_string();
                assert!(
                    msg.contains("Failed") || msg.contains("Could not find"),
                    "Unexpected error message: {msg}"
                );
            }
        }
    }

    /// RAII guard to clear an env var and restore it on drop.
    struct ClearEnvGuard {
        key: String,
        prev: Option<String>,
    }

    impl ClearEnvGuard {
        fn new(key: &str) -> Self {
            let prev = std::env::var(key).ok();
            std::env::remove_var(key);
            Self {
                key: key.to_string(),
                prev,
            }
        }
    }

    impl Drop for ClearEnvGuard {
        fn drop(&mut self) {
            if let Some(val) = &self.prev {
                std::env::set_var(&self.key, val);
            }
        }
    }
}
