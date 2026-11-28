//! A read-only map stored as one sorted slice.
//!
//! The dependency source index holds about 260,000 small maps from a variable
//! name to its type, most with a few entries. A `BTreeMap` allocates a node of
//! eleven slots for each of them, so the maps took several times the bytes of
//! their entries. `SortedMap` holds exactly its entries. It serializes as the
//! map a `BTreeMap` of the same entries serializes as, so the summaries on disk
//! stay byte for byte the same.

use std::fmt;
use std::marker::PhantomData;

/// Entries sorted by key, each key once. Built from an iterator: on a
/// duplicate key the last value wins, as when collecting into a `BTreeMap`.
#[derive(Clone, PartialEq, Eq)]
pub struct SortedMap<K, V> {
    entries: Box<[(K, V)]>,
}

impl<K, V> Default for SortedMap<K, V> {
    fn default() -> Self {
        Self {
            entries: Box::default(),
        }
    }
}

impl<K: Ord + std::borrow::Borrow<str>, V> SortedMap<K, V> {
    pub fn get(&self, key: &str) -> Option<&V> {
        self.entries
            .binary_search_by(|(stored, _)| stored.borrow().cmp(key))
            .ok()
            .map(|index| &self.entries[index].1)
    }
}

impl<K, V> SortedMap<K, V> {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.entries.iter().map(|(key, value)| (key, value))
    }

    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.iter().map(|(key, _)| key)
    }

    /// The entries as one slice, for size accounting.
    pub fn entries(&self) -> &[(K, V)] {
        &self.entries
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for SortedMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut entries: Vec<(K, V)> = iter.into_iter().collect();
        // A stable sort keeps duplicates in insertion order, so keeping the
        // last of each run keeps the value inserted last.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut deduped: Vec<(K, V)> = Vec::with_capacity(entries.len());
        for entry in entries {
            match deduped.last_mut() {
                Some(last) if last.0 == entry.0 => *last = entry,
                _ => deduped.push(entry),
            }
        }
        Self {
            entries: deduped.into_boxed_slice(),
        }
    }
}

impl<K: fmt::Debug, V: fmt::Debug> fmt::Debug for SortedMap<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K: serde::Serialize, V: serde::Serialize> serde::Serialize for SortedMap<K, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.iter())
    }
}

impl<'de, K, V> serde::Deserialize<'de> for SortedMap<K, V>
where
    K: serde::Deserialize<'de> + Ord,
    V: serde::Deserialize<'de>,
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> serde::de::Visitor<'de> for Visitor<K, V>
        where
            K: serde::Deserialize<'de> + Ord,
            V: serde::Deserialize<'de>,
        {
            type Value = SortedMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(entries.into_iter().collect())
            }
        }

        deserializer.deserialize_map(Visitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::SortedMap;
    use std::collections::BTreeMap;

    #[test]
    fn matches_a_btreemap_built_from_the_same_entries() {
        let entries = [
            ("rec", "Customer"),
            ("acc", "G/L Account"),
            ("rec", "Vendor"),
            ("", "Item"),
            ("zz", "Item"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()));
        let sorted: SortedMap<String, String> = entries.iter().cloned().collect();
        let tree: BTreeMap<String, String> = entries.iter().cloned().collect();

        assert_eq!(
            sorted.iter().collect::<Vec<_>>(),
            tree.iter().collect::<Vec<_>>()
        );
        for key in ["rec", "acc", "", "zz", "missing"] {
            assert_eq!(sorted.get(key), tree.get(key), "{key}");
        }
        assert_eq!(
            serde_json::to_string(&sorted).unwrap(),
            serde_json::to_string(&tree).unwrap()
        );
        let read: SortedMap<String, String> =
            serde_json::from_str(&serde_json::to_string(&tree).unwrap()).unwrap();
        assert!(read == sorted);
        assert_eq!(format!("{sorted:?}"), format!("{tree:?}"));
    }

    #[test]
    fn an_empty_map_serializes_as_an_empty_object() {
        let empty = SortedMap::<String, String>::default();
        assert_eq!(serde_json::to_string(&empty).unwrap(), "{}");
        assert!(empty.get("x").is_none());
    }
}
