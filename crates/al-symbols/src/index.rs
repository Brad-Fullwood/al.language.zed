//! Concurrent symbol index backed by DashMap.
//!
//! Provides fast lookup by name, object kind+ID, and substring search
//! across all loaded packages.

use std::path::Path;
use std::sync::Arc;

use dashmap::DashMap;
use tracing::{debug, warn};

use super::app_reader;
use super::model::{ObjectKind, SymbolEntry, SymbolPackage};

const DEFAULT_COMPLETIONS_CAP: usize = 30;

/// Thread-safe symbol index over multiple AL packages.
#[derive(Debug)]
pub struct SymbolIndex {
    /// Objects keyed by lowercase name. Multiple objects can share a name
    /// (e.g., a Table and a Page with the same name, or objects from different packages).
    by_name: DashMap<String, Vec<Arc<SymbolEntry>>>,
    by_kind_id: DashMap<(ObjectKind, i32), Vec<Arc<SymbolEntry>>>,
    by_kind: DashMap<ObjectKind, Vec<Arc<SymbolEntry>>>,
    by_extends: DashMap<String, Vec<Arc<SymbolEntry>>>,
    all: DashMap<usize, (Arc<SymbolEntry>, String)>,
    next_id: std::sync::atomic::AtomicUsize,
    app_paths: DashMap<String, std::path::PathBuf>,
    source_path_cache: DashMap<(String, ObjectKind, i32), String>,
    composed_cache: DashMap<(ObjectKind, String), Arc<super::model::ComposedObject>>,
    /// Pre-computed slice of the first DEFAULT_COMPLETIONS_CAP entries for O(1)
    /// default completion responses. Populated by add_entries/add_entries_owned.
    default_completions: std::sync::RwLock<Vec<Arc<SymbolEntry>>>,
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
            by_kind_id: DashMap::new(),
            by_kind: DashMap::new(),
            by_extends: DashMap::new(),
            all: DashMap::new(),
            next_id: std::sync::atomic::AtomicUsize::new(0),
            app_paths: DashMap::new(),
            source_path_cache: DashMap::new(),
            composed_cache: DashMap::new(),
            default_completions: std::sync::RwLock::new(Vec::new()),
        }
    }

    pub fn cache_source_path(&self, package: String, kind: ObjectKind, id: i32, path: String) {
        self.source_path_cache
            .insert((package.to_lowercase(), kind, id), path);
    }

    pub fn get_cached_source_path(
        &self,
        package: &str,
        kind: ObjectKind,
        id: i32,
    ) -> Option<String> {
        self.source_path_cache
            .get(&(package.to_lowercase(), kind, id))
            .map(|s| s.value().clone())
    }

    pub fn is_package_indexed(&self, package: &str) -> bool {
        self.app_paths.contains_key(&package.to_lowercase())
    }

    /// Load and index all .app files from the given paths.
    ///
    /// Files that fail to parse are logged and skipped.
    pub fn load_packages(&self, paths: &[impl AsRef<Path> + Sync]) -> Vec<SymbolPackage> {
        use rayon::prelude::*;

        // Parse in parallel, then mutate the shared indexes sequentially. The
        // old implementation performed remove/add operations from rayon workers,
        // so duplicate versions of one package could interleave and leave both
        // copies indexed. Keeping the expensive ZIP/JSON work parallel while
        // applying results in input order is deterministic and idempotent.
        let parsed: Vec<_> = paths
            .par_iter()
            .filter_map(|path| {
                let path = path.as_ref();
                match app_reader::read_app_file(path) {
                    Ok(pkg) => Some((path.to_path_buf(), pkg)),
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to load .app file");
                        None
                    }
                }
            })
            .collect();

        let mut results = Vec::with_capacity(parsed.len());
        for (path, pkg) in parsed {
            results.push(self.index_loaded_package(pkg, Some(&path), false));
        }
        results
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
    ) -> Vec<SymbolPackage> {
        use rayon::prelude::*;

        let parsed: Vec<_> = paths
            .par_iter()
            .filter_map(|path| {
                let path = path.as_ref();

                if let Some(pkg) = cache.load(path) {
                    return Some((path.to_path_buf(), pkg, true));
                }

                match app_reader::read_app_file(path) {
                    Ok(pkg) => {
                        debug!(
                            name = %pkg.name,
                            objects = pkg.objects.len(),
                            "Loaded package (cache miss)"
                        );
                        if let Err(e) = cache.save(path, &pkg) {
                            warn!(path = %path.display(), error = %e, "Failed to save to cache");
                        }
                        Some((path.to_path_buf(), pkg, false))
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to load .app file");
                        None
                    }
                }
            })
            .collect();

        let mut results = Vec::with_capacity(parsed.len());
        for (path, pkg, from_cache) in parsed {
            results.push(self.index_loaded_package(pkg, Some(&path), from_cache));
        }
        results
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
    ) -> Vec<SymbolPackage> {
        use rayon::prelude::*;

        let parsed: Vec<_> = paths
            .par_iter()
            .filter_map(|path| {
                let path = path.as_ref();
                if let Some(pkg) = cache.load(path) {
                    return Some((path.to_path_buf(), pkg, true));
                }
                match app_reader::read_app_file(path) {
                    Ok(pkg) => {
                        if let Err(error) = cache.save(path, &pkg) {
                            warn!(path = %path.display(), %error, "Failed to save to cache");
                        }
                        Some((path.to_path_buf(), pkg, false))
                    }
                    Err(error) => {
                        warn!(path = %path.display(), %error, "Failed to load .app file");
                        None
                    }
                }
            })
            .collect();

        if !paths.is_empty() && parsed.is_empty() {
            warn!(
                attempted = paths.len(),
                "All replacement symbol packages failed to load; keeping the previous index generation"
            );
            return Vec::new();
        }

        self.clear_loaded_packages();
        let mut results = Vec::with_capacity(parsed.len());
        for (path, pkg, from_cache) in parsed {
            results.push(self.index_loaded_package(pkg, Some(&path), from_cache));
        }
        results
    }

    fn index_loaded_package(
        &self,
        mut pkg: SymbolPackage,
        path: Option<&Path>,
        from_cache: bool,
    ) -> SymbolPackage {
        // Re-loading a downloaded or changed package replaces its old symbols
        // rather than duplicating every object in all secondary indexes.
        self.remove_package_entries(&pkg.name);
        if let Some(path) = path {
            self.app_paths
                .insert(pkg.name.to_lowercase(), path.to_path_buf());
        }
        debug!(
            name = %pkg.name,
            objects = pkg.objects.len(),
            from_cache,
            "Indexing package"
        );
        self.add_entries_owned(std::mem::take(&mut pkg.objects));
        pkg
    }

    pub fn load_package_bytes(
        &self,
        data: &[u8],
    ) -> Result<SymbolPackage, app_reader::AppReaderError> {
        let pkg = app_reader::read_app_bytes(data)?;
        self.remove_package_entries(&pkg.name);
        self.add_entries(&pkg.objects);
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
                variables: Vec::new(),
            });
        }

        if !entries.is_empty() {
            debug!(count = entries.len(), "Loaded runtime enum definitions");
            self.add_entries(&entries);
        }
    }

    fn add_arc(&self, arc: Arc<SymbolEntry>) -> Arc<SymbolEntry> {
        let name_lower = arc.name.to_lowercase();
        let seq = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.all.insert(seq, (Arc::clone(&arc), name_lower.clone()));
        self.by_name
            .entry(name_lower)
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
        self.invalidate_composed_for_entries(entries.iter());
        let new_arcs: Vec<Arc<SymbolEntry>> = entries
            .iter()
            .map(|entry| self.add_arc(Arc::new(entry.clone())))
            .collect();
        self.update_default_completions(&new_arcs);
    }

    /// Like `add_entries` but takes owned entries, avoiding the clone into Arc.
    pub fn add_entries_owned(&self, entries: Vec<SymbolEntry>) {
        self.invalidate_composed_for_entries(entries.iter());
        let new_arcs: Vec<Arc<SymbolEntry>> = entries
            .into_iter()
            .map(|entry| self.add_arc(Arc::new(entry)))
            .collect();
        self.update_default_completions(&new_arcs);
    }

    /// Append newly-added entries into the default_completions cache up to the cap.
    ///
    /// Called after all entries from a batch are indexed. Uses a fast read-check
    /// to skip acquiring the write lock when the cache is already full.
    fn update_default_completions(&self, new_arcs: &[Arc<SymbolEntry>]) {
        if self
            .default_completions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
            >= DEFAULT_COMPLETIONS_CAP
        {
            return;
        }
        let mut cache = self
            .default_completions
            .write()
            .unwrap_or_else(|e| e.into_inner());
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
        self.default_completions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Arc<SymbolEntry>> {
        if limit == 0 {
            return Vec::new();
        }

        let query_lower = query.to_lowercase();
        let mut matches: Vec<(u8, String, usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let seq = *entry.key();
                let (arc, name_lower) = entry.value();
                if arc.synthetic || !name_lower.contains(&query_lower) {
                    return None;
                }
                let rank = if query_lower.is_empty() || *name_lower == query_lower {
                    0
                } else if name_lower.starts_with(&query_lower) {
                    1
                } else {
                    2
                };
                Some((rank, name_lower.clone(), seq, Arc::clone(arc)))
            })
            .collect();
        matches.sort_unstable_by(|a, b| {
            (a.0, &a.1, a.3.kind, a.3.id, &a.3.package, a.2).cmp(&(
                b.0,
                &b.1,
                b.3.kind,
                b.3.id,
                &b.3.package,
                b.2,
            ))
        });
        matches.truncate(limit);
        matches.into_iter().map(|(_, _, _, arc)| arc).collect()
    }

    pub fn search_in_package(&self, package_name: &str, query: &str) -> Vec<Arc<SymbolEntry>> {
        let query_lower = query.to_lowercase();
        let mut results: Vec<(String, usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let (arc, name_lower) = entry.value();
                (!arc.synthetic
                    && arc.package.eq_ignore_ascii_case(package_name)
                    && name_lower.contains(&query_lower))
                .then(|| (name_lower.clone(), *entry.key(), Arc::clone(arc)))
            })
            .collect();
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
            .map(|e| {
                let (arc, _) = e.value();
                Arc::clone(arc)
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn app_path(&self, package_name: &str) -> Option<std::path::PathBuf> {
        self.app_paths
            .get(&package_name.to_lowercase())
            .map(|v| v.value().clone())
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
    /// Used to clear previously registered workspace entries before re-adding them,
    /// preventing duplicates when the call graph is rebuilt.
    ///
    /// Every secondary index that stores `Vec<Arc<SymbolEntry>>` must be passed
    /// through `retain_arcs_not_in`; missing one leaves dangling
    /// references the next caller will see. The helper makes the discipline
    /// uniform — adding a new secondary index requires adding exactly one
    /// `Self::retain_arcs_not_in(&self.new_index, &ptrs);` line below.
    pub fn remove_package_entries(&self, package_name: &str) {
        self.remove_packages_named(&std::collections::HashSet::from([
            package_name.to_lowercase()
        ]));
    }

    fn clear_loaded_packages(&self) {
        let package_names: std::collections::HashSet<String> = self
            .app_paths
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        self.remove_packages_named(&package_names);
    }

    fn remove_packages_named(&self, package_names: &std::collections::HashSet<String>) {
        if package_names.is_empty() {
            return;
        }
        let to_remove: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let (arc, _) = entry.value();
                if package_names.contains(&arc.package.to_lowercase()) {
                    Some((*entry.key(), Arc::clone(arc)))
                } else {
                    None
                }
            })
            .collect();

        if to_remove.is_empty() {
            for package_name in package_names {
                self.app_paths.remove(package_name);
                self.source_path_cache
                    .retain(|(package, _, _), _| package != package_name);
            }
            return;
        }

        let ptrs: std::collections::HashSet<*const SymbolEntry> =
            to_remove.iter().map(|(_, arc)| Arc::as_ptr(arc)).collect();

        for (seq, _) in &to_remove {
            self.all.remove(seq);
        }

        Self::retain_arcs_not_in(&self.by_name, &ptrs);
        Self::retain_arcs_not_in(&self.by_kind_id, &ptrs);
        Self::retain_arcs_not_in(&self.by_kind, &ptrs);
        Self::retain_arcs_not_in(&self.by_extends, &ptrs);

        for package_name in package_names {
            self.app_paths.remove(package_name);
            self.source_path_cache
                .retain(|(package, _, _), _| package != package_name);
        }

        // Package additions/removals can change any composed base-extension
        // relationship. Clear the small derived cache rather than serving a
        // stale view after hot symbol reload.
        self.composed_cache.clear();
        self.rebuild_default_completions();
    }

    fn rebuild_default_completions(&self) {
        let mut remaining: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let (arc, _) = entry.value();
                (!arc.synthetic).then(|| (*entry.key(), Arc::clone(arc)))
            })
            .collect();
        remaining.sort_unstable_by_key(|(seq, _)| *seq);
        remaining.truncate(DEFAULT_COMPLETIONS_CAP);

        let mut cache = self
            .default_completions
            .write()
            .unwrap_or_else(|e| e.into_inner());
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
        // regression: synthetic Option enums emitted by table-
        // field OptionMembers carry id: -1 (model.rs:731). Pre-fix they all
        // accumulated under (Enum, -1) in by_kind_id and a get_by_id(Enum, -1)
        // returned every synthetic enum across the workspace, drowning real
        // lookups. The fix: only positive ids enter by_kind_id.
        let index = SymbolIndex::new();
        index.add_entries(&[
            make_entry(ObjectKind::Enum, -1, "InlineOpt1"),
            make_entry(ObjectKind::Enum, -1, "InlineOpt2"),
            make_entry(ObjectKind::Enum, 50200, "RealEnum"),
            // builtin-style sentinel: id == 0 ("not assigned").
            make_entry(ObjectKind::Codeunit, 0, "BuiltinHelper"),
        ]);

        // Sentinel ids must NOT be reachable through by_kind_id.
        assert!(
            index.get_by_id(ObjectKind::Enum, -1).is_empty(),
            "synthetic-enum sentinel id -1 must not be queryable"
        );
        assert!(
            index.get_by_id(ObjectKind::Codeunit, 0).is_empty(),
            "no-id sentinel 0 must not be queryable"
        );

        // Real positive id stays reachable as before.
        let real = index.get_by_id(ObjectKind::Enum, 50200);
        assert_eq!(real.len(), 1, "real enum id must still be reachable");
        assert_eq!(real[0].name, "RealEnum");

        // The entries are still discoverable via by_name (which doesn't gate
        // on sentinel ids). This proves we only narrowed by_kind_id, not
        // dropped the entries entirely.
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

    /// regression: removing a package must clear EVERY secondary
    /// index in lockstep. Adds entries that populate by_name, by_kind_id,
    /// by_kind, and by_extends, then removes the package and asserts each
    /// secondary index is empty for those entries. Adding a new secondary
    /// index later without wiring it through `retain_arcs_not_in` would
    /// leave its entries dangling — this test would still pass, so the
    /// followup discipline lives in the doc comment on the helper.
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
        index
            .app_paths
            .insert("drop".into(), "/tmp/drop.app".into());
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

    // ----- `appLocalFolderPaths` — what is actually supported -----
    //
    // The `al.appLocalFolderPaths` setting is parsed into `AlConfig`
    // (`al-project`) but is NOT yet wired into symbol loading — nothing reads
    // that field to feed paths into the index. What IS supported, and what such
    // wiring would ultimately call, is loading `.app` packages from an
    // *arbitrary directory* via `SymbolIndex::load_packages`. This test pins
    // that supported behavior: a package dropped in a non-`.alpackages` folder
    // resolves, its objects become queryable, and the index records the folder
    // the symbols came from.

    /// Build a minimal NAVX `.app` (header + ZIP of NavxManifest.xml +
    /// SymbolReference.json) so the loader has a real package to read.
    fn build_app(name: &str, table_id: i32, table_name: &str) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let manifest = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Package><App Id="00000000-0000-0000-0000-000000000001" Name="{name}" Publisher="Contoso" Version="1.0.0.0" /></Package>"#
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
        let loaded = index.load_packages(std::slice::from_ref(&app_path));

        // The package parsed and contributed its object...
        assert_eq!(loaded.len(), 1, "the local-folder .app should load");
        assert_eq!(loaded[0].name, "Local Lib");

        // ...the object is queryable through the normal index...
        let hits = index.get_by_name("Local Widget");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 50123);
        assert_eq!(hits[0].kind, ObjectKind::Table);

        // ...and the index records the exact local folder the symbols came from
        // (the resolution path that `appLocalFolderPaths` would drive).
        assert_eq!(
            index.app_path("Local Lib").as_deref(),
            Some(app_path.as_path())
        );
    }

    #[test]
    fn reloading_same_package_replaces_symbols_instead_of_duplicating() {
        let dir = tempfile::tempdir().unwrap();
        let app_path = dir.path().join("Contoso_LocalLib.app");
        std::fs::write(&app_path, build_app("Local Lib", 50_123, "Old Widget")).unwrap();
        let index = SymbolIndex::new();
        assert_eq!(
            index.load_packages(std::slice::from_ref(&app_path)).len(),
            1
        );

        std::fs::write(&app_path, build_app("Local Lib", 50_124, "New Widget")).unwrap();
        assert_eq!(
            index.load_packages(std::slice::from_ref(&app_path)).len(),
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
        index.load_packages_cached(&[first.clone(), second.clone()], &cache);

        index.replace_packages_cached(std::slice::from_ref(&second), &cache);

        assert!(index.get_by_name("First Table").is_empty());
        assert_eq!(index.get_by_name("Second Table").len(), 1);
        assert!(index.app_path("First").is_none());
        assert_eq!(index.app_path("Second").as_deref(), Some(second.as_path()));
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
                .len(),
            1
        );

        let loaded = index.replace_packages_cached(std::slice::from_ref(&invalid), &cache);

        assert!(loaded.is_empty());
        assert!(index.find_by_name("Keep Table").is_some());
        assert_eq!(index.app_path("Keep").as_deref(), Some(valid.as_path()));
    }
}
