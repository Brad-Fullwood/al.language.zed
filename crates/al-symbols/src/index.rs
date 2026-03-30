//! Concurrent symbol index backed by DashMap.
//!
//! Provides fast lookup by name, object kind+ID, and substring search
//! across all loaded packages.

use std::path::Path;
use std::sync::Arc;

use dashmap::DashMap;
use tracing::{debug, warn};

use crate::app_reader;
use crate::model::{ObjectKind, SymbolEntry, SymbolPackage};

/// Maximum number of entries kept in the default-completion cache.
const DEFAULT_COMPLETIONS_CAP: usize = 30;

/// Thread-safe symbol index over multiple AL packages.
#[derive(Debug)]
pub struct SymbolIndex {
    /// Objects keyed by lowercase name. Multiple objects can share a name
    /// (e.g., a Table and a Page with the same name, or objects from different packages).
    by_name: DashMap<String, Vec<Arc<SymbolEntry>>>,
    /// Objects keyed by (ObjectKind, id).
    by_kind_id: DashMap<(ObjectKind, i32), Vec<Arc<SymbolEntry>>>,
    /// Objects keyed by ObjectKind (secondary index for O(1) kind lookups).
    by_kind: DashMap<ObjectKind, Vec<Arc<SymbolEntry>>>,
    /// Extension objects keyed by lowercase extends name (secondary index).
    by_extends: DashMap<String, Vec<Arc<SymbolEntry>>>,
    /// All entries with pre-computed lowercase names (for search).
    all: DashMap<usize, (Arc<SymbolEntry>, String)>,
    /// Next ID for the `all` map.
    next_id: std::sync::atomic::AtomicUsize,
    /// Maps lowercase package name -> .app file path on disk.
    app_paths: DashMap<String, std::path::PathBuf>,
    /// Lazy-loaded mapping of (PackageName, ObjectKind, ID) -> ZIP internal path.
    source_path_cache: DashMap<(String, ObjectKind, i32), String>,
    /// Cached composed views keyed by (ObjectKind, lowercase name).
    composed_cache: DashMap<(ObjectKind, String), Arc<crate::model::ComposedObject>>,
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
    /// Create an empty index.
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

    /// Store a cached source path mapping.
    pub fn cache_source_path(&self, package: String, kind: ObjectKind, id: i32, path: String) {
        self.source_path_cache
            .insert((package.to_lowercase(), kind, id), path);
    }

    /// Retrieve a cached source path mapping.
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

