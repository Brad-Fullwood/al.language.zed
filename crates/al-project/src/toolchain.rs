//! AL toolchain discovery, validation, and diagnostics.
//!
//! Contains `AlToolchain`, `AnalyzerPaths`, `find_toolchain()` and all discovery
//! helpers (formerly in al-protocol). Also provides `validate_toolchain()` and
//! `doctor()` for health-check operations.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::errors::DiscoveryError;
use crate::project::home_dir;

/// Build a `dotnet <alc.dll> …` command for invoking the AL toolchain
/// (compiler `alc.dll`, the native debugger, DAP editor services).
///
/// Microsoft ships `alc.dll` (and friends) as **net8.0** apps. A user may only
/// have a NEWER major .NET runtime installed (e.g. .NET 10), in which case the
/// host refuses to start with "You must install or update .NET to run this
/// application … Framework 'Microsoft.NETCore.App', version '8.0.0' not found"
/// and AL compilation / debugging fails. Setting `DOTNET_ROLL_FORWARD=Major`
/// tells the .NET host to roll forward onto the next available major, so the
/// net8.0 tool runs on .NET 10+. (Same fix as the bridge's csproj RollForward,
/// but applied to Microsoft's binaries we cannot edit — via the environment.)
///
/// `dotnet_command()` returns a `std::process::Command`; `dotnet_command_async`
/// the tokio equivalent. Both seed the first arg with the `alc` dll path.
pub fn dotnet_command(alc: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("dotnet");
    cmd.arg(alc);
    cmd.env("DOTNET_ROLL_FORWARD", "Major");
    cmd
}

/// Async (tokio) counterpart of [`dotnet_command`]. See its docs for the
/// roll-forward rationale.
/// Path to `altool.dll` — the ALTool CLI assembly (v17+) that hosts the
/// `launchlspserver` / `launchmcpserver` commands. It ships as a SIBLING of
/// the `alc.dll` compiler the toolchain discovers (alc itself does NOT
/// understand those commands). Returns None for pre-v17 toolchains.
pub fn find_altool(toolchain: &AlToolchain) -> Option<std::path::PathBuf> {
    let altool = toolchain.alc.with_file_name("altool.dll");
    altool.is_file().then_some(altool)
}

/// Compose the command that launches Microsoft's official AL Language
/// Server (`altool launchlspserver`, ALTool v17+). Used by
/// `al-lsp --official-lsp` to delegate the whole stdio LSP session to the
/// official server (F-OPEN-260).
///
/// NOTE: altool is an ASP.NET Core app — it additionally requires the
/// Microsoft.AspNetCore.App shared framework at runtime (the dotnet host
/// reports a precise error if it's missing).
pub fn official_lsp_command(altool: &Path, extra_args: &[String]) -> std::process::Command {
    let mut cmd = std::process::Command::new("dotnet");
    cmd.env("DOTNET_ROLL_FORWARD", "Major");
    cmd.arg(altool);
    cmd.arg("launchlspserver");
    cmd.args(extra_args);
    cmd
}

pub fn dotnet_command_async(alc: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("dotnet");
    cmd.arg(alc);
    cmd.env("DOTNET_ROLL_FORWARD", "Major");
    cmd
}

