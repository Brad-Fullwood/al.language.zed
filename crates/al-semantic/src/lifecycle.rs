//! In-memory cache over builtin types.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{BuiltinMethod, BuiltinType};

/// In-memory cache of builtin types indexed by name for O(1) lookups.
///
/// Built from the `Vec<BuiltinType>` loaded from the .NET bridge (or disk cache).
/// Replaces O(n*m) linear scans in resolution.rs with hash lookups.
///
/// Version-aware: tracks the toolchain version it was built for. Callers can
/// check `is_stale()` to detect when the cache needs rebuilding after a
/// toolchain update.
pub struct SemanticCache {
    types: HashMap<String, BuiltinType>,
    /// Method name → list of (type_key, method_idx). Keys and type_key are lowercase.
    method_index: HashMap<String, Vec<(String, usize)>>,
    version: String,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl SemanticCache {
    pub fn new() -> Self {
        Self {
            types: HashMap::new(),
            method_index: HashMap::new(),
            version: String::new(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn build(builtins: &[BuiltinType], version: String) -> Self {
        let mut types = HashMap::with_capacity(builtins.len());
        for bt in builtins {
            let type_key = bt.name.to_lowercase();
            // CodeAnalysis catalogs are expected to be unique
            // case-insensitively. If a future toolchain returns duplicates,
            // use the last complete entry and build indexes only after
            // deduplication so they cannot point into a replaced value.
            types.insert(type_key, bt.clone());
        }

        let mut method_index: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        for (type_key, bt) in &types {
            for (i, method) in bt.methods.iter().enumerate() {
                method_index
                    .entry(method.name.to_lowercase())
                    .or_default()
                    .push((type_key.clone(), i));
            }
        }
        Self {
            types,
            method_index,
            version,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn get_type(&self, name: &str) -> Option<&BuiltinType> {
        let result = self.types.get(&name.to_lowercase());
        if result.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    pub fn get_method(&self, type_name: &str, method_name: &str) -> Option<&BuiltinMethod> {
        let Some(bt) = self.types.get(&type_name.to_lowercase()) else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let lower = method_name.to_lowercase();
        let method = bt
            .methods
            .iter()
            .find(|m| m.name.eq_ignore_ascii_case(&lower));
        if method.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        method
    }

    pub fn find_methods_by_name(&self, method_name: &str) -> Vec<(&str, &BuiltinMethod)> {
        let lower = method_name.to_lowercase();
        let results: Vec<_> = self
            .method_index
            .get(&lower)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|(type_key, method_idx)| {
                        let bt = self.types.get(type_key)?;
                        let method = bt.methods.get(*method_idx)?;
                        Some((bt.name.as_str(), method))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if results.is_empty() {
            self.misses.fetch_add(1, Ordering::Relaxed);
        } else {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        results
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn is_stale(&self, current_version: &str) -> bool {
        self.version.is_empty() || self.version != current_version
    }

    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty()
    }

    /// Cache hit/miss statistics: (hits, misses).
    pub fn stats(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }
}

impl Default for SemanticCache {
    fn default() -> Self {
        Self::new()
    }
}
