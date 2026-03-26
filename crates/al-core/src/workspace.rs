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
use crate::insight::index::CallGraph;
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
    /// Cached call graph. Built lazily after insight graph; invalidated with it.
    pub call_graph: std::sync::RwLock<Option<CallGraph>>,
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
            call_graph: std::sync::RwLock::new(None),
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
        if let Ok(mut guard) = self.call_graph.write() {
            *guard = None;
        }
    }

    /// Get (or lazily build) the cached CallGraph.
    ///
    /// Builds a workspace-enriched InsightGraph (symbol index + workspace file
    /// objects/procedures/subscribers), then builds the CallGraph and populates
    /// Tier 1 call edges. The enriched InsightGraph replaces the cached one.
    pub fn get_or_build_call_graph(&self) -> (Arc<InsightGraph>, std::sync::RwLockReadGuard<'_, Option<CallGraph>>) {
        // Check if call graph already exists
        {
            let cg_guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
            if cg_guard.is_some() {
                let insight = self.get_or_build_insight_graph();
                return (insight, cg_guard);
            }
        }

        // Build enriched InsightGraph: symbols + workspace nodes
        let mut graph = InsightGraph::new();
        graph.build_from_index(&self.symbols);
        // Register workspace objects, procedures, events, and subscribers
        crate::insight::calls::register_workspace_nodes(
            &self.file_index, &self.symbols, &mut graph,
        );
        let insight = Arc::new(graph);

        // Cache the enriched InsightGraph (replaces symbol-only version)
        if let Ok(mut ig_guard) = self.insight_graph.write() {
            *ig_guard = Some(Arc::clone(&insight));
        }

        // Build CallGraph and populate Tier 1 call edges
        let mut cg = CallGraph::build_from_insight(&insight);
        crate::insight::calls::populate_workspace_call_edges(
            &self.file_index, &self.symbols, &insight, &mut cg,
        );

        let mut guard = self.call_graph.write().unwrap_or_else(|e| e.into_inner());
        *guard = Some(cg);
        drop(guard);

        let guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
        (insight, guard)
    }

    /// Approximate memory statistics for the workspace.
    pub fn memory_stats(&self) -> WorkspaceMemoryStats {
        let symbol_count = self.symbols.len();
        let open_docs = self.documents.len();
        let workspace_files = self.file_index.files.len();
        let procedure_index_entries = self.file_index.procedures.len();
        let error_code_count = self.error_codes.len();
        let builtin_count = self.builtins
            .read()
            .map(|b| b.len())
            .unwrap_or(0);
        let package_count = self.package_info
            .read()
            .map(|p| p.len())
            .unwrap_or(0);

        WorkspaceMemoryStats {
            symbol_count,
            open_docs,
            workspace_files,
            procedure_index_entries,
            error_code_count,
            builtin_count,
            package_count,
        }
    }
}

/// Approximate memory statistics for diagnostic/observability.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceMemoryStats {
    pub symbol_count: usize,
    pub open_docs: usize,
    pub workspace_files: usize,
    pub procedure_index_entries: usize,
    pub error_code_count: usize,
    pub builtin_count: usize,
    pub package_count: usize,
}

/// Result of a successful core workspace initialization.
///
/// Callers (LSP and daemon) consume this to perform their transport-specific
/// post-init steps (sending notifications, opening files in DocumentStore, etc.).
pub struct CoreInitResult {
    /// Number of workspace .al files found by the file scanner.
    pub file_count: usize,
    /// Number of symbol packages loaded.
    pub package_count: usize,
    /// Total symbols across all loaded packages.
    pub total_symbols: usize,
    /// Whether a toolchain was found.
    pub has_toolchain: bool,
}

/// Initialize the common parts of a workspace.
///
/// Shared by the LSP and daemon initialisation paths.  Performs:
/// 1. `find_project(project_root)` — discover `app.json` and package paths.
/// 2. `load_packages_cached(packages, cache)` — load symbol packages.
/// 3. `workspace.symbols.load_runtime_enums()` — load runtime enum definitions.
/// 4. `workspace.invalidate_insight_graph()` — reset the cached graph.
/// 5. `file_index.scan(root)` — discover workspace .al files.
/// 6. Store package metadata in `workspace.package_info`.
/// 7. Write the project into `workspace.project`.
/// 8. `find_toolchain()` — discover the AL toolchain.
/// 9. Write the toolchain into `workspace.toolchain`.
///
/// The caller is responsible for transport-specific steps such as: notifying the
/// user, opening files in DocumentStore, auto-downloading missing packages, or
/// loading builtins from disk cache.
///
/// Returns `Ok(CoreInitResult)` on success; the workspace may be partially
/// initialised (e.g. project not found but toolchain found) — errors are logged
/// via `tracing` and represented as partial results.
pub async fn initialize_core_workspace(
    workspace: &Workspace,
    project_root: &std::path::Path,
) -> CoreInitResult {
    let file_count;
    let mut package_count = 0usize;
    let mut total_symbols = 0usize;

    match crate::project::find_project(project_root) {
        Ok(project) => {
            tracing::info!(
                name = %project.app_json.name,
                root = %project.root.display(),
                packages = project.packages.len(),
                "workspace: project discovered"
            );

            // Load symbol packages (with disk cache for fast warm starts).
            let cache = crate::symbols::cache::SymbolCache::default_location();
            let loaded = workspace.symbols.load_packages_cached(&project.packages, &cache);
            total_symbols = loaded.iter().map(|p| p.objects.len()).sum();
            package_count = loaded.len();
            tracing::info!(packages = package_count, symbols = total_symbols, "workspace: loaded symbol packages");

            // Load runtime enum definitions (compiler built-ins not in any package).
            workspace.symbols.load_runtime_enums();

            // Invalidate insight graph — packages changed.
            workspace.invalidate_insight_graph();

            // Store package metadata for the `packages` query.
            let pkg_info: Vec<PackageInfo> = loaded
                .iter()
                .map(|p| PackageInfo {
                    name: p.name.clone(),
                    publisher: p.publisher.clone(),
                    version: p.version.clone(),
                    object_count: p.objects.len(),
                })
                .collect();
            *workspace.package_info.write().unwrap_or_else(|e| e.into_inner()) = pkg_info; // SILENT: recover from RwLock poison

            // Scan workspace .al files.
            file_count = workspace.file_index.scan(&project.root);

            // Store project info.
            *workspace.project.write().await = Some(project);

            tracing::info!(
                symbols = workspace.symbols.len(),
                workspace_files = file_count,
                workspace_objects = workspace.file_index.objects.len(),
                "workspace: initialized"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, root = %project_root.display(), "workspace: project discovery failed");

            // Still scan for .al files even without a project.
            file_count = workspace.file_index.scan(project_root);
        }
    }

    // Discover toolchain.
    let has_toolchain = match crate::toolchain::find_toolchain() {
        Ok(tc) => {
            tracing::info!(version = %tc.version, "workspace: toolchain found");
            *workspace.toolchain.write().await = Some(tc);
            true
        }
        Err(e) => {
            tracing::info!(error = %e, "workspace: no toolchain (syntax-only mode)");
            false
        }
    };

    CoreInitResult { file_count, package_count, total_symbols, has_toolchain }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}
