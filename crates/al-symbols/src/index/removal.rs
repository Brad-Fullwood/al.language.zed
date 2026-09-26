//! Removing packages and entries, and the cache invalidation each removal
//! forces.

use super::{fold_name, SymbolIndex};
use super::{read_derived_cache, write_derived_cache};
use crate::model::SymbolEntry;
use std::sync::Arc;

impl SymbolIndex {
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

    /// Remove the entries of `package_name` that `keep` rejects.
    ///
    /// Workspace objects enter the index when the call graph is built, so a
    /// deleted file's object stayed searchable until the next build; this
    /// drops just those without waiting for it.
    pub fn retain_package_entries(&self, package_name: &str, keep: impl Fn(&SymbolEntry) -> bool) {
        let package = fold_name(package_name);
        let to_remove: Vec<(usize, Arc<SymbolEntry>)> = self
            .all
            .iter()
            .filter_map(|entry| {
                let indexed = entry.value();
                (fold_name(&indexed.arc.package) == package && !keep(&indexed.arc))
                    .then(|| (*entry.key(), Arc::clone(&indexed.arc)))
            })
            .collect();
        self.remove_selected_entries(to_remove);
    }

    pub(super) fn clear_loaded_packages(&self) {
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
        self.drop_source_indexes(|record| package_names.contains(&record.1));
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
    pub(super) fn remove_package_identities(&self, identities: &std::collections::HashSet<String>) {
        if identities.is_empty() {
            return;
        }
        self.drop_source_indexes(|record| identities.contains(&record.0));
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

    /// Release the process-global `.app` source index of every package this
    /// removal drops. Without this the index outlives the symbols it belongs
    /// to for the life of the process.
    pub(super) fn drop_source_indexes(&self, selected: impl Fn(&(String, String)) -> bool) {
        let paths: Vec<std::path::PathBuf> = self
            .app_paths
            .iter()
            .filter(|record| selected(&(record.key().clone(), record.value().name_key.clone())))
            .map(|record| record.value().path.clone())
            .collect();
        for path in paths {
            crate::source_index::remove_source_index(&path);
        }
    }

    /// Shared core of both removal flavors: prune the primary map and every
    /// secondary index for the selected entries, then repair derived caches.
    pub(super) fn remove_selected_entries(&self, to_remove: Vec<(usize, Arc<SymbolEntry>)>) {
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
}

#[cfg(test)]
mod tests {
    use super::super::{package_identity_key, AppPathRecord};
    use crate::index::test_support::*;
    use crate::index::SymbolIndex;
    use crate::model::ObjectKind;

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
        assert_eq!(index.default_completions_snapshot().len(), 30);
        assert!(index
            .default_completions_snapshot()
            .iter()
            .all(|entry| entry.package == "Keep"));
    }
}
