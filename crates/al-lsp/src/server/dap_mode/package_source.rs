//! The source file a snapshot replay shows for an object of a dependency
//! package.
//!
//! Standard route checked: `al_symbols::virtual_file::get_or_create` is the
//! extractor that go to definition uses for package objects, so a frame opens
//! the same file. Packages are the `.app` files directly under
//! `<project root>/.alpackages`, the highest version first, read through the
//! symbol cache the language server also fills.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use al_dap::dap::bc_debug::snapshot_capture::version_key;
use al_dap::dap::native_dap::kind_to_object_type;
use al_symbols::model::SymbolPackage;

/// The `.alpackages` packages of a project, loaded on first use.
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
            let Ok(entries) = std::fs::read_dir(self.project_root.join(".alpackages")) else {
                return Vec::new();
            };
            let cache = al_symbols::cache::SymbolCache::default_location();
            let mut loaded: Vec<(PathBuf, SymbolPackage)> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")))
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
    fn a_project_without_packages_has_no_package_source() {
        let dir = tempfile::tempdir().unwrap();
        let sources = PackageSources::new(dir.path().to_path_buf());
        assert_eq!(sources.source_path(2, 27), None);
    }
}
