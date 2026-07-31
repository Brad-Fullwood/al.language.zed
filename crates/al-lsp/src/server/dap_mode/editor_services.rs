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
    match std::env::var_os("AL_EDITOR_SERVICES_PATH") {
        Some(path) if path.is_empty() => {
            return Err(DapError::EditorServicesNotFound(
                "AL_EDITOR_SERVICES_PATH is set but empty".to_string(),
            ));
        }
        Some(path) => {
            let resolved = resolve_explicit_editor_services(PathBuf::from(path))?;
            info!(
                "Found EditorServices.Host via $AL_EDITOR_SERVICES_PATH: {}",
                resolved.display()
            );
            return Ok(resolved);
        }
        None => {}
    }

    let alongside_alc = toolchain.dotnet_root.join(HOST_BINARY);
    if existing_file(&alongside_alc)? {
        info!(
            "Found EditorServices.Host next to ALTool: {}",
            alongside_alc.display()
        );
        return Ok(alongside_alc);
    }

    if let Some(home) = home_dir() {
        let cache_dir = home.join(".cache/al-lsp/editor-services");
        let cached = cache_dir.join(HOST_BINARY);
        if existing_file(&cached)? {
            info!("Found EditorServices.Host in cache: {}", cached.display());
            return Ok(cached);
        }
    }

    if let Some(path) = find_in_vscode_extensions()? {
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

fn resolve_explicit_editor_services(path: PathBuf) -> Result<PathBuf, DapError> {
    let metadata = std::fs::metadata(&path).map_err(|error| {
        DapError::EditorServicesNotFound(format!(
            "Cannot inspect AL_EDITOR_SERVICES_PATH '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.is_file() {
        return Ok(path);
    }
    if !metadata.is_dir() {
        return Err(DapError::EditorServicesNotFound(format!(
            "AL_EDITOR_SERVICES_PATH '{}' is neither a file nor a directory",
            path.display()
        )));
    }
    let in_dir = path.join(HOST_BINARY);
    if existing_file(&in_dir)? {
        return Ok(in_dir);
    }
    Err(DapError::EditorServicesNotFound(format!(
        "AL_EDITOR_SERVICES_PATH directory '{}' does not contain {HOST_BINARY}",
        path.display()
    )))
}

fn existing_file(path: &std::path::Path) -> Result<bool, DapError> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(DapError::EditorServicesNotFound(format!(
            "Cannot inspect EditorServices candidate '{}': {error}",
            path.display()
        ))),
    }
}

fn find_in_vscode_extensions() -> Result<Option<PathBuf>, DapError> {
    let Some(home) = home_dir() else {
        return Ok(None);
    };

    let extension_dirs = [
        home.join(".vscode/extensions"),
        home.join(".vscode-insiders/extensions"),
        home.join(".cursor/extensions"),
        home.join(".vscodium/extensions"),
    ];

    for ext_dir in &extension_dirs {
        match std::fs::metadata(ext_dir) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(DapError::EditorServicesNotFound(format!(
                    "Editor extension search path '{}' is not a directory",
                    ext_dir.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(DapError::EditorServicesNotFound(format!(
                    "Cannot inspect editor extension directory '{}': {error}",
                    ext_dir.display()
                )));
            }
        }

        let entries = std::fs::read_dir(ext_dir).map_err(|error| {
            DapError::EditorServicesNotFound(format!(
                "Cannot read editor extension directory '{}': {error}",
                ext_dir.display()
            ))
        })?;
        let mut al_dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                DapError::EditorServicesNotFound(format!(
                    "Cannot inspect an entry in '{}': {error}",
                    ext_dir.display()
                ))
            })?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("ms-dynamics-smb.al-")
            {
                al_dirs.push(entry.path());
            }
        }

        al_dirs.sort();
        al_dirs.reverse();

        for al_dir in al_dirs {
            let host = al_dir.join("bin").join(PLATFORM_DIR).join(HOST_BINARY);
            if existing_file(&host)? {
                return Ok(Some(host));
            }
        }
    }

    Ok(None)
}

fn home_dir() -> Option<PathBuf> {
    dirs::home_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_editor_services_directory_must_contain_host() {
        let directory = tempfile::tempdir().unwrap();
        let error = resolve_explicit_editor_services(directory.path().to_path_buf())
            .expect_err("empty explicit directory must fail");
        assert!(error.to_string().contains(HOST_BINARY));
    }

    #[test]
    fn explicit_editor_services_file_is_used_exactly() {
        let directory = tempfile::tempdir().unwrap();
        let host = directory.path().join("custom-host");
        std::fs::write(&host, b"host").unwrap();
        assert_eq!(
            resolve_explicit_editor_services(host.clone()).unwrap(),
            host
        );
    }
}
