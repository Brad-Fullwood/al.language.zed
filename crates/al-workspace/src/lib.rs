//! Workspace state container.
//!
//! Per-project documents, symbols, configuration, and semantic state.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod doctor;
mod semantic_lifecycle;
mod test_results;
pub use doctor::{doctor, DoctorReport, ProjectInfo, ToolchainInfo};
pub use semantic_lifecycle::{
    ensure_builtins_loaded, ensure_error_codes_loaded, get_or_init_bridge, restart_bridge,
    restart_bridge_if_current, set_builtins, shutdown_bridge,
};
pub use test_results::TestResultStore;

use al_project::project::AlProject;
use al_project::toolchain::AlToolchain;
use al_semantic::BuiltinType;
use al_symbols::SymbolIndex;
use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::RwLock;

use al_insight::graph::InsightGraph;
use al_insight::index::CallGraph;
use al_project::config::AlConfig;
use al_semantic::SemanticCache;
use al_source::documents::DocumentStore;
use al_source::file_index::FileIndex;

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

struct DependencySourceCache {
    /// `(canonical app path, byte length, modified nanos)` in stable order.
    fingerprint: Vec<(PathBuf, u64, u128)>,
    index: Arc<FileIndex>,
}

/// ## Lock strategy
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
    pub toolchain: RwLock<Option<AlToolchain>>,
    /// Discovered AL project (app.json manifest, packages).
    pub project: RwLock<Option<AlProject>>,
    /// .NET semantic bridge for CodeAnalysis features.
    pub semantic: RwLock<Option<al_semantic::SemanticBridge>>,
    /// Serializes lazy initialization and restart so concurrent requests cannot
    /// initialize multiple in-process CLR bridge generations.
    semantic_lifecycle_lock: tokio::sync::Mutex<()>,
    pub file_index: Arc<FileIndex>,
    /// Merged workspace configuration (settings from client + project defaults).
    pub config: RwLock<AlConfig>,
    /// Builtins loaded once at init, read-only afterward.
    pub builtins: std::sync::RwLock<Arc<Vec<BuiltinType>>>,
    /// Compiler error codes -- code -> description mapping for diagnostic enrichment.
    pub error_codes: DashMap<String, String>,
    /// Number of times the semantic bridge has been restarted (capped at MAX_RESTARTS).
    pub bridge_restart_count: std::sync::atomic::AtomicU32,
    /// One-shot guard for user-visible initialization failure notifications.
    pub semantic_init_failure_reported: std::sync::atomic::AtomicBool,
    pub package_info: std::sync::RwLock<Vec<PackageInfo>>,
    /// In-memory cache of builtin types indexed by name for O(1) lookups.
    pub semantic_cache: std::sync::RwLock<SemanticCache>,
    pub debug_session: tokio::sync::Mutex<Option<al_dap::native_debug::NativeDebugSession>>,
    /// Optional callback for user-visible notifications (bridge failures, etc.).
    ///
    /// Set by al-lsp after workspace construction. In the LSP path the closure
    /// calls `client.show_message`; in the daemon path it logs. Not set in tests.
    pub notify_sink: std::sync::OnceLock<NotifySink>,
    /// Cached insight graph, built lazily and invalidated when packages reload.
    /// Private — access via `get_or_build_insight_graph()` / `invalidate_insight_graph()`
    /// only, so the DCL build path and the `call_graph → insight_graph` lock-ordering
    /// invariant cannot be bypassed from outside the module.
    insight_graph: std::sync::RwLock<Option<Arc<InsightGraph>>>,
    /// Cached call graph. Built lazily after insight graph; invalidated with it.
    /// Private; access via `get_or_build_call_graph()` only.
    call_graph: std::sync::RwLock<Option<CallGraph>>,
    /// Serialises concurrent call-graph builds so two callers can't waste CPU
    /// running the (100-200ms) build twice. *Not* the data lock — readers and
    /// the build itself don't take this; only the slow-path build acquires it.
    /// Without this, the slow path would have to hold the `call_graph` write
    /// lock for the whole build, blocking every reader during initial warmup.
    call_graph_build_lock: std::sync::Mutex<()>,
    /// Parsed Microsoft/third-party object sources extracted from loaded `.app`
    /// packages. The fingerprint makes this cache independent from ordinary
    /// workspace-file graph invalidation while still rebuilding after package
    /// download/replacement.
    dependency_source_index: std::sync::RwLock<Option<DependencySourceCache>>,
    /// Active profiler session loaded from a `.alcpuprofile` file.
    ///
    /// When a profile is loaded the hints are stored here so that `code_lens`
    /// can add timing/hit-count lenses alongside the reference-count lenses.
    /// `None` means no profile is active.
    pub profiler_session: std::sync::RwLock<Option<al_types::ProfilerSession>>,
    /// Accumulated test results from the last (or current) test run.
    ///
    /// Uses `std::sync::RwLock` (not `tokio::sync::RwLock`) so sync query code
    /// can access it without `.await`. Wrapped in `Arc` so multiple async
    /// tasks (daemon dispatchers, code-lens queries) can share the underlying
    /// store cheaply without cloning records.
    pub test_results: std::sync::RwLock<Option<std::sync::Arc<crate::TestResultStore>>>,
    /// Set of file paths that received `al-compiler` diagnostics in the
    /// most recent `al.compile` run. Used by the LSP `al.compile` handler
    /// to clear stale diagnostics: any file in this set absent from the
    /// new compile result needs an empty (or syntax-only) republish so
    /// the editor squiggles disappear after a clean rebuild.
    pub last_compile_affected: tokio::sync::Mutex<std::collections::HashSet<String>>,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            documents: DocumentStore::new(),
            symbols: Arc::new(SymbolIndex::new()),
            toolchain: RwLock::new(None),
            project: RwLock::new(None),
            semantic: RwLock::new(None),
            semantic_lifecycle_lock: tokio::sync::Mutex::new(()),
            file_index: Arc::new(FileIndex::new()),
            config: RwLock::new(AlConfig::default()),
            builtins: std::sync::RwLock::new(Arc::new(Vec::new())),
            error_codes: DashMap::new(),
            bridge_restart_count: std::sync::atomic::AtomicU32::new(0),
            semantic_init_failure_reported: std::sync::atomic::AtomicBool::new(false),
            package_info: std::sync::RwLock::new(Vec::new()),
            semantic_cache: std::sync::RwLock::new(SemanticCache::new()),
            debug_session: tokio::sync::Mutex::new(None),
            notify_sink: std::sync::OnceLock::new(),
            insight_graph: std::sync::RwLock::new(None),
            call_graph: std::sync::RwLock::new(None),
            call_graph_build_lock: std::sync::Mutex::new(()),
            dependency_source_index: std::sync::RwLock::new(None),
            profiler_session: std::sync::RwLock::new(None),
            test_results: std::sync::RwLock::new(None),
            last_compile_affected: tokio::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Get or lazily build the cached insight graph.
    pub fn get_or_build_insight_graph(&self) -> Arc<InsightGraph> {
        if let Ok(guard) = self.insight_graph.read() {
            if let Some(arc) = guard.as_ref() {
                return Arc::clone(arc);
            }
        }
        let mut guard = self
            .insight_graph
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(arc) = guard.as_ref() {
            return Arc::clone(arc);
        }
        let build = || {
            let mut g = InsightGraph::new();
            g.build_from_index(&self.symbols);
            g
        };
        let graph = match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(build)
            }
            _ => build(),
        };
        let arc = Arc::new(graph);
        *guard = Some(Arc::clone(&arc));
        arc
    }

    /// Invalidate both graph caches.
    pub fn invalidate_insight_graph(&self) {
        let mut ig = self
            .insight_graph
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *ig = None;
        let mut cg = self.call_graph.write().unwrap_or_else(|e| e.into_inner());
        *cg = None;
    }

    /// Invalidate the call graph while retaining the insight graph.
    pub fn invalidate_call_graph_only(&self) {
        let mut cg = self.call_graph.write().unwrap_or_else(|e| e.into_inner());
        *cg = None;
    }

    /// Return a coherent parsed index of every AL object body embedded in the
    /// currently loaded Microsoft/third-party packages.
    ///
    /// Package source is immutable during normal editing, so it is cached
    /// separately from the workspace graph. A stable path/size/mtime
    /// fingerprint forces a rebuild when a package is downloaded or replaced.
    pub fn get_or_build_dependency_source_index(&self) -> Arc<FileIndex> {
        let fingerprint = self.dependency_package_fingerprint();
        if let Ok(cache) = self.dependency_source_index.read() {
            if let Some(cache) = cache
                .as_ref()
                .filter(|cache| cache.fingerprint == fingerprint)
            {
                return Arc::clone(&cache.index);
            }
        }

        let mut cache = self
            .dependency_source_index
            .write()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = cache
            .as_ref()
            .filter(|existing| existing.fingerprint == fingerprint)
        {
            return Arc::clone(&existing.index);
        }

        let index = Arc::new(FileIndex::new());
        for (app_path, _, _) in &fingerprint {
            let source_index = match al_symbols::source_index::get_or_build(app_path) {
                Ok(index) => index,
                Err(error) => {
                    tracing::debug!(
                        path = %app_path.display(),
                        %error,
                        "loaded package has no readable embedded AL source"
                    );
                    continue;
                }
            };
            let sources = match source_index.extract_all_sources() {
                Ok(sources) => sources,
                Err(error) => {
                    tracing::warn!(
                        path = %app_path.display(),
                        %error,
                        "failed to extract dependency AL sources"
                    );
                    continue;
                }
            };
            for (archive_path, source) in sources {
                index.add_file(dependency_virtual_path(app_path, &archive_path), source);
            }
        }
        tracing::info!(
            packages = fingerprint.len(),
            source_files = index.len(),
            "dependency AL source index ready"
        );
        let index_for_return = Arc::clone(&index);
        *cache = Some(DependencySourceCache { fingerprint, index });
        index_for_return
    }

    fn dependency_package_fingerprint(&self) -> Vec<(PathBuf, u64, u128)> {
        let mut package_names: std::collections::HashSet<String> = self
            .symbols
            .all_entries()
            .into_iter()
            .filter(|entry| !entry.synthetic && !entry.package.eq_ignore_ascii_case("workspace"))
            .map(|entry| entry.package.clone())
            .collect();
        let mut fingerprint = Vec::new();
        for package in package_names.drain() {
            let Some(path) = self.symbols.app_path(&package) else {
                continue;
            };
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            fingerprint.push((path, metadata.len(), modified));
        }
        fingerprint.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        fingerprint.dedup_by(|left, right| left.0 == right.0);
        fingerprint
    }

    /// Get (or lazily build) the cached CallGraph.
    ///
    /// **Lock ordering invariant:** This function takes locks in the order
    /// `call_graph_build_lock` → `dependency_source_index` → `insight_graph`
    /// (write, brief) → `call_graph` (write, brief). The expensive build itself
    /// runs without either graph data lock, so readers are not blocked. Any new
    /// code that touches these locks MUST follow that ordering or the daemon can
    /// deadlock.
    pub fn get_or_build_call_graph(
        &self,
    ) -> (
        Arc<InsightGraph>,
        std::sync::RwLockReadGuard<'_, Option<CallGraph>>,
    ) {
        {
            let cg_guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
            if cg_guard.is_some() {
                let insight = self.get_or_build_insight_graph();
                return (insight, cg_guard);
            }
        }

        let _build_lock = self
            .call_graph_build_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        {
            let cg_guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
            if cg_guard.is_some() {
                drop(cg_guard);
                let insight = self.get_or_build_insight_graph();
                let guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
                return (insight, guard);
            }
        }

        let build = || {
            let dependency_sources = self.get_or_build_dependency_source_index();
            let mut graph = InsightGraph::new();
            graph.build_from_index(&self.symbols);
            al_insight::calls::register_workspace_nodes(
                &self.file_index,
                &self.symbols,
                &mut graph,
            );
            al_insight::calls::register_dependency_source_nodes(&dependency_sources, &mut graph);
            let insight = Arc::new(graph);

            let mut cg = CallGraph::build_from_insight(&insight);
            al_insight::calls::populate_workspace_call_edges(
                &self.file_index,
                &self.symbols,
                &insight,
                &mut cg,
            );
            al_insight::calls::populate_workspace_call_edges(
                &dependency_sources,
                &self.symbols,
                &insight,
                &mut cg,
            );
            (insight, cg)
        };
        let (insight, cg) = match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(build)
            }
            _ => build(),
        };

        {
            let mut ig_guard = self
                .insight_graph
                .write()
                .unwrap_or_else(|e| e.into_inner());
            *ig_guard = Some(Arc::clone(&insight));
        }
        {
            let mut cg_guard = self.call_graph.write().unwrap_or_else(|e| e.into_inner());
            *cg_guard = Some(cg);
        }

        let guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
        (insight, guard)
    }

    pub fn memory_stats(&self) -> WorkspaceMemoryStats {
        let symbol_count = self.symbols.len();
        let open_docs = self.documents.len();
        let workspace_files = self.file_index.files.len();
        let procedure_index_entries = self.file_index.procedures.len();
        let error_code_count = self.error_codes.len();
        let builtin_count = self.builtins.read().map(|b| b.len()).unwrap_or(0);
        let package_count = self.package_info.read().map(|p| p.len()).unwrap_or(0);

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

fn dependency_virtual_path(app_path: &Path, archive_path: &str) -> PathBuf {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    app_path.hash(&mut hasher);
    let mut path = PathBuf::from("/__al_dependency_sources__");
    path.push(format!("{:016x}", hasher.finish()));
    let component_count_before = path.components().count();
    for component in Path::new(archive_path).components() {
        if let std::path::Component::Normal(component) = component {
            path.push(component);
        }
    }
    if path.components().count() == component_count_before {
        path.push("source.al");
    }
    path
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
    pub file_count: usize,
    pub package_count: usize,
    pub total_symbols: usize,
    pub has_toolchain: bool,
}

/// Initialize the common parts of a workspace.
///
/// Shared by the LSP and daemon initialization paths. Discovers the project and
/// toolchain, loads symbols, and indexes workspace files. The caller remains
/// responsible for transport-specific notifications and document setup.
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

    // Bridge sync filesystem walks (find_project + file_index.scan) onto the
    // Tokio blocking pool so they don't stall the worker for the hundreds of
    // stat()/read_dir() calls a real BC workspace performs. block_in_place
    // is used because we hold a `&Workspace` borrow that cannot be moved into
    // spawn_blocking. The al-lsp runtime is multi-threaded.
    let project_result =
        tokio::task::block_in_place(|| al_project::project::find_project(project_root));
    match project_result {
        Ok(mut project) => {
            let config = workspace.config.read().await.clone();
            project.apply_symbol_settings(&config);
            tracing::info!(
                name = %project.app_json.name,
                root = %project.root.display(),
                packages = project.packages.len(),
                "workspace: project discovered"
            );

            let cache = al_symbols::cache::SymbolCache::default_location();
            let loaded = workspace
                .symbols
                .load_packages_cached(&project.packages, &cache);
            total_symbols = loaded.iter().map(|p| p.object_count).sum();
            package_count = loaded.len();
            tracing::info!(
                packages = package_count,
                symbols = total_symbols,
                "workspace: loaded symbol packages"
            );
            let attempted = project.packages.len();
            if package_count < attempted {
                let missing = attempted - package_count;
                tracing::error!(
                    attempted,
                    loaded = package_count,
                    missing,
                    "workspace: {missing}/{attempted} .app packages failed to load — check earlier warnings for paths"
                );
                if let Some(sink) = workspace.notify_sink.get() {
                    sink(&format!(
                        "AL workspace: {missing}/{attempted} symbol packages failed to load. Symbol index is partial; some completions and references may be missing. See al-lsp log for details."
                    ));
                }
            }

            // Load runtime enum definitions (compiler built-ins not in any package).
            workspace.symbols.load_runtime_enums();

            workspace.invalidate_insight_graph();

            let pkg_info: Vec<PackageInfo> = loaded
                .iter()
                .map(|p| PackageInfo {
                    name: p.name.clone(),
                    publisher: p.publisher.clone(),
                    version: p.version.clone(),
                    object_count: p.object_count,
                })
                .collect();
            *workspace
                .package_info
                .write()
                .unwrap_or_else(|e| e.into_inner()) = pkg_info;

            // Scan workspace .al files. file_index.scan walks the tree
            // synchronously (read_dir + read_to_string per file) so wrap
            // it in block_in_place — we still hold the &Workspace borrow.
            file_count = tokio::task::block_in_place(|| workspace.file_index.scan(&project.root));

            *workspace.project.write().await = Some(project);

            tracing::info!(
                symbols = workspace.symbols.len(),
                workspace_files = file_count,
                workspace_objects = workspace.file_index.object_count(),
                "workspace: initialized"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, root = %project_root.display(), "workspace: project discovery failed");

            // Still scan for .al files even without a project. Same
            // block_in_place bridge as the happy path above.
            file_count = tokio::task::block_in_place(|| workspace.file_index.scan(project_root));
        }
    }

    let has_toolchain = match al_project::toolchain::find_toolchain() {
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

    CoreInitResult {
        file_count,
        package_count,
        total_symbols,
        has_toolchain,
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

/// Update workspace index and document cache when a file is opened or changed.
///
/// Parses the text exactly once, warms the document cache with the resulting
/// tree, then updates the file index via [`FileIndex::add_file_with_tree`] —
/// avoiding the double-parse that occurred when `al-lsp` called `parse_quick`
/// followed by `file_index.add_file` (which also called `parse_quick` internally).
///
/// If `uri` is not a `file://` URI, the full composed symbol cache is invalidated
/// as a safe fallback.
pub fn on_document_change(workspace: &Workspace, uri: &url::Url, text: &str) {
    let result = al_syntax::AlParser::parse_quick(text);

    let version = workspace.documents.get_version(uri).unwrap_or(0);
    workspace
        .documents
        .cache_tree(uri, version, result.tree.clone());

    // Capture procedure names before re-indexing to distinguish topology
    // changes from body-only edits.
    let topology_change = if let Ok(path) = uri.to_file_path() {
        let prev_procs: std::collections::HashSet<String> = workspace
            .file_index
            .procedures_snapshot(&path)
            .into_iter()
            .collect();

        workspace
            .file_index
            .add_file_with_tree(path.clone(), text.to_string(), result.tree);

        if let Some(info) = workspace.file_index.object_info.get(&path) {
            workspace.symbols.invalidate_composed(&info.name);
        } else {
            workspace.symbols.invalidate_all_composed();
        }

        // Compare new procedure set to the snapshot — same set ⇒ no topology
        // change ⇒ insight_graph stays valid.
        let new_procs: std::collections::HashSet<String> = workspace
            .file_index
            .procedures_snapshot(&path)
            .into_iter()
            .collect();
        prev_procs != new_procs
    } else {
        workspace.symbols.invalidate_all_composed();
        // Non-file URI (virtual buffer etc.) — conservative: full invalidate.
        true
    };

    if topology_change {
        workspace.invalidate_insight_graph();
    } else {
        // Body-only edit — call edges may have shifted, but the insight
        // graph's node topology is still valid. Drop only call_graph so
        // the next cross-file query pays the (~smaller) call-graph rebuild
        // instead of the full insight + call build.
        workspace.invalidate_call_graph_only();
    }
}

/// Invalidate the composed symbol cache when a file is closed.
///
/// Only handles symbol cache invalidation — the decision about whether to remove
/// the file from the file index (based on `diagnostics_scope`) is left to the
/// caller (`al-lsp`), which has access to config and transport concerns.
pub fn on_document_close(workspace: &Workspace, uri: &url::Url) {
    if let Ok(path) = uri.to_file_path() {
        if let Some(info) = workspace.file_index.object_info.get(&path) {
            workspace.symbols.invalidate_composed(&info.name);
        } else {
            workspace.symbols.invalidate_all_composed();
        }
    } else {
        workspace.symbols.invalidate_all_composed();
    }
    // Same reason as on_document_change: file leaving the index changes
    // the call/reference graph topology.
    workspace.invalidate_insight_graph();
}

#[cfg(test)]
mod workspace_lifecycle_tests {
    use super::*;
    use url::Url;

    fn make_workspace() -> Workspace {
        Workspace::new()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn insight_graph_dcl_returns_same_arc_under_concurrency() {
        let workspace = std::sync::Arc::new(make_workspace());
        let mut handles = Vec::with_capacity(8);
        for _ in 0..8 {
            let ws = std::sync::Arc::clone(&workspace);
            handles.push(tokio::spawn(async move {
                tokio::task::spawn_blocking(move || ws.get_or_build_insight_graph())
                    .await
                    .unwrap()
            }));
        }
        let mut graphs = Vec::new();
        for h in handles {
            graphs.push(h.await.unwrap());
        }
        let first = graphs.remove(0);
        for g in &graphs {
            assert!(
                std::sync::Arc::ptr_eq(&first, g),
                "DCL invariant: all concurrent callers must observe the same Arc<InsightGraph>"
            );
        }
    }

    #[test]
    fn insight_graph_invalidation_yields_new_arc() {
        let workspace = make_workspace();
        let first = workspace.get_or_build_insight_graph();
        workspace.invalidate_insight_graph();
        let second = workspace.get_or_build_insight_graph();
        assert!(
            !std::sync::Arc::ptr_eq(&first, &second),
            "invalidation should drop the cached graph; next call must rebuild a new Arc"
        );
    }

    #[test]
    fn on_document_change_populates_cache_and_index() {
        let workspace = make_workspace();
        let uri = Url::from_file_path("/tmp/test_on_doc_change/TestTable.al").unwrap();
        let text = r#"table 50100 "Test Table" {
    fields {
        field(1; "No."; Code[20]) { }
    }
}"#;

        // Open the document first so get_version works.
        workspace.documents.open(uri.clone(), text.to_string());

        on_document_change(&workspace, &uri, text);

        let path = uri.to_file_path().unwrap();
        assert!(
            workspace.file_index.files.contains_key(&path),
            "file_index.files must contain the file after on_document_change"
        );
        assert!(
            workspace.file_index.object_info.contains_key(&path),
            "file_index.object_info must be populated after on_document_change"
        );
        let info = workspace.file_index.object_info.get(&path).unwrap();
        assert_eq!(info.name.to_lowercase(), "test table");
    }

    #[test]
    fn on_document_change_non_file_uri_does_not_panic() {
        let workspace = make_workspace();
        let uri = Url::parse("http://example.com/Untitled-1.al").unwrap();
        let text = r#"codeunit 50200 "Test" { }"#;

        on_document_change(&workspace, &uri, text);
        assert!(workspace.file_index.is_empty());
    }

    #[tokio::test]
    async fn last_compile_affected_starts_empty() {
        let workspace = make_workspace();
        let guard = workspace.last_compile_affected.lock().await;
        assert!(
            guard.is_empty(),
            "fresh workspace must have no compile-affected files"
        );
    }

    #[test]
    fn stale_is_set_difference_previous_minus_current() {
        let previous: std::collections::HashSet<String> = ["/p/A.al", "/p/B.al", "/p/C.al"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let current: std::collections::HashSet<String> = ["/p/A.al", "/p/D.al"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let mut stale: Vec<String> = previous.difference(&current).cloned().collect();
        stale.sort();
        assert_eq!(stale, vec!["/p/B.al".to_string(), "/p/C.al".to_string()]);
    }

    #[test]
    fn no_stale_when_compile_set_unchanged() {
        let previous: std::collections::HashSet<String> = ["/p/A.al", "/p/B.al"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let current = previous.clone();
        let stale: Vec<String> = previous.difference(&current).cloned().collect();
        assert!(stale.is_empty(), "no diff means no stale clears");
    }

    /// Per-test unique temp directory (mirrors project.rs::tempdir()), so
    /// filesystem-touching tests don't collide across the test binary.
    fn unique_tempdir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "al-core-workspace-test-{}-{}-{}",
            tag,
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build_test_app(name: &str, table_name: &str) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let manifest = format!(
            r#"<?xml version="1.0"?><Package><App Id="00000000-0000-0000-0000-000000000001" Name="{name}" Publisher="Test" Version="1.0.0.0" /></Package>"#
        );
        let symbols = format!(
            r#"{{"Tables":[{{"Id":50123,"Name":"{table_name}","Fields":[],"Methods":[]}}]}}"#
        );
        let mut data = Vec::from(&b"NAVX"[..]);
        data.resize(40, 0);
        let mut zip_data = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(Cursor::new(&mut zip_data));
            let options = SimpleFileOptions::default();
            zip.start_file("NavxManifest.xml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("SymbolReference.json", options).unwrap();
            zip.write_all(symbols.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_data);
        data
    }

    /// memory_stats reflects the *actual* live state of the workspace, not
    /// constant zeros. After indexing one .al file the workspace_files count
    /// and procedure_index_entries must rise above the empty baseline.
    #[test]
    fn memory_stats_reflects_indexed_file() {
        let workspace = make_workspace();

        let empty = workspace.memory_stats();
        assert_eq!(empty.workspace_files, 0, "fresh workspace has no files");
        assert_eq!(empty.open_docs, 0, "fresh workspace has no open docs");

        let dir = unique_tempdir("memstats");
        let file = dir.join("MyCodeunit.al");
        let uri = Url::from_file_path(&file).unwrap();
        let text = r#"codeunit 50300 "Mem Stats CU"
{
    procedure DoThing()
    begin
    end;

    procedure DoOther()
    begin
    end;
}"#;
        workspace.documents.open(uri.clone(), text.to_string());
        on_document_change(&workspace, &uri, text);

        let after = workspace.memory_stats();
        assert_eq!(
            after.workspace_files, 1,
            "indexing one file must report exactly one workspace file"
        );
        assert_eq!(
            after.open_docs, 1,
            "one opened document must be counted in open_docs"
        );
        assert!(
            after.procedure_index_entries >= 2,
            "both procedures must be indexed (got {})",
            after.procedure_index_entries
        );
    }

    /// memory_stats counts loaded package metadata. Writing a PackageInfo into
    /// the workspace's package_info store must be visible via memory_stats.
    #[test]
    fn memory_stats_counts_package_info_and_error_codes() {
        let workspace = make_workspace();
        assert_eq!(workspace.memory_stats().package_count, 0);
        assert_eq!(workspace.memory_stats().error_code_count, 0);

        workspace.package_info.write().unwrap().push(PackageInfo {
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "1.0.0.0".to_string(),
            object_count: 42,
        });
        workspace
            .error_codes
            .insert("AL0118".to_string(), "Unknown identifier".to_string());

        let stats = workspace.memory_stats();
        assert_eq!(stats.package_count, 1, "one package must be counted");
        assert_eq!(stats.error_code_count, 1, "one error code must be counted");
    }

    /// on_document_close with a real file URI invalidates the composed cache
    /// for that file's object and invalidates the insight graph (topology can
    /// change when a file leaves the working set). The cached insight graph
    /// must be dropped so the next access rebuilds a fresh Arc.
    #[test]
    fn on_document_close_file_uri_invalidates_insight_graph() {
        let workspace = make_workspace();
        let dir = unique_tempdir("close");
        let file = dir.join("CloseTable.al");
        let uri = Url::from_file_path(&file).unwrap();
        let text = r#"table 50400 "Close Table"
{
    fields
    {
        field(1; "No."; Code[20]) { }
    }
}"#;
        workspace.documents.open(uri.clone(), text.to_string());
        on_document_change(&workspace, &uri, text);

        // Object info must exist so the file-URI branch (not the fallback)
        // is exercised.
        let path = uri.to_file_path().unwrap();
        assert!(
            workspace.file_index.object_info.contains_key(&path),
            "precondition: object_info populated"
        );

        let before = workspace.get_or_build_insight_graph();
        on_document_close(&workspace, &uri);
        let after = workspace.get_or_build_insight_graph();
        assert!(
            !std::sync::Arc::ptr_eq(&before, &after),
            "on_document_close must invalidate the cached insight graph"
        );
    }

    #[test]
    fn on_document_close_non_file_uri_does_not_panic() {
        let workspace = make_workspace();
        let uri = Url::parse("http://example.com/Untitled-1.al").unwrap();
        on_document_close(&workspace, &uri);
        assert!(workspace.file_index.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invalidate_call_graph_only_keeps_insight_graph() {
        let workspace = make_workspace();

        // Build both graphs. get_or_build_call_graph builds an *enriched*
        // insight graph and stores it, so capture the insight Arc it leaves
        // cached (NOT an earlier symbol-only one).
        let insight_before = {
            let (ig, cg) = workspace.get_or_build_call_graph();
            assert!(cg.is_some(), "call graph must be built");
            ig
        };

        workspace.invalidate_call_graph_only();

        let insight_after = workspace.get_or_build_insight_graph();
        assert!(
            std::sync::Arc::ptr_eq(&insight_before, &insight_after),
            "invalidate_call_graph_only must NOT drop the insight graph"
        );

        let (_ig2, cg2) = workspace.get_or_build_call_graph();
        assert!(cg2.is_some(), "call graph must rebuild after invalidation");
    }

    /// initialize_core_workspace on a directory with no app.json takes the
    /// project-discovery-failure branch: it still scans for .al files and
    /// returns a CoreInitResult with zero packages.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_no_project_scans_files() {
        let workspace = make_workspace();
        let dir = unique_tempdir("noproject");
        // A loose .al file but NO app.json anywhere up the tree.
        std::fs::write(dir.join("Loose.al"), r#"codeunit 50500 "Loose" { }"#).unwrap();

        let result = initialize_core_workspace(&workspace, &dir).await;

        assert_eq!(result.package_count, 0, "no app.json => no packages loaded");
        assert_eq!(
            result.total_symbols, 0,
            "no packages => zero package symbols"
        );
        assert_eq!(
            result.file_count, 1,
            "the loose .al file must still be scanned without a project"
        );
        assert!(
            workspace.project.read().await.is_none(),
            "no project must be stored when discovery fails"
        );
    }

    /// initialize_core_workspace with a valid app.json takes the happy path:
    /// the project is discovered and stored, files are scanned, and package
    /// metadata is recorded. (No .alpackages => zero packages, which is fine.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_with_project_stores_project() {
        let workspace = make_workspace();
        let dir = unique_tempdir("withproject");
        std::fs::write(
            dir.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "InitTest",
                "publisher": "Tester",
                "version": "1.0.0.0"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.join("Obj.al"), r#"codeunit 50600 "Init Obj" { }"#).unwrap();

        let result = initialize_core_workspace(&workspace, &dir).await;

        assert_eq!(
            result.file_count, 1,
            "the one .al file must be scanned under the discovered project"
        );
        let stored = workspace.project.read().await;
        let project = stored
            .as_ref()
            .expect("project must be stored on the happy path");
        assert_eq!(
            project.app_json.name, "InitTest",
            "the discovered project's manifest name must be stored"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_loads_configured_local_package_folder() {
        let workspace = make_workspace();
        let dir = unique_tempdir("localpackages");
        let local = dir.join("shared-symbols");
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(
            dir.join("app.json"),
            serde_json::json!({
                "id": "00000000-0000-0000-0000-000000000099",
                "name": "InitTest",
                "publisher": "Tester",
                "version": "1.0.0.0"
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            local.join("Shared.app"),
            build_test_app("Shared", "Shared Local Table"),
        )
        .unwrap();
        workspace.config.write().await.app_local_folder_paths =
            vec![std::path::PathBuf::from("shared-symbols")];

        let result = initialize_core_workspace(&workspace, &dir).await;

        assert_eq!(result.package_count, 1);
        assert_eq!(workspace.symbols.get_by_name("Shared Local Table").len(), 1);
        let stored = workspace.project.read().await;
        assert!(stored
            .as_ref()
            .unwrap()
            .packages
            .iter()
            .any(|path| path.starts_with(&local)));
    }
}
