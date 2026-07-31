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

type DependencyFingerprint = Vec<(PathBuf, u64, std::time::SystemTime)>;

struct DependencySourceCache {
    /// `(canonical app path, byte length, modified time)` in stable order.
    fingerprint: DependencyFingerprint,
    index: Arc<FileIndex>,
}

/// A synchronization failure that makes workspace state unsafe to inspect.
///
/// Rust lock poisoning means a writer panicked while it held the lock. Query
/// paths must not reuse the possibly half-written value or translate it into an
/// empty/healthy-looking response. Full cache invalidation is the one exception:
/// it overwrites the protected value without reading it and clears the poison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceStateError {
    #[error("workspace state lock '{component}' is poisoned")]
    Poisoned { component: &'static str },
}

impl WorkspaceStateError {
    fn poisoned(component: &'static str) -> Self {
        Self::Poisoned { component }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DependencySourceError {
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    #[error("failed to inspect loaded symbol package '{}': {source}", path.display())]
    InspectPackage {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to index embedded source in loaded package '{}': {source}", path.display())]
    IndexPackage {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to extract embedded source from loaded package '{}': {source}", path.display())]
    ExtractPackage {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "embedded AL source '{archive_path}' in package '{}' did not parse cleanly: {details}",
        package_path.display()
    )]
    ParseSource {
        package_path: PathBuf,
        archive_path: String,
        details: String,
    },
    #[error(
        "embedded AL source '{archive_path}' in package '{}' has no object declaration",
        package_path.display()
    )]
    MissingObjectDeclaration {
        package_path: PathBuf,
        archive_path: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CallGraphBuildError {
    #[error(transparent)]
    DependencySource(#[from] DependencySourceError),
    #[error(transparent)]
    SourceGraph(#[from] al_insight::calls::SourceGraphError),
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
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
    /// Serializes publication of a fully staged source/package/project
    /// generation. LSP request handlers hold a read guard while querying; a
    /// reindex holds the write guard only for the final in-memory swap.
    pub generation_lock: tokio::sync::RwLock<()>,
    /// Monotonic publication revision used by operations that stage work under
    /// a read guard and then upgrade by reacquiring the write lock.
    pub generation_revision: std::sync::atomic::AtomicU64,
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
    /// Exact dependency-source generation used to build `call_graph`.
    ///
    /// A cached graph is reusable only while this equals the current package
    /// fingerprint. Keeping the association explicit prevents a deleted,
    /// replaced, or corrupt `.app` from being hidden by a previously successful
    /// graph build.
    call_graph_dependency_fingerprint: std::sync::RwLock<Option<DependencyFingerprint>>,
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
            generation_lock: tokio::sync::RwLock::new(()),
            generation_revision: std::sync::atomic::AtomicU64::new(0),
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
            call_graph_dependency_fingerprint: std::sync::RwLock::new(None),
            call_graph_build_lock: std::sync::Mutex::new(()),
            dependency_source_index: std::sync::RwLock::new(None),
            profiler_session: std::sync::RwLock::new(None),
            test_results: std::sync::RwLock::new(None),
            last_compile_affected: tokio::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Current monotonic revision of the published workspace generation.
    #[inline]
    pub fn generation_revision(&self) -> u64 {
        self.generation_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Mark a source, symbol, project, or configuration mutation as visible.
    ///
    /// Optimistic long-running operations snapshot this value, do blocking
    /// work without holding [`generation_lock`](Self::generation_lock), and
    /// publish only when the revision still matches.
    #[inline]
    pub fn mark_generation_changed(&self) {
        self.generation_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// Replace the complete package inventory for a published generation.
    ///
    /// The inventory is derived from the validated package batch. If an older
    /// writer panicked, the replacement payload is complete, so overwriting the
    /// inaccessible value and clearing the poison is safe.
    pub fn replace_package_info(&self, packages: Vec<PackageInfo>) {
        match self.package_info.write() {
            Ok(mut guard) => *guard = packages,
            Err(poisoned) => {
                *poisoned.into_inner() = packages;
                self.package_info.clear_poison();
                tracing::warn!(
                    "discarded poisoned package inventory during complete generation replacement"
                );
            }
        }
    }

    /// Get or lazily build the cached insight graph.
    pub fn get_or_build_insight_graph(&self) -> Result<Arc<InsightGraph>, WorkspaceStateError> {
        {
            let guard = self
                .insight_graph
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?;
            if let Some(arc) = guard.as_ref() {
                return Ok(Arc::clone(arc));
            }
        }
        let mut guard = self
            .insight_graph
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?;
        if let Some(arc) = guard.as_ref() {
            return Ok(Arc::clone(arc));
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
        Ok(arc)
    }

    /// Invalidate both graph caches.
    pub fn invalidate_insight_graph(&self) {
        reset_optional_cache(&self.insight_graph, "insight_graph");
        reset_optional_cache(&self.call_graph, "call_graph");
        reset_optional_cache(
            &self.call_graph_dependency_fingerprint,
            "call_graph_dependency_fingerprint",
        );
    }

    /// Invalidate the call graph while retaining the insight graph.
    pub fn invalidate_call_graph_only(&self) {
        reset_optional_cache(&self.call_graph, "call_graph");
        reset_optional_cache(
            &self.call_graph_dependency_fingerprint,
            "call_graph_dependency_fingerprint",
        );
    }

    /// Return a coherent parsed index of every AL object body embedded in the
    /// currently loaded Microsoft/third-party packages.
    ///
    /// Package source is immutable during normal editing, so it is cached
    /// separately from the workspace graph. A stable path/size/mtime
    /// fingerprint forces a rebuild when a package is downloaded or replaced.
    pub fn get_or_build_dependency_source_index(
        &self,
    ) -> Result<Arc<FileIndex>, DependencySourceError> {
        self.get_or_build_dependency_source_generation()
            .map(|(_, index)| index)
    }

    fn get_or_build_dependency_source_generation(
        &self,
    ) -> Result<(DependencyFingerprint, Arc<FileIndex>), DependencySourceError> {
        let fingerprint = self.dependency_package_fingerprint()?;
        {
            let cache = self
                .dependency_source_index
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("dependency_source_index"))?;
            if let Some(cache) = cache
                .as_ref()
                .filter(|cache| cache.fingerprint == fingerprint)
            {
                return Ok((fingerprint, Arc::clone(&cache.index)));
            }
        }

        let mut cache = self
            .dependency_source_index
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("dependency_source_index"))?;
        if let Some(existing) = cache
            .as_ref()
            .filter(|existing| existing.fingerprint == fingerprint)
        {
            return Ok((fingerprint, Arc::clone(&existing.index)));
        }

        let index = Arc::new(FileIndex::new());
        for (app_path, _, _) in &fingerprint {
            let source_index =
                al_symbols::source_index::get_or_build(app_path).map_err(|source| {
                    DependencySourceError::IndexPackage {
                        path: app_path.clone(),
                        source,
                    }
                })?;
            let sources = source_index.extract_all_sources().map_err(|source| {
                DependencySourceError::ExtractPackage {
                    path: app_path.clone(),
                    source,
                }
            })?;
            for (archive_path, source) in sources {
                let parsed = al_syntax::AlParser::parse_quick(&source);
                if !parsed.errors.is_empty() {
                    let details = parsed
                        .errors
                        .iter()
                        .take(3)
                        .map(|error| {
                            format!(
                                "{} at {}:{}",
                                error.message,
                                error.range.start_point.row + 1,
                                error.range.start_point.column + 1
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    return Err(DependencySourceError::ParseSource {
                        package_path: app_path.clone(),
                        archive_path,
                        details,
                    });
                }
                if al_syntax::find_object_declaration(&parsed.tree, &source).is_none() {
                    return Err(DependencySourceError::MissingObjectDeclaration {
                        package_path: app_path.clone(),
                        archive_path,
                    });
                }
                index.add_file_with_tree(
                    dependency_virtual_path(app_path, &archive_path),
                    source,
                    parsed.tree,
                );
            }
        }
        tracing::info!(
            packages = fingerprint.len(),
            source_files = index.len(),
            "dependency AL source index ready"
        );
        let index_for_return = Arc::clone(&index);
        *cache = Some(DependencySourceCache {
            fingerprint: fingerprint.clone(),
            index,
        });
        Ok((fingerprint, index_for_return))
    }

    fn dependency_package_fingerprint(
        &self,
    ) -> Result<DependencyFingerprint, DependencySourceError> {
        let mut fingerprint = Vec::new();
        for path in self.symbols.loaded_package_paths() {
            let metadata = std::fs::metadata(&path).map_err(|source| {
                DependencySourceError::InspectPackage {
                    path: path.clone(),
                    source,
                }
            })?;
            let modified =
                metadata
                    .modified()
                    .map_err(|source| DependencySourceError::InspectPackage {
                        path: path.clone(),
                        source,
                    })?;
            fingerprint.push((path, metadata.len(), modified));
        }
        fingerprint.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        fingerprint.dedup_by(|left, right| left.0 == right.0);
        Ok(fingerprint)
    }

    /// Get (or lazily build) the cached CallGraph.
    ///
    /// **Lock ordering invariant:** This function briefly validates
    /// `dependency_source_index` before reading graph caches. On the slow path
    /// it then takes `call_graph_build_lock`, revalidates dependency source, and
    /// publishes `insight_graph` → `call_graph` → its dependency fingerprint.
    /// No dependency or graph data guard is held while acquiring the build
    /// mutex. Any new code that touches these locks MUST preserve that ordering
    /// or the daemon can deadlock.
    pub fn get_or_build_call_graph(
        &self,
    ) -> Result<
        (
            Arc<InsightGraph>,
            std::sync::RwLockReadGuard<'_, Option<CallGraph>>,
        ),
        CallGraphBuildError,
    > {
        let (dependency_fingerprint, _) = self.get_or_build_dependency_source_generation()?;
        {
            let insight = self
                .insight_graph
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?
                .as_ref()
                .cloned();
            let cg_guard = self
                .call_graph
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("call_graph"))?;
            let fingerprint_guard = self
                .call_graph_dependency_fingerprint
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("call_graph_dependency_fingerprint"))?;
            if let Some(insight) = insight.filter(|_| {
                cg_guard.is_some() && fingerprint_guard.as_ref() == Some(&dependency_fingerprint)
            }) {
                drop(fingerprint_guard);
                return Ok((insight, cg_guard));
            }
        }

        let _build_lock = self
            .call_graph_build_lock
            .lock()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph_build_lock"))?;
        let (dependency_fingerprint, dependency_sources) =
            self.get_or_build_dependency_source_generation()?;
        {
            let insight = self
                .insight_graph
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?
                .as_ref()
                .cloned();
            let cg_guard = self
                .call_graph
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("call_graph"))?;
            let fingerprint_guard = self
                .call_graph_dependency_fingerprint
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("call_graph_dependency_fingerprint"))?;
            if let Some(insight) = insight.filter(|_| {
                cg_guard.is_some() && fingerprint_guard.as_ref() == Some(&dependency_fingerprint)
            }) {
                drop(fingerprint_guard);
                return Ok((insight, cg_guard));
            }
        }

        let build = || {
            let mut graph = InsightGraph::new();
            graph.build_from_index(&self.symbols);
            al_insight::calls::register_workspace_nodes(
                &self.file_index,
                &self.symbols,
                &mut graph,
            )?;
            al_insight::calls::register_dependency_source_nodes(&dependency_sources, &mut graph)?;
            let insight = Arc::new(graph);

            let mut cg = CallGraph::build_from_insight(&insight);
            al_insight::calls::populate_workspace_call_edges(
                &self.file_index,
                &self.symbols,
                &insight,
                &mut cg,
            )?;
            al_insight::calls::populate_workspace_call_edges(
                &dependency_sources,
                &self.symbols,
                &insight,
                &mut cg,
            )?;
            Ok::<_, CallGraphBuildError>((insight, cg))
        };
        let (insight, cg) = match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(build)
            }
            _ => build(),
        }?;

        // Acquire every publication lock before mutating any of them. If one
        // lock is poisoned, no partial graph generation is published.
        let mut ig_guard = self
            .insight_graph
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?;
        let mut cg_guard = self
            .call_graph
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph"))?;
        let mut fingerprint_guard = self
            .call_graph_dependency_fingerprint
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph_dependency_fingerprint"))?;
        *ig_guard = Some(Arc::clone(&insight));
        *cg_guard = Some(cg);
        *fingerprint_guard = Some(dependency_fingerprint);
        drop(fingerprint_guard);
        drop(cg_guard);
        drop(ig_guard);

        let guard = self
            .call_graph
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph"))?;
        Ok((insight, guard))
    }

    pub fn memory_stats(&self) -> Result<WorkspaceMemoryStats, WorkspaceStateError> {
        let symbol_count = self.symbols.len();
        let open_docs = self.documents.len();
        let workspace_files = self.file_index.files.len();
        let procedure_index_entries = self.file_index.procedures.len();
        let error_code_count = self.error_codes.len();
        let builtin_count = self
            .builtins
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("builtins"))?
            .len();
        let packages = self
            .package_info
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("package_info"))?;
        let package_count = packages.len();
        let symbol_index_memory = self.symbols.memory_stats();
        let document_store_memory = self.documents.memory_stats();
        let file_index_memory = self.file_index.memory_stats();
        let package_metadata_bytes = packages
            .iter()
            .map(|package| {
                std::mem::size_of::<PackageInfo>()
                    + package.name.capacity()
                    + package.publisher.capacity()
                    + package.version.capacity()
            })
            .sum();
        drop(packages);
        let insight_graph_memory = self
            .insight_graph
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?
            .as_ref()
            .map(|graph| graph.memory_stats());
        let call_graph_memory = self
            .call_graph
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph"))?
            .as_ref()
            .map(|graph| graph.memory_stats());

        Ok(WorkspaceMemoryStats {
            symbol_count,
            open_docs,
            workspace_files,
            procedure_index_entries,
            error_code_count,
            builtin_count,
            package_count,
            symbol_index_memory,
            document_store_memory,
            file_index_memory,
            package_metadata_bytes,
            insight_graph_memory,
            call_graph_memory,
        })
    }
}

/// Reset an optional cache without reading the protected value.
///
/// This is a deliberate recovery boundary: unlike query paths, invalidation
/// can safely repair a poisoned cache because `None` replaces the whole value.
fn reset_optional_cache<T>(lock: &std::sync::RwLock<Option<T>>, component: &'static str) {
    match lock.write() {
        Ok(mut guard) => *guard = None,
        Err(poisoned) => {
            *poisoned.into_inner() = None;
            lock.clear_poison();
            tracing::warn!(component, "discarded and repaired poisoned workspace cache");
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
    pub symbol_index_memory: al_symbols::SymbolIndexMemoryStats,
    pub document_store_memory: al_source::documents::DocumentStoreMemoryStats,
    pub file_index_memory: al_source::file_index::FileIndexMemoryStats,
    pub package_metadata_bytes: usize,
    pub insight_graph_memory: Option<al_insight::graph::InsightGraphMemoryStats>,
    pub call_graph_memory: Option<al_insight::index::CallGraphMemoryStats>,
}

/// Result of a successful core workspace initialization.
///
/// Callers (LSP and daemon) consume this to perform their transport-specific
/// post-init steps (sending notifications, opening files in DocumentStore, etc.).
#[derive(Debug)]
pub struct CoreInitResult {
    pub file_count: usize,
    pub package_count: usize,
    pub total_symbols: usize,
    pub has_toolchain: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreInitError {
    #[error(transparent)]
    Project(#[from] al_project::errors::DiscoveryError),
    #[error(transparent)]
    SourceScan(#[from] al_source::file_index::ScanError),
    #[error(transparent)]
    SymbolPackages(#[from] al_symbols::PackageLoadError),
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
}

/// Initialize the common parts of a workspace.
///
/// Shared by the LSP and daemon initialization paths. Discovers the project and
/// toolchain, loads symbols, and indexes workspace files. The caller remains
/// responsible for transport-specific notifications and document setup.
///
/// A directory without `app.json` is a valid syntax-only workspace. Invalid
/// project metadata or an incomplete source scan is not: those states return a
/// concrete error instead of publishing an apparently complete partial index.
pub async fn initialize_core_workspace(
    workspace: &Workspace,
    project_root: &std::path::Path,
) -> Result<CoreInitResult, CoreInitError> {
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
            project.apply_symbol_settings(&config)?;
            tracing::info!(
                name = %project.app_json.name,
                root = %project.root.display(),
                packages = project.packages.len(),
                "workspace: project discovered"
            );

            let cache = al_symbols::cache::SymbolCache::default_location();
            let loaded = workspace
                .symbols
                .load_packages_cached(&project.packages, &cache)?;
            total_symbols = loaded.iter().map(|p| p.object_count).sum();
            package_count = loaded.len();
            tracing::info!(
                packages = package_count,
                symbols = total_symbols,
                "workspace: loaded symbol packages"
            );
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
            workspace.replace_package_info(pkg_info);

            // Scan workspace .al files. file_index.scan walks the tree
            // synchronously (read_dir + read_to_string per file) so wrap
            // it in block_in_place — we still hold the &Workspace borrow.
            file_count = tokio::task::block_in_place(|| workspace.file_index.scan(&project.root))?;

            *workspace.project.write().await = Some(project);

            tracing::info!(
                symbols = workspace.symbols.len(),
                workspace_files = file_count,
                workspace_objects = workspace.file_index.object_count(),
                "workspace: initialized"
            );
        }
        Err(al_project::errors::DiscoveryError::NoProjectFound { .. }) => {
            tracing::info!(root = %project_root.display(), "workspace: no app.json; using syntax-only workspace");

            // Still scan for .al files even without a project. Same
            // block_in_place bridge as the happy path above.
            file_count = tokio::task::block_in_place(|| workspace.file_index.scan(project_root))?;
        }
        Err(e) => {
            tracing::warn!(error = %e, root = %project_root.display(), "workspace: project discovery failed");
            return Err(CoreInitError::Project(e));
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

    Ok(CoreInitResult {
        file_count,
        package_count,
        total_symbols,
        has_toolchain,
    })
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
        let previous_identity = workspace.file_index.object_info.get(&path).map(|info| {
            (
                info.kind.to_ascii_lowercase(),
                info.id,
                info.name.to_ascii_lowercase(),
            )
        });
        let prev_procs: std::collections::HashSet<String> = workspace
            .file_index
            .procedures_snapshot(&path)
            .into_iter()
            .collect();

        workspace
            .file_index
            .add_file_with_tree(path.clone(), text.to_string(), result.tree);

        let new_identity = workspace.file_index.object_info.get(&path).map(|info| {
            (
                info.kind.to_ascii_lowercase(),
                info.id,
                info.name.to_ascii_lowercase(),
            )
        });
        if let Some((_, _, name)) = &previous_identity {
            workspace.symbols.invalidate_composed(name);
        }
        if let Some((_, _, name)) = &new_identity {
            workspace.symbols.invalidate_composed(name);
        }
        if previous_identity.is_none() && new_identity.is_none() {
            workspace.symbols.invalidate_all_composed();
        }

        // Compare new procedure set to the snapshot — same set ⇒ no topology
        // change ⇒ insight_graph stays valid.
        let new_procs: std::collections::HashSet<String> = workspace
            .file_index
            .procedures_snapshot(&path)
            .into_iter()
            .collect();
        prev_procs != new_procs || previous_identity != new_identity
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
    workspace.mark_generation_changed();
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
    workspace.mark_generation_changed();
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
        let first = workspace.get_or_build_insight_graph().unwrap();
        workspace.invalidate_insight_graph();
        let second = workspace.get_or_build_insight_graph().unwrap();
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
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
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
    fn object_identity_change_invalidates_old_and_new_composition_and_graph() {
        let workspace = make_workspace();
        workspace.symbols.add_entries(&[
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Codeunit,
                id: 50_100,
                name: "Old Name".to_string(),
                package: "Test".to_string(),
                ..Default::default()
            },
            al_symbols::SymbolEntry {
                kind: al_symbols::ObjectKind::Codeunit,
                id: 50_101,
                name: "New Name".to_string(),
                package: "Test".to_string(),
                ..Default::default()
            },
        ]);
        let uri = Url::from_file_path("/tmp/object_identity_change/Codeunit.al").unwrap();
        let old = r#"codeunit 50100 "Old Name" { procedure Run() begin end; }"#;
        let new = r#"codeunit 50101 "New Name" { procedure Run() begin end; }"#;
        workspace
            .documents
            .open(uri.clone(), old.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, old);

        let _ = workspace
            .symbols
            .get_composed_cached(al_symbols::ObjectKind::Codeunit, "Old Name");
        let _ = workspace
            .symbols
            .get_composed_cached(al_symbols::ObjectKind::Codeunit, "New Name");
        assert!(!workspace.symbols.is_composed_cache_empty());
        let graph_before = workspace.get_or_build_insight_graph().unwrap();
        let revision_before = workspace.generation_revision();

        workspace
            .documents
            .replace_or_open(uri.clone(), new.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, new);

        assert!(
            workspace.symbols.is_composed_cache_empty(),
            "both the discarded and replacement object identities must be invalidated"
        );
        let graph_after = workspace.get_or_build_insight_graph().unwrap();
        assert!(
            !Arc::ptr_eq(&graph_before, &graph_after),
            "an identity change with the same procedure set is still a topology change"
        );
        assert!(workspace.generation_revision() > revision_before);
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
        let symbols = format!(
            r#"{{"Tables":[{{"Id":50123,"Name":"{table_name}","Fields":[],"Methods":[]}}]}}"#
        );
        build_test_app_with_sources(name, &symbols, &[])
    }

    fn build_test_app_with_sources(name: &str, symbols: &str, sources: &[(&str, &str)]) -> Vec<u8> {
        use std::io::{Cursor, Write};
        use zip::write::SimpleFileOptions;

        let manifest = format!(
            r#"<?xml version="1.0"?><Package><App Id="00000000-0000-0000-0000-000000000001" Name="{name}" Publisher="Test" Version="1.0.0.0" /></Package>"#
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
            for (path, source) in sources {
                zip.start_file(*path, options).unwrap();
                zip.write_all(source.as_bytes()).unwrap();
            }
            zip.finish().unwrap();
        }
        data.extend_from_slice(&zip_data);
        data
    }

    #[test]
    fn dependency_source_index_includes_package_with_no_symbol_entries() {
        let workspace = make_workspace();
        let dir = unique_tempdir("source-only-package");
        let app_path = dir.join("SourceOnly.app");
        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "SourceOnly",
                r#"{"Tables":[]}"#,
                &[
                    (
                        "src/Cod50124.SourceOnly.al",
                        r#"codeunit 50124 "Source Only" { procedure Run() begin end; }"#,
                    ),
                    (
                        "src/SourceContract.Interface.al",
                        r#"interface "Source Contract" { procedure Run(); }"#,
                    ),
                ],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(std::slice::from_ref(&app_path))
            .unwrap();

        let index = workspace
            .get_or_build_dependency_source_index()
            .expect("file-backed package source must not depend on symbol entry count");
        assert_eq!(index.len(), 2);
        let (_, call_graph) = workspace
            .get_or_build_call_graph()
            .expect("ID-less dependency objects must participate in graph construction");
        assert!(call_graph.is_some());
        assert_eq!(
            workspace.symbols.loaded_package_paths(),
            vec![app_path.canonicalize().unwrap()],
            "loaded package paths use the canonical source-index cache identity"
        );
    }

    #[test]
    fn failed_dependency_parse_publishes_no_partial_graph_or_source_index() {
        let workspace = make_workspace();
        let dir = unique_tempdir("dependency-parse-atomic");
        let app_path = dir.join("BrokenSource.app");
        let symbols = r#"{"Codeunits":[{"Id":50125,"Name":"Broken Source","Methods":[]}]}"#;
        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "BrokenSource",
                symbols,
                &[
                    (
                        "src/Cod50125.Good.al",
                        r#"codeunit 50125 "Broken Source" { procedure Good() begin end; }"#,
                    ),
                    (
                        "src/Cod50126.Broken.al",
                        "codeunit 50126 Broken { procedure Incomplete(",
                    ),
                ],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(std::slice::from_ref(&app_path))
            .unwrap();

        let error = match workspace.get_or_build_call_graph() {
            Err(error) => error,
            Ok(_) => panic!("one malformed embedded source must reject the whole graph generation"),
        };
        assert!(
            matches!(
                error,
                CallGraphBuildError::DependencySource(DependencySourceError::ParseSource { .. })
            ),
            "{error}"
        );
        assert!(workspace.dependency_source_index.read().unwrap().is_none());
        assert!(workspace.insight_graph.read().unwrap().is_none());
        assert!(workspace.call_graph.read().unwrap().is_none());
        assert!(workspace
            .call_graph_dependency_fingerprint
            .read()
            .unwrap()
            .is_none());

        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "BrokenSource",
                symbols,
                &[
                    (
                        "src/Cod50125.Good.al",
                        r#"codeunit 50125 "Broken Source" { procedure Good() begin end; }"#,
                    ),
                    (
                        "src/Cod50126.Repaired.al",
                        r#"codeunit 50126 "Repaired Source"
{
    procedure Repaired()
    begin
    end;
}"#,
                    ),
                ],
            ),
        )
        .unwrap();

        let (_, graph) = workspace
            .get_or_build_call_graph()
            .expect("a repaired package must build a fresh complete generation");
        assert!(graph.is_some());
        assert_eq!(
            workspace
                .dependency_source_index
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .index
                .len(),
            2
        );
    }

    #[test]
    fn cached_call_graph_does_not_hide_package_deletion_or_corruption() {
        let workspace = make_workspace();
        let dir = unique_tempdir("dependency-cache-revalidation");
        let app_path = dir.join("Mutable.app");
        std::fs::write(
            &app_path,
            build_test_app_with_sources(
                "Mutable",
                r#"{"Tables":[{"Id":50127,"Name":"Mutable Table","Fields":[],"Methods":[]}]}"#,
                &[(
                    "src/Tab50127.Mutable.al",
                    r#"table 50127 "Mutable Table" { fields { field(1; Value; Integer) { } } }"#,
                )],
            ),
        )
        .unwrap();
        workspace
            .symbols
            .load_packages(std::slice::from_ref(&app_path))
            .unwrap();
        let (_, initial) = workspace.get_or_build_call_graph().unwrap();
        assert!(initial.is_some());
        drop(initial);

        std::fs::write(
            &app_path,
            b"NAVX corrupt replacement with a different length",
        )
        .unwrap();
        let corrupt_error = match workspace.get_or_build_call_graph() {
            Err(error) => error,
            Ok(_) => panic!("a cached graph must not conceal package corruption"),
        };
        assert!(
            matches!(
                corrupt_error,
                CallGraphBuildError::DependencySource(DependencySourceError::IndexPackage { .. })
            ),
            "{corrupt_error}"
        );

        std::fs::remove_file(&app_path).unwrap();
        let missing_error = match workspace.get_or_build_call_graph() {
            Err(error) => error,
            Ok(_) => panic!("a cached graph must not conceal package deletion"),
        };
        assert!(
            matches!(
                missing_error,
                CallGraphBuildError::DependencySource(DependencySourceError::InspectPackage { .. })
            ),
            "{missing_error}"
        );
    }

    /// memory_stats reflects the *actual* live state of the workspace, not
    /// constant zeros. After indexing one .al file the workspace_files count
    /// and procedure_index_entries must rise above the empty baseline.
    #[test]
    fn memory_stats_reflects_indexed_file() {
        let workspace = make_workspace();

        let empty = workspace.memory_stats().unwrap();
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
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, text);

        let after = workspace.memory_stats().unwrap();
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
        assert_eq!(workspace.memory_stats().unwrap().package_count, 0);
        assert_eq!(workspace.memory_stats().unwrap().error_code_count, 0);

        workspace.package_info.write().unwrap().push(PackageInfo {
            name: "Base Application".to_string(),
            publisher: "Microsoft".to_string(),
            version: "1.0.0.0".to_string(),
            object_count: 42,
        });
        workspace
            .error_codes
            .insert("AL0118".to_string(), "Unknown identifier".to_string());

        let stats = workspace.memory_stats().unwrap();
        assert_eq!(stats.package_count, 1, "one package must be counted");
        assert_eq!(stats.error_code_count, 1, "one error code must be counted");
    }

    #[test]
    fn memory_stats_exposes_symbol_index_bytes() {
        let workspace = make_workspace();
        let empty = workspace.memory_stats().unwrap().symbol_index_memory;
        workspace.symbols.add_entries(&[al_symbols::SymbolEntry {
            kind: al_symbols::ObjectKind::Table,
            id: 50_100,
            name: "Measured Workspace Table".to_string(),
            package: "Test Package".to_string(),
            ..Default::default()
        }]);

        let populated = workspace.memory_stats().unwrap().symbol_index_memory;
        assert!(populated.symbol_payload_bytes > 0);
        assert!(populated.tracked_bytes > empty.tracked_bytes);
    }

    #[test]
    fn memory_stats_accounts_for_document_file_package_and_graph_caches() {
        let workspace = make_workspace();
        let dir = unique_tempdir("memory-components");
        let path = dir.join("Measured.al");
        let uri = Url::from_file_path(&path).unwrap();
        let text = "codeunit 50101 Measured { procedure Run() begin end; }";
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, text);
        workspace.file_index.add_file(path, text.to_string());
        workspace.package_info.write().unwrap().push(PackageInfo {
            name: "Measured Package".to_string(),
            publisher: "Test".to_string(),
            version: "1.0.0.0".to_string(),
            object_count: 1,
        });
        let _ = workspace.get_or_build_call_graph().unwrap();

        let stats = workspace.memory_stats().unwrap();
        assert!(stats.document_store_memory.document_text_bytes >= text.len());
        assert!(stats.file_index_memory.source_text_bytes >= text.len());
        assert!(stats.package_metadata_bytes > 0);
        assert!(stats.insight_graph_memory.is_some());
        assert!(stats.call_graph_memory.is_some());
    }

    #[test]
    fn memory_stats_reports_poisoned_state_instead_of_zero() {
        let workspace = Arc::new(make_workspace());
        let poison_target = Arc::clone(&workspace);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.builtins.write().unwrap();
            panic!("poison builtins for test");
        })
        .join();

        let error = workspace.memory_stats().unwrap_err();
        assert_eq!(
            error,
            WorkspaceStateError::Poisoned {
                component: "builtins"
            }
        );
    }

