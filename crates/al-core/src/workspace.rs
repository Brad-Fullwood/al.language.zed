//! Workspace state container.
//!
//! The `Workspace` struct owns all per-project state: documents, symbols,
//! semantic bridge, and configuration. It is the central coordination point
//! for all queries routed through al-core.

use std::sync::Arc;

use al_protocol::{AlProject, AlToolchain};
use al_semantic::BuiltinType;
use al_symbols::SymbolIndex;
use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::config::AlConfig;
use crate::documents::DocumentStore;
use crate::file_index::FileIndex;

/// Summary metadata for a loaded symbol package.
#[derive(Debug, Clone, Serialize)]
pub struct PackageInfo {
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub object_count: usize,
}

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
    /// Index of all .al files in the workspace directory.
    pub file_index: FileIndex,
    /// Merged workspace configuration (settings from client + project defaults).
    pub config: RwLock<AlConfig>,
    /// Builtins loaded once at init, read-only afterward.
    pub builtins: std::sync::RwLock<Arc<Vec<BuiltinType>>>,
    /// Compiler error codes — code → description mapping for diagnostic enrichment.
    pub error_codes: std::sync::RwLock<Arc<DashMap<String, String>>>,
    /// Number of times the semantic bridge has been restarted (capped at MAX_RESTARTS).
    pub bridge_restart_count: std::sync::atomic::AtomicU32,
    /// Whether the user approved generating symbol outlines for packages without source.
    pub outline_fallback_approved: std::sync::atomic::AtomicBool,
    /// Summary metadata for loaded symbol packages.
    pub package_info: std::sync::RwLock<Vec<PackageInfo>>,
    /// Active AL debug session (None if not debugging).
    pub debug_session: tokio::sync::Mutex<Option<al_dap_client::session::DebugSession>>,
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
            file_index: FileIndex::new(),
            config: RwLock::new(AlConfig::default()),
            builtins: std::sync::RwLock::new(Arc::new(Vec::new())),
            error_codes: std::sync::RwLock::new(Arc::new(DashMap::new())),
            bridge_restart_count: std::sync::atomic::AtomicU32::new(0),
            outline_fallback_approved: std::sync::atomic::AtomicBool::new(false),
            package_info: std::sync::RwLock::new(Vec::new()),
            debug_session: tokio::sync::Mutex::new(None),
        }
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}
