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

        let results: Vec<_> = paths
            .par_iter()
            .filter_map(|path| {
                let path = path.as_ref();
                match app_reader::read_app_file(path) {
                    Ok(mut pkg) => {
                        debug!(
                            name = %pkg.name,
                            objects = pkg.objects.len(),
                            "Loaded package"
                        );
                        self.app_paths
                            .insert(pkg.name.to_lowercase(), path.to_path_buf());
                        self.add_entries_owned(std::mem::take(&mut pkg.objects));
                        Some(pkg)
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to load .app file");
                        None
                    }
                }
            })
            .collect();

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

        let results: Vec<_> = paths
            .par_iter()
            .filter_map(|path| {
                let path = path.as_ref();

                if let Some(mut pkg) = cache.load(path) {
                    self.app_paths
                        .insert(pkg.name.to_lowercase(), path.to_path_buf());
                    self.add_entries_owned(std::mem::take(&mut pkg.objects));
                    return Some(pkg);
                }

                match app_reader::read_app_file(path) {
                    Ok(mut pkg) => {
                        debug!(
                            name = %pkg.name,
                            objects = pkg.objects.len(),
                            "Loaded package (cache miss)"
                        );
                        if let Err(e) = cache.save(path, &pkg) {
                            warn!(path = %path.display(), error = %e, "Failed to save to cache");
                        }
                        self.app_paths
                            .insert(pkg.name.to_lowercase(), path.to_path_buf());
                        self.add_entries_owned(std::mem::take(&mut pkg.objects));
                        Some(pkg)
                    }
                    Err(e) => {
                        warn!(path = %path.display(), error = %e, "Failed to load .app file");
                        None
                    }
                }
            })
            .collect();

        results
    }

    pub fn load_package_bytes(
        &self,
        data: &[u8],
    ) -> Result<SymbolPackage, app_reader::AppReaderError> {
        let pkg = app_reader::read_app_bytes(data)?;
        self.add_entries(&pkg.objects);
        Ok(pkg)
    }

    /// Load well-known runtime enum types that are built into the AL compiler
    /// but not published in any .app package's SymbolReference.json.
    pub fn load_runtime_enums(&self) {
        use super::model::{EnumValueSymbol, SymbolEntry};

        let mut entries = Vec::new();
        for re in super::language_data::runtime_enums() {
            if !self.get_by_name(&re.name).is_empty() {
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
        // pollute lookups via by_kind_id (T029 / c2d935).
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
        let new_arcs: Vec<Arc<SymbolEntry>> = entries
            .iter()
            .map(|entry| self.add_arc(Arc::new(entry.clone())))
            .collect();
        self.update_default_completions(&new_arcs);
    }

    /// Like `add_entries` but takes owned entries, avoiding the clone into Arc.
    pub fn add_entries_owned(&self, entries: Vec<SymbolEntry>) {
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
            if cache.len() >= DEFAULT_COMPLETIONS_CAP {
                break;
            }
            cache.push(Arc::clone(arc));
        }
    }

    /// Return the pre-computed default completion entries (up to DEFAULT_COMPLETIONS_CAP).
    ///
    /// O(1) pointer copies — never iterates the full symbol index. Use in place of
    /// `search("", 30)` on completion hot paths (ISSUE-162).
    pub fn get_default_completions(&self) -> Vec<Arc<SymbolEntry>> {
        self.default_completions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Arc<SymbolEntry>> {
        let mut results = Vec::new();

        // Synthetic entries (pseudo-enums fabricated from Option-typed
        // fields) stay out of user-facing search results — they swamped
        // real enums with `id: -1` rows (FB-2). Exact-name lookups
        // (`get_by_name`) still see them for type resolution.
        if query.is_empty() {
            for entry in self.all.iter() {
                let (arc, _) = entry.value();
                if arc.synthetic {
                    continue;
                }
                results.push(Arc::clone(arc));
                if results.len() >= limit {
                    break;
                }
            }
        } else {
            let query_lower = query.to_lowercase();
            for entry in self.all.iter() {
                let (arc, name_lower) = entry.value();
                if !arc.synthetic && name_lower.contains(&query_lower) {
                    results.push(Arc::clone(arc));
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }

        results
    }

    pub fn search_in_package(&self, package_name: &str, query: &str) -> Vec<Arc<SymbolEntry>> {
        let mut results = Vec::new();
        let query_lower = query.to_lowercase();

        for entry in self.all.iter() {
            let (arc, name_lower) = entry.value();
            if !arc.synthetic
                && arc.package.eq_ignore_ascii_case(package_name)
                && (query_lower.is_empty() || name_lower.contains(&query_lower))
            {
                results.push(Arc::clone(arc));
            }
        }

        results
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
        let composed = super::composition::get_composed(self, kind, name)?;
        let arc = Arc::new(composed);
        self.composed_cache.insert(key, Arc::clone(&arc));
        Some(arc)
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

    pub fn get_events(&self, query: &str) -> super::events::EventResults {
        super::events::get_events(self, query)
    }

    /// Remove all entries whose `package` field matches `package_name` (case-insensitive).
    ///
    /// Used to clear previously registered workspace entries before re-adding them,
    /// preventing duplicates when the call graph is rebuilt.
    ///
    /// T049: every secondary index that stores `Vec<Arc<SymbolEntry>>` MUST be
    /// passed through `retain_arcs_not_in` here; missing one leaves dangling
    /// references the next caller will see. The helper makes the discipline
    /// uniform — adding a new secondary index requires adding exactly one
    /// `Self::retain_arcs_not_in(&self.new_index, &ptrs);` line below.
    pub fn remove_package_entries(&self, package_name: &str) {
        let to_remove: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let (arc, _) = entry.value();
                if arc.package.eq_ignore_ascii_case(package_name) {
                    Some((*entry.key(), Arc::clone(arc)))
                } else {
                    None
                }
            })
            .collect();

        if to_remove.is_empty() {
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

        // Rebuild default_completions — it may reference removed entries.
        let mut cache = self
            .default_completions
            .write()
            .unwrap_or_else(|e| e.into_inner());
        cache.retain(|arc| !ptrs.contains(&Arc::as_ptr(arc)));
    }

    /// Helper: filter a `DashMap<K, Vec<Arc<SymbolEntry>>>` secondary index
    /// in place, removing every Arc whose pointer appears in `ptrs` and
    /// dropping any key whose Vec becomes empty as a result.
    ///
    /// T049: extracted from the four-times-repeated retain pattern in
    /// remove_package_entries. New secondary indexes that follow the
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
        // T029 / c2d935 regression: synthetic Option enums emitted by table-
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

    /// T049 regression: removing a package must clear EVERY secondary
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

    /// T049 negative: remove_package_entries on a non-existent package is
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

    // ----- C7: `appLocalFolderPaths` — what is actually supported -----
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
        let loaded = index.load_packages(&[app_path.clone()]);

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
}