    #[test]
    fn dependency_source_cache_poison_is_an_explicit_error() {
        let workspace = Arc::new(make_workspace());
        let poison_target = Arc::clone(&workspace);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.dependency_source_index.write().unwrap();
            panic!("poison dependency source cache for test");
        })
        .join();

        let error = match workspace.get_or_build_dependency_source_index() {
            Err(error) => error,
            Ok(_) => panic!("poisoned dependency source state must not produce an index"),
        };
        assert!(matches!(
            error,
            DependencySourceError::State(WorkspaceStateError::Poisoned {
                component: "dependency_source_index"
            })
        ));
    }

    #[test]
    fn graph_query_rejects_poison_and_invalidation_repairs_the_cache() {
        let workspace = Arc::new(make_workspace());
        let poison_target = Arc::clone(&workspace);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target.insight_graph.write().unwrap();
            panic!("poison insight graph for test");
        })
        .join();

        let error = match workspace.get_or_build_call_graph() {
            Err(error) => error,
            Ok(_) => panic!("poisoned graph state must not produce a successful graph"),
        };
        assert!(matches!(
            error,
            CallGraphBuildError::State(WorkspaceStateError::Poisoned {
                component: "insight_graph"
            })
        ));

        workspace.invalidate_insight_graph();
        assert!(
            workspace.get_or_build_call_graph().is_ok(),
            "full invalidation overwrites poisoned cache state and clears the poison"
        );
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
        workspace
            .documents
            .open(uri.clone(), text.to_string())
            .unwrap();
        on_document_change(&workspace, &uri, text);

        // Object info must exist so the file-URI branch (not the fallback)
        // is exercised.
        let path = uri.to_file_path().unwrap();
        assert!(
            workspace.file_index.object_info.contains_key(&path),
            "precondition: object_info populated"
        );

        let before = workspace.get_or_build_insight_graph().unwrap();
        on_document_close(&workspace, &uri);
        let after = workspace.get_or_build_insight_graph().unwrap();
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
            let (ig, cg) = workspace.get_or_build_call_graph().unwrap();
            assert!(cg.is_some(), "call graph must be built");
            ig
        };

        workspace.invalidate_call_graph_only();

        let insight_after = workspace.get_or_build_insight_graph().unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&insight_before, &insight_after),
            "invalidate_call_graph_only must NOT drop the insight graph"
        );

        let (_ig2, cg2) = workspace.get_or_build_call_graph().unwrap();
        assert!(cg2.is_some(), "call graph must rebuild after invalidation");
    }

    /// initialize_core_workspace on a directory with no app.json takes the
    /// syntax-only branch: it still scans for .al files and
    /// returns a CoreInitResult with zero packages.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_no_project_scans_files() {
        let workspace = make_workspace();
        let dir = unique_tempdir("noproject");
        // A loose .al file but NO app.json anywhere up the tree.
        std::fs::write(dir.join("Loose.al"), r#"codeunit 50500 "Loose" { }"#).unwrap();

        let result = initialize_core_workspace(&workspace, &dir)
            .await
            .expect("syntax-only workspace must initialize");

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

        let result = initialize_core_workspace(&workspace, &dir)
            .await
            .expect("valid project must initialize");

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

        let result = initialize_core_workspace(&workspace, &dir)
            .await
            .expect("valid project and package must initialize");

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_core_workspace_rejects_invalid_manifest_instead_of_scanning_partially() {
        let workspace = make_workspace();
        let dir = unique_tempdir("invalidproject");
        std::fs::write(dir.join("app.json"), "{ not json").unwrap();
        std::fs::write(dir.join("Obj.al"), r#"codeunit 50600 "Init Obj" { }"#).unwrap();

        let error = initialize_core_workspace(&workspace, &dir)
            .await
            .expect_err("invalid app.json must fail initialization");
        assert!(matches!(error, CoreInitError::Project(_)), "{error}");
        assert!(workspace.file_index.is_empty());
        assert!(workspace.project.read().await.is_none());
    }
}
