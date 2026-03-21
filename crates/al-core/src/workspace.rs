//! Workspace state container.
//!
//! The `Workspace` struct owns all per-project state: documents, symbols,
//! semantic bridge, and configuration. It is the central coordination point
//! for all queries routed through al-core.

use std::sync::Arc;

use crate::project::AlProject;
use crate::toolchain::AlToolchain;
use al_semantic::BuiltinType;
use al_symbols::SymbolIndex;
use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::RwLock;

use crate::config::AlConfig;
use crate::documents::DocumentStore;
use crate::file_index::FileIndex;
use crate::insight::graph::InsightGraph;
use crate::semantic::SemanticCache;

/// Callback for surfacing bridge/toolchain notifications to the user.
///
/// In the LSP path this calls `client.show_message`; in the daemon path it logs.
/// Takes only `&str` -- no tower-lsp types in al-core.
pub type NotifySink = Arc<dyn Fn(&str) + Send + Sync>;

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
///
/// ## Lock strategy (ISSUE-114)
///
/// Two RwLock flavors are used intentionally:
/// - `tokio::sync::RwLock` -- fields written in async contexts that hold the lock
///   across `.await` points: `toolchain`, `project`, `semantic`, `config`.
/// - `std::sync::RwLock` -- fields accessed from sync query code only:
///   `builtins`, `package_info`, `semantic_cache`, `insight_graph`.
///   Using `tokio::sync` here would require `.await` in every sync query caller.
///
/// Never hold a `std::sync::RwLock` guard across an `.await` -- that deadlocks
/// the tokio executor.
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
    /// Compiler error codes -- code -> description mapping for diagnostic enrichment.
    pub error_codes: DashMap<String, String>,
    /// Number of times the semantic bridge has been restarted (capped at MAX_RESTARTS).
    pub bridge_restart_count: std::sync::atomic::AtomicU32,
    /// Summary metadata for loaded symbol packages.
    pub package_info: std::sync::RwLock<Vec<PackageInfo>>,
    /// In-memory cache of builtin types indexed by name for O(1) lookups.
    pub semantic_cache: std::sync::RwLock<SemanticCache>,
    /// Active AL debug session (None if not debugging).
    pub debug_session: tokio::sync::Mutex<Option<crate::native_debug::NativeDebugSession>>,
    /// Optional callback for user-visible notifications (bridge failures, etc.).
    ///
    /// Set by al-lsp after workspace construction. In the LSP path the closure
    /// calls `client.show_message`; in the daemon path it logs. Not set in tests.
    pub notify_sink: std::sync::OnceLock<NotifySink>,
    /// Cached insight graph. Built lazily; invalidated when packages reload (ISSUE-132 fix).
    pub insight_graph: std::sync::RwLock<Option<Arc<InsightGraph>>>,
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
            error_codes: DashMap::new(),
            bridge_restart_count: std::sync::atomic::AtomicU32::new(0),
            package_info: std::sync::RwLock::new(Vec::new()),
            semantic_cache: std::sync::RwLock::new(SemanticCache::new()),
            debug_session: tokio::sync::Mutex::new(None),
            notify_sink: std::sync::OnceLock::new(),
            insight_graph: std::sync::RwLock::new(None),
        }
    }

    /// Get (or lazily build) the cached InsightGraph (ISSUE-132 fix).
    ///
    /// The graph is built once from the current symbol index and cached.
    /// Call `invalidate_insight_graph()` after reloading packages.
    pub fn get_or_build_insight_graph(&self) -> Arc<InsightGraph> {
        if let Ok(guard) = self.insight_graph.read() {
            if let Some(arc) = guard.as_ref() {
                return Arc::clone(arc);
            }
        }
        // Build and cache
        let mut graph = InsightGraph::new();
        graph.build_from_index(&self.symbols);
        let arc = Arc::new(graph);
        if let Ok(mut guard) = self.insight_graph.write() {
            *guard = Some(Arc::clone(&arc));
        }
        arc
    }

    /// Invalidate the cached InsightGraph (call when packages reload).
    pub fn invalidate_insight_graph(&self) {
        if let Ok(mut guard) = self.insight_graph.write() {
            *guard = None;
        }
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}
