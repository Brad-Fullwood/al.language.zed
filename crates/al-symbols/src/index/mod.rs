//! Concurrent symbol index backed by DashMap.
//!
//! Provides fast lookup by name, object kind+ID, and substring search
//! across all loaded packages.

mod caches;
mod entries;
mod errors;
mod loading;
mod query;
mod removal;
mod stats;

#[cfg(test)]
mod test_support;

pub use errors::{PackageLoadError, PackageLoadFailure};
pub use stats::SymbolIndexMemoryStats;

use crate::events;
use crate::model::{ObjectKind, SymbolEntry};
use dashmap::DashMap;
use std::sync::Arc;
use tracing::warn;

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
    composed_cache: DashMap<(ObjectKind, String), Arc<crate::model::ComposedObject>>,
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

/// Acquire a derived-cache read guard, discarding (never inspecting) a value
/// left behind by a panicked writer and replacing it with `T::default()`.
///
/// These locks protect only caches derived from the authoritative DashMap
/// indexes. Rebuilding or emptying them is therefore lossless; applying this
/// recovery rule to authoritative symbol state would not be safe.
pub(super) fn read_derived_cache<'a, T: Default>(
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
pub(super) fn write_derived_cache<'a, T: Default>(
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

    /// The current entry-set generation.
    ///
    /// Derived caches outside this crate key on it, so replacing or reloading
    /// a symbol package invalidates whatever they built from the old entries.
    pub fn generation(&self) -> u64 {
        self.mutation.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn len(&self) -> usize {
        self.all.len()
    }

    pub fn is_empty(&self) -> bool {
        self.all.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use crate::index::test_support::*;
    use crate::index::SymbolIndex;
    use crate::model::ObjectKind;

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
