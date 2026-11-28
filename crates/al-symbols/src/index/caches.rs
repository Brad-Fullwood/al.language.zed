//! The caches derived from the entries: composed objects, the event catalog,
//! source paths and package availability.

use super::{fold_name, package_identity_key, SymbolIndex};
use super::{read_derived_cache, write_derived_cache};
use crate::events;
use crate::model::{ObjectKind, SymbolEntry};
use crate::source_availability::{self, SourceAvailability, SourceAvailabilitySummary};
use std::sync::Arc;

impl SymbolIndex {
    /// Shared, lazily rebuilt event catalog for the current entry generation.
    pub(crate) fn event_catalog(&self) -> Arc<events::EventCatalog> {
        let generation = self.mutation.load(std::sync::atomic::Ordering::Acquire);
        {
            let (cache, _) = read_derived_cache(&self.event_catalog, "event_catalog");
            if let Some(cached) = cache.as_ref() {
                if cached.generation == generation {
                    return Arc::clone(&cached.catalog);
                }
            }
        }
        let catalog = Arc::new(events::build_event_catalog(self));
        let (mut cache, _) = write_derived_cache(&self.event_catalog, "event_catalog");
        // A concurrent builder may have stored its own catalog; either is a
        // complete snapshot, so last-write-wins is safe.
        *cache = Some(events::EventCatalogCache {
            generation,
            catalog: Arc::clone(&catalog),
        });
        catalog
    }

    pub fn get_events(&self, query: &str) -> crate::events::EventResults {
        crate::events::get_events(self, query)
    }

