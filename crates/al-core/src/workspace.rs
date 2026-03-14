//! Workspace state container.
//!
//! The `Workspace` struct owns all per-project state: documents, symbols,
//! semantic bridge, and configuration. It is the central coordination point
//! for all queries routed through al-core.

use std::path::PathBuf;
use std::sync::Arc;

use al_protocol::{AlProject, AlToolchain};
use al_semantic::BuiltinType;
use al_symbols::SymbolIndex;
use dashmap::DashMap;
use tokio::sync::RwLock;

use crate::documents::DocumentStore;

/// Central state container for an AL project.
///
/// Owns all per-project state: documents, symbols, workspace file index,
/// semantic bridge, builtins, and configuration.
pub struct Workspace {
    /// Open document management with rope-based text and parse tree caching.
    pub documents: DocumentStore,
    /// Symbol index for .alpackages and workspace objects.
    pub symbols: Arc<SymbolIndex>,
    /// Discovered AL toolchain (ALTool paths).
    pub toolchain: RwLock<Option<AlToolchain>>,
    /// Discovered AL project (app.json manifest, packages).
    pub project: RwLock<Option<AlProject>>,
    /// .NET semantic bridge for CodeAnalysis features.
    pub semantic: RwLock<Option<al_semantic::SemanticBridge>>,
    /// Raw text of all .al files in the workspace (not just open ones).
    pub workspace_files: DashMap<PathBuf, String>,
    /// Object name (lowercased) → file path index for fast lookups.
    pub workspace_objects: DashMap<String, PathBuf>,
    /// Reverse index: file path → object name (for O(1) cleanup in did_close).
    pub file_to_object: DashMap<PathBuf, String>,
    /// Builtins loaded once at init, read-only afterward.
    pub builtins: std::sync::RwLock<Arc<Vec<BuiltinType>>>,
    /// Compiler error codes — code → description mapping for diagnostic enrichment.
    pub error_codes: std::sync::RwLock<Arc<DashMap<String, String>>>,
    /// Whether the user approved generating symbol outlines for packages without source.
    pub outline_fallback_approved: std::sync::atomic::AtomicBool,
}

impl Workspace {
    /// Create a new empty workspace.
    pub fn new() -> Self {
        Self {
            documents: DocumentStore::new(),
            symbols: Arc::new(SymbolIndex::new()),
            toolchain: RwLock::new(None),
            project: RwLock::new(None),
            semantic: RwLock::new(None),
            workspace_files: DashMap::new(),
            workspace_objects: DashMap::new(),
            file_to_object: DashMap::new(),
            builtins: std::sync::RwLock::new(Arc::new(Vec::new())),
            error_codes: std::sync::RwLock::new(Arc::new(DashMap::new())),
            outline_fallback_approved: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}
