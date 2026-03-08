//! DashMap-backed concurrent symbol index.

use std::path::PathBuf;
use std::sync::Arc;

use crate::model::{ObjectKind, SymbolEntry};

/// Concurrent symbol index for fast lookups.
pub struct SymbolIndex {
    _inner: dashmap::DashMap<String, Vec<Arc<SymbolEntry>>>,
}

impl SymbolIndex {
    pub fn new() -> Self {
        Self {
            _inner: dashmap::DashMap::new(),
        }
    }

    pub fn load_packages(packages: &[PathBuf]) -> Result<Self, crate::app_reader::AppReaderError> {
        let _ = packages;
        todo!("Port index loading from v2")
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Arc<SymbolEntry>> {
        let _ = (query, limit);
        todo!("Port search from v2")
    }

    pub fn get_by_name(&self, name: &str) -> Vec<Arc<SymbolEntry>> {
        let _ = name;
        todo!("Port get_by_name from v2")
    }

    pub fn get_by_id(&self, kind: ObjectKind, id: i64) -> Option<Arc<SymbolEntry>> {
        let _ = (kind, id);
        todo!("Port get_by_id from v2")
    }
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self::new()
    }
}
