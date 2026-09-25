//! Reading `.app` packages into the index, with the on-disk derived cache
//! that lets a warm start skip re-parsing them.

use super::write_derived_cache;
use super::{fold_name, package_identity_key, AppPathRecord, IndexedEntry, SymbolIndex};
use super::{PackageLoadError, PackageLoadFailure};
use crate::app_reader;
use crate::model::{ObjectKind, SymbolEntry, SymbolPackage};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{debug, warn};

pub(super) fn complete_package_batch<T>(
    parsed: Vec<Result<T, PackageLoadFailure>>,
) -> Result<Vec<T>, PackageLoadError> {
    let failures = parsed
        .iter()
        .filter_map(|result| result.as_ref().err().cloned())
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        return Err(PackageLoadError { failures });
    }
    Ok(parsed
        .into_iter()
        .map(|result| result.expect("failures were checked above"))
        .collect())
}

/// Keep one package per identity (app id, display name as fallback): the
/// highest version, or the first seen when versions tie or do not compare.
/// Kept items stay in input order.
///
/// `.alpackages` often holds two versions of one app whose file names do not
/// share a `Publisher_Name_` prefix, so the file-name dedup in al-project
/// misses them. Indexing both made the later file in path order replace the
/// earlier, which could put the older version in the index and left the
/// package list reporting two rows for one set of symbols.
pub(super) fn newest_per_identity<T>(
    items: Vec<T>,
    package: impl Fn(&T) -> &SymbolPackage,
) -> Vec<T> {
    let mut kept: Vec<Option<T>> = Vec::with_capacity(items.len());
    let mut slot_by_identity: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for item in items {
        let pkg = package(&item);
        let identity = package_identity_key(&pkg.app_id, &pkg.name);
        match slot_by_identity.get(&identity) {
            Some(&slot) => {
                let existing = package(kept[slot].as_ref().expect("slots are filled"));
                let newer = crate::model::version_at_least(&pkg.version, &existing.version)
                    && !crate::model::version_at_least(&existing.version, &pkg.version);
                let (dropped, winner) = if newer {
                    (&existing.version, &pkg.version)
                } else {
                    (&pkg.version, &existing.version)
                };
                debug!(
                    name = %pkg.name,
                    app_id = %pkg.app_id,
                    kept = %winner,
                    superseded = %dropped,
                    "two versions of one package; indexing the newest"
                );
                if newer {
                    kept[slot] = Some(item);
                }
            }
            None => {
                slot_by_identity.insert(identity, kept.len());
                kept.push(Some(item));
            }
        }
    }
    kept.into_iter().flatten().collect()
}

/// One `.app` parsed through the disk cache: `(path, package, from_cache)`.
type CachedPackageLoad = Result<(PathBuf, SymbolPackage, bool), PackageLoadFailure>;

