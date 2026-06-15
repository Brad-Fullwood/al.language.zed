//! Locate Microsoft.Dynamics.Nav.EditorServices.Host binary.
//!
//! Search order:
//! 1. `$AL_EDITOR_SERVICES_PATH` environment variable
//! 2. Next to alc.dll (in case the user extracted it alongside ALTool)
//! 3. `~/.cache/al-lsp/editor-services/` (downloaded and cached)
//! 4. VS Code / Cursor AL extension installations
//!
//! If not found, returns an error with instructions to download.

use std::path::PathBuf;

use crate::toolchain::AlToolchain;
use tracing::info;

use super::DapError;

#[cfg(target_os = "linux")]
const HOST_BINARY: &str = "Microsoft.Dynamics.Nav.EditorServices.Host";
#[cfg(target_os = "macos")]
const HOST_BINARY: &str = "Microsoft.Dynamics.Nav.EditorServices.Host";
#[cfg(target_os = "windows")]
const HOST_BINARY: &str = "Microsoft.Dynamics.Nav.EditorServices.Host.exe";

#[cfg(target_os = "linux")]
const PLATFORM_DIR: &str = "linux";
#[cfg(target_os = "macos")]
const PLATFORM_DIR: &str = "darwin";
#[cfg(target_os = "windows")]
const PLATFORM_DIR: &str = "win32";

pub fn find_editor_services(toolchain: &AlToolchain) -> Result<PathBuf, DapError> {
    // Strategy 1: explicit env var
    if let Ok(path) = std::env::var("AL_EDITOR_SERVICES_PATH") {
        let p = PathBuf::from(&path);
        if p.is_file() {
            info!(
                "Found EditorServices.Host via $AL_EDITOR_SERVICES_PATH: {}",
                p.display()
            );
            return Ok(p);
        }
        let in_dir = p.join(HOST_BINARY);
        if in_dir.is_file() {
            info!(
                "Found EditorServices.Host via $AL_EDITOR_SERVICES_PATH: {}",
                in_dir.display()
            );
            return Ok(in_dir);
        }
    }

    // Strategy 2: next to alc.dll
    let alongside_alc = toolchain.dotnet_root.join(HOST_BINARY);
    if alongside_alc.is_file() {
        info!(
            "Found EditorServices.Host next to ALTool: {}",
            alongside_alc.display()
        );
        return Ok(alongside_alc);
    }

    // Strategy 3: cached download location
    if let Some(home) = home_dir() {
        let cache_dir = home.join(".cache/al-lsp/editor-services");
        let cached = cache_dir.join(HOST_BINARY);
        if cached.is_file() {
            info!("Found EditorServices.Host in cache: {}", cached.display());
            return Ok(cached);
        }
    }

    // Strategy 4: VS Code / Cursor AL extension installations
    if let Some(path) = find_in_vscode_extensions() {
        info!(
            "Found EditorServices.Host in IDE extension: {}",
            path.display()
        );
        return Ok(path);
    }

    Err(DapError::EditorServicesNotFound(format!(
        "Microsoft.Dynamics.Nav.EditorServices.Host not found.\n\n\
         The AL debugger requires EditorServices.Host from the AL Language extension.\n\n\
         Options:\n\
         1. Set $AL_EDITOR_SERVICES_PATH to the directory containing {HOST_BINARY}\n\
         2. Place {HOST_BINARY} next to alc.dll at: {}\n\
         3. Install the AL extension in VS Code or Cursor (the binary will be found automatically)\n\
         4. Download the .vsix from the VS Code marketplace and extract\n\
            extension/bin/{PLATFORM_DIR}/ to ~/.cache/al-lsp/editor-services/",
        toolchain.dotnet_root.display()
    )))
}

fn find_in_vscode_extensions() -> Option<PathBuf> {
    let home = home_dir()?;

    let extension_dirs = [
        home.join(".vscode/extensions"),
        home.join(".vscode-insiders/extensions"),
        home.join(".cursor/extensions"),
        home.join(".vscodium/extensions"),
    ];

    for ext_dir in &extension_dirs {
        if !ext_dir.is_dir() {
            continue;
        }

        let entries = match std::fs::read_dir(ext_dir) {
            Ok(e) => e,
            Err(_) => continue, // SILENT: unreadable directories are skipped, not fatal
        };
        let mut al_dirs: Vec<PathBuf> = entries
            .filter_map(|e| e.ok()) // SILENT: directory entries may be unreadable
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("ms-dynamics-smb.al-")
            })
            .map(|e| e.path())
            .collect();

        al_dirs.sort();
        al_dirs.reverse();

        for al_dir in al_dirs {
            let host = al_dir.join("bin").join(PLATFORM_DIR).join(HOST_BINARY);
            if host.is_file() {
                return Some(host);
            }
        }
    }

    None
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}
