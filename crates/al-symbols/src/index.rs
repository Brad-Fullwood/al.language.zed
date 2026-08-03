//! Concurrent symbol index backed by DashMap.
//!
//! Provides fast lookup by name, object kind+ID, and substring search
//! across all loaded packages.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use tracing::{debug, warn};

use super::app_reader;
use super::events;
use super::model::{ObjectKind, SymbolEntry, SymbolPackage};
use super::source_availability::{self, SourceAvailability, SourceAvailabilitySummary};

const DEFAULT_COMPLETIONS_CAP: usize = 30;

/// Canonical case-folding for AL package and object names.
///
/// Every name-keyed map in the index uses this one helper so lookups and
/// comparisons can never disagree about non-ASCII folding (`İ`, `ß`, …).
pub(crate) fn fold_name(name: &str) -> String {
    name.to_lowercase()
}

/// Canonical identity key for a loaded package: the app GUID when the
/// manifest provides one, with the folded display name as fallback.
///
/// Two vendors can legitimately ship apps that share a display name; keying
/// package generations by this identity keeps their symbols independent.
pub(crate) fn package_identity_key(app_id: &str, name: &str) -> String {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        format!("name:{}", fold_name(name))
    } else {
        format!("id:{}", app_id.to_ascii_lowercase())
    }
}

/// One entry in the primary `all` map: the shared symbol plus the folded
/// object name and the canonical identity of the package that contributed it.
#[derive(Debug, Clone)]
struct IndexedEntry {
    arc: Arc<SymbolEntry>,
    name_key: String,
    package_key: String,
}

/// A file-backed package path published under its canonical identity key,
/// with the folded display name retained for legacy name-based lookups.
#[derive(Debug, Clone)]
struct AppPathRecord {
    name_key: String,
    path: std::path::PathBuf,
}

/// Acquire a derived-cache read guard, discarding (never inspecting) a value
/// left behind by a panicked writer and replacing it with `T::default()`.
///
/// These locks protect only caches derived from the authoritative DashMap
/// indexes. Rebuilding or emptying them is therefore lossless; applying this
/// recovery rule to authoritative symbol state would not be safe.
fn read_derived_cache<'a, T: Default>(
    lock: &'a std::sync::RwLock<T>,
    component: &'static str,
) -> (std::sync::RwLockReadGuard<'a, T>, bool) {
    let mut repaired = false;
    loop {
        match lock.read() {
            Ok(guard) => return (guard, repaired),
            Err(poisoned) => {
                // Drop the inaccessible read view, then acquire an exclusive
                // guard so the whole cache can be replaced.
                drop(poisoned.into_inner());
                let (guard, recovered) = write_derived_cache(lock, component);
                drop(guard);
                repaired = true;
                if !recovered {
                    // Another thread repaired the cache between our read and
                    // write attempts. Its complete replacement is authoritative.
                    continue;
                }
            }
        }
    }
}