    /// Check if a package has been indexed for source paths.
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
                        // Use add_entries_owned to move objects into Arc without cloning.
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
        cache: &crate::cache::SymbolCache,
    ) -> Vec<SymbolPackage> {
        use rayon::prelude::*;

        let results: Vec<_> = paths
            .par_iter()
            .filter_map(|path| {
                let path = path.as_ref();

                // Try cache first
                if let Some(mut pkg) = cache.load(path) {
                    self.app_paths
                        .insert(pkg.name.to_lowercase(), path.to_path_buf());
                    self.add_entries_owned(std::mem::take(&mut pkg.objects));
                    return Some(pkg);
                }

                // Cache miss — parse from .app file
                match app_reader::read_app_file(path) {
                    Ok(mut pkg) => {
                        debug!(
                            name = %pkg.name,
                            objects = pkg.objects.len(),
                            "Loaded package (cache miss)"
                        );
                        // Save to cache for next time (before taking ownership of objects)
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

    /// Load a package from raw bytes (useful for in-memory / test scenarios).
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
        use crate::model::{EnumValueSymbol, SymbolEntry};

        let mut entries = Vec::new();
        for re in crate::language_data::runtime_enums() {
            // Skip if already present in the index (from a package)
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

    /// Insert a single Arc<SymbolEntry> into all five index maps.
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
        if arc.id != 0 {
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

    /// Add a collection of symbol entries to the index.
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
        // Fast path: cache already full — skip write lock entirely.
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

    /// Case-insensitive substring search across all object names.
    /// Returns up to `limit` matching entries.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Arc<SymbolEntry>> {
        let mut results = Vec::new();

        if query.is_empty() {
            // Short-circuit: return the first `limit` entries without filtering
            for entry in self.all.iter() {
                let (arc, _) = entry.value();
                results.push(Arc::clone(arc));
                if results.len() >= limit {
                    break;
                }
            }
        } else {
            let query_lower = query.to_lowercase();
            for entry in self.all.iter() {
                let (arc, name_lower) = entry.value();
                if name_lower.contains(&query_lower) {
                    results.push(Arc::clone(arc));
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }

        results
    }

    /// Search for objects within a specific package, case-insensitive substring search.
    pub fn search_in_package(&self, package_name: &str, query: &str) -> Vec<Arc<SymbolEntry>> {
        let mut results = Vec::new();
        let query_lower = query.to_lowercase();
        // The package name on the entry might be differently cased, but usually it matches
        let target_pkg = package_name.to_lowercase();

        for entry in self.all.iter() {
            let (arc, name_lower) = entry.value();
            if arc.package.to_lowercase() == target_pkg
                && (query_lower.is_empty() || name_lower.contains(&query_lower))
            {
                results.push(Arc::clone(arc));
            }
        }

        results
    }

    /// Exact name match (case-insensitive). Returns all entries with that name.
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

    /// Lookup by object kind and ID.
    pub fn get_by_id(&self, kind: ObjectKind, id: i32) -> Vec<Arc<SymbolEntry>> {
        self.by_kind_id
            .get(&(kind, id))
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Get all entries of a specific object kind.
    pub fn get_by_kind(&self, kind: ObjectKind) -> Vec<Arc<SymbolEntry>> {
        self.by_kind
            .get(&kind)
            .map(|v| v.value().clone())
            .unwrap_or_default()
    }

    /// Get all extensions that extend a given object name.
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

    /// Total number of indexed entries.
    pub fn len(&self) -> usize {
        self.all.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    /// Get the `.app` file path for a package name.
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
        kind: crate::model::ObjectKind,
        name: &str,
    ) -> Option<Arc<crate::model::ComposedObject>> {
        let key = (kind, name.to_lowercase());
        if let Some(cached) = self.composed_cache.get(&key) {
            return Some(Arc::clone(cached.value()));
        }
        let composed = crate::composition::get_composed(self, kind, name)?;
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

    /// Invalidate all cached composed views.
    pub fn invalidate_all_composed(&self) {
        self.composed_cache.clear();
    }

    /// Check if the composed cache is empty (for testing).
    pub fn is_composed_cache_empty(&self) -> bool {
        self.composed_cache.is_empty()
    }

    /// Find event publishers and subscribers matching a name pattern.
    ///
    /// Convenience method that delegates to [`crate::events::get_events`].
    pub fn get_events(&self, query: &str) -> crate::events::EventResults {
        crate::events::get_events(self, query)
    }

    /// Remove all entries whose `package` field matches `package_name` (case-insensitive).
    ///
    /// Used to clear previously registered workspace entries before re-adding them,
    /// preventing duplicates when the call graph is rebuilt.
    pub fn remove_package_entries(&self, package_name: &str) {
        let package_lower = package_name.to_lowercase();

        // Collect sequence IDs of entries to remove and their Arc pointers.
        let to_remove: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let (arc, _) = entry.value();
                if arc.package.to_lowercase() == package_lower {
                    Some((*entry.key(), Arc::clone(arc)))
                } else {
                    None
                }
            })
            .collect();

        if to_remove.is_empty() {
            return;
        }

        // Use Arc pointer equality to filter entries from the secondary maps.
        let ptrs: std::collections::HashSet<*const SymbolEntry> =
            to_remove.iter().map(|(_, arc)| Arc::as_ptr(arc)).collect();

        // Remove from `all`.
        for (seq, _) in &to_remove {
            self.all.remove(seq);
        }

        // Filter `by_name` Vecs.
        self.by_name.retain(|_, vec| {
            vec.retain(|arc| !ptrs.contains(&Arc::as_ptr(arc)));
            !vec.is_empty()
        });

        // Filter `by_kind_id` Vecs.
        self.by_kind_id.retain(|_, vec| {
            vec.retain(|arc| !ptrs.contains(&Arc::as_ptr(arc)));
            !vec.is_empty()
        });

        // Filter `by_kind` Vecs.
        self.by_kind.retain(|_, vec| {
            vec.retain(|arc| !ptrs.contains(&Arc::as_ptr(arc)));
            !vec.is_empty()
        });

        // Filter `by_extends` Vecs.
        self.by_extends.retain(|_, vec| {
            vec.retain(|arc| !ptrs.contains(&Arc::as_ptr(arc)));
            !vec.is_empty()
        });

        // Rebuild default_completions — it may reference removed entries.
        let mut cache = self
            .default_completions
            .write()
            .unwrap_or_else(|e| e.into_inner());
        cache.retain(|arc| !ptrs.contains(&Arc::as_ptr(arc)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{FieldSymbol, MethodSymbol, ObjectKind, SymbolEntry};

    fn make_entry(kind: ObjectKind, id: i32, name: &str) -> SymbolEntry {
        SymbolEntry {
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

        // Substring search
        let results = index.search("customer", 10);
        assert_eq!(results.len(), 3); // Customer, Customer Ledger Entry, Customer Card

        // Limit
        let results = index.search("customer", 2);
        assert_eq!(results.len(), 2);

        // Case insensitive
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
}
