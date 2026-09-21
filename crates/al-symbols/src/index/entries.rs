//! Adding entries to the index and the lookup tables derived from them.

use super::{fold_name, IndexedEntry, SymbolIndex, DEFAULT_COMPLETIONS_CAP};
use super::{package_identity_key, read_derived_cache, write_derived_cache};
use crate::model::SymbolEntry;
use std::sync::Arc;

impl SymbolIndex {
    pub(super) fn add_arc(&self, arc: Arc<SymbolEntry>, package_key: String) -> Arc<SymbolEntry> {
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
    pub(super) fn add_entries_owned_with_key(
        &self,
        entries: Vec<SymbolEntry>,
        package_key: Option<String>,
    ) {
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

    pub(super) fn update_sorted_names_after_add(
        &self,
        entries: &[Arc<SymbolEntry>],
        update_in_place: bool,
    ) {
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
    pub(super) fn update_default_completions(&self, new_arcs: &[Arc<SymbolEntry>]) {
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
    pub fn default_completions_snapshot(&self) -> Vec<Arc<SymbolEntry>> {
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

    pub(super) fn rebuild_default_completions(&self) {
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
    pub(super) fn retain_arcs_not_in<K>(
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
    use crate::index::test_support::*;
    use crate::index::SymbolIndex;
    use crate::model::{FieldSymbol, MethodSymbol, ObjectKind};

    #[test]
    fn default_completions_exclude_synthetic_entries() {
        let index = SymbolIndex::new();
        let mut synthetic = make_entry(ObjectKind::Enum, -1, "Synthetic Option");
        synthetic.synthetic = true;
        index.add_entries(&[synthetic, make_entry(ObjectKind::Table, 1, "Real Table")]);

        let defaults = index.default_completions_snapshot();
        assert_eq!(defaults.len(), 1);
        assert_eq!(defaults[0].name, "Real Table");
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