/// Load one `.app`, preferring the on-disk symbol cache and repopulating it on
/// a miss.
///
/// The three cached entry points ([`SymbolIndex::load_packages_cached`],
/// [`SymbolIndex::load_packages_cached_lenient`] and
/// [`SymbolIndex::replace_packages_cached`]) had byte-identical copies of this
/// body and differed only in what they do with the failures afterwards. Keep
/// the caching/prewarm policy in exactly one place so the three paths cannot
/// drift apart.
pub(super) fn load_package_via_cache(
    path: &Path,
    cache: &crate::cache::SymbolCache,
) -> CachedPackageLoad {
    if let Some(pkg) = cache.load(path) {
        prewarm_source_index(path);
        return Ok((path.to_path_buf(), pkg, true));
    }

    match app_reader::read_app_file(path) {
        Ok(pkg) => {
            prewarm_source_index(path);
            debug!(
                name = %pkg.name,
                objects = pkg.objects.len(),
                path = %path.display(),
                "Loaded package (cache miss)"
            );
            if let Err(error) = cache.save(path, &pkg) {
                warn!(path = %path.display(), %error, "Failed to save to cache");
            }
            Ok((path.to_path_buf(), pkg, false))
        }
        Err(error) => Err(PackageLoadFailure {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// Parse a whole batch of `.app` files in parallel through the disk cache.
///
/// ZIP/JSON decoding is the expensive part and is embarrassingly parallel;
/// callers apply the results to the shared indexes sequentially, in input
/// order, so indexing stays deterministic.
pub(super) fn load_package_batch_via_cache(
    paths: &[impl AsRef<Path> + Sync],
    cache: &crate::cache::SymbolCache,
) -> Vec<CachedPackageLoad> {
    use rayon::prelude::*;

    paths
        .par_iter()
        .map(|path| load_package_via_cache(path.as_ref(), cache))
        .collect()
}

pub(super) fn prewarm_source_index(path: &Path) {
    if let Err(error) = crate::source_index::get_or_build(path) {
        debug!(
            path = %path.display(),
            %error,
            "Package has no usable embedded-source index"
        );
    }
}

impl SymbolIndex {
    /// Replace the entire symbol generation with a separately validated index.
    ///
    /// This deliberately rebuilds secondary indexes from the staged entries
    /// instead of copying their internal maps independently. Callers publishing
    /// into a live workspace must hold the workspace-generation write lock.
    pub fn replace_with(&self, replacement: &SymbolIndex) {
        // Share the staged Arc payloads instead of deep-cloning every
        // SymbolEntry: a generation swap must not duplicate the entire
        // multi-megabyte symbol payload.
        let mut entries: Vec<(usize, IndexedEntry)> = replacement
            .all
            .iter()
            .map(|entry| (*entry.key(), entry.value().clone()))
            .collect();
        entries.sort_unstable_by_key(|(seq, _)| *seq);
        let app_paths = replacement
            .app_paths
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect::<Vec<_>>();
        let source_paths = replacement
            .source_path_cache
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect::<Vec<_>>();

        self.by_name.clear();
        *write_derived_cache(&self.sorted_names, "sorted_names").0 = None;
        self.by_kind_id.clear();
        self.by_kind.clear();
        self.by_extends.clear();
        self.by_package.clear();
        self.all.clear();
        self.next_id.store(0, std::sync::atomic::Ordering::Relaxed);
        self.app_paths.clear();
        self.source_path_cache.clear();
        self.composed_cache.clear();
        write_derived_cache(&self.default_completions, "default_completions")
            .0
            .clear();

        let arcs: Vec<Arc<SymbolEntry>> = entries
            .into_iter()
            .map(|(_, entry)| self.add_arc(entry.arc, entry.package_key))
            .collect();
        self.update_sorted_names_after_add(&arcs, false);
        self.update_default_completions(&arcs);
        for (key, value) in app_paths {
            self.app_paths.insert(key, value);
        }
        for (key, value) in source_paths {
            self.source_path_cache.insert(key, value);
        }
        self.note_mutation();
    }

    /// Load and index all .app files from the given paths.
    ///
    /// The batch is atomic: if any configured package is unreadable or invalid,
    /// nothing from this call is indexed and every failed path is returned.
    pub fn load_packages(
        &self,
        paths: &[impl AsRef<Path> + Sync],
    ) -> Result<Vec<SymbolPackage>, PackageLoadError> {
        use rayon::prelude::*;

        // Parse in parallel, then mutate the shared indexes sequentially. The
        // old implementation performed remove/add operations from rayon workers,
        // so duplicate versions of one package could interleave and leave both
        // copies indexed. Keeping the expensive ZIP/JSON work parallel while
        // applying results in input order is deterministic and idempotent.
        let parsed: Vec<_> = paths
            .par_iter()
            .map(|path| {
                let path = path.as_ref();
                match app_reader::read_app_file(path) {
                    Ok(pkg) => {
                        prewarm_source_index(path);
                        Ok((path.to_path_buf(), pkg))
                    }
                    Err(error) => Err(PackageLoadFailure {
                        path: path.to_path_buf(),
                        message: error.to_string(),
                    }),
                }
            })
            .collect();
        let parsed = newest_per_identity(complete_package_batch(parsed)?, |(_, pkg)| pkg);

        let mut results = Vec::with_capacity(parsed.len());
        for (path, pkg) in parsed {
            results.push(self.index_loaded_package(pkg, Some(&path), false));
        }
        Ok(results)
    }

    /// Load and index .app files with disk caching.
    ///
    /// For each .app file, checks the disk cache first. If the cache is valid
    /// (same file modification time and size), loads from cache. Otherwise,
    /// parses the .app file and saves the result to cache.
    pub fn load_packages_cached(
        &self,
        paths: &[impl AsRef<Path> + Sync],
        cache: &crate::cache::SymbolCache,
    ) -> Result<Vec<SymbolPackage>, PackageLoadError> {
        cache.gc_once();
        let parsed = newest_per_identity(
            complete_package_batch(load_package_batch_via_cache(paths, cache))?,
            |(_, pkg, _)| pkg,
        );

        let mut results = Vec::with_capacity(parsed.len());
        for (path, pkg, from_cache) in parsed {
            results.push(self.index_loaded_package(pkg, Some(&path), from_cache));
        }
        Ok(results)
    }

    /// Like [`Self::load_packages_cached`], but degrades per package instead
    /// of failing the whole batch: every readable package is indexed and each
    /// unreadable/corrupt one is reported in the returned failure list. Use
    /// this on initialization paths where one truncated `.app` (e.g. an
    /// interrupted download) must not abort the entire workspace.
    pub fn load_packages_cached_lenient(
        &self,
        paths: &[impl AsRef<Path> + Sync],
        cache: &crate::cache::SymbolCache,
    ) -> (Vec<SymbolPackage>, Vec<PackageLoadFailure>) {
        cache.gc_once();
        let mut loaded = Vec::new();
        let mut failures = Vec::new();
        for item in load_package_batch_via_cache(paths, cache) {
            match item {
                Ok(load) => loaded.push(load),
                Err(failure) => failures.push(failure),
            }
        }

        let results = newest_per_identity(loaded, |(_, pkg, _)| pkg)
            .into_iter()
            .map(|(path, pkg, from_cache)| self.index_loaded_package(pkg, Some(&path), from_cache))
            .collect();
        (results, failures)
    }

    /// Replace every file-backed package currently in the index with `paths`.
    ///
    /// Parsing completes before the existing generation is removed, so a bad or
    /// slow package cannot leave the index empty while it is being read. This is
    /// used when package-path settings change at runtime.
    pub fn replace_packages_cached(
        &self,
        paths: &[impl AsRef<Path> + Sync],
        cache: &crate::cache::SymbolCache,
    ) -> Result<Vec<SymbolPackage>, PackageLoadError> {
        cache.gc_once();
        let parsed = newest_per_identity(
            complete_package_batch(load_package_batch_via_cache(paths, cache))?,
            |(_, pkg, _)| pkg,
        );

        self.clear_loaded_packages();
        let mut results = Vec::with_capacity(parsed.len());
        for (path, pkg, from_cache) in parsed {
            results.push(self.index_loaded_package(pkg, Some(&path), from_cache));
        }
        Ok(results)
    }

    pub(super) fn index_loaded_package(
        &self,
        mut pkg: SymbolPackage,
        path: Option<&Path>,
        from_cache: bool,
    ) -> SymbolPackage {
        // Re-loading a downloaded or changed package replaces its old symbols
        // rather than duplicating every object in all secondary indexes.
        // Replacement is keyed by the canonical package identity (app GUID,
        // display name as fallback), so two distinct apps that merely share a
        // display name never evict each other's generation.
        let identity = package_identity_key(&pkg.app_id, &pkg.name);
        self.remove_package_identities(&std::collections::HashSet::from([identity.clone()]));
        if let Some(path) = path {
            // The source-index cache canonicalizes package paths before
            // warming them. Publish the same identity so cache-only user
            // queries work through symlinks and macOS `/var` aliases.
            let indexed_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            // A path holds exactly one package. If a manifest rewrite changed
            // the identity stored for this same file, drop the stale
            // generation so it cannot linger under the old key.
            let stale: std::collections::HashSet<String> = self
                .app_paths
                .iter()
                .filter(|record| record.value().path == indexed_path && record.key() != &identity)
                .map(|record| record.key().clone())
                .collect();
            if !stale.is_empty() {
                self.remove_package_identities(&stale);
            }
            self.app_paths.insert(
                identity.clone(),
                AppPathRecord {
                    name_key: fold_name(&pkg.name),
                    path: indexed_path,
                },
            );
        }
        debug!(
            name = %pkg.name,
            app_id = %pkg.app_id,
            objects = pkg.objects.len(),
            from_cache,
            "Indexing package"
        );
        self.add_entries_owned_with_key(std::mem::take(&mut pkg.objects), Some(identity));
        pkg
    }

    pub fn load_package_bytes(
        &self,
        data: &[u8],
    ) -> Result<SymbolPackage, app_reader::AppReaderError> {
        let pkg = app_reader::read_app_bytes(data)?;
        let identity = package_identity_key(&pkg.app_id, &pkg.name);
        self.remove_package_identities(&std::collections::HashSet::from([identity.clone()]));
        self.add_entries_owned_with_key(pkg.objects.clone(), Some(identity));
        Ok(pkg)
    }

    /// Load well-known runtime enum types that are built into the AL compiler
    /// but not published in any .app package's SymbolReference.json.
    pub fn load_runtime_enums(&self) {
        use crate::model::{EnumValueSymbol, SymbolEntry};

        let mut entries = Vec::new();
        for re in al_syntax::language_data::runtime_enums() {
            if self
                .get_by_name(&re.name)
                .iter()
                .any(|entry| entry.kind == ObjectKind::Enum)
            {
                continue;
            }
            let enum_values: Vec<EnumValueSymbol> = re
                .values
                .iter()
                .enumerate()
                .map(|(i, v)| EnumValueSymbol {
                    ordinal: i as i32,
                    name: v.clone(),
                })
                .collect();
            entries.push(SymbolEntry {
                kind: ObjectKind::Enum,
                id: -1,
                // Real platform enums (not synthetic): browsable, but the
                // -1 sentinel is never displayed as an object ID.
                synthetic: false,
                name: re.name.clone(),
                extends: None,
                implements: Vec::new(),
                package: "Runtime".to_string(),
                namespace: String::new(),
                methods: Vec::new(),
                fields: Vec::new(),
                controls: Vec::new(),
                enum_values,
                keys: Vec::new(),
                properties: Vec::new(),
                permissions: Vec::new(),
                variables: Vec::new(),
            });
        }

        if !entries.is_empty() {
            debug!(count = entries.len(), "Loaded runtime enum definitions");
            self.add_entries(&entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::newest_per_identity;
    use crate::index::test_support::*;
    use crate::index::SymbolIndex;
    use crate::model::{ObjectKind, SymbolPackage};

    fn package(app_id: &str, name: &str, version: &str, object_names: &[&str]) -> SymbolPackage {
        let objects: Vec<_> = object_names
            .iter()
            .enumerate()
            .map(|(offset, object)| {
                let mut entry = make_entry(ObjectKind::Table, 50_000 + offset as i32, object);
                entry.package = name.to_string();
                entry
            })
            .collect();
        SymbolPackage {
            app_id: app_id.to_string(),
            name: name.to_string(),
            publisher: "Microsoft".to_string(),
            version: version.to_string(),
            object_count: objects.len(),
            objects,
        }
    }

    const SYSTEM_ID: &str = "8874ed3a-0643-4247-9ced-7a7002f7135d";

    #[test]
    fn the_newest_version_of_one_app_is_kept_whatever_the_path_order() {
        let older = package(SYSTEM_ID, "System", "27.0.46760.0", &["Old"]);
        let newer = package(SYSTEM_ID, "System", "28.0.51202.0", &["New"]);
        for batch in [
            vec![older.clone(), newer.clone()],
            vec![newer.clone(), older.clone()],
        ] {
            let kept = newest_per_identity(batch, |pkg| pkg);
            assert_eq!(kept.len(), 1);
            assert_eq!(kept[0].version, "28.0.51202.0");
        }
    }

    #[test]
    fn distinct_apps_sharing_a_name_are_both_kept_in_order() {
        let first = package(
            "11111111-0000-0000-0000-000000000000",
            "Utilities",
            "1.0.0.0",
            &[],
        );
        let second = package(
            "22222222-0000-0000-0000-000000000000",
            "Utilities",
            "1.0.0.0",
            &[],
        );
        let base = package("", "Base Application", "28.0.0.0", &[]);
        let kept = newest_per_identity(vec![first, base, second], |pkg| pkg);
        let ids: Vec<_> = kept.iter().map(|pkg| pkg.app_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "11111111-0000-0000-0000-000000000000",
                "",
                "22222222-0000-0000-0000-000000000000"
            ]
        );
    }

    #[test]
    fn a_tie_or_an_unparseable_version_keeps_the_first() {
        let first = package(SYSTEM_ID, "System", "preview", &["First"]);
        let second = package(SYSTEM_ID, "System", "28.0.0.0", &["Second"]);
        let kept = newest_per_identity(vec![first, second], |pkg| pkg);
        assert_eq!(kept[0].objects[0].name, "First");
    }

    #[test]
    fn availability_counts_one_identity_not_every_package_with_its_name() {
        let index = SymbolIndex::new();
        index.index_loaded_package(
            package(
                "11111111-0000-0000-0000-000000000000",
                "Utilities",
                "1.0.0.0",
                &["A", "B"],
            ),
            None,
            false,
        );
        index.index_loaded_package(
            package(
                "22222222-0000-0000-0000-000000000000",
                "Utilities",
                "1.0.0.0",
                &["C"],
            ),
            None,
            false,
        );
        let total = |summary: crate::source_availability::SourceAvailabilitySummary| {
            summary.workspace_source
                + summary.embedded_source
                + summary.generated_outline
                + summary.metadata_only
        };
        assert_eq!(
            total(index.package_source_availability_for(
                "11111111-0000-0000-0000-000000000000",
                "Utilities"
            )),
            2
        );
        assert_eq!(
            total(index.package_source_availability_for(
                "22222222-0000-0000-0000-000000000000",
                "Utilities"
            )),
            1
        );
        assert_eq!(
            total(index.package_source_availability_for(
                "33333333-0000-0000-0000-000000000000",
                "Utilities"
            )),
            0,
            "an identity that was never indexed reports nothing"
        );
        assert_eq!(total(index.package_source_availability("Utilities")), 3);
    }

    #[test]
    fn replace_with_rebuilds_every_lookup_and_completion_index() {
        let active = SymbolIndex::new();
        active.add_entries(&[make_entry(ObjectKind::Table, 50_100, "Old")]);
        assert_eq!(active.default_completions_snapshot().len(), 1);
        let _ = active.search("Old", 10);

        let staged = SymbolIndex::new();
        staged.add_entries(&[
            make_entry(ObjectKind::Page, 50_101, "New"),
            make_extension(ObjectKind::PageExtension, 50_102, "New Ext", "New"),
        ]);

        active.replace_with(&staged);

        assert!(active.find_by_name("Old").is_none());
        assert!(active.get_by_id(ObjectKind::Table, 50_100).is_empty());
        assert!(active.search("Old", 10).is_empty());
        assert_eq!(active.find_by_name("New").unwrap().kind, ObjectKind::Page);
        assert_eq!(
            active
                .get_by_id(ObjectKind::Page, 50_101)
                .first()
                .unwrap()
                .name,
            "New"
        );
        assert_eq!(active.get_extensions_of("New").len(), 1);
        assert_eq!(active.default_completions_snapshot().len(), 2);
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn runtime_enum_is_not_shadowed_by_non_enum_with_same_name() {
        let index = SymbolIndex::new();
        let runtime_name = al_syntax::language_data::runtime_enums()[0].name.clone();
        index.add_entries(&[make_entry(ObjectKind::Table, 50_100, &runtime_name)]);

        index.load_runtime_enums();

        assert!(
            index
                .get_by_name(&runtime_name)
                .iter()
                .any(|entry| entry.kind == ObjectKind::Enum),
            "a table name collision must not suppress the compiler runtime enum"
        );
    }

    #[test]
    fn load_packages_resolves_app_from_an_arbitrary_local_folder() {
        // A folder that is deliberately NOT `.alpackages` — i.e. the kind of
        // path a user would list in `appLocalFolderPaths`.
        let dir = tempfile::tempdir().unwrap();
        let local_folder = dir.path().join("local-apps");
        std::fs::create_dir_all(&local_folder).unwrap();
        let app_path = local_folder.join("Contoso_LocalLib.app");
        std::fs::write(&app_path, build_app("Local Lib", 50123, "Local Widget")).unwrap();

        let index = SymbolIndex::new();
        let loaded = index
            .load_packages(std::slice::from_ref(&app_path))
            .expect("valid local package");

        // The package parsed and contributed its object...
        assert_eq!(loaded.len(), 1, "the local-folder .app should load");
        assert_eq!(loaded[0].name, "Local Lib");

        // ...the object is queryable through the normal index...
        let hits = index.get_by_name("Local Widget");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 50123);
        assert_eq!(hits[0].kind, ObjectKind::Table);

        // ...and the index records the resolved local package file that
        // `appLocalFolderPaths` discovered.
        let resolved_app_path = std::fs::canonicalize(&app_path).unwrap();
        assert_eq!(
            index.app_path("Local Lib").as_deref(),
            Some(resolved_app_path.as_path())
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn package_path_alias_keeps_prewarmed_source_index_reachable() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let real_folder = dir.path().join("real-packages");
        let alias_folder = dir.path().join("package-alias");
        std::fs::create_dir_all(&real_folder).unwrap();
        symlink(&real_folder, &alias_folder).unwrap();

        let real_app_path = real_folder.join("Contoso_Aliased.app");
        let alias_app_path = alias_folder.join("Contoso_Aliased.app");
        std::fs::write(
            &real_app_path,
            build_app("Aliased", 50_125, "Aliased Widget"),
        )
        .unwrap();

        let index = SymbolIndex::new();
        assert_eq!(
            index
                .load_packages(std::slice::from_ref(&alias_app_path))
                .expect("valid aliased package")
                .len(),
            1
        );

        let indexed_path = index.app_path("Aliased").unwrap();
        assert_eq!(
            indexed_path,
            std::fs::canonicalize(&alias_app_path).unwrap()
        );
        assert!(
            crate::source_index::get_cached(&indexed_path).is_some(),
            "the path published by SymbolIndex must address the prewarmed source index"
        );
    }

    #[test]
    fn reloading_same_package_replaces_symbols_instead_of_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let app_path = dir.path().join("Contoso_LocalLib.app");
        std::fs::write(&app_path, build_app("Local Lib", 50_123, "Old Widget")).unwrap();
        let index = SymbolIndex::new();
        assert_eq!(
            index
                .load_packages(std::slice::from_ref(&app_path))
                .expect("valid first generation")
                .len(),
            1
        );

        std::fs::write(&app_path, build_app("Local Lib", 50_124, "New Widget")).unwrap();
        assert_eq!(
            index
                .load_packages(std::slice::from_ref(&app_path))
                .expect("valid replacement generation")
                .len(),
            1
        );

        assert_eq!(index.len(), 1);
        assert!(index.get_by_name("Old Widget").is_empty());
        assert_eq!(index.get_by_name("New Widget").len(), 1);
    }

    #[test]
    fn replace_packages_removes_paths_no_longer_configured() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("First.app");
        let second = dir.path().join("Second.app");
        std::fs::write(&first, build_app("First", 50_001, "First Table")).unwrap();
        std::fs::write(&second, build_app("Second", 50_002, "Second Table")).unwrap();
        let cache = crate::cache::SymbolCache::at(dir.path().join("cache"));
        let index = SymbolIndex::new();
        index
            .load_packages_cached(&[first.clone(), second.clone()], &cache)
            .expect("valid initial package generation");

        index
            .replace_packages_cached(std::slice::from_ref(&second), &cache)
            .expect("valid replacement package generation");

        assert!(index.get_by_name("First Table").is_empty());
        assert_eq!(index.get_by_name("Second Table").len(), 1);
        assert!(index.app_path("First").is_none());
        let resolved_second = std::fs::canonicalize(&second).unwrap();
        assert_eq!(
            index.app_path("Second").as_deref(),
            Some(resolved_second.as_path())
        );
    }

    #[test]
    fn replacement_keeps_previous_generation_when_every_new_file_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let valid = dir.path().join("Valid.app");
        let invalid = dir.path().join("Invalid.app");
        std::fs::write(&valid, build_app("Keep", 50_001, "Keep Table")).unwrap();
        std::fs::write(&invalid, b"not an app").unwrap();
        let cache = crate::cache::SymbolCache::at(dir.path().join("cache"));
        let index = SymbolIndex::new();
        assert_eq!(
            index
                .load_packages_cached(std::slice::from_ref(&valid), &cache)
                .expect("valid initial package")
                .len(),
            1
        );

        let error = index
            .replace_packages_cached(std::slice::from_ref(&invalid), &cache)
            .expect_err("invalid replacement must fail");

        assert_eq!(error.failures.len(), 1);
        assert_eq!(error.failures[0].path, invalid);
        assert!(index.find_by_name("Keep Table").is_some());
        let resolved_valid = std::fs::canonicalize(&valid).unwrap();
        assert_eq!(
            index.app_path("Keep").as_deref(),
            Some(resolved_valid.as_path())
        );
    }

    #[test]
    fn package_batches_are_atomic_when_one_file_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let valid = dir.path().join("Valid.app");
        let invalid = dir.path().join("Invalid.app");
        std::fs::write(&valid, build_app("Valid", 50_001, "Valid Table")).unwrap();
        std::fs::write(&invalid, b"not an app").unwrap();

        let direct = SymbolIndex::new();
        let error = direct
            .load_packages(&[valid.clone(), invalid.clone()])
            .expect_err("mixed direct batch must fail");
        assert_eq!(error.failures.len(), 1);
        assert!(direct.is_empty(), "direct load published a partial batch");

        let cached = SymbolIndex::new();
        let cache = crate::cache::SymbolCache::at(dir.path().join("cache"));
        let error = cached
            .load_packages_cached(&[valid, invalid], &cache)
            .expect_err("mixed cached batch must fail");
        assert_eq!(error.failures.len(), 1);
        assert!(cached.is_empty(), "cached load published a partial batch");
    }

    #[test]
    fn lenient_load_indexes_valid_packages_and_reports_corrupt_ones() {
        let dir = tempfile::tempdir().unwrap();
        let valid = dir.path().join("Valid.app");
        let invalid = dir.path().join("Invalid.app");
        std::fs::write(&valid, build_app("Valid", 50_001, "Valid Table")).unwrap();
        std::fs::write(&invalid, b"NAVX truncated download").unwrap();
        let cache = crate::cache::SymbolCache::at(dir.path().join("cache"));

        let index = SymbolIndex::new();
        let (loaded, failures) =
            index.load_packages_cached_lenient(&[valid, invalid.clone()], &cache);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Valid");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].path, invalid);
        assert_eq!(
            index.get_by_name("Valid Table").len(),
            1,
            "the valid package must be indexed despite the corrupt sibling"
        );
    }

    /// Two apps that share a display name but carry different app GUIDs must
    /// keep independent symbol generations: loading (or reloading) one must
    /// never evict the other's symbols or app path.
    #[test]
    fn packages_sharing_a_display_name_keep_independent_generations() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("VendorA_Library.app");
        let second = dir.path().join("VendorB_Library.app");
        std::fs::write(
            &first,
            build_app_with_id(
                "11111111-1111-1111-1111-111111111111",
                "Library",
                50_100,
                "Vendor A Widget",
            ),
        )
        .unwrap();
        std::fs::write(
            &second,
            build_app_with_id(
                "22222222-2222-2222-2222-222222222222",
                "Library",
                50_200,
                "Vendor B Widget",
            ),
        )
        .unwrap();

        let index = SymbolIndex::new();
        index
            .load_packages(&[first.clone(), second.clone()])
            .expect("both same-named packages load");

        assert_eq!(index.get_by_name("Vendor A Widget").len(), 1);
        assert_eq!(index.get_by_name("Vendor B Widget").len(), 1);
        assert_eq!(index.len(), 2, "both generations must coexist");

        // Reloading the second package must replace only its own generation.
        std::fs::write(
            &second,
            build_app_with_id(
                "22222222-2222-2222-2222-222222222222",
                "Library",
                50_201,
                "Vendor B Widget V2",
            ),
        )
        .unwrap();
        index
            .load_packages(std::slice::from_ref(&second))
            .expect("reload of one same-named package");

        assert_eq!(
            index.get_by_name("Vendor A Widget").len(),
            1,
            "reloading vendor B must not evict vendor A's symbols"
        );
        assert!(index.get_by_name("Vendor B Widget").is_empty());
        assert_eq!(index.get_by_name("Vendor B Widget V2").len(), 1);
        assert!(
            index.app_path("Library").is_some(),
            "a name-based path lookup must still resolve deterministically"
        );
    }
}
