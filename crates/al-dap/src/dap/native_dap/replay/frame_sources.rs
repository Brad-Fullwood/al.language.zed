//! Which file a stack frame of a replay shows for each object.
//!
//! The order follows Microsoft's AL host (`GetOrAddDocumentAsync`):
//! 1. the workspace declares the object: the workspace file,
//! 2. the snapshot zip ships the object's source: the extracted read-only copy,
//! 3. a dependency package has the source: the virtual file that go to
//!    definition opens for package objects,
//! 4. none of these: the frame has no source.
//!
//! Standard route checked: the workspace and package lookups are the existing
//! `resolve_path` callback and the al-symbols virtual file, both injected by
//! al-lsp. This module only decides the order.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use al_bc::snapshot::archive::{ObjectType, SnapshotArchive};

/// Maps a BC object type number and id to the virtual source file of a
/// dependency package that ships source. `None` when no package has the
/// object, the package has no source, or only a generated outline exists.
/// Injected by the caller (al-lsp) because it needs the package symbols.
pub type PackageSourceResolver = Arc<dyn Fn(i32, i32) -> Option<PathBuf> + Send + Sync>;

/// The chosen file per object and the Output texts to send at launch.
pub(super) struct FrameSources {
    pub files: HashMap<(ObjectType, i32), PathBuf>,
    pub notices: Vec<String>,
}

/// Every object a frame, a context or the zip can name, in a stable order.
fn objects(archive: &SnapshotArchive) -> Vec<(ObjectType, i32)> {
    let mut all = archive.source_ids();
    for ctx in archive.method_contexts() {
        all.push((ctx.id.object_type, ctx.id.number));
        all.extend(ctx.frames.iter().map(|f| (f.id.object_type, f.id.number)));
    }
    all.sort_by_key(|(t, n)| (t.as_i8(), *n));
    all.dedup();
    all
}

/// Line endings, a leading BOM and trailing blank space do not make two
/// sources different.
fn same_text(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    norm(a).trim_end() == norm(b).trim_end()
}

/// Pick the file for every object of the archive.
///
/// `extracted` holds the zip's own sources as written to the cache.
/// `read` reads a workspace file for the comparison with the zip's source.
pub(super) fn choose(
    archive: &SnapshotArchive,
    extracted: &HashMap<(ObjectType, i32), PathBuf>,
    workspace: &dyn Fn(i32, i32) -> Option<PathBuf>,
    package: &dyn Fn(i32, i32) -> Option<PathBuf>,
    read: &dyn Fn(&Path) -> Option<String>,
) -> FrameSources {
    let mut files = HashMap::new();
    let mut notices = Vec::new();
    for (t, n) in objects(archive) {
        let key = (t, n);
        let kind = i32::from(t.as_i8());
        if let Some(path) = workspace(kind, n) {
            if let (Some(ran), Some(now)) = (archive.source(t, n), read(&path)) {
                if !same_text(ran, &now) {
                    notices.push(format!(
                        "The workspace file {} differs from the code that ran for {} {n}, so line numbers can be off.\n",
                        path.display(),
                        t.name()
                    ));
                }
            }
            files.insert(key, path);
        } else if let Some(path) = extracted.get(&key) {
            files.insert(key, path.clone());
        } else if let Some(path) = package(kind, n) {
            files.insert(key, path);
        } else {
            notices.push(format!("No source is available for {} {n}.\n", t.name()));
        }
    }
    FrameSources { files, notices }
}
