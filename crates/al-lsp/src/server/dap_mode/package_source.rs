//! The source file a snapshot replay shows for an object of a dependency
//! package.
//!
//! Standard route checked: `al_symbols::virtual_file::get_or_create` is the
//! extractor that go to definition uses for package objects, so a frame opens
//! the same file. Packages are the `.app` files directly under
//! the project's package folders, the highest version first, read through the
//! symbol cache the language server also fills. The package folders are the
//! ones the DAP's compile step uses (`packageCachePath` and
//! `appLocalFolderPaths`), found by [`load_dap_project`].

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use al_dap::dap::bc_debug::snapshot_capture::version_key;
use al_dap::dap::native_dap::kind_to_object_type;
use al_symbols::model::SymbolPackage;

/// The packages of a project, loaded on first use.
pub struct PackageSources {
    project_root: PathBuf,
    packages: OnceLock<Vec<(PathBuf, SymbolPackage)>>,
}

impl PackageSources {
    pub fn new(project_root: PathBuf) -> Self {
        PackageSources {
            project_root,
            packages: OnceLock::new(),
        }
    }

    fn packages(&self) -> &[(PathBuf, SymbolPackage)] {
        self.packages.get_or_init(|| {
            let list = match load_dap_project(&self.project_root) {
                Ok((_, project)) => project.packages,
                Err(error) => {
                    tracing::warn!(%error, "no package sources for the replay");
                    return Vec::new();
                }
            };
            let cache = al_symbols::cache::SymbolCache::default_location();
            let mut loaded: Vec<(PathBuf, SymbolPackage)> = list
                .into_iter()
                .filter_map(|p| {
                    let package = cache
                        .load(&p)
                        .or_else(|| al_symbols::app_reader::read_app_file(&p).ok())?;
                    Some((p, package))
                })
                .collect();
            loaded.sort_by_key(|(_, pkg)| std::cmp::Reverse(version_key(&pkg.version)));
            loaded
        })
    }

    /// The virtual source file of object `(object_type, object_id)`, or `None`
    /// when no package declares it, the package ships no source, or the file
    /// would be an outline built from symbols (its lines are not source lines).
    pub fn source_path(&self, object_type: i32, object_id: i32) -> Option<PathBuf> {
        for (app, package) in self.packages() {
            let Some(entry) = package.objects.iter().find(|o| {
                !o.synthetic
                    && o.id == object_id
                    && kind_to_object_type(&o.kind.to_string()) == object_type
            }) else {
                continue;
            };
            if !al_symbols::virtual_file::app_has_source(app).unwrap_or(false) {
                continue;
            }
            let path = al_symbols::virtual_file::get_or_create(entry, Some(app)).ok()?;
            return keep_real_source(path);
        }
        None
    }
}

/// `path` unless it holds a generated outline.
fn keep_real_source(path: PathBuf) -> Option<PathBuf> {
    let text = std::fs::read_to_string(&path).ok()?;
    (!al_symbols::virtual_file::is_generated_outline(&text)).then_some(path)
}

/// The effective configuration of the project at `project_root` and the
/// project with its symbol settings applied. The compile step and the package
/// source lookup both call this, so they read the same package folders.
///
/// The configuration is the project's own, then `AL_DAP_SETTINGS_JSON` from the
/// extension, then the trust gate.
pub fn load_dap_project(
    project_root: &Path,
) -> Result<(al_project::config::AlConfig, al_project::project::AlProject), String> {
    load_project_with_settings(project_root, std::env::var("AL_DAP_SETTINGS_JSON"))
}

fn load_project_with_settings(
    project_root: &Path,
    settings: Result<String, std::env::VarError>,
) -> Result<(al_project::config::AlConfig, al_project::project::AlProject), String> {
    let mut config = al_project::config::AlConfig::load_effective(project_root)
        .map_err(|error| error.to_string())?;
    match settings {
        Ok(settings) => {
            let unknown = config
                .merge_editor_settings_json(&settings)
                .map_err(|error| format!("invalid AL_DAP_SETTINGS_JSON from extension: {error}"))?;
            if !unknown.is_empty() {
                return Err(format!(
                    "unknown AL_DAP_SETTINGS_JSON keys: {}",
                    unknown.join(", ")
                ));
            }
        }
        Err(std::env::VarError::NotPresent) => {}
        Err(error) => return Err(format!("cannot read AL_DAP_SETTINGS_JSON: {error}")),
    }
    // The extension reads Zed's merged settings, so a value the worktree's own
    // `.zed/settings.json` supplied arrives here looking exactly like one the
    // user wrote.
    let decision =
        al_project::trust::gate(project_root, &mut config).map_err(|error| error.to_string())?;
    if let Some(advisory) = decision.advisory() {
        tracing::warn!("{advisory}");
    }
    if let Some(advisory) = al_project::trust::enforce_dotnet_path(project_root) {
        tracing::warn!("{advisory}");
    }
    let mut project =
        al_project::project::find_project(project_root).map_err(|error| error.to_string())?;
    project
        .apply_symbol_settings(&config)
        .map_err(|error| error.to_string())?;
    Ok((config, project))
}

/// The resolver the native adapter takes, for the project at `project_root`.
pub fn resolver(project_root: &Path) -> al_dap::dap::native_dap::PackageSourceResolver {
    let sources = PackageSources::new(project_root.to_path_buf());
    std::sync::Arc::new(move |t, n| sources.source_path(t, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outline_file_is_refused_and_real_source_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let outline = dir.path().join("outline.al");
        std::fs::write(
            &outline,
            format!(
                "{}codeunit 1 X\n{{\n}}\n",
                al_symbols::virtual_file::OUTLINE_NOTE
            ),
        )
        .unwrap();
        let metadata = dir.path().join("metadata.al");
        std::fs::write(
            &metadata,
            format!(
                "{}codeunit 1 X\n",
                al_symbols::virtual_file::METADATA_ONLY_NOTE
            ),
        )
        .unwrap();
        let real = dir.path().join("real.al");
        std::fs::write(&real, "codeunit 1 X\n{\n}\n").unwrap();
        assert_eq!(keep_real_source(outline), None);
        assert_eq!(keep_real_source(metadata), None);
        assert_eq!(keep_real_source(real.clone()), Some(real));
    }

    #[test]
    fn packages_come_from_the_configured_package_cache_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("app.json"),
            r#"{"id":"11111111-1111-1111-1111-111111111111","name":"T","publisher":"P","version":"1.0.0.0"}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".alpackages")).unwrap();
        std::fs::write(root.join(".alpackages/Default_Pkg_1.0.0.0.app"), b"x").unwrap();
        std::fs::create_dir_all(root.join("custom-cache")).unwrap();
        std::fs::write(root.join("custom-cache/Custom_Pkg_1.0.0.0.app"), b"x").unwrap();
        let settings = r#"{"packageCachePath":"custom-cache"}"#.to_string();
        let (_, project) = load_project_with_settings(root, Ok(settings)).expect("project loads");
        assert_eq!(project.packages_dir, root.join("custom-cache"));
        assert_eq!(
            project.packages,
            vec![root.join("custom-cache/Custom_Pkg_1.0.0.0.app")]
        );
    }

    #[test]
    fn a_project_without_packages_has_no_package_source() {
        let dir = tempfile::tempdir().unwrap();
        let sources = PackageSources::new(dir.path().to_path_buf());
        assert_eq!(sources.source_path(2, 27), None);
    }
}
