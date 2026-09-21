//! Measured memory use of the index.

use super::read_derived_cache;
use super::{AppPathRecord, IndexedEntry, SymbolIndex};
use crate::model::{ObjectKind, SymbolEntry};
use std::sync::Arc;

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
    /// Process-global `.app` source indexes. Not owned by this index, but
    /// filled and emptied by package loading, and large enough for a
    /// source-bearing Base Application that leaving it out made the report
    /// understate the process by more than it reported.
    pub package_source_index_bytes: usize,
    pub package_source_index_count: usize,
    pub tracked_bytes: usize,
}

impl SymbolIndex {
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
                    + std::mem::size_of::<crate::model::ComposedObject>()
                    + value.extensions.capacity() * arc_bytes
                    + value.all_fields.capacity() * std::mem::size_of::<crate::model::FieldSymbol>()
                    + value.all_methods.capacity()
                        * std::mem::size_of::<crate::model::MethodSymbol>()
                    + value.all_controls.capacity()
                        * std::mem::size_of::<crate::model::ControlSymbol>()
                    + value.all_enum_values.capacity()
                        * std::mem::size_of::<crate::model::EnumValueSymbol>()
            })
            .sum::<usize>();

        let package_source_index_bytes = crate::source_index::cached_memory_bytes();
        SymbolIndexMemoryStats {
            symbol_payload_bytes,
            lookup_index_bytes,
            path_cache_bytes,
            composed_cache_bytes,
            package_source_index_bytes,
            package_source_index_count: crate::source_index::cached_index_count(),
            tracked_bytes: symbol_payload_bytes
                + lookup_index_bytes
                + path_cache_bytes
                + composed_cache_bytes
                + package_source_index_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::index::test_support::*;
    use crate::index::SymbolIndex;
    use crate::model::ObjectKind;

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
                // The package source-index cache is process-global, so its
                // size depends on what else the process has loaded.
                + populated.package_source_index_bytes
        );
    }
}