/// Acquire a derived-cache write guard. A poisoned value is overwritten in
/// full before the poison flag is cleared.
fn write_derived_cache<'a, T: Default>(
    lock: &'a std::sync::RwLock<T>,
    component: &'static str,
) -> (std::sync::RwLockWriteGuard<'a, T>, bool) {
    match lock.write() {
        Ok(guard) => (guard, false),
        Err(poisoned) => {
            let mut guard = poisoned.into_inner();
            *guard = T::default();
            lock.clear_poison();
            warn!(
                component,
                "discarded and repaired poisoned derived symbol cache"
            );
            (guard, true)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLoadFailure {
    pub path: std::path::PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLoadError {
    pub failures: Vec<PackageLoadFailure>,
}

impl std::fmt::Display for PackageLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} symbol package{} failed to load",
            self.failures.len(),
            if self.failures.len() == 1 { "" } else { "s" }
        )?;
        for failure in &self.failures {
            write!(
                formatter,
                "; '{}': {}",
                failure.path.display(),
                failure.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for PackageLoadError {}

fn complete_package_batch<T>(
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
fn load_package_via_cache(path: &Path, cache: &super::cache::SymbolCache) -> CachedPackageLoad {
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
fn load_package_batch_via_cache(
    paths: &[impl AsRef<Path> + Sync],
    cache: &super::cache::SymbolCache,
) -> Vec<CachedPackageLoad> {
    use rayon::prelude::*;

    paths
        .par_iter()
        .map(|path| load_package_via_cache(path.as_ref(), cache))
        .collect()
}

fn prewarm_source_index(path: &Path) {
    if let Err(error) = super::source_index::get_or_build(path) {
        debug!(
            path = %path.display(),
            %error,
            "Package has no usable embedded-source index"
        );
    }
}

/// Byte-level accounting for allocations owned by the symbol index.
///
/// `tracked_bytes` covers symbol payloads, duplicated lookup keys, vector
/// capacities, cache payloads, and stored paths. Allocator bucket/slab
/// bookkeeping is intentionally left to the process RSS metric exposed by the
/// workspace diagnostics endpoint.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolIndexMemoryStats {
    pub symbol_payload_bytes: usize,
    pub lookup_index_bytes: usize,
    pub path_cache_bytes: usize,
    pub composed_cache_bytes: usize,
    pub tracked_bytes: usize,
}

/// Thread-safe symbol index over multiple AL packages.
#[derive(Debug)]
pub struct SymbolIndex {
    /// Objects keyed by lowercase name. Multiple objects can share a name
    /// (e.g., a Table and a Page with the same name, or objects from different packages).
    by_name: DashMap<String, Vec<Arc<SymbolEntry>>>,
    /// Lazily-built sorted unique names make relevance-ranked search
    /// deterministic without adding work to package loading.
    sorted_names: std::sync::RwLock<Option<Arc<Vec<String>>>>,
    by_kind_id: DashMap<(ObjectKind, i32), Vec<Arc<SymbolEntry>>>,
    by_kind: DashMap<ObjectKind, Vec<Arc<SymbolEntry>>>,
    by_extends: DashMap<String, Vec<Arc<SymbolEntry>>>,
    /// Entries grouped by folded package *display name*. Serves per-package
    /// search/summary queries without a full-index scan; two identity-distinct
    /// packages sharing a display name legitimately share one bucket here.
    by_package: DashMap<String, Vec<Arc<SymbolEntry>>>,
    all: DashMap<usize, IndexedEntry>,
    next_id: std::sync::atomic::AtomicUsize,
    /// Package paths keyed by canonical identity (`package_identity_key`).
    app_paths: DashMap<String, AppPathRecord>,
    source_path_cache: DashMap<(String, ObjectKind, i32), String>,
    composed_cache: DashMap<(ObjectKind, String), Arc<super::model::ComposedObject>>,
    /// Pre-computed slice of the first DEFAULT_COMPLETIONS_CAP entries for O(1)
    /// default completion responses. Populated by add_entries/add_entries_owned.
    default_completions: std::sync::RwLock<Vec<Arc<SymbolEntry>>>,
    /// Monotonic counter bumped on every entry addition/removal; used to
    /// invalidate derived caches (currently the event catalog) cheaply.
    mutation: std::sync::atomic::AtomicU64,
    /// Cached publisher/subscriber catalog rebuilt lazily when `mutation`
    /// advances, so repeated event queries avoid a full-index scan.
    event_catalog: std::sync::RwLock<Option<events::EventCatalogCache>>,
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self {
            by_name: DashMap::new(),
            sorted_names: std::sync::RwLock::new(None),
            by_kind_id: DashMap::new(),
            by_kind: DashMap::new(),
            by_extends: DashMap::new(),
            by_package: DashMap::new(),
            all: DashMap::new(),
            next_id: std::sync::atomic::AtomicUsize::new(0),
            app_paths: DashMap::new(),
            source_path_cache: DashMap::new(),
            composed_cache: DashMap::new(),
            default_completions: std::sync::RwLock::new(Vec::new()),
            mutation: std::sync::atomic::AtomicU64::new(0),
            event_catalog: std::sync::RwLock::new(None),
        }
    }

    /// Bump the mutation counter so lazily rebuilt derived caches (event
    /// catalog) know the entry set changed.
    fn note_mutation(&self) {
        self.mutation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Shared, lazily rebuilt event catalog for the current entry generation.
    pub(crate) fn event_catalog(&self) -> Arc<events::EventCatalog> {
        let generation = self.mutation.load(std::sync::atomic::Ordering::Acquire);
        {
            let (cache, _) = read_derived_cache(&self.event_catalog, "event_catalog");
            if let Some(cached) = cache.as_ref() {
                if cached.generation == generation {
                    return Arc::clone(&cached.catalog);
                }
            }
        }
        let catalog = Arc::new(events::build_event_catalog(self));
        let (mut cache, _) = write_derived_cache(&self.event_catalog, "event_catalog");
        // A concurrent builder may have stored its own catalog; either is a
        // complete snapshot, so last-write-wins is safe.
        *cache = Some(events::EventCatalogCache {
            generation,
            catalog: Arc::clone(&catalog),
        });
        catalog
    }

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

    pub fn memory_stats(&self) -> SymbolIndexMemoryStats {
        let arc_allocation_overhead = 2 * std::mem::size_of::<usize>();
        let symbol_payload_bytes = self
            .all
            .iter()
            .map(|entry| entry.value().arc.owned_bytes() + arc_allocation_overhead)
            .sum::<usize>();

        let arc_bytes = std::mem::size_of::<Arc<SymbolEntry>>();
        let mut lookup_index_bytes = std::mem::size_of::<Self>();
        lookup_index_bytes += self
            .by_name
            .iter()
            .map(|entry| {
                std::mem::size_of::<String>()
                    + entry.key().capacity()
                    + std::mem::size_of::<Vec<Arc<SymbolEntry>>>()
                    + entry.value().capacity() * arc_bytes
            })
            .sum::<usize>();
        lookup_index_bytes += self
            .by_kind_id
            .iter()
            .map(|entry| {
                std::mem::size_of::<(ObjectKind, i32)>()
                    + std::mem::size_of::<Vec<Arc<SymbolEntry>>>()
                    + entry.value().capacity() * arc_bytes
            })
            .sum::<usize>();
        lookup_index_bytes += self
            .by_kind
            .iter()
            .map(|entry| {
                std::mem::size_of::<ObjectKind>()
                    + std::mem::size_of::<Vec<Arc<SymbolEntry>>>()
                    + entry.value().capacity() * arc_bytes
            })
            .sum::<usize>();
        lookup_index_bytes += self
            .by_extends
            .iter()
            .map(|entry| {
                std::mem::size_of::<String>()
                    + entry.key().capacity()
                    + std::mem::size_of::<Vec<Arc<SymbolEntry>>>()
                    + entry.value().capacity() * arc_bytes
            })
            .sum::<usize>();
        lookup_index_bytes += self
            .by_package
            .iter()
            .map(|entry| {
                std::mem::size_of::<String>()
                    + entry.key().capacity()
                    + std::mem::size_of::<Vec<Arc<SymbolEntry>>>()
                    + entry.value().capacity() * arc_bytes
            })
            .sum::<usize>();
        lookup_index_bytes += self
            .all
            .iter()
            .map(|entry| {
                std::mem::size_of::<usize>()
                    + std::mem::size_of::<IndexedEntry>()
                    + entry.value().name_key.capacity()
                    + entry.value().package_key.capacity()
            })
            .sum::<usize>();
        let (sorted_names, _) = read_derived_cache(&self.sorted_names, "sorted_names");
        if let Some(names) = sorted_names.as_ref() {
            lookup_index_bytes += std::mem::size_of::<Vec<String>>()
                + names.capacity() * std::mem::size_of::<String>()
                + names.iter().map(String::capacity).sum::<usize>();
        }
        drop(sorted_names);
        lookup_index_bytes += read_derived_cache(&self.default_completions, "default_completions")
            .0
            .capacity()
            * arc_bytes;

        let path_cache_bytes = self
            .app_paths
            .iter()
            .map(|entry| {
                std::mem::size_of::<String>()
                    + entry.key().capacity()
                    + std::mem::size_of::<AppPathRecord>()
                    + entry.value().name_key.capacity()
                    + entry.value().path.as_os_str().len()
            })
            .sum::<usize>()
            + self
                .source_path_cache
                .iter()
                .map(|entry| {
                    std::mem::size_of::<(String, ObjectKind, i32)>()
                        + entry.key().0.capacity()
                        + std::mem::size_of::<String>()
                        + entry.value().capacity()
                })
                .sum::<usize>();

        let composed_cache_bytes = self
            .composed_cache
            .iter()
            .map(|entry| {
                let value = entry.value();
                std::mem::size_of::<(ObjectKind, String)>()
                    + entry.key().1.capacity()
                    + arc_allocation_overhead
                    + std::mem::size_of::<super::model::ComposedObject>()
                    + value.extensions.capacity() * arc_bytes
                    + value.all_fields.capacity() * std::mem::size_of::<super::model::FieldSymbol>()
                    + value.all_methods.capacity()
                        * std::mem::size_of::<super::model::MethodSymbol>()
                    + value.all_controls.capacity()
                        * std::mem::size_of::<super::model::ControlSymbol>()
                    + value.all_enum_values.capacity()
                        * std::mem::size_of::<super::model::EnumValueSymbol>()
            })
            .sum::<usize>();

        SymbolIndexMemoryStats {
            symbol_payload_bytes,
            lookup_index_bytes,
            path_cache_bytes,
            composed_cache_bytes,
            tracked_bytes: symbol_payload_bytes
                + lookup_index_bytes
                + path_cache_bytes
                + composed_cache_bytes,
        }
    }

    pub fn cache_source_path(&self, package: String, kind: ObjectKind, id: i32, path: String) {
        self.source_path_cache
            .insert((fold_name(&package), kind, id), path);
    }

    pub fn get_cached_source_path(
        &self,
        package: &str,
        kind: ObjectKind,
        id: i32,
    ) -> Option<String> {
        self.source_path_cache
            .get(&(fold_name(package), kind, id))
            .map(|s| s.value().clone())
    }

    pub fn is_package_indexed(&self, package: &str) -> bool {
        let name_key = fold_name(package);
        self.app_paths
            .iter()
            .any(|record| record.value().name_key == name_key)
    }

    /// Report what kind of source navigation is genuinely available for an
    /// indexed object. Source indexes are cached per package path, so repeated
    /// calls do not rescan archives.
    pub fn source_availability(&self, entry: &SymbolEntry) -> SourceAvailability {
        let app_path = self.app_path(&entry.package);
        source_availability::classify(entry, app_path.as_deref())
    }

    /// Count source representations for every object in one package.
    ///
    /// The `.app` path is resolved **once** for the whole package rather than
    /// per entry: [`Self::app_path`] scans every `app_paths` record with a
    /// `min_by`, so calling it inside the loop made this O(entries × packages)
    /// on a workspace with a full BC symbol set. Every entry in the bucket
    /// shares the package key, so one lookup is also the correct one.
    pub fn package_source_availability(&self, package: &str) -> SourceAvailabilitySummary {
        let mut summary = SourceAvailabilitySummary::default();
        let app_path = self.app_path(package);
        if let Some(entries) = self.by_package.get(&fold_name(package)) {
            for symbol in entries.value() {
                summary.record(source_availability::classify(symbol, app_path.as_deref()));
            }
        }
        summary
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
        let parsed = complete_package_batch(parsed)?;

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
        cache: &super::cache::SymbolCache,
    ) -> Result<Vec<SymbolPackage>, PackageLoadError> {
        cache.gc_once();
        let parsed = complete_package_batch(load_package_batch_via_cache(paths, cache))?;

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
        cache: &super::cache::SymbolCache,
    ) -> (Vec<SymbolPackage>, Vec<PackageLoadFailure>) {
        cache.gc_once();
        let parsed = load_package_batch_via_cache(paths, cache);

        let mut results = Vec::new();
        let mut failures = Vec::new();
        for item in parsed {
            match item {
                Ok((path, pkg, from_cache)) => {
                    results.push(self.index_loaded_package(pkg, Some(&path), from_cache));
                }
                Err(failure) => failures.push(failure),
            }
        }
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
        cache: &super::cache::SymbolCache,
    ) -> Result<Vec<SymbolPackage>, PackageLoadError> {
        cache.gc_once();
        let parsed = complete_package_batch(load_package_batch_via_cache(paths, cache))?;

        self.clear_loaded_packages();
        let mut results = Vec::with_capacity(parsed.len());
        for (path, pkg, from_cache) in parsed {
            results.push(self.index_loaded_package(pkg, Some(&path), from_cache));
        }
        Ok(results)
    }

    fn index_loaded_package(
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
        use super::model::{EnumValueSymbol, SymbolEntry};

        let mut entries = Vec::new();
        for re in super::language_data::runtime_enums() {
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

    fn add_arc(&self, arc: Arc<SymbolEntry>, package_key: String) -> Arc<SymbolEntry> {
        let name_lower = fold_name(&arc.name);
        let seq = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.all.insert(
            seq,
            IndexedEntry {
                arc: Arc::clone(&arc),
                name_key: name_lower.clone(),
                package_key,
            },
        );
        self.by_name
            .entry(name_lower)
            .or_default()
            .push(Arc::clone(&arc));
        self.by_package
            .entry(fold_name(&arc.package))
            .or_default()
            .push(Arc::clone(&arc));
        // Only index entries with a real positive object id. Sentinel ids
        // (0 = "no id assigned" for builtins; -1 = synthetic-enum marker
        // emitted by table-field inline OptionMembers — see model.rs:731)
        // would otherwise all collide under (Enum, -1) / (kind, 0) and
        // pollute lookups via `by_kind_id`.
        if arc.id > 0 {
            self.by_kind_id
                .entry((arc.kind, arc.id))
                .or_default()
                .push(Arc::clone(&arc));
        }
        self.by_kind
            .entry(arc.kind)
            .or_default()
            .push(Arc::clone(&arc));
        if let Some(ref extends) = arc.extends {
            self.by_extends
                .entry(extends.to_lowercase())
                .or_default()
                .push(Arc::clone(&arc));
        }
        arc
    }

    pub fn add_entries(&self, entries: &[SymbolEntry]) {
        self.add_entries_owned_with_key(entries.to_vec(), None);
    }

    /// Like `add_entries` but takes owned entries, avoiding the clone into Arc.
    pub fn add_entries_owned(&self, entries: Vec<SymbolEntry>) {
        self.add_entries_owned_with_key(entries, None);
    }

    /// Add a batch of entries under one canonical package identity. When
    /// `package_key` is `None` (direct `add_entries*` callers such as
    /// workspace registration), each entry falls back to a name-derived key.
    fn add_entries_owned_with_key(&self, entries: Vec<SymbolEntry>, package_key: Option<String>) {
        self.invalidate_composed_for_entries(entries.iter());
        let update_sorted_names = entries.len() <= 64
            && read_derived_cache(&self.sorted_names, "sorted_names")
                .0
                .is_some();
        let new_arcs: Vec<Arc<SymbolEntry>> = entries
            .into_iter()
            .map(|entry| {
                let key = package_key
                    .clone()
                    .unwrap_or_else(|| package_identity_key("", &entry.package));
                self.add_arc(Arc::new(entry), key)
            })
            .collect();
        self.update_sorted_names_after_add(&new_arcs, update_sorted_names);
        self.update_default_completions(&new_arcs);
        self.note_mutation();
    }

    fn update_sorted_names_after_add(&self, entries: &[Arc<SymbolEntry>], update_in_place: bool) {
        let (mut cache, repaired) = write_derived_cache(&self.sorted_names, "sorted_names");
        if repaired || !update_in_place {
            *cache = None;
            return;
        }
        let Some(names) = cache.as_mut() else {
            return;
        };
        let names = Arc::make_mut(names);
        for entry in entries {
            let name = entry.name.to_lowercase();
            if let Err(position) = names.binary_search(&name) {
                names.insert(position, name);
            }
        }
    }

    /// Append newly-added entries into the default_completions cache up to the cap.
    ///
    /// Called after all entries from a batch are indexed. Uses a fast read-check
    /// to skip acquiring the write lock when the cache is already full.
    fn update_default_completions(&self, new_arcs: &[Arc<SymbolEntry>]) {
        let (cache, repaired) =
            read_derived_cache(&self.default_completions, "default_completions");
        if repaired {
            drop(cache);
            self.rebuild_default_completions();
            return;
        }
        if cache.len() >= DEFAULT_COMPLETIONS_CAP {
            return;
        }
        drop(cache);
        let (mut cache, repaired) =
            write_derived_cache(&self.default_completions, "default_completions");
        if repaired {
            drop(cache);
            self.rebuild_default_completions();
            return;
        }
        for arc in new_arcs {
            if arc.synthetic {
                continue;
            }
            if cache.len() >= DEFAULT_COMPLETIONS_CAP {
                break;
            }
            cache.push(Arc::clone(arc));
        }
    }

    /// Return the pre-computed default completion entries (up to DEFAULT_COMPLETIONS_CAP).
    ///
    /// O(1) pointer copies — never iterates the full symbol index. Use in place of
    /// `search("", 30)` on completion hot paths.
    pub fn get_default_completions(&self) -> Vec<Arc<SymbolEntry>> {
        let (cache, repaired) =
            read_derived_cache(&self.default_completions, "default_completions");
        if repaired {
            drop(cache);
            self.rebuild_default_completions();
            return read_derived_cache(&self.default_completions, "default_completions")
                .0
                .clone();
        }
        cache.clone()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Arc<SymbolEntry>> {
        if limit == 0 {
            return Vec::new();
        }

        let query_lower = query.to_lowercase();
        let names = self.sorted_names();
        let mut results = Vec::with_capacity(limit.min(DEFAULT_COMPLETIONS_CAP));

        if !query_lower.is_empty() {
            self.append_search_name(&query_lower, limit, &mut results);
        }

        if results.len() < limit {
            let start = names.partition_point(|name| name < &query_lower);
            for name in &names[start..] {
                if !name.starts_with(&query_lower) {
                    break;
                }
                if name != &query_lower {
                    self.append_search_name(name, limit, &mut results);
                    if results.len() == limit {
                        return results;
                    }
                }
            }
        }

        if !query_lower.is_empty() && results.len() < limit {
            for name in names.iter() {
                if name.contains(&query_lower) && !name.starts_with(&query_lower) {
                    self.append_search_name(name, limit, &mut results);
                    if results.len() == limit {
                        break;
                    }
                }
            }
        }
        results
    }

    fn append_search_name(&self, name: &str, limit: usize, results: &mut Vec<Arc<SymbolEntry>>) {
        let Some(entries) = self.by_name.get(name) else {
            return;
        };
        let mut entries: Vec<_> = entries
            .iter()
            .filter(|entry| !entry.synthetic)
            .cloned()
            .collect();
        entries.sort_unstable_by(|left, right| {
            (left.kind, left.id, &left.package).cmp(&(right.kind, right.id, &right.package))
        });
        results.extend(
            entries
                .into_iter()
                .take(limit.saturating_sub(results.len())),
        );
    }

    fn sorted_names(&self) -> Arc<Vec<String>> {
        let (cache, _) = read_derived_cache(&self.sorted_names, "sorted_names");
        if let Some(names) = cache.as_ref() {
            return Arc::clone(names);
        }
        drop(cache);

        let (mut cache, _) = write_derived_cache(&self.sorted_names, "sorted_names");
        if let Some(names) = cache.as_ref() {
            return Arc::clone(names);
        }
        let mut names: Vec<String> = self
            .by_name
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        names.sort_unstable();
        names.dedup();
        let names = Arc::new(names);
        *cache = Some(Arc::clone(&names));
        names
    }

    pub fn search_in_package(&self, package_name: &str, query: &str) -> Vec<Arc<SymbolEntry>> {
        let query_lower = fold_name(query);
        let Some(entries) = self.by_package.get(&fold_name(package_name)) else {
            return Vec::new();
        };
        let mut results: Vec<(String, usize, Arc<SymbolEntry>)> = entries
            .value()
            .iter()
            .enumerate()
            .filter_map(|(position, arc)| {
                if arc.synthetic {
                    return None;
                }
                let name_lower = fold_name(&arc.name);
                name_lower
                    .contains(&query_lower)
                    .then(|| (name_lower, position, Arc::clone(arc)))
            })
            .collect();
        drop(entries);
        results.sort_unstable_by(|a, b| {
            (&a.0, a.2.kind, a.2.id, a.1).cmp(&(&b.0, b.2.kind, b.2.id, b.1))
        });
        results.into_iter().map(|(_, _, arc)| arc).collect()
    }

    pub fn get_by_name(&self, name: &str) -> Vec<Arc<SymbolEntry>> {
        let key = name.to_lowercase();
        self.by_name
            .get(&key)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Exact name match returning the first entry (case-insensitive).
    /// Avoids cloning the entire Vec when only one match is needed.
    pub fn find_by_name(&self, name: &str) -> Option<Arc<SymbolEntry>> {
        let key = name.to_lowercase();
        self.by_name
            .get(&key)
            .and_then(|v| v.first().map(Arc::clone))
    }

    pub fn get_by_id(&self, kind: ObjectKind, id: i32) -> Vec<Arc<SymbolEntry>> {
        self.by_kind_id
            .get(&(kind, id))
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    pub fn get_by_kind(&self, kind: ObjectKind) -> Vec<Arc<SymbolEntry>> {
        self.by_kind
            .get(&kind)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    pub fn get_extensions_of(&self, base_name: &str) -> Vec<Arc<SymbolEntry>> {
        let target = base_name.to_lowercase();
        self.by_extends
            .get(&target)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Return all indexed entries.
    ///
    /// Prefer this over `search("", usize::MAX)` when iteration over all
    /// symbols is the intent — it makes the purpose explicit and avoids the
    /// internal limit check overhead.
    pub fn all_entries(&self) -> Vec<Arc<SymbolEntry>> {
        self.all
            .iter()
            .map(|e| Arc::clone(&e.value().arc))
            .collect()
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    /// Resolve a package's `.app` path by display name.
    ///
    /// Paths are stored under canonical identity keys (app GUID + name
    /// fallback), so two same-named packages keep independent records; a
    /// name-only lookup over an ambiguous name returns the record with the
    /// smallest identity key for determinism.
    pub fn app_path(&self, package_name: &str) -> Option<std::path::PathBuf> {
        let name_key = fold_name(package_name);
        self.app_paths
            .iter()
            .filter(|record| record.value().name_key == name_key)
            .min_by(|a, b| a.key().cmp(b.key()))
            .map(|record| record.value().path.clone())
    }

    /// Return every file-backed package path currently published in the index.
    ///
    /// Package paths remain authoritative even when `SymbolReference.json`
    /// contains no object entries. Deriving this list from [`Self::all_entries`]
    /// would make an empty-symbol package (which may still contain embedded AL
    /// source) invisible to dependency-source and call-graph construction.
    pub fn loaded_package_paths(&self) -> Vec<std::path::PathBuf> {
        let mut paths: Vec<_> = self
            .app_paths
            .iter()
            .map(|entry| entry.value().path.clone())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths
    }

    /// Get a composed view with caching. Returns Arc for zero-copy sharing.
    ///
    /// Cached results are returned on repeat calls. Use [`invalidate_composed`]
    /// when workspace files change to clear stale entries.
    pub fn get_composed_cached(
        &self,
        kind: super::model::ObjectKind,
        name: &str,
    ) -> Option<Arc<super::model::ComposedObject>> {
        let key = (kind, name.to_lowercase());
        if let Some(cached) = self.composed_cache.get(&key) {
            return Some(Arc::clone(cached.value()));
        }
        let composed = Arc::new(super::composition::get_composed(self, kind, name)?);
        Some(Arc::clone(
            self.composed_cache
                .entry(key)
                .or_insert_with(|| Arc::clone(&composed))
                .value(),
        ))
    }

    /// Invalidate cached composed views for a given object name.
    ///
    /// Call when a workspace file defining or extending this object changes.
    pub fn invalidate_composed(&self, name: &str) {
        let lower = name.to_lowercase();
        self.composed_cache.retain(|k, _| k.1 != lower);
    }

    pub fn invalidate_all_composed(&self) {
        self.composed_cache.clear();
    }

    pub fn is_composed_cache_empty(&self) -> bool {
        self.composed_cache.is_empty()
    }

    fn invalidate_composed_for_entries<'a>(&self, entries: impl Iterator<Item = &'a SymbolEntry>) {
        if self.composed_cache.is_empty() {
            return;
        }
        let affected: std::collections::HashSet<String> = entries
            .flat_map(|entry| {
                [
                    Some(entry.name.to_lowercase()),
                    entry.extends.as_deref().map(str::to_lowercase),
                ]
            })
            .flatten()
            .collect();
        if !affected.is_empty() {
            self.composed_cache
                .retain(|(_, name), _| !affected.contains(name));
        }
    }

    pub fn get_events(&self, query: &str) -> super::events::EventResults {
        super::events::get_events(self, query)
    }

    /// Remove all entries whose `package` field matches `package_name` (case-insensitive).
    ///
    /// Clears registered workspace entries before re-adding them,
    /// preventing duplicates when the call graph is rebuilt.
    ///
    /// Every secondary index that stores `Vec<Arc<SymbolEntry>>` must be passed
    /// through `retain_arcs_not_in`; missing one leaves dangling
    /// references the next caller will see. The helper makes the discipline
    /// uniform — adding a new secondary index requires adding exactly one
    /// `Self::retain_arcs_not_in(&self.new_index, &ptrs);` line below.
    pub fn remove_package_entries(&self, package_name: &str) {
        self.remove_packages_named(&std::collections::HashSet::from([fold_name(package_name)]));
    }

    fn clear_loaded_packages(&self) {
        let identities: std::collections::HashSet<String> = self
            .app_paths
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        self.remove_package_identities(&identities);
    }

    /// Remove every entry whose *display* package name folds to one of
    /// `package_names`, plus the matching path/source caches. This is the
    /// legacy name-scoped removal used by workspace re-registration; with an
    /// ambiguous display name it removes all same-named generations.
    fn remove_packages_named(&self, package_names: &std::collections::HashSet<String>) {
        if package_names.is_empty() {
            return;
        }
        let to_remove: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let indexed = entry.value();
                if package_names.contains(&fold_name(&indexed.arc.package)) {
                    Some((*entry.key(), Arc::clone(&indexed.arc)))
                } else {
                    None
                }
            })
            .collect();
        self.app_paths
            .retain(|_, record| !package_names.contains(&record.name_key));
        self.source_path_cache
            .retain(|(package, _, _), _| !package_names.contains(package));
        self.remove_selected_entries(to_remove);
    }

    /// Remove every entry contributed under one of the canonical package
    /// identity keys, plus the matching path/source caches. Same-named
    /// packages with different app GUIDs are untouched.
    fn remove_package_identities(&self, identities: &std::collections::HashSet<String>) {
        if identities.is_empty() {
            return;
        }
        let to_remove: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let indexed = entry.value();
                if identities.contains(&indexed.package_key) {
                    Some((*entry.key(), Arc::clone(&indexed.arc)))
                } else {
                    None
                }
            })
            .collect();
        // Purge the name-keyed source-path cache for the removed identities'
        // display names, but only when no *other* identity still publishes
        // that display name.
        let removed_names: std::collections::HashSet<String> = self
            .app_paths
            .iter()
            .filter(|record| identities.contains(record.key()))
            .map(|record| record.value().name_key.clone())
            .collect();
        for identity in identities {
            self.app_paths.remove(identity);
        }
        let surviving_names: std::collections::HashSet<String> = self
            .app_paths
            .iter()
            .map(|record| record.value().name_key.clone())
            .collect();
        self.source_path_cache.retain(|(package, _, _), _| {
            !removed_names.contains(package) || surviving_names.contains(package)
        });
        self.remove_selected_entries(to_remove);
    }

    /// Shared core of both removal flavors: prune the primary map and every
    /// secondary index for the selected entries, then repair derived caches.
    fn remove_selected_entries(&self, to_remove: Vec<(usize, Arc<SymbolEntry>)>) {
        if to_remove.is_empty() {
            return;
        }

        let ptrs: std::collections::HashSet<*const SymbolEntry> =
            to_remove.iter().map(|(_, arc)| Arc::as_ptr(arc)).collect();
        let update_sorted_names = to_remove.len() <= 64
            && read_derived_cache(&self.sorted_names, "sorted_names")
                .0
                .is_some();
        let removed_names: Vec<String> = if update_sorted_names {
            to_remove
                .iter()
                .map(|(_, entry)| entry.name.to_lowercase())
                .collect()
        } else {
            Vec::new()
        };

        for (seq, _) in &to_remove {
            self.all.remove(seq);
        }

        Self::retain_arcs_not_in(&self.by_name, &ptrs);
        Self::retain_arcs_not_in(&self.by_kind_id, &ptrs);
        Self::retain_arcs_not_in(&self.by_kind, &ptrs);
        Self::retain_arcs_not_in(&self.by_extends, &ptrs);
        Self::retain_arcs_not_in(&self.by_package, &ptrs);
        let (mut sorted_names, repaired) = write_derived_cache(&self.sorted_names, "sorted_names");
        if update_sorted_names && !repaired {
            if let Some(names) = sorted_names.as_mut() {
                let names = Arc::make_mut(names);
                for name in removed_names {
                    if !self.by_name.contains_key(&name) {
                        if let Ok(position) = names.binary_search(&name) {
                            names.remove(position);
                        }
                    }
                }
            }
        } else {
            *sorted_names = None;
        }

        // Package additions/removals can change any composed base-extension
        // relationship. Clear the small derived cache rather than serving a
        // stale view after hot symbol reload.
        self.composed_cache.clear();
        self.rebuild_default_completions();
        self.note_mutation();
    }

    fn rebuild_default_completions(&self) {
        let mut remaining: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let indexed = entry.value();
                (!indexed.arc.synthetic).then(|| (*entry.key(), Arc::clone(&indexed.arc)))
            })
            .collect();
        remaining.sort_unstable_by_key(|(seq, _)| *seq);
        remaining.truncate(DEFAULT_COMPLETIONS_CAP);

        let (mut cache, _) = write_derived_cache(&self.default_completions, "default_completions");
        *cache = remaining.into_iter().map(|(_, arc)| arc).collect();
    }

    /// Helper: filter a `DashMap<K, Vec<Arc<SymbolEntry>>>` secondary index
    /// in place, removing every Arc whose pointer appears in `ptrs` and
    /// dropping any key whose Vec becomes empty as a result.
    ///
    /// New secondary indexes that follow the
    /// `DashMap<K, Vec<Arc<SymbolEntry>>>` shape MUST be passed through
    /// this helper; the contract is that no Arc reachable from any
    /// secondary index can reference an entry no longer in `self.all`.
    fn retain_arcs_not_in<K>(
        index: &dashmap::DashMap<K, Vec<Arc<SymbolEntry>>>,
        ptrs: &std::collections::HashSet<*const SymbolEntry>,
    ) where
        K: Eq + std::hash::Hash + Clone,
    {
        index.retain(|_, vec| {
            vec.retain(|arc| !ptrs.contains(&Arc::as_ptr(arc)));
            !vec.is_empty()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FieldSymbol, MethodSymbol, ObjectKind, SymbolEntry};

    fn make_entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind,
            id,
            name: name.to_string(),
            extends: None,
            implements: Vec::new(),
            namespace: String::new(),
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    fn make_extension(kind: ObjectKind, id: i32, name: &str, extends: &str) -> SymbolEntry {
        SymbolEntry {
            synthetic: false,
            kind,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            implements: Vec::new(),
            namespace: String::new(),
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
            permissions: Vec::new(),
            variables: Vec::new(),
        }
    }

    #[test]
    fn add_and_search() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 50100, "Customer"),
            make_entry(ObjectKind::Table, 50101, "Customer Ledger Entry"),
            make_entry(ObjectKind::Page, 50100, "Customer Card"),
            make_entry(ObjectKind::Codeunit, 50100, "Sales Management"),
        ]);

        let results = index.search("customer", 10);
        assert_eq!(results.len(), 3); // Customer, Customer Ledger Entry, Customer Card

        let results = index.search("customer", 2);
        assert_eq!(results.len(), 2);

        let results = index.search("SALES", 10);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Sales Management");
    }

    #[test]
    fn replace_with_rebuilds_every_lookup_and_completion_index() {
        let active = SymbolIndex::new();
        active.add_entries(&[make_entry(ObjectKind::Table, 50_100, "Old")]);
        assert_eq!(active.get_default_completions().len(), 1);
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
        assert_eq!(active.get_default_completions().len(), 2);
        assert_eq!(active.len(), 2);
    }

    #[test]
    fn search_ranks_exact_then_prefix_then_substring_deterministically() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 1, "My Customer Archive"),
            make_entry(ObjectKind::Table, 2, "Customer Ledger Entry"),
            make_entry(ObjectKind::Table, 3, "Customer"),
        ]);

        let names: Vec<_> = index
            .search("customer", 3)
            .into_iter()
            .map(|entry| entry.name.clone())
            .collect();
        assert_eq!(
            names,
            vec!["Customer", "Customer Ledger Entry", "My Customer Archive"]
        );
        assert!(index.search("customer", 0).is_empty());
    }

    #[test]
    fn search_name_cache_tracks_additions_after_it_is_built() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_entry(ObjectKind::Table, 1, "Alpha")]);
        assert_eq!(index.search("alpha", 10).len(), 1);

        index.add_entries(&[make_entry(ObjectKind::Table, 2, "Beta")]);
        assert_eq!(index.search("beta", 10).len(), 1);

        index.add_entries_owned(vec![make_entry(ObjectKind::Table, 3, "Gamma")]);
        assert_eq!(index.search("gamma", 10).len(), 1);

        let large_batch: Vec<_> = (0..65)
            .map(|offset| {
                make_entry(
                    ObjectKind::Table,
                    10_000 + offset,
                    &format!("Bulk {offset:02}"),
                )
            })
            .collect();
        index.add_entries_owned(large_batch);
        assert_eq!(index.search("bulk", 100).len(), 65);
    }

    #[test]
    fn search_name_cache_tracks_removals_after_it_is_built() {
        let index = SymbolIndex::new();
        let mut drop_entry = make_entry(ObjectKind::Table, 1, "Drop Me");
        drop_entry.package = "Drop".into();
        let mut keep_entry = make_entry(ObjectKind::Table, 2, "Keep Me");
        keep_entry.package = "Keep".into();
        index.add_entries(&[drop_entry, keep_entry]);
        assert_eq!(index.search("me", 10).len(), 2);

        index.remove_package_entries("drop");

        assert!(index.search("drop", 10).is_empty());
        assert_eq!(index.search("keep", 10).len(), 1);
    }

    #[test]
    fn get_by_name_exact() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 50100, "Customer"),
            make_entry(ObjectKind::Page, 50100, "Customer"),
        ]);

        let results = index.get_by_name("customer");
        assert_eq!(results.len(), 2);

        let results = index.get_by_name("nonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn get_by_id() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 50100, "Customer"),
            make_entry(ObjectKind::Page, 50100, "Customer Card"),
        ]);

        let results = index.get_by_id(ObjectKind::Table, 50100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Customer");

        let results = index.get_by_id(ObjectKind::Table, 99999);
        assert!(results.is_empty());
    }

    #[test]
    fn synthetic_enum_sentinel_id_does_not_pollute_lookup() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Enum, -1, "InlineOpt1"),
            make_entry(ObjectKind::Enum, -1, "InlineOpt2"),
            make_entry(ObjectKind::Enum, 50200, "RealEnum"),
            // builtin-style sentinel: id == 0 ("not assigned").
            make_entry(ObjectKind::Codeunit, 0, "BuiltinHelper"),
        ]);

        assert!(
            index.get_by_id(ObjectKind::Enum, -1).is_empty(),
            "synthetic-enum sentinel id -1 must not be queryable"
        );
        assert!(
            index.get_by_id(ObjectKind::Codeunit, 0).is_empty(),
            "no-id sentinel 0 must not be queryable"
        );

        let real = index.get_by_id(ObjectKind::Enum, 50200);
        assert_eq!(real.len(), 1, "real enum id must still be reachable");
        assert_eq!(real[0].name, "RealEnum");

        let by_name = index.get_by_name("InlineOpt1");
        assert!(
            !by_name.is_empty(),
            "sentinel-id entries must still be name-resolvable"
        );
    }

    #[test]
    fn get_by_kind() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 1, "A"),
            make_entry(ObjectKind::Table, 2, "B"),
            make_entry(ObjectKind::Page, 1, "C"),
        ]);

        let tables = index.get_by_kind(ObjectKind::Table);
        assert_eq!(tables.len(), 2);
    }

    #[test]
    fn get_extensions_of() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 18, "Customer"),
            make_extension(ObjectKind::TableExtension, 50100, "Cust Ext 1", "Customer"),
            make_extension(ObjectKind::TableExtension, 50101, "Cust Ext 2", "Customer"),
            make_extension(ObjectKind::TableExtension, 50102, "Vendor Ext", "Vendor"),
        ]);

        let exts = index.get_extensions_of("Customer");
        assert_eq!(exts.len(), 2);

        let exts = index.get_extensions_of("Vendor");
        assert_eq!(exts.len(), 1);

        let exts = index.get_extensions_of("Nonexistent");
        assert!(exts.is_empty());
    }

    #[test]
    fn len_and_is_empty() {
        let index = SymbolIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);

        index.add_entries(&[make_entry(ObjectKind::Table, 1, "T")]);
        assert!(!index.is_empty());
        assert_eq!(index.len(), 1);
    }

    #[test]
    fn memory_stats_measure_live_symbol_and_lookup_allocations() {
        let index = SymbolIndex::new();
        let empty = index.memory_stats();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 1, "Measured Table"),
            make_entry(ObjectKind::Page, 2, "Measured Page"),
        ]);
        // Build the lazy name catalogue so its allocation is included too.
        assert_eq!(index.search("measured", 10).len(), 2);

        let populated = index.memory_stats();
        assert!(populated.symbol_payload_bytes > 0);
        assert!(populated.lookup_index_bytes > empty.lookup_index_bytes);
        assert_eq!(
            populated.tracked_bytes,
            populated.symbol_payload_bytes
                + populated.lookup_index_bytes
                + populated.path_cache_bytes
                + populated.composed_cache_bytes
        );
    }

    #[test]
    fn poisoned_derived_caches_are_discarded_and_rebuilt_from_symbol_indexes() {
        let index = Arc::new(SymbolIndex::new());
        index.add_entries(&[
            make_entry(ObjectKind::Table, 1, "Alpha"),
            make_entry(ObjectKind::Page, 2, "Beta"),
        ]);
        assert_eq!(index.search("alpha", 10).len(), 1);
        assert_eq!(index.get_default_completions().len(), 2);

        let poison_target = Arc::clone(&index);
        let _ = std::thread::spawn(move || {
            let mut names = poison_target.sorted_names.write().unwrap();
            let mut completions = poison_target.default_completions.write().unwrap();
            *names = Some(Arc::new(vec!["corrupt".to_string()]));
            completions.clear();
            panic!("poison derived symbol caches for test");
        })
        .join();

        let search = index.search("alpha", 10);
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].name, "Alpha");

        let completions = index.get_default_completions();
        assert_eq!(completions.len(), 2);
        assert!(completions.iter().any(|entry| entry.name == "Alpha"));
        assert!(completions.iter().any(|entry| entry.name == "Beta"));
        assert!(!index.sorted_names.is_poisoned());
        assert!(!index.default_completions.is_poisoned());
    }

    #[test]
    fn remove_package_entries_clears_all_secondary_indexes() {
        let index = SymbolIndex::new();
        let mut entries = vec![
            make_entry(ObjectKind::Table, 50100, "Customer"),
            make_extension(ObjectKind::PageExtension, 50100, "Customer Ext", "Customer"),
        ];
        for e in &mut entries {
            e.package = "Drop Target".to_string();
        }
        index.add_entries(&entries);

        assert!(!index.get_by_name("Customer").is_empty());
        assert!(!index.get_by_name("Customer Ext").is_empty());
        assert!(!index.get_by_id(ObjectKind::Table, 50100).is_empty());
        assert!(!index.get_by_id(ObjectKind::PageExtension, 50100).is_empty());
        assert!(!index.get_by_kind(ObjectKind::Table).is_empty());
        assert!(!index.get_extensions_of("Customer").is_empty());

        index.remove_package_entries("Drop Target");

        assert!(index.get_by_name("Customer").is_empty(), "by_name leak");
        assert!(
            index.get_by_name("Customer Ext").is_empty(),
            "by_name leak (ext)"
        );
        assert!(
            index.get_by_id(ObjectKind::Table, 50100).is_empty(),
            "by_kind_id leak"
        );
        assert!(
            index.get_by_id(ObjectKind::PageExtension, 50100).is_empty(),
            "by_kind_id leak (ext)"
        );
        assert!(
            index.get_by_kind(ObjectKind::Table).is_empty(),
            "by_kind leak"
        );
        assert!(
            index.get_extensions_of("Customer").is_empty(),
            "by_extends leak"
        );
        assert!(index.search("Customer", 10).is_empty(), "sorted-name leak");
        assert_eq!(index.len(), 0, "primary `all` index leak");
    }

    /// negative: remove_package_entries on a non-existent package is
    /// a no-op (no panic, no spurious removals).
    #[test]
    fn remove_package_entries_no_match_is_noop() {
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Table, 50100, "Customer"),
            make_entry(ObjectKind::Page, 50100, "Customer Card"),
        ]);
        let before_len = index.len();
        index.remove_package_entries("ThisPackageDoesNotExist");
        assert_eq!(
            index.len(),
            before_len,
            "removing a non-existent package must not change the index"
        );
        assert!(!index.get_by_name("Customer").is_empty());
    }

    #[test]
    fn removal_clears_path_source_and_composition_caches_and_refills_defaults() {
        let index = SymbolIndex::new();
        let mut entries: Vec<_> = (0..40)
            .map(|i| make_entry(ObjectKind::Table, 50_000 + i, &format!("Table {i:02}")))
            .collect();
        for entry in entries.iter_mut().take(10) {
            entry.package = "Drop".into();
        }
        for entry in entries.iter_mut().skip(10) {
            entry.package = "Keep".into();
        }
        index.add_entries(&entries);
        index.app_paths.insert(
            package_identity_key("", "Drop"),
            AppPathRecord {
                name_key: "drop".into(),
                path: "/tmp/drop.app".into(),
            },
        );
        index.cache_source_path(
            "Drop".into(),
            ObjectKind::Table,
            50_000,
            "src/Drop.al".into(),
        );
        let _ = index.get_composed_cached(ObjectKind::Table, "Table 00");

        index.remove_package_entries("DROP");

        assert!(index.app_path("Drop").is_none());
        assert!(index
            .get_cached_source_path("Drop", ObjectKind::Table, 50_000)
            .is_none());
        assert!(index.is_composed_cache_empty());
        assert_eq!(index.get_default_completions().len(), 30);
        assert!(index
            .get_default_completions()
            .iter()
            .all(|entry| entry.package == "Keep"));
    }

    #[test]
    fn default_completions_exclude_synthetic_entries() {
        let index = SymbolIndex::new();
        let mut synthetic = make_entry(ObjectKind::Enum, -1, "Synthetic Option");
        synthetic.synthetic = true;
        index.add_entries(&[synthetic, make_entry(ObjectKind::Table, 1, "Real Table")]);

        let defaults = index.get_default_completions();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].name, "Real Table");
    }

    #[test]
    fn adding_extension_invalidates_cached_composition() {
        let index = SymbolIndex::new();
        index.add_entries(&[make_entry(ObjectKind::Table, 18, "Customer")]);
        let before = index
            .get_composed_cached(ObjectKind::Table, "Customer")
            .unwrap();
        assert!(before.extensions.is_empty());

        index.add_entries(&[make_extension(
            ObjectKind::TableExtension,
            50_100,
            "Customer Ext",
            "Customer",
        )]);

        let after = index
            .get_composed_cached(ObjectKind::Table, "Customer")
            .unwrap();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(after.extensions.len(), 1);
    }

    #[test]
    fn runtime_enum_is_not_shadowed_by_non_enum_with_same_name() {
        let index = SymbolIndex::new();
        let runtime_name = crate::language_data::runtime_enums()[0].name.clone();
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
    fn index_with_methods_and_fields() {
        let index = SymbolIndex::new();
        let mut entry = make_entry(ObjectKind::Table, 50100, "My Table");
        entry.fields = vec![FieldSymbol {
            id: 1,
            name: "No.".to_string(),
            type_name: "Code".to_string(),
            properties: vec![],
        }];
        entry.methods = vec![MethodSymbol {
            name: "DoWork".to_string(),
            parameters: Vec::new(),
            return_type: Some("Boolean".to_string()),
            attributes: Vec::new(),
            is_local: false,
        }];
        index.add_entries(&[entry]);

        let results = index.get_by_name("My Table");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fields.len(), 1);
        assert_eq!(results[0].methods.len(), 1);
    }

    fn build_app(name: &str, table_id: i32, table_name: &str) -> Vec<u8> {
        build_app_with_id(
            "00000000-0000-0000-0000-000000000001",
            name,
            table_id,
            table_name,
        )
    }

    fn build_app_with_id(app_id: &str, name: &str, table_id: i32, table_name: &str) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let manifest = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Package><App Id="{app_id}" Name="{name}" Publisher="Contoso" Version="1.0.0.0" /></Package>"#
        );
        let symbols = format!(
            r#"{{ "Tables": [ {{ "Id": {table_id}, "Name": "{table_name}", "Fields": [], "Methods": [] }} ] }}"#
        );

        let mut data = Vec::new();
        data.extend_from_slice(b"NAVX");
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]);

        let mut zip_buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut zip_buf));
            let opts = SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", opts).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("SymbolReference.json", opts).unwrap();
            zip.write_all(symbols.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_buf);
        data
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

    /// The index folds package names with one canonical (Unicode) rule.
    /// `İSTANBUL` folds to `i̇stanbul` (i + combining dot); an ASCII-only
    /// comparison would report zero matches for that folded query.
    #[test]
    fn package_name_matching_uses_one_unicode_folding() {
        let index = SymbolIndex::new();
        let mut entry = make_entry(ObjectKind::Table, 1, "Turkish Object");
        entry.package = "İSTANBUL".to_string();
        index.add_entries(&[entry]);

        let folded = "İSTANBUL".to_lowercase();
        assert_ne!(folded, "İSTANBUL");
        assert_eq!(index.search_in_package(&folded, "").len(), 1);
        assert_eq!(index.search_in_package("İSTANBUL", "turkish").len(), 1);
    }
}