    /// Get a composed view with caching. Returns Arc for zero-copy sharing.
    ///
    /// Cached results are returned on repeat calls. Use [`SymbolIndex::invalidate_composed`]
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
        let composed = Arc::new(crate::composition::get_composed(self, kind, name)?);
        Some(Arc::clone(
            self.composed_cache
                .entry(key)
                .or_insert_with(|| Arc::clone(&composed))
                .value(),
        ))
    }

    /// Get the composed view of one indexed base entry, cached per package.
    ///
    /// Two packages can each declare an object with the same kind and name.
    /// A view keyed by name alone gave both packages the same base and
    /// extensions. The key here is `<lower name>\u{1}<package key>`, so each
    /// package keeps its own view. [`SymbolIndex::invalidate_composed`] removes
    /// every package's view of a name.
    ///
    /// `package_key_of` scans all indexed entries, so each call costs O(entries),
    /// cache hits included. An entry the index does not hold has no package key,
    /// so it is composed without caching.
    pub fn get_composed_for_entry(
        &self,
        entry: &Arc<SymbolEntry>,
    ) -> Option<Arc<crate::model::ComposedObject>> {
        let Some(package_key) = self.package_key_of(entry) else {
            return crate::composition::get_composed_for_base(self, Arc::clone(entry))
                .map(Arc::new);
        };
        let key = (
            entry.kind,
            format!("{}\u{1}{package_key}", entry.name.to_lowercase()),
        );
        if let Some(cached) = self.composed_cache.get(&key) {
            return Some(Arc::clone(cached.value()));
        }
        let composed = Arc::new(crate::composition::get_composed_for_base(
            self,
            Arc::clone(entry),
        )?);
        Some(Arc::clone(
            self.composed_cache
                .entry(key)
                .or_insert_with(|| Arc::clone(&composed))
                .value(),
        ))
    }

    /// Invalidate cached composed views for a given object name.
    ///
    /// Removes the name's view and every per-package view of that name.
    ///
    /// Call when a workspace file defining or extending this object changes.
    pub fn invalidate_composed(&self, name: &str) {
        let lower = name.to_lowercase();
        let per_package_prefix = format!("{lower}\u{1}");
        self.composed_cache
            .retain(|k, _| k.1 != lower && !k.1.starts_with(per_package_prefix.as_str()));
    }

    pub fn invalidate_all_composed(&self) {
        self.composed_cache.clear();
    }

    pub fn is_composed_cache_empty(&self) -> bool {
        self.composed_cache.is_empty()
    }

    pub(super) fn invalidate_composed_for_entries<'a>(
        &self,
        entries: impl Iterator<Item = &'a SymbolEntry>,
    ) {
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
            // A per-package key is `<name>\u{1}<package key>`. Only the name part is compared.
            self.composed_cache.retain(|(_, key), _| {
                let name = key.split('\u{1}').next().unwrap_or_default();
                !affected.contains(name)
            });
        }
    }

    pub fn cache_source_path(&self, package: String, kind: ObjectKind, id: i32, path: String) {
        self.source_path_cache
            .insert((fold_name(&package), kind, id), path);
    }

    pub fn get_cached_source_path(
        &self,
        package: &str,
        kind: ObjectKind,
        id: i32,
    ) -> Option<String> {
        self.source_path_cache
            .get(&(fold_name(package), kind, id))
            .map(|s| s.value().clone())
    }

    /// Report what kind of source navigation is genuinely available for an
    /// indexed object. Source indexes are cached per package path, so repeated
    /// calls do not rescan archives.
    pub fn source_availability(&self, entry: &SymbolEntry) -> SourceAvailability {
        let app_path = self.app_path(&entry.package);
        source_availability::classify(entry, app_path.as_deref())
    }

    /// Count source representations for every object in one package.
    /// Synthetic Option enums are not objects and are left out.
    ///
    /// The `.app` path is resolved **once** for the whole package rather than
    /// per entry: [`Self::app_path`] scans every `app_paths` record with a
    /// `min_by`, so calling it inside the loop made this O(entries × packages)
    /// on a workspace with a full BC symbol set. Every entry in the bucket
    /// shares the package key, so one lookup is also the correct one.
    pub fn package_source_availability(&self, package: &str) -> SourceAvailabilitySummary {
        let mut summary = SourceAvailabilitySummary::default();
        let app_path = self.app_path(package);
        if let Some(entries) = self.by_package.get(&fold_name(package)) {
            for symbol in entries.value().iter().filter(|symbol| !symbol.synthetic) {
                summary.record(source_availability::classify(symbol, app_path.as_deref()));
            }
        }
        summary
    }

    /// Source availability for the one package with this identity (app id,
    /// display name when the id is empty).
    ///
    /// [`Self::package_source_availability`] pools every package that shares
    /// a display name, so two apps named `System` reported the same counts.
    /// A package whose identity is not indexed reports all zeros.
    pub fn package_source_availability_for(
        &self,
        app_id: &str,
        name: &str,
    ) -> SourceAvailabilitySummary {
        let identity = package_identity_key(app_id, name);
        let app_path = self
            .app_paths
            .get(&identity)
            .map(|record| record.value().path.clone());
        let mut summary = SourceAvailabilitySummary::default();
        for entry in self.all.iter() {
            let indexed = entry.value();
            if indexed.package_key == identity && !indexed.arc.synthetic {
                summary.record(source_availability::classify(
                    &indexed.arc,
                    app_path.as_deref(),
                ));
            }
        }
        summary
    }

    pub fn is_package_indexed(&self, package: &str) -> bool {
        let name_key = fold_name(package);
        self.app_paths
            .iter()
            .any(|record| record.value().name_key == name_key)
    }

    /// Resolve a package's `.app` path by display name.
    ///
    /// Paths are stored under canonical identity keys (app GUID + name
    /// fallback), so two same-named packages keep independent records; a
    /// name-only lookup over an ambiguous name returns the record with the
    /// smallest identity key for determinism.
    pub fn app_path(&self, package_name: &str) -> Option<std::path::PathBuf> {
        let name_key = fold_name(package_name);
        self.app_paths
            .iter()
            .filter(|record| record.value().name_key == name_key)
            .min_by(|a, b| a.key().cmp(b.key()))
            .map(|record| record.value().path.clone())
    }

    /// Resolve the `.app` path of the package `entry` was indexed from.
    ///
    /// [`Self::app_path`] looks a package up by display name and picks one of
    /// several same-named packages, so an entry of one version or publisher of
    /// `Base Application` would open the other's source. This finds the
    /// indexed record holding `entry` and reads the path stored under its
    /// package identity. An entry the index does not hold falls back to the
    /// name lookup.
    pub fn app_path_for_entry(&self, entry: &SymbolEntry) -> Option<std::path::PathBuf> {
        match self.package_key_of(entry) {
            Some(key) => self
                .app_paths
                .get(&key)
                .map(|record| record.value().path.clone()),
            None => self.app_path(&entry.package),
        }
    }

    /// The package identity key (`id:<guid>` or `name:<name>`) of the indexed
    /// record holding `entry`, found by address.
    pub(crate) fn package_key_of(&self, entry: &SymbolEntry) -> Option<String> {
        self.all
            .iter()
            .find(|indexed| std::ptr::eq(indexed.value().arc.as_ref(), entry))
            .map(|indexed| indexed.value().package_key.clone())
    }

    /// Return every file-backed package path currently published in the index.
    ///
    /// Package paths remain authoritative even when `SymbolReference.json`
    /// contains no object entries. Deriving this list from [`Self::all_entries`]
    /// would make an empty-symbol package (which may still contain embedded AL
    /// source) invisible to dependency-source and call-graph construction.
    pub fn loaded_package_paths(&self) -> Vec<std::path::PathBuf> {
        let mut paths: Vec<_> = self
            .app_paths
            .iter()
            .map(|entry| entry.value().path.clone())
            .collect();
        paths.sort_unstable();
        paths.dedup();
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_support::*;
    use crate::index::SymbolIndex;
    use crate::model::ObjectKind;

    #[test]
    fn poisoned_derived_caches_are_discarded_and_rebuilt_from_symbol_indexes() {
        let index = Arc::new(SymbolIndex::new());
        index.add_entries(&[
            make_entry(ObjectKind::Table, 1, "Alpha"),
            make_entry(ObjectKind::Page, 2, "Beta"),
        ]);
        assert_eq!(index.search("alpha", 10).len(), 1);

        let poison_target = Arc::clone(&index);
        let _ = std::thread::spawn(move || {
            let mut names = poison_target.sorted_names.write().unwrap();
            *names = Some(Arc::new(vec!["corrupt".to_string()]));
            panic!("poison derived symbol caches for test");
        })
        .join();

        let search = index.search("alpha", 10);
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].name, "Alpha");

        assert!(!index.sorted_names.is_poisoned());
    }

    #[test]
    fn app_path_for_entry_uses_package_key() {
        let index = SymbolIndex::new();
        let package = |app_id: &str, id: i32| crate::model::SymbolPackage {
            app_id: app_id.to_string(),
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "1.0.0.0".to_string(),
            objects: vec![SymbolEntry {
                package: "Base Application".to_string(),
                ..make_entry(ObjectKind::Table, id, "Thing")
            }],
            object_count: 1,
        };
        let first = std::path::PathBuf::from("/packages/first.app");
        let second = std::path::PathBuf::from("/packages/second.app");
        index.index_loaded_package(
            package("11111111-1111-1111-1111-111111111111", 1),
            Some(&first),
            false,
        );
        index.index_loaded_package(
            package("22222222-2222-2222-2222-222222222222", 2),
            Some(&second),
            false,
        );

        let entries = index.get_by_name("Thing");
        assert_eq!(entries.len(), 2);
        for entry in entries {
            let expected = if entry.id == 1 { &first } else { &second };
            assert_eq!(index.app_path_for_entry(&entry).as_ref(), Some(expected));
        }
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
    fn per_package_composed_views_drop_with_their_name() {
        let index = SymbolIndex::new();
        let package = |app_id: &str| crate::model::SymbolPackage {
            app_id: app_id.to_string(),
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "1.0.0.0".to_string(),
            objects: vec![SymbolEntry {
                package: "Base Application".to_string(),
                ..make_entry(ObjectKind::Table, 1, "Setup")
            }],
            object_count: 1,
        };
        let first = std::path::PathBuf::from("/packages/first.app");
        let second = std::path::PathBuf::from("/packages/second.app");
        index.index_loaded_package(
            package("11111111-1111-1111-1111-111111111111"),
            Some(&first),
            false,
        );
        index.index_loaded_package(
            package("22222222-2222-2222-2222-222222222222"),
            Some(&second),
            false,
        );

        for entry in index.get_by_name("Setup") {
            assert!(index.get_composed_for_entry(&entry).is_some());
        }
        assert_eq!(index.composed_cache.len(), 2);

        index.invalidate_composed("SETUP");
        assert!(index.is_composed_cache_empty());

        for entry in index.get_by_name("Setup") {
            assert!(index.get_composed_for_entry(&entry).is_some());
        }
        index.add_entries(&[make_extension(
            ObjectKind::TableExtension,
            50_100,
            "Setup Ext",
            "Setup",
        )]);
        assert!(index.is_composed_cache_empty());
    }
}
