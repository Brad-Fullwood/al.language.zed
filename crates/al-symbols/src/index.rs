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
        }
    }

    /// Load and index all .app files from the given paths.
    ///
    /// Files that fail to parse are logged and skipped.
    pub fn load_packages(&self, paths: &[impl AsRef<Path>]) -> Vec<SymbolPackage> {
        let mut packages = Vec::new();

        for path in paths {
            let path = path.as_ref();
            match app_reader::read_app_file(path) {
                Ok(pkg) => {
                    debug!(
                        name = %pkg.name,
                        objects = pkg.objects.len(),
                        "Loaded package"
                    );
                    self.add_entries(&pkg.objects);
                    packages.push(pkg);
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "Failed to load .app file");
                }
            }
        }

        packages
    }

    /// Load a package from raw bytes (useful for in-memory / test scenarios).
    pub fn load_package_bytes(&self, data: &[u8]) -> Result<SymbolPackage, app_reader::AppReaderError> {
        let pkg = app_reader::read_app_bytes(data)?;
        self.add_entries(&pkg.objects);
        Ok(pkg)
    }

    /// Add a collection of symbol entries to the index.
    pub fn add_entries(&self, entries: &[SymbolEntry]) {
        for entry in entries {
            let arc = Arc::new(entry.clone());
            let id = self
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name_lower = entry.name.to_lowercase();
            self.all.insert(id, (Arc::clone(&arc), name_lower.clone()));

            // Index by lowercase name
            self.by_name
                .entry(name_lower)
                .or_default()
                .push(Arc::clone(&arc));

            // Index by (kind, id) — skip id=0 as those are unnamed/synthetic
            if entry.id != 0 {
                self.by_kind_id
                    .entry((entry.kind, entry.id))
                    .or_default()
                    .push(Arc::clone(&arc));
            }

            // Index by object kind
            self.by_kind
                .entry(entry.kind)
                .or_default()
                .push(Arc::clone(&arc));

            // Index extensions by lowercase extends name
            if let Some(ref extends) = entry.extends {
                self.by_extends
                    .entry(extends.to_lowercase())
                    .or_default()
                    .push(Arc::clone(&arc));
            }
        }
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

    /// Exact name match (case-insensitive). Returns all entries with that name.
    pub fn get_by_name(&self, name: &str) -> Vec<Arc<SymbolEntry>> {
        let key = name.to_lowercase();
        self.by_name
            .get(&key)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Lookup by object kind and ID.
    pub fn get_by_id(&self, kind: ObjectKind, id: i32) -> Vec<Arc<SymbolEntry>> {
        self.by_kind_id
            .get(&(kind, id))
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get all entries of a specific object kind.
    pub fn get_by_kind(&self, kind: ObjectKind) -> Vec<Arc<SymbolEntry>> {
        self.by_kind
            .get(&kind)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Get all extensions that extend a given object name.
    pub fn get_extensions_of(&self, base_name: &str) -> Vec<Arc<SymbolEntry>> {
        let target = base_name.to_lowercase();
        self.by_extends
            .get(&target)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Total number of indexed entries.
    pub fn len(&self) -> usize {
        self.all.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
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
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
        }
    }

    fn make_extension(kind: ObjectKind, id: i32, name: &str, extends: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id,
            name: name.to_string(),
            extends: Some(extends.to_string()),
            package: "TestPkg".to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
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
        entry.fields = vec![
            FieldSymbol {
                id: 1,
                name: "No.".to_string(),
                type_name: "Code".to_string(),
            },
        ];
        entry.methods = vec![
            MethodSymbol {
                name: "DoWork".to_string(),
                parameters: Vec::new(),
                return_type: Some("Boolean".to_string()),
                attributes: Vec::new(),
                is_local: false,
            },
        ];
        index.add_entries(&[entry]);

        let results = index.get_by_name("My Table");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fields.len(), 1);
        assert_eq!(results[0].methods.len(), 1);
    }
}