#[derive(Debug, Clone)]
pub struct AlToolchain {
    pub alc: PathBuf,
    pub aldoc: Option<PathBuf>,
    pub code_analysis: PathBuf,
    pub analyzers: AnalyzerPaths,
    pub dotnet_root: PathBuf,
    pub version: String,
}

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
/// 1. `$AL_TOOL_PATH` environment variable
/// 2. Known dotnet tool store roots:
///    - `~/.dotnet/tools/.store/`
///    - `~/.local/bin/.store/` (user-local install)
///    - `~/.local/share/dotnet/tools/.store/`
/// 3. System PATH — `which alc`, then `which al` (Microsoft wrapper) with
///    sibling `.store/` probe.
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
        for rel in [
            ".dotnet/tools/.store",
            ".local/bin/.store",
            ".local/share/dotnet/tools/.store",
        ] {
            let store = home.join(rel);
            if store.is_dir() {
                if let Some(tc) = search_dotnet_tool_store(&store) {
                    return Ok(tc);
                }
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
        if p.is_file() {
            Some(p)
        } else {
            None
        }
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
                if parts.len() >= 2 && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
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

    // Canonicalize the root once so we can confine the traversal to it. The
    // .store directories searched here live in user-writable locations, so a
    // symlink planted inside (e.g. `.store/evil -> /etc`) must not let the
    // search escape into arbitrary filesystem locations and pick up a
    // malicious alc.dll. If the root itself can't be canonicalized we fall
    // back to no bounds check rather than aborting discovery.
    let canonical_root = std::fs::canonicalize(root).ok();

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
                // Reject directories whose canonical path escapes the root
                // (e.g. via a symlink). `is_dir()` transparently follows
                // symlinks, so this check is what actually confines us.
                if let Some(root) = canonical_root.as_ref() {
                    match std::fs::canonicalize(&path) {
                        Ok(canon) if canon.starts_with(root) => {}
                        _ => continue,
                    }
                }
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
    if let Some(tc) = search_path_for("alc") {
        return Some(tc);
    }
    // Microsoft's dotnet-tool wrapper installs as `al`, not `alc`. When found,
    // its parent directory typically holds a sibling `.store/` containing the
    // actual `alc.dll` package payload.
    search_path_for("al")
}

fn search_path_for(cmd_name: &str) -> Option<AlToolchain> {
    // Use `where` on Windows, `which` on Unix — both are non-fatal if missing.
    #[cfg(target_os = "windows")]
    let which_cmd = "where";
    #[cfg(not(target_os = "windows"))]
    let which_cmd = "which";

    let output = std::process::Command::new(which_cmd)
        .arg(cmd_name)
        .output()
        .ok()?; // ok(): command missing is non-fatal

    if !output.status.success() {
        return None;
    }

    // Take only the first line — `where` (Windows) can return multiple matches.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("").trim();
    let cmd_path = PathBuf::from(first_line);
    if !cmd_path.is_file() {
        return None;
    }

    let dir = cmd_path.parent()?;

    if dir.join(ALC_DLL).is_file() {
        if let Ok(tc) = build_toolchain(dir) {
            return Some(tc);
        }
    }

    // Wrapper script (`al`) lives in e.g. `~/.local/bin/`; the dotnet tool
    // payload is under `<dir>/.store/<package>/<version>/.../tools/net8.0/any`.
    let sibling_store = dir.join(".store");
    if sibling_store.is_dir() {
        if let Some(tc) = search_dotnet_tool_store(&sibling_store) {
            return Some(tc);
        }
    }

    search_dir_recursive(dir)
}

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



#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn dotnet_command_sets_roll_forward_and_alc_arg() {
        let alc = std::path::Path::new("/some/tools/net8.0/any/alc.dll");
        let cmd = dotnet_command(alc);
        assert_eq!(cmd.get_program(), "dotnet");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args.first().map(String::as_str),
            Some("/some/tools/net8.0/any/alc.dll")
        );
        let rf = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("DOTNET_ROLL_FORWARD"))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert_eq!(rf.as_deref(), Some("Major"));
    }

    /// F-OPEN-260: the official-LSP delegation command must run the
    /// discovered alc assembly's `launchlspserver` entry point under dotnet
    /// with roll-forward (net8 assembly on newer majors), forwarding args.
    #[test]
    fn official_lsp_command_composes_launchlspserver_invocation() {
        let dir = tempfile::TempDir::new().unwrap();
        let tc = fake_toolchain(dir.path());
        // find_altool requires the sibling altool.dll (v17+); absent → None.
        assert!(find_altool(&tc).is_none(), "no altool.dll yet");
        std::fs::write(dir.path().join("altool.dll"), b"").unwrap();
        let altool = find_altool(&tc).expect("altool.dll sibling discovered");
        let extra = vec!["/x/proj".to_string()];
        let cmd = official_lsp_command(&altool, &extra);
        assert_eq!(cmd.get_program(), std::ffi::OsStr::new("dotnet"));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], altool.display().to_string());
        assert_eq!(args[1], "launchlspserver");
        assert_eq!(args[2], "/x/proj");
        let rf = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("DOTNET_ROLL_FORWARD"))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert_eq!(rf.as_deref(), Some("Major"));
    }

    #[test]
    fn dotnet_command_async_sets_roll_forward() {
        let alc = std::path::Path::new("/x/alc.dll");
        let cmd = dotnet_command_async(alc);
        let std_cmd = cmd.as_std();
        let rf = std_cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("DOTNET_ROLL_FORWARD"))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned());
        assert_eq!(rf.as_deref(), Some("Major"));
    }

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



    /// Build a minimal tools/net8.0/any layout under the given store with
    /// the Microsoft package prefix; returns the leaf tools dir.
    fn make_fake_dotnet_tool_store(store: &std::path::Path, version: &str) -> PathBuf {
        let leaf = store
            .join(format!("{DOTNET_TOOL_PACKAGE_PREFIX}/{version}/microsoft.dynamics.businesscentral.development.tools/{version}/tools/net8.0/any"));
        std::fs::create_dir_all(&leaf).unwrap();
        for f in [
            ALC_DLL,
            ALDOC_DLL,
            CODE_ANALYSIS_DLL,
            "Microsoft.Dynamics.Nav.CodeCop.dll",
            "Microsoft.Dynamics.Nav.AppSourceCop.dll",
            "Microsoft.Dynamics.Nav.UICop.dll",
            "Microsoft.Dynamics.Nav.PerTenantExtensionCop.dll",
            "Microsoft.Dynamics.Nav.Analyzers.Common.dll",
        ] {
            std::fs::write(leaf.join(f), b"").unwrap();
        }
        leaf
    }

    #[test]
    fn search_dotnet_tool_store_finds_user_local_install() {
        // Mirrors the F-004 reproduction layout: ~/.local/bin/.store/...
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join(".local/bin/.store");
        std::fs::create_dir_all(&store).unwrap();
        let leaf = make_fake_dotnet_tool_store(&store, "17.0.34.45391");

        let tc = search_dotnet_tool_store(&store).expect("toolchain not found in user-local store");
        assert_eq!(tc.alc, leaf.join(ALC_DLL));
        assert!(tc.code_analysis.is_file());
    }

    #[test]
    fn search_dotnet_tool_store_missing_package_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join(".store");
        std::fs::create_dir_all(&store).unwrap();
        // Intentionally do not create any package directory.
        assert!(search_dotnet_tool_store(&store).is_none());
    }

    #[test]
    fn search_dotnet_tool_store_ignores_unrelated_packages() {
        // A neighbouring (non-AL) dotnet tool must not confuse discovery.
        let tmp = tempfile::tempdir().unwrap();
        let store = tmp.path().join(".store");
        std::fs::create_dir_all(&store).unwrap();
        let other = store.join("some.other.dotnet.tool/1.0.0/tools/net8.0/any");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(ALC_DLL), b"").unwrap();
        let leaf = make_fake_dotnet_tool_store(&store, "17.0.34.45391");

        let tc = search_dotnet_tool_store(&store).expect("expected AL package");
        assert_eq!(tc.alc, leaf.join(ALC_DLL));
    }

    #[test]
    fn search_dir_recursive_finds_alc_in_nested_layout() {
        // Mirrors `<.store-root>/<pkg>/<ver>/.../tools/net8.0/any/alc.dll` layout.
        let tmp = tempfile::tempdir().unwrap();
        let leaf = tmp.path().join("a/b/c/d/tools/net8.0/any");
        std::fs::create_dir_all(&leaf).unwrap();
        std::fs::write(leaf.join(ALC_DLL), b"").unwrap();
        std::fs::write(leaf.join(CODE_ANALYSIS_DLL), b"").unwrap();

        let tc = search_dir_recursive(tmp.path()).expect("expected to find alc.dll deep");
        assert_eq!(tc.alc, leaf.join(ALC_DLL));
    }

    #[test]
    fn search_dir_recursive_no_alc_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let leaf = tmp.path().join("a/b/c");
        std::fs::create_dir_all(&leaf).unwrap();
        // Put a sibling DLL but not alc.dll.
        std::fs::write(leaf.join("Other.dll"), b"").unwrap();

        assert!(search_dir_recursive(tmp.path()).is_none());
    }

    /// Serializes tests that mutate process-global env (`PATH`, `AL_TOOL_PATH`).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn write_minimal_toolchain(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(ALC_DLL), b"").unwrap();
        std::fs::write(dir.join(CODE_ANALYSIS_DLL), b"").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn search_dir_recursive_rejects_symlink_escaping_root() {
        // A symlink planted inside the search root pointing outside it must NOT
        // let discovery pick up an alc.dll that lives outside the root.
        let outside = tempfile::tempdir().unwrap();
        write_minimal_toolchain(&outside.path().join("evil"));

        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("evil"), root.path().join("link")).unwrap();

        // Without the bounds check, the traversal would follow `link` into
        // `outside/evil` and return its alc.dll. With the fix it must not.
        assert!(
            search_dir_recursive(root.path()).is_none(),
            "search escaped the root via a symlink"
        );
    }

    #[cfg(unix)]
    #[test]
    fn search_dir_recursive_still_finds_real_subdir_alongside_symlink() {
        // The bounds check must not break legitimate in-root discovery.
        let outside = tempfile::tempdir().unwrap();
        write_minimal_toolchain(&outside.path().join("evil"));

        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("evil"), root.path().join("link")).unwrap();
        let real = root.path().join("real/tools/net8.0/any");
        write_minimal_toolchain(&real);

        let tc = search_dir_recursive(root.path()).expect("in-root toolchain should be found");
        // Resolve symlinks on the temp dir prefix (macOS /var -> /private/var).
        let expected = std::fs::canonicalize(real.join(ALC_DLL)).unwrap();
        let got = std::fs::canonicalize(&tc.alc).unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn search_path_for_missing_command_returns_none() {
        // A command that cannot exist on PATH must be handled gracefully.
        assert!(search_path_for("al-lsp-definitely-not-a-real-command-xyz").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn search_path_for_discovers_toolchain_via_which() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Lay out a fake `alc` executable next to a real alc.dll payload.
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path();
        write_minimal_toolchain(bin);
        // The discovery shells out to `which alc`, which needs an executable
        // named `alc` on PATH; create one and mark it executable.
        let alc_exe = bin.join("alc");
        std::fs::write(&alc_exe, b"#!/bin/sh\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&alc_exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let orig_path = std::env::var_os("PATH");
        let new_path = match &orig_path {
            Some(p) => {
                let mut joined = std::ffi::OsString::from(bin);
                joined.push(":");
                joined.push(p);
                joined
            }
            None => std::ffi::OsString::from(bin),
        };
        // SAFETY: synchronised via ENV_LOCK above.
        unsafe { std::env::set_var("PATH", &new_path) };

        let result = search_path_for("alc");

        // Restore PATH before asserting so a failure can't leak state.
        // SAFETY: synchronised via ENV_LOCK above.
        unsafe {
            match orig_path {
                Some(p) => std::env::set_var("PATH", p),
                None => std::env::remove_var("PATH"),
            }
        }

        let tc = result.expect("toolchain not discovered via `which alc`");
        assert_eq!(tc.alc, bin.join(ALC_DLL));
        assert!(tc.code_analysis.is_file());
    }

    #[test]
    fn find_toolchain_uses_al_tool_path_direct_layout() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("toolchain");
        write_minimal_toolchain(&dir);

        let orig = std::env::var_os("AL_TOOL_PATH");
        // SAFETY: synchronised via ENV_LOCK above.
        unsafe { std::env::set_var("AL_TOOL_PATH", &dir) };

        let result = find_toolchain();

        // SAFETY: synchronised via ENV_LOCK above.
        unsafe {
            match orig {
                Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                None => std::env::remove_var("AL_TOOL_PATH"),
            }
        }

        let tc = result.expect("AL_TOOL_PATH toolchain should be discovered");
        assert_eq!(tc.alc, dir.join(ALC_DLL));
    }

    #[test]
    fn find_toolchain_uses_al_tool_path_nested_layout() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let tmp = tempfile::tempdir().unwrap();
        // alc.dll buried below AL_TOOL_PATH — exercises the recursive fallback.
        let leaf = tmp.path().join("root/tools/net8.0/any");
        write_minimal_toolchain(&leaf);

        let orig = std::env::var_os("AL_TOOL_PATH");
        // SAFETY: synchronised via ENV_LOCK above.
        unsafe { std::env::set_var("AL_TOOL_PATH", tmp.path().join("root")) };

        let result = find_toolchain();

        // SAFETY: synchronised via ENV_LOCK above.
        unsafe {
            match orig {
                Some(v) => std::env::set_var("AL_TOOL_PATH", v),
                None => std::env::remove_var("AL_TOOL_PATH"),
            }
        }

        let tc = result.expect("nested AL_TOOL_PATH toolchain should be discovered");
        assert_eq!(tc.alc, leaf.join(ALC_DLL));
    }
}
