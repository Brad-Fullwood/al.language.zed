//! Workspace state container.
//!
//! The `Workspace` struct owns all per-project state: documents, symbols,
//! semantic bridge, and configuration. It is the central coordination point
//! for all queries routed through al-core.

use std::sync::Arc;

use crate::project::AlProject;
use crate::semantic::BuiltinType;
use crate::symbols::SymbolIndex;
use crate::toolchain::AlToolchain;
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
    pub semantic: RwLock<Option<crate::semantic::SemanticBridge>>,
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
    /// Private — access via `get_or_build_insight_graph()` / `invalidate_insight_graph()`
    /// only, so the DCL build path and the `call_graph → insight_graph` lock-ordering
    /// invariant cannot be bypassed from outside the module (T005).
    insight_graph: std::sync::RwLock<Option<Arc<InsightGraph>>>,
    /// Cached call graph. Built lazily after insight graph; invalidated with it.
    /// Private — access via `get_or_build_call_graph()` only (T005).
    call_graph: std::sync::RwLock<Option<CallGraph>>,
    /// Serialises concurrent call-graph builds so two callers can't waste CPU
    /// running the (100-200ms) build twice. *Not* the data lock — readers and
    /// the build itself don't take this; only the slow-path build acquires it.
    /// Without this, the slow path would have to hold the `call_graph` write
    /// lock for the whole build, blocking every reader during initial warmup.
    call_graph_build_lock: std::sync::Mutex<()>,
    /// Active profiler session loaded from a `.alcpuprofile` file.
    ///
    /// When a profile is loaded the hints are stored here so that `code_lens`
    /// can add timing/hit-count lenses alongside the reference-count lenses.
    /// `None` means no profile is active.
    pub profiler_session:
        std::sync::RwLock<Option<crate::queries::profiler_hints::ProfilerSession>>,
    /// Accumulated test results from the last (or current) test run.
    ///
    /// Uses `std::sync::RwLock` (not `tokio::sync::RwLock`) so sync query code
    /// can access it without `.await`. Wrapped in `Arc` so multiple async
    /// tasks (daemon dispatchers, code-lens queries) can share the underlying
    /// store cheaply without cloning records.
    pub test_results:
        std::sync::RwLock<Option<std::sync::Arc<crate::test_engine::TestResultStore>>>,
    /// Set of file paths that received `al-compiler` diagnostics in the
    /// most recent `al.compile` run. Used by the LSP `al.compile` handler
    /// to clear stale diagnostics: any file in this set absent from the
    /// new compile result needs an empty (or syntax-only) republish so
    /// the editor squiggles disappear after a clean rebuild. F-008.
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
            call_graph_build_lock: std::sync::Mutex::new(()),
            profiler_session: std::sync::RwLock::new(None),
            test_results: std::sync::RwLock::new(None),
            last_compile_affected: tokio::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Get (or lazily build) the cached InsightGraph (ISSUE-132 fix).
    ///
    /// The graph is built once from the current symbol index and cached.
    /// Call `invalidate_insight_graph()` after reloading packages.
    ///
    /// The expensive `build_from_index` runs WITHOUT any lock held — holding
    /// a `std::sync::RwLock` write guard across the build would park the
    /// tokio executor thread for the duration when this is called from an
    /// async query path. We tolerate the (rare) duplicate-build race in
    /// exchange for not blocking the executor: the second-arriving thread
    /// returns the first thread's stored graph.
    ///
    /// Cold-cache cost can reach 50–200 ms on large BC workspaces. The build
    /// is therefore wrapped in `tokio::task::block_in_place` when called from
    /// inside a multi-threaded Tokio runtime so the worker thread is yielded
    /// to the blocking pool for the duration. Outside a Tokio runtime (tests,
    /// daemon utilities) `try_handle()` returns `Err` and we fall back to a
    /// direct call.
    pub fn get_or_build_insight_graph(&self) -> Arc<InsightGraph> {
        // Fast path: read lock.
        if let Ok(guard) = self.insight_graph.read() {
            if let Some(arc) = guard.as_ref() {
                return Arc::clone(arc);
            }
        }
        // Slow path: acquire the write lock, then re-check (double-checked
        // locking). Without DCL, N concurrent first-callers each independently
        // build a 50–200 ms graph and discard all but the first — wasted CPU
        // on cold cache. The build runs INSIDE this write lock because the
        // graph build is short enough (≤200 ms) that holding the data lock
        // is cheaper than introducing a separate build-coordination mutex.
        // `get_or_build_call_graph` uses the build-coordination-mutex pattern
        // because its build is longer and includes this build internally.
        // Recover from a poisoned lock via `into_inner` so a panic inside an
        // earlier build does not permanently freeze the cache.
        let mut guard = self
            .insight_graph
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(arc) = guard.as_ref() {
            return Arc::clone(arc);
        }
        // Build inside the write lock. block_in_place yields the worker to
        // the blocking pool when we are inside a multi-threaded Tokio
        // runtime; otherwise the closure runs inline (block_in_place panics
        // in current_thread mode, so we guard with try_handle and runtime
        // flavor detection).
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

    /// Invalidate the cached InsightGraph (call when packages reload).
    ///
    /// Recovers from poisoned locks via `into_inner` so a panic during a build
    /// does not permanently disable invalidation — without recovery the
    /// poisoned guard would silently skip `*guard = None` and leave a stale
    /// graph cached forever.
    pub fn invalidate_insight_graph(&self) {
        let mut ig = self
            .insight_graph
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *ig = None;
        let mut cg = self.call_graph.write().unwrap_or_else(|e| e.into_inner());
        *cg = None;
    }

    /// Invalidate ONLY the call_graph, leaving the insight_graph intact.
    /// Used by `on_document_change` when the edit is body-only (procedure
    /// signatures unchanged) — the insight graph's node topology stays
    /// valid; only call-edge edges might have shifted. F-OPEN-066.
    pub fn invalidate_call_graph_only(&self) {
        let mut cg = self.call_graph.write().unwrap_or_else(|e| e.into_inner());
        *cg = None;
    }

    /// Get (or lazily build) the cached CallGraph.
    ///
    /// Builds a workspace-enriched InsightGraph (symbol index + workspace file
    /// objects/procedures/subscribers), then builds the CallGraph and populates
    /// Tier 1 call edges. The enriched InsightGraph replaces the cached one.
    ///
    /// Uses double-checked locking to prevent the TOCTOU race where two concurrent
    /// callers both see `None` and both build the graph. The write lock is acquired
    /// before building, and re-checked inside the lock so at most one build runs.
    ///
    /// **Lock ordering invariant:** This function takes locks in the order
    /// `call_graph_build_lock` → `insight_graph` (write, brief) → `call_graph`
    /// (write, brief). The expensive build itself runs while holding ONLY
    /// the build coordination mutex — no readers are blocked during the
    /// 100-200ms build pass. Any new code that touches the data locks MUST
    /// follow that ordering or the daemon can deadlock.
    pub fn get_or_build_call_graph(
        &self,
    ) -> (
        Arc<InsightGraph>,
        std::sync::RwLockReadGuard<'_, Option<CallGraph>>,
    ) {
        // Fast path: call graph already exists — return it under a read lock.
        {
            let cg_guard = self.call_graph.read().unwrap_or_else(|e| e.into_inner());
            if cg_guard.is_some() {
                let insight = self.get_or_build_insight_graph();
                return (insight, cg_guard);
            }
        }

        // Slow path: serialise concurrent builds on a *separate* mutex —
        // the data lock stays available to readers for the whole build.
        // Double-checked locking guards against the case where another
        // caller built the graph while we waited for the build mutex.
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

        // Build enriched InsightGraph + CallGraph. This is a CPU-intensive
        // pass over every workspace file and the symbol index; on a daemon
        // running in tokio's multi-threaded scheduler we MUST yield the
        // worker via block_in_place so other handlers (diagnostics, hover,
        // hover-followups) keep responding while the build runs. Guarded
        // by try_handle + flavor check because block_in_place panics on
        // current_thread runtimes (e.g. CLI tests).
        //
        // Important: no read/write guard on `call_graph` or `insight_graph`
        // is held across this build. Readers see the previous (or symbol-
        // only) graph until the swap below.
        let build = || {
            let mut graph = InsightGraph::new();
            graph.build_from_index(&self.symbols);
            // Register workspace objects, procedures, events, and subscribers
            crate::insight::calls::register_workspace_nodes(
                &self.file_index,
                &self.symbols,
                &mut graph,
            );
            let insight = Arc::new(graph);

            let mut cg = CallGraph::build_from_insight(&insight);
            crate::insight::calls::populate_workspace_call_edges(
                &self.file_index,
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

        // Atomic-ish swap: take each write lock just long enough to store the
        // built artefact, then release before re-acquiring the call_graph
        // read guard the function returns. Recover from poison so a panic
        // earlier in the daemon's lifetime can't silently discard the build.
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

    // Bridge sync filesystem walks (find_project + file_index.scan) onto the
    // Tokio blocking pool so they don't stall the worker for the hundreds of
    // stat()/read_dir() calls a real BC workspace performs. block_in_place
    // is used because we hold a `&Workspace` borrow that cannot be moved into
    // spawn_blocking. The al-lsp runtime is multi-threaded.
    let project_result = tokio::task::block_in_place(|| crate::project::find_project(project_root));
    match project_result {
        Ok(project) => {
            tracing::info!(
                name = %project.app_json.name,
                root = %project.root.display(),
                packages = project.packages.len(),
                "workspace: project discovered"
            );

            let cache = crate::symbols::cache::SymbolCache::default_location();
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
            // F-OPEN-071: when one or more `.app` packages fail to load, the
            // symbol count is silently lower and the user sees no surface
            // beyond a daemon-log warn. Notify via the workspace's sink so
            // editor clients (LSP) and CLI tooling can report partial state.
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
                .unwrap_or_else(|e| e.into_inner()) = pkg_info; // SILENT: recover from RwLock poison

            // Scan workspace .al files. file_index.scan walks the tree
            // synchronously (read_dir + read_to_string per file) so wrap
            // it in block_in_place — we still hold the &Workspace borrow.
            file_count = tokio::task::block_in_place(|| workspace.file_index.scan(&project.root));

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

            // Still scan for .al files even without a project. Same
            // block_in_place bridge as the happy path above.
            file_count = tokio::task::block_in_place(|| workspace.file_index.scan(project_root));
        }
    }

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
    let result = crate::syntax::AlParser::parse_quick(text);

    let version = workspace.documents.get_version(uri).unwrap_or(0);
    workspace
        .documents
        .cache_tree(uri, version, result.tree.clone());

    // F-OPEN-066: capture the procedure-name set BEFORE re-indexing so we
    // can tell whether this edit changed graph topology (added/removed/
    // renamed a procedure) or was body-only. Body-only edits invalidate
    // just the call_graph; topology changes invalidate the full graph.
    let topology_change = if let Ok(path) = uri.to_file_path() {
        let prev_procs: std::collections::HashSet<String> = workspace
            .file_index
            .procedures_snapshot(&path)
            .into_iter()
            .collect();

        workspace
            .file_index
            .add_file_with_tree(path.clone(), text.to_string(), result.tree);

        // Invalidate only the composed view for the object in this file.
        // add_file_with_tree already updated object_info, so we can read the name immediately.
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
        // Procedure/event/subscriber set changed — both graphs need rebuild.
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

    /// T005 regression: get_or_build_insight_graph must collapse concurrent
    /// first-callers onto a single Arc. Without DCL, N callers each ran the
    /// 50–200 ms build independently and discarded all but the first result;
    /// the fix mirrors get_or_build_call_graph.
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

    /// T005 negative companion: invalidating the graph must let the next
    /// call build a fresh one (a different Arc).
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

        // File index must have the file.
        let path = uri.to_file_path().unwrap();
        assert!(
            workspace.file_index.files.contains_key(&path),
            "file_index.files must contain the file after on_document_change"
        );
        // Object info must be populated.
        assert!(
            workspace.file_index.object_info.contains_key(&path),
            "file_index.object_info must be populated after on_document_change"
        );
        let info = workspace.file_index.object_info.get(&path).unwrap();
        assert_eq!(info.name.to_lowercase(), "test table");
    }

    /// Negative test: on_document_change with a non-file URI falls back gracefully.
    ///
    /// Uses a URI whose scheme guarantees `to_file_path()` returns `Err` (e.g. http://),
    /// so the fallback path `invalidate_all_composed` is exercised and no file is added.
    #[test]
    fn on_document_change_non_file_uri_does_not_panic() {
        let workspace = make_workspace();
        // http: URIs are never valid file paths — to_file_path() always returns Err.
        let uri = Url::parse("http://example.com/Untitled-1.al").unwrap();
        let text = r#"codeunit 50200 "Test" { }"#;

        // Must not panic; composed cache is invalidated as a fallback.
        on_document_change(&workspace, &uri, text);
        // No file was added to the index (non-file URI).
        assert!(workspace.file_index.is_empty());
    }

    /// F-008: a fresh workspace starts with an empty `last_compile_affected`
    /// set so the first compile run has no stale entries to clear.
    #[tokio::test]
    async fn f008_last_compile_affected_starts_empty() {
        let workspace = make_workspace();
        let guard = workspace.last_compile_affected.lock().await;
        assert!(
            guard.is_empty(),
            "fresh workspace must have no compile-affected files"
        );
    }

    /// F-008: the al.compile handler computes "stale" as set difference
    /// (previous - current). This unit-tests that pure computation in
    /// isolation from the LSP/toolchain plumbing.
    #[test]
    fn f008_stale_is_set_difference_previous_minus_current() {
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

    /// F-008 negative: when the new compile result equals the previous
    /// result, no stale entries are produced (no spurious clears).
    #[test]
    fn f008_no_stale_when_compile_set_unchanged() {
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

        // Build & cache a graph, then close: close must drop it.
        let before = workspace.get_or_build_insight_graph();
        on_document_close(&workspace, &uri);
        let after = workspace.get_or_build_insight_graph();
        assert!(
            !std::sync::Arc::ptr_eq(&before, &after),
            "on_document_close must invalidate the cached insight graph"
        );
    }

    /// on_document_close with a non-file URI takes the fallback branch
    /// (invalidate_all_composed) and must not panic.
    #[test]
    fn on_document_close_non_file_uri_does_not_panic() {
        let workspace = make_workspace();
        let uri = Url::parse("http://example.com/Untitled-1.al").unwrap();
        // Must not panic; full composed-cache invalidation is the fallback.
        on_document_close(&workspace, &uri);
        // Closing a never-opened virtual buffer leaves the index empty.
        assert!(workspace.file_index.is_empty());
    }

    /// invalidate_call_graph_only drops the call graph but keeps the insight
    /// graph cached (F-OPEN-066 body-only-edit optimisation). The insight Arc
    /// must survive; only the call graph is rebuilt.
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

        // Insight graph Arc is unchanged (same pointer) — only the call
        // graph was dropped. A fresh read returns the still-cached insight.
        let insight_after = workspace.get_or_build_insight_graph();
        assert!(
            std::sync::Arc::ptr_eq(&insight_before, &insight_after),
            "invalidate_call_graph_only must NOT drop the insight graph"
        );

        // And the call graph really was dropped: rebuilding it succeeds.
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
        // Project discovery failed => workspace.project stays None.
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
}
