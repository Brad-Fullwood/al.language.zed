//! Lookup and search over the loaded entries.

use super::{fold_name, SymbolIndex};
use super::{read_derived_cache, write_derived_cache, DEFAULT_COMPLETIONS_CAP};
use crate::model::{ObjectKind, SymbolEntry};
use std::sync::Arc;

impl SymbolIndex {
    /// Names the substring stage of [`search`] may examine for one query.
    ///
    /// [`search`]: Self::search
    pub const SUBSTRING_SCAN_BUDGET: usize = 20_000;

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

        // Third stage: a fragment in the middle of a name. There is no index
        // for that, so it scans, and a Base Application-scale catalogue is
        // ~50 000 names. The budget keeps one keystroke's worth of work bounded
        // rather than letting an unmatched fragment walk the whole catalogue.
        if !query_lower.is_empty() && results.len() < limit {
            let mut budget = Self::SUBSTRING_SCAN_BUDGET;
            for name in names.iter() {
                if budget == 0 {
                    tracing::debug!(
                        query = %query_lower,
                        budget = Self::SUBSTRING_SCAN_BUDGET,
                        "workspace symbol search: substring stage hit its scan budget"
                    );
                    break;
                }
                budget -= 1;
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

    pub(super) fn append_search_name(
        &self,
        name: &str,
        limit: usize,
        results: &mut Vec<Arc<SymbolEntry>>,
    ) {
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

    pub(super) fn sorted_names(&self) -> Arc<Vec<String>> {
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
}

#[cfg(test)]
mod tests {
    use crate::index::test_support::*;
    use crate::index::SymbolIndex;
    use crate::model::ObjectKind;

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

    /// The substring stage has no index behind it, so a fragment that matches
    /// nothing must not walk a Base Application-scale catalogue.
    #[test]
    fn the_substring_stage_stops_at_its_scan_budget() {
        let index = SymbolIndex::new();
        let over_budget = SymbolIndex::SUBSTRING_SCAN_BUDGET + 500;
        let entries: Vec<_> = (0..over_budget)
            .map(|i| make_entry(ObjectKind::Table, i as i32, &format!("Aaa Object {i:06}")))
            .collect();
        index.add_entries(&entries);
        // A name only the very end of the catalogue carries.
        index.add_entries(&[make_entry(ObjectKind::Table, 999_999, "Zzz Needle Object")]);

        let hits = index.search("needle", 10);
        assert!(
            hits.is_empty(),
            "the scan must stop at the budget rather than reaching the tail: {hits:?}"
        );
        // A prefix query still finds it: that stage is indexed, not scanned.
        assert_eq!(index.search("zzz needle", 10).len(), 1);
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
}
