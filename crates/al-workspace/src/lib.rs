//! Workspace state container.
//!
//! Per-project documents, symbols, configuration, and semantic state.

use std::path::{Path, PathBuf};
use std::sync::Arc;

mod dependency_sources;
mod doctor;
mod semantic_lifecycle;
mod source_cache;
mod test_results;
pub use dependency_sources::{
    DependencySourceMemoryStats, DependencySources, PackageSourceSummary,
};
pub use doctor::{doctor, DoctorReport, ProjectInfo, ToolchainInfo};
pub use semantic_lifecycle::{
    ensure_builtins_loaded, ensure_error_codes_loaded, get_or_init_bridge, restart_bridge,
    restart_bridge_if_current, set_builtins, shutdown_bridge,
};
pub use source_cache::{PackageKey, SourceSummaryCache};
pub use test_results::{project_data_dir, TestResultStore};

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
/// Takes only `&str` -- no tower-lsp types in al-workspace.
pub type NotifySink = Arc<dyn Fn(&str) + Send + Sync>;

/// Summary metadata for a loaded symbol package.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PackageInfo {
    /// The manifest's app id; empty for a package without one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub app_id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub object_count: usize,
}

impl From<&al_symbols::model::SymbolPackage> for PackageInfo {
    fn from(package: &al_symbols::model::SymbolPackage) -> Self {
        Self {
            app_id: package.app_id.clone(),
            name: package.name.clone(),
            publisher: package.publisher.clone(),
            version: package.version.clone(),
            object_count: package.object_count,
        }
    }
}

type DependencyFingerprint = Vec<(PathBuf, u64, std::time::SystemTime)>;

struct DependencySourceCache {
    /// `(canonical app path, byte length, modified time)` in stable order.
    fingerprint: DependencyFingerprint,
    index: Arc<DependencySources>,
    /// Embedded `.al` files this generation had to skip — one that did not
    /// parse cleanly, or one without an object declaration. Indexing degrades
    /// per file, so a non-zero count is the only signal that dependency-backed
    /// navigation is incomplete; it is kept with the generation rather than
    /// only written to the log.
    skipped_files: usize,
    /// Packages this generation could not index at all, each with the reason.
    /// Their objects are missing from dependency-backed navigation, so the
    /// list travels with the generation rather than only reaching the log.
    skipped_packages: Vec<String>,
}

/// One package's summary and where it came from.
struct LoadedPackageSummary {
    summary: Arc<PackageSourceSummary>,
    /// Read from the summary cache rather than built from the `.app`.
    from_disk: bool,
    /// The package's entry name in the summary cache, which the next garbage
    /// collection must keep. `None` without a cache.
    entry: Option<std::ffi::OsString>,
}

impl LoadedPackageSummary {
    fn built(summary: Arc<PackageSourceSummary>) -> Self {
        Self {
            summary,
            from_disk: false,
            entry: None,
        }
    }
}

/// Live counters for the dependency AL source index build.
///
/// Plain atomics rather than a lock: every reader is a status query that must
/// answer while the build holds the index write lock, which is exactly when a
/// lock-based field would be unreadable.
#[derive(Debug, Default)]
struct DependencySourceProgress {
    /// 0 idle, 1 building, 2 ready, 3 failed. See [`DependencySourceState`].
    state: std::sync::atomic::AtomicU8,
    packages_done: std::sync::atomic::AtomicUsize,
    packages_total: std::sync::atomic::AtomicUsize,
    files_done: std::sync::atomic::AtomicUsize,
    /// Packages read from the summary cache rather than built.
    packages_from_disk: std::sync::atomic::AtomicUsize,
    /// Milliseconds the current or last build has taken.
    elapsed_ms: std::sync::atomic::AtomicU64,
    started_at: std::sync::RwLock<Option<std::time::Instant>>,
}

/// What the dependency source index, or the call graph, is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DependencySourceState {
    /// No query has needed it yet.
    Idle,
    Building,
    Ready,
    Failed,
}

impl DependencySourceState {
    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Building,
            2 => Self::Ready,
            3 => Self::Failed,
            _ => Self::Idle,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Building => 1,
            Self::Ready => 2,
            Self::Failed => 3,
        }
    }
}

impl DependencySourceProgress {
    fn begin(&self, packages_total: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        if let Ok(mut started) = self.started_at.write() {
            *started = Some(std::time::Instant::now());
        }
        self.packages_total.store(packages_total, Relaxed);
        self.packages_done.store(0, Relaxed);
        self.files_done.store(0, Relaxed);
        self.packages_from_disk.store(0, Relaxed);
        self.elapsed_ms.store(0, Relaxed);
        self.state
            .store(DependencySourceState::Building.code(), Relaxed);
    }

    fn finished_package(&self, files_indexed: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        self.packages_done.fetch_add(1, Relaxed);
        self.files_done.store(files_indexed, Relaxed);
        self.tick();
    }

    fn indexed_file(&self) {
        self.indexed_files(1);
    }

    fn indexed_files(&self, count: usize) {
        self.files_done
            .fetch_add(count, std::sync::atomic::Ordering::Relaxed);
    }

    fn loaded_package_from_disk(&self) {
        self.packages_from_disk
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    fn finish(&self, files_indexed: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        self.files_done.store(files_indexed, Relaxed);
        self.tick();
        self.state
            .store(DependencySourceState::Ready.code(), Relaxed);
    }

    fn tick(&self) {
        let elapsed = self
            .started_at
            .read()
            .ok()
            .and_then(|started| *started)
            .map(|started| started.elapsed().as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or(0);
        self.elapsed_ms
            .store(elapsed, std::sync::atomic::Ordering::Relaxed);
    }

    fn snapshot(&self) -> DependencySourceProgressSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        let state = DependencySourceState::from_code(self.state.load(Relaxed));
        // A running build only writes `elapsed_ms` at package boundaries, and
        // a package can take tens of seconds, so recompute while building.
        let elapsed_ms = if state == DependencySourceState::Building {
            self.started_at
                .read()
                .ok()
                .and_then(|started| *started)
                .map(|started| started.elapsed().as_millis().min(u64::MAX as u128) as u64)
                .unwrap_or(0)
        } else {
            self.elapsed_ms.load(Relaxed)
        };
        DependencySourceProgressSnapshot {
            state,
            packages_done: self.packages_done.load(Relaxed),
            packages_total: self.packages_total.load(Relaxed),
            files_done: self.files_done.load(Relaxed),
            packages_from_disk: self.packages_from_disk.load(Relaxed),
            elapsed_ms,
        }
    }
}

/// A snapshot of the dependency source index build, for `status` and `diag`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencySourceProgressSnapshot {
    pub state: DependencySourceState,
    pub packages_done: usize,
    pub packages_total: usize,
    /// Files indexed so far. While building this only grows; there is no total
    /// because the file count of a package is not known until it is opened.
    pub files_done: usize,
    /// Packages whose summaries were read from the summary cache instead of
    /// being built from the `.app`.
    pub packages_from_disk: usize,
    pub elapsed_ms: u64,
}

/// Whether the call graph is being built, for `status`.
///
/// The build starts when the dependency source index is ready. On the medium
/// benchmark project the index was ready at 23.5 s and a cold `trace`
/// answered at 35.5 s, so a client that stopped waiting when the index
/// reported ready gave up in the middle of the build.
#[derive(Debug, Default)]
struct CallGraphProgress {
    /// Codes as in [`DependencySourceState`]. `ready` means the last build
    /// finished: an edit since then makes the next query build again.
    state: std::sync::atomic::AtomicU8,
    /// Milliseconds the last finished build took.
    elapsed_ms: std::sync::atomic::AtomicU64,
    started_at: std::sync::RwLock<Option<std::time::Instant>>,
}

impl CallGraphProgress {
    fn begin(&self) -> CallGraphBuildMark<'_> {
        if let Ok(mut started) = self.started_at.write() {
            *started = Some(std::time::Instant::now());
        }
        self.state.store(
            DependencySourceState::Building.code(),
            std::sync::atomic::Ordering::Relaxed,
        );
        CallGraphBuildMark {
            progress: self,
            succeeded: false,
        }
    }

    fn elapsed_now(&self) -> u64 {
        self.started_at
            .read()
            .ok()
            .and_then(|started| *started)
            .map(|started| started.elapsed().as_millis().min(u64::MAX as u128) as u64)
            .unwrap_or(0)
    }

    fn snapshot(&self) -> CallGraphProgressSnapshot {
        use std::sync::atomic::Ordering::Relaxed;
        let state = DependencySourceState::from_code(self.state.load(Relaxed));
        let elapsed_ms = if state == DependencySourceState::Building {
            self.elapsed_now()
        } else {
            self.elapsed_ms.load(Relaxed)
        };
        CallGraphProgressSnapshot { state, elapsed_ms }
    }
}

/// Ends a call graph build in [`CallGraphProgress`] when dropped: `failed`
/// unless [`Self::succeeded`] ran, so an error or a panic in the build does
/// not leave `status` reporting `building`.
struct CallGraphBuildMark<'a> {
    progress: &'a CallGraphProgress,
    succeeded: bool,
}

impl CallGraphBuildMark<'_> {
    fn succeeded(mut self) {
        self.succeeded = true;
    }
}

impl Drop for CallGraphBuildMark<'_> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.progress
            .elapsed_ms
            .store(self.progress.elapsed_now(), Relaxed);
        let state = if self.succeeded {
            DependencySourceState::Ready
        } else {
            DependencySourceState::Failed
        };
        self.progress.state.store(state.code(), Relaxed);
    }
}

/// A snapshot of the call graph build, for `status`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallGraphProgressSnapshot {
    pub state: DependencySourceState,
    /// Milliseconds the running build has taken so far, or the last one took.
    pub elapsed_ms: u64,
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
    /// Monotonic revision of the *package* generation: the loaded project, its
    /// resolved `.app` set and the symbol index built from them.
    ///
    /// Separate from [`generation_revision`](Self::generation_revision), which
    /// every keystroke bumps. Staging a package generation takes seconds on a
    /// real project, so a loop that retried whenever the source generation
    /// moved never converged while the user typed. Nothing a document edit
    /// does invalidates a package generation, so those loops key on this.
    pub package_revision: std::sync::atomic::AtomicU64,
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
    /// The daemon's native debug session. Its methods take `&mut self`, so each
    /// debug command holds this lock across its Business Central call. Each such
    /// call ends at the SignalR invoke timeout, and nothing it awaits takes this
    /// lock.
    pub debug_session: tokio::sync::Mutex<Option<al_dap::native_debug::NativeDebugSession>>,
    /// Optional callback for user-visible notifications (bridge failures, etc.).
    ///
    /// Set by al-lsp after workspace construction. In the LSP path the closure
    /// calls `client.show_message`; in the daemon path it logs. Not set in tests.
    pub notify_sink: std::sync::OnceLock<NotifySink>,
    /// What the project's own settings files asked for and did not get,
    /// because the project root is not trusted. `None` inside the `OnceLock`
    /// means nothing was ignored. Reported by `doctor` and by the MCP
    /// server's first tool result so the message is not only in a log.
    pub trust_advisory: std::sync::OnceLock<Option<String>>,
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
    /// Monotonic count of insight-graph invalidations.
    ///
    /// A build reads the file index over 100-200 ms without holding a data
    /// lock, so an edit can land between the read and the publication. Each
    /// graph is published tagged with the counter value read before the build
    /// started, and a cached graph is a hit only while its tag still equals
    /// the counter, so a graph that missed a concurrent edit is rebuilt on the
    /// next query instead of standing in for the current one.
    insight_invalidation_revision: std::sync::atomic::AtomicU64,
    /// Monotonic count of call-graph invalidations. Bumped by both
    /// invalidators, since dropping the insight graph drops the call graph.
    call_invalidation_revision: std::sync::atomic::AtomicU64,
    /// The invalidation revision `insight_graph` was built from.
    insight_graph_revision: std::sync::RwLock<Option<u64>>,
    /// The invalidation revision `call_graph` was built from.
    call_graph_revision: std::sync::RwLock<Option<u64>>,
    /// Summarized Microsoft/third-party object sources extracted from loaded
    /// `.app` packages. The fingerprint makes this cache independent from
    /// ordinary workspace-file graph invalidation while still rebuilding after
    /// package download/replacement.
    dependency_source_index: std::sync::RwLock<Option<DependencySourceCache>>,
    /// Where package summaries are kept between daemon starts. Unset, the
    /// index is built from the packages every time.
    source_summary_cache: std::sync::OnceLock<SourceSummaryCache>,
    /// How far the dependency source index has got.
    ///
    /// The build takes about a minute on Base Application and every method
    /// that needs it blocks until it finishes. Without a progress signal the
    /// caller's only observation is a timeout, and the natural response to a
    /// timeout is a retry into the next one.
    dependency_source_progress: DependencySourceProgress,
    /// Whether the call graph build is running, readable while it runs.
    call_graph_progress: CallGraphProgress,
    /// How many times the call graph has actually been built.
    ///
    /// Exists so single-flight is testable: a cold build is 86 s on a project
    /// with Base Application, and concurrent or retried callers running their
    /// own copy of it is the difference between one wait and several.
    call_graph_builds: std::sync::atomic::AtomicU64,
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
    /// Reference counts behind the code-lens "N references" titles.
    ///
    /// Building them walks every workspace file, so a fresh build per
    /// `textDocument/codeLens` made the lens cost proportional to the project
    /// on every document open. The entry is reusable only while both its
    /// generation and its document match.
    pub code_lens_reference_counts: std::sync::RwLock<Option<CodeLensReferenceCounts>>,
}

/// Cached code-lens reference counts, keyed to the generation and document
/// they were computed for.
pub struct CodeLensReferenceCounts {
    /// [`Workspace::generation_revision`] at build time.
    pub generation: u64,
    /// The `current_uri` the counts were built for. The open document's own
    /// text takes part in the count, so counts built for another document are
    /// not reusable.
    pub document: String,
    /// `(uri, line, character)` of a canonical declaration -> distinct call
    /// sites binding to it.
    pub counts: Arc<std::collections::HashMap<(String, u32, u32), usize>>,
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
            package_revision: std::sync::atomic::AtomicU64::new(0),
            config: RwLock::new(AlConfig::default()),
            builtins: std::sync::RwLock::new(Arc::new(Vec::new())),
            error_codes: DashMap::new(),
            bridge_restart_count: std::sync::atomic::AtomicU32::new(0),
            semantic_init_failure_reported: std::sync::atomic::AtomicBool::new(false),
            package_info: std::sync::RwLock::new(Vec::new()),
            semantic_cache: std::sync::RwLock::new(SemanticCache::new()),
            debug_session: tokio::sync::Mutex::new(None),
            notify_sink: std::sync::OnceLock::new(),
            trust_advisory: std::sync::OnceLock::new(),
            insight_graph: std::sync::RwLock::new(None),
            call_graph: std::sync::RwLock::new(None),
            call_graph_dependency_fingerprint: std::sync::RwLock::new(None),
            call_graph_build_lock: std::sync::Mutex::new(()),
            insight_invalidation_revision: std::sync::atomic::AtomicU64::new(0),
            call_invalidation_revision: std::sync::atomic::AtomicU64::new(0),
            insight_graph_revision: std::sync::RwLock::new(None),
            call_graph_revision: std::sync::RwLock::new(None),
            dependency_source_index: std::sync::RwLock::new(None),
            source_summary_cache: std::sync::OnceLock::new(),
            dependency_source_progress: DependencySourceProgress::default(),
            call_graph_progress: CallGraphProgress::default(),
            call_graph_builds: std::sync::atomic::AtomicU64::new(0),
            profiler_session: std::sync::RwLock::new(None),
            test_results: std::sync::RwLock::new(None),
            last_compile_affected: tokio::sync::Mutex::new(std::collections::HashSet::new()),
            code_lens_reference_counts: std::sync::RwLock::new(None),
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

    /// Current monotonic revision of the published package generation.
    #[inline]
    pub fn package_revision(&self) -> u64 {
        self.package_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Mark the project, its `.app` set or the symbol index as replaced. Every
    /// caller also calls [`mark_generation_changed`](Self::mark_generation_changed),
    /// since a new package generation is a new source generation too.
    #[inline]
    pub fn mark_package_generation_changed(&self) {
        self.package_revision
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
            let revision = self.insight_revision();
            let guard = self
                .insight_graph
                .read()
                .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?;
            if let Some(arc) = guard.as_ref() {
                if self.cached_revision_matches(
                    &self.insight_graph_revision,
                    "insight_graph_revision",
                    revision,
                )? {
                    return Ok(Arc::clone(arc));
                }
            }
        }
        let mut guard = self
            .insight_graph
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?;
        // Invalidation takes this same write lock, so the revision read here
        // still describes the graph built below.
        let revision = self.insight_revision();
        if let Some(arc) = guard.as_ref() {
            if self.cached_revision_matches(
                &self.insight_graph_revision,
                "insight_graph_revision",
                revision,
            )? {
                return Ok(Arc::clone(arc));
            }
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
        *self
            .insight_graph_revision
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("insight_graph_revision"))? = Some(revision);
        Ok(arc)
    }

    /// Invalidate both graph caches.
    pub fn invalidate_insight_graph(&self) {
        self.insight_invalidation_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.call_invalidation_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        reset_optional_cache(&self.insight_graph, "insight_graph");
        reset_optional_cache(&self.insight_graph_revision, "insight_graph_revision");
        reset_optional_cache(&self.call_graph, "call_graph");
        reset_optional_cache(&self.call_graph_revision, "call_graph_revision");
        reset_optional_cache(
            &self.call_graph_dependency_fingerprint,
            "call_graph_dependency_fingerprint",
        );
    }

    /// Invalidate the call graph while retaining the insight graph.
    pub fn invalidate_call_graph_only(&self) {
        self.call_invalidation_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        reset_optional_cache(&self.call_graph, "call_graph");
        reset_optional_cache(&self.call_graph_revision, "call_graph_revision");
        reset_optional_cache(
            &self.call_graph_dependency_fingerprint,
            "call_graph_dependency_fingerprint",
        );
    }

    #[inline]
    fn insight_revision(&self) -> u64 {
        self.insight_invalidation_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    #[inline]
    fn call_graph_revision_now(&self) -> u64 {
        self.call_invalidation_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn cached_revision_matches(
        &self,
        lock: &std::sync::RwLock<Option<u64>>,
        component: &'static str,
        current: u64,
    ) -> Result<bool, WorkspaceStateError> {
        Ok(lock
            .read()
            .map_err(|_| WorkspaceStateError::poisoned(component))?
            .as_ref()
            == Some(&current))
    }

    /// Return a coherent summary of every AL object body embedded in the
    /// currently loaded Microsoft/third-party packages.
    ///
    /// Package source is immutable during normal editing, so it is cached
    /// separately from the workspace graph. A stable path/size/mtime
    /// fingerprint forces a rebuild when a package is downloaded or replaced.
    pub fn get_or_build_dependency_source_index(
        &self,
    ) -> Result<Arc<DependencySources>, DependencySourceError> {
        self.get_or_build_dependency_source_generation()
            .map(|(_, index)| index)
    }

    /// How many embedded `.al` files the current dependency-source generation
    /// had to skip (unparseable or declaration-free).
    ///
    /// Building the index degrades per file, so a non-zero count means
    /// dependency-backed navigation and call-graph edges are incomplete.
    /// Returns `None` when no generation has been built yet.
    pub fn dependency_source_skipped_files(&self) -> Result<Option<usize>, DependencySourceError> {
        let cache = self
            .dependency_source_index
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("dependency_source_index"))?;
        Ok(cache.as_ref().map(|cache| cache.skipped_files))
    }

    /// Why the current generation left packages out, one message per package.
    pub fn dependency_source_skipped_packages(&self) -> Result<Vec<String>, DependencySourceError> {
        let cache = self
            .dependency_source_index
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("dependency_source_index"))?;
        Ok(cache
            .as_ref()
            .map(|cache| cache.skipped_packages.clone())
            .unwrap_or_default())
    }

    fn get_or_build_dependency_source_generation(
        &self,
    ) -> Result<(DependencyFingerprint, Arc<DependencySources>), DependencySourceError> {
        let (fingerprint, mut skipped_packages) = self.dependency_package_fingerprint_reporting();
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

        let mut packages = Vec::with_capacity(fingerprint.len());
        let mut skipped_files = 0usize;
        let mut files_done = 0usize;
        let mut from_disk = 0usize;
        let mut entries = std::collections::HashSet::new();
        // Decided once: a generation that finds the directory open to other
        // users reads nothing from it, even after its own writes close it.
        let disk_readable = self
            .source_summary_cache
            .get()
            .is_some_and(SourceSummaryCache::is_readable);
        self.dependency_source_progress.begin(fingerprint.len());
        for (app_path, _, _) in &fingerprint {
            self.dependency_source_progress.finished_package(files_done);
            // Degrade per package the way the build degrades per file: one
            // `.app` whose embedded source trips a limit, or that was
            // rewritten mid-build, must not take call-graph and insight
            // features down for every other package.
            match self.package_source_summary(app_path, disk_readable) {
                Ok(loaded) => {
                    skipped_files += loaded.summary.skipped_files;
                    files_done += loaded.summary.files.len();
                    if loaded.from_disk {
                        from_disk += 1;
                        self.dependency_source_progress.loaded_package_from_disk();
                    }
                    entries.extend(loaded.entry);
                    packages.push((app_path.clone(), loaded.summary));
                }
                Err(error) => {
                    tracing::warn!(
                        %error,
                        "dependency source index: skipping a package whose source cannot be summarized"
                    );
                    skipped_packages.push(error.to_string());
                }
            }
        }
        if let Some(disk) = self.source_summary_cache.get() {
            disk.retain(&entries);
        }
        let index = Arc::new(DependencySources::new(packages));
        self.dependency_source_progress.finish(index.len());
        tracing::info!(
            packages = fingerprint.len(),
            packages_from_disk = from_disk,
            source_files = index.len(),
            skipped_files,
            skipped_packages = skipped_packages.len(),
            "dependency AL source index ready"
        );
        *cache = Some(DependencySourceCache {
            fingerprint: fingerprint.clone(),
            index: Arc::clone(&index),
            skipped_files,
            skipped_packages,
        });
        Ok((fingerprint, index))
    }

    /// Keep package summaries in `cache` between starts. Returns `false` when
    /// a cache was already set, which stays in use.
    pub fn enable_source_summary_cache(&self, cache: SourceSummaryCache) -> bool {
        self.source_summary_cache.set(cache).is_ok()
    }

    /// The summarized source of one package: its entry in the summary cache
    /// when `disk_readable` and one matches, otherwise summarized from the
    /// `.app` and written back.
    fn package_source_summary(
        &self,
        app_path: &Path,
        disk_readable: bool,
    ) -> Result<LoadedPackageSummary, DependencySourceError> {
        let build = || {
            PackageSourceSummary::build(app_path, || self.dependency_source_progress.indexed_file())
                .map(Arc::new)
        };
        let Some(disk) = self.source_summary_cache.get() else {
            return build().map(LoadedPackageSummary::built);
        };
        let key = match PackageKey::of(app_path) {
            Ok(key) => key,
            Err(error) => {
                tracing::debug!(
                    package = %app_path.display(),
                    %error,
                    "source summary cache: cannot hash the package; building without the cache"
                );
                return build().map(LoadedPackageSummary::built);
            }
        };
        let entry = disk.entry_name(app_path, &key);
        if let Some(summary) = disk_readable.then(|| disk.load(app_path, &key)).flatten() {
            self.dependency_source_progress
                .indexed_files(summary.files.len());
            return Ok(LoadedPackageSummary {
                summary: Arc::new(summary),
                from_disk: true,
                entry: Some(entry),
            });
        }
        let summary = build()?;
        // A package without embedded source costs nothing to summarize again,
        // and an entry for it would only take disk space.
        if summary.files.is_empty() {
            return Ok(LoadedPackageSummary::built(summary));
        }
        if let Err(error) = disk.save(app_path, &key, &summary) {
            tracing::warn!(
                package = %app_path.display(),
                dir = %disk.dir().display(),
                %error,
                "source summary cache: could not write the entry"
            );
        }
        Ok(LoadedPackageSummary {
            summary,
            from_disk: false,
            entry: Some(entry),
        })
    }

    /// How far the dependency AL source index has got.
    ///
    /// Readable while the build holds the index write lock, which is the only
    /// time the answer matters.
    pub fn dependency_source_progress(&self) -> DependencySourceProgressSnapshot {
        self.dependency_source_progress.snapshot()
    }

    /// Whether the call graph is being built, and how long it has taken.
    /// Readable while the build runs.
    pub fn call_graph_progress(&self) -> CallGraphProgressSnapshot {
        self.call_graph_progress.snapshot()
    }

    /// Whether the cached call graph matches the current workspace files and
    /// packages. Builds nothing and does not wait for a build in progress.
    ///
    /// Workspace objects' fields and methods enter the symbol index with the
    /// graph, so a lookup that must answer at once asks this before it uses
    /// them.
    pub fn call_graph_is_current(&self) -> bool {
        let (dependency_fingerprint, _) = self.dependency_package_fingerprint_reporting();
        let revision = self.call_graph_revision_now();
        let revision_matches = matches!(
            self.call_graph_revision.try_read(),
            Ok(built_at) if *built_at == Some(revision)
        );
        let built = matches!(self.call_graph.try_read(), Ok(graph) if graph.is_some());
        let packages_match = matches!(
            self.call_graph_dependency_fingerprint.try_read(),
            Ok(built_from) if built_from.as_ref() == Some(&dependency_fingerprint)
        );
        revision_matches && built && packages_match
    }

    /// How many times the call graph has been built since this workspace was
    /// created. Callers that join an in-flight build do not add to it.
    pub fn call_graph_build_count(&self) -> u64 {
        self.call_graph_builds
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The fingerprint plus one message per loaded package that could not be
    /// inspected.
    ///
    /// A package deleted or renamed since it was loaded is left out rather
    /// than failing the whole workspace: the shorter fingerprint already
    /// forces the rebuild that drops it, and the message travels with the
    /// generation so the omission is visible.
    fn dependency_package_fingerprint_reporting(&self) -> (DependencyFingerprint, Vec<String>) {
        let mut fingerprint = Vec::new();
        let mut missing = Vec::new();
        for path in self.symbols.loaded_package_paths() {
            let stamp = std::fs::metadata(&path).and_then(|metadata| {
                let modified = metadata.modified()?;
                Ok((metadata.len(), modified))
            });
            match stamp {
                Ok((len, modified)) => fingerprint.push((path, len, modified)),
                Err(source) => {
                    tracing::warn!(
                        package = %path.display(),
                        %source,
                        "skipping a loaded package that can no longer be inspected"
                    );
                    missing.push(
                        DependencySourceError::InspectPackage {
                            path: path.clone(),
                            source,
                        }
                        .to_string(),
                    );
                }
            }
        }
        fingerprint.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        fingerprint.dedup_by(|left, right| left.0 == right.0);
        (fingerprint, missing)
    }

    /// Resolve the outgoing call edges of every workspace procedure in the
    /// cached call graph.
    ///
    /// The build resolves only high-fanout files eagerly; the rest resolve
    /// when a query reaches them. A query that reads *incoming* edges
    /// (`entrypoints`: who calls this?) or walks a subscriber's body (`trace`)
    /// never reaches them, so a procedure called only from a low-fanout file
    /// looked uncalled. Holding `call_graph_build_lock` keeps a rebuild from
    /// publishing a new graph while this fills in the current one; the lock
    /// order (build lock, then graph locks) is the builder's own.
    /// Already-resolved procedures are skipped, so repeat calls are cheap.
    pub fn complete_workspace_call_edges(&self) -> Result<(), CallGraphBuildError> {
        drop(self.get_or_build_call_graph()?);
        let _build_lock = self
            .call_graph_build_lock
            .lock()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph_build_lock"))?;
        let Some(insight) = self
            .insight_graph
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("insight_graph"))?
            .as_ref()
            .cloned()
        else {
            return Ok(());
        };
        let mut guard = self
            .call_graph
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph"))?;
        let Some(call_graph) = guard.as_mut() else {
            return Ok(());
        };
        let mut resolve = || {
            al_insight::calls::resolve_all_workspace_call_edges(
                &self.file_index,
                &self.symbols,
                &insight,
                call_graph,
            )
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(resolve)
            }
            _ => resolve(),
        }?;
        Ok(())
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
            let revision = self.call_graph_revision_now();
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
            let current = self.cached_revision_matches(
                &self.call_graph_revision,
                "call_graph_revision",
                revision,
            )?;
            if let Some(insight) = insight.filter(|_| {
                current
                    && cg_guard.is_some()
                    && fingerprint_guard.as_ref() == Some(&dependency_fingerprint)
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
            let revision = self.call_graph_revision_now();
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
            let current = self.cached_revision_matches(
                &self.call_graph_revision,
                "call_graph_revision",
                revision,
            )?;
            if let Some(insight) = insight.filter(|_| {
                current
                    && cg_guard.is_some()
                    && fingerprint_guard.as_ref() == Some(&dependency_fingerprint)
            }) {
                drop(fingerprint_guard);
                return Ok((insight, cg_guard));
            }
        }

        // Read before the build so an invalidation that lands while the build
        // runs leaves the published graphs tagged with the older revision, and
        // the next query rebuilds instead of reusing them.
        let built_at_insight_revision = self.insight_revision();
        let built_at_call_revision = self.call_graph_revision_now();
        // Counted so single-flight can be asserted: concurrent callers block
        // on `call_graph_build_lock` above, find the published graph in the
        // re-check, and never reach here.
        self.call_graph_builds
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let build_mark = self.call_graph_progress.begin();
        let build = || {
            let mut graph = InsightGraph::new();
            graph.build_from_index(&self.symbols);
            al_insight::calls::register_workspace_nodes(
                &self.file_index,
                &self.symbols,
                &mut graph,
            )?;
            let dependency_files = dependency_sources.files();
            al_insight::calls::register_dependency_summary_nodes(&dependency_files, &mut graph)?;
            let insight = Arc::new(graph);

            let mut cg = CallGraph::build_from_insight(&insight);
            al_insight::calls::populate_workspace_call_edges(
                &self.file_index,
                &self.symbols,
                &insight,
                &mut cg,
            )?;
            al_insight::calls::populate_summary_call_edges(
                &dependency_files,
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
        let mut ig_revision_guard = self
            .insight_graph_revision
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("insight_graph_revision"))?;
        let mut cg_revision_guard = self
            .call_graph_revision
            .write()
            .map_err(|_| WorkspaceStateError::poisoned("call_graph_revision"))?;
        *ig_guard = Some(Arc::clone(&insight));
        *cg_guard = Some(cg);
        *fingerprint_guard = Some(dependency_fingerprint);
        *ig_revision_guard = Some(built_at_insight_revision);
        *cg_revision_guard = Some(built_at_call_revision);
        drop(cg_revision_guard);
        drop(ig_revision_guard);
        drop(fingerprint_guard);
        drop(cg_guard);
        drop(ig_guard);
        build_mark.succeeded();

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
        let dependency_source = self
            .dependency_source_index
            .read()
            .map_err(|_| WorkspaceStateError::poisoned("dependency_source_index"))?
            .as_ref()
            .map(|cache| (cache.index.memory_stats(), cache.index.len()));
        let (dependency_source_index_memory, dependency_source_files) = match dependency_source {
            Some((stats, files)) => (Some(stats), files),
            None => (None, 0),
        };
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
            dependency_source_index_memory,
            dependency_source_files,
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
    /// The summarized AL source of every loaded package, when that index is
    /// built.
    pub dependency_source_index_memory: Option<DependencySourceMemoryStats>,
    pub dependency_source_files: usize,
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
    /// Configured symbol packages that could not be loaded (corrupt/truncated
    /// `.app` files). Initialization proceeds without them; callers can
    /// surface these as diagnostics/notifications.
    pub package_load_failures: Vec<al_symbols::PackageLoadFailure>,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreInitError {
    #[error(transparent)]
    Project(#[from] al_project::errors::DiscoveryError),
    #[error(transparent)]
    SourceScan(#[from] al_source::file_index::ScanError),
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
    let mut package_load_failures = Vec::new();

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
            // Load per package instead of atomically: one truncated/corrupt
            // `.app` in `.alpackages` (a common state after an interrupted
            // download) must not abort the entire workspace initialization.
            let (loaded, failures) = workspace
                .symbols
                .load_packages_cached_lenient(&project.packages, &cache);
            for failure in &failures {
                tracing::warn!(
                    path = %failure.path.display(),
                    message = %failure.message,
                    "workspace: skipping unreadable symbol package"
                );
            }
            package_load_failures = failures;
            total_symbols = loaded.iter().map(|p| p.object_count).sum();
            package_count = loaded.len();
            tracing::info!(
                packages = package_count,
                symbols = total_symbols,
                failed_packages = package_load_failures.len(),
                "workspace: loaded symbol packages"
            );
            // Load runtime enum definitions (compiler built-ins not in any package).
            workspace.symbols.load_runtime_enums();

            workspace.invalidate_insight_graph();

            let pkg_info: Vec<PackageInfo> = loaded.iter().map(PackageInfo::from).collect();
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
        package_load_failures,
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

/// Bring the file index in line with the `.al` files under `root` on disk:
/// re-read the ones whose size or modification time changed, add new ones,
/// drop deleted ones, and invalidate what was derived from them.
///
/// For callers that hold no editor overlays. The daemon behind al-explorer and
/// MCP read the workspace once at startup and answered from that snapshot
/// until it exited, so a file an agent had just written or edited was
/// invisible to every query. An editor's open documents are newer than the
/// disk, so the LSP server must not use this for them.
pub fn refresh_workspace_files(
    workspace: &Workspace,
    root: &std::path::Path,
) -> Result<al_source::file_index::ScanDelta, al_source::file_index::ScanError> {
    let delta = workspace.file_index.incremental_scan(root)?;
    if delta.is_empty() {
        return Ok(delta);
    }
    if delta.topology_changed {
        forget_vanished_workspace_objects(workspace);
    }
    // A table extension's edit changes the composed table under another
    // object's name, so the cache is dropped whole; it refills per object.
    workspace.symbols.invalidate_all_composed();
    if delta.topology_changed {
        workspace.invalidate_insight_graph();
    } else {
        workspace.invalidate_call_graph_only();
    }
    workspace.mark_generation_changed();
    Ok(delta)
}

/// Apply files the editor reported changed on disk and does not have open:
/// `Some(text)` re-indexes the file, `None` drops it. Then invalidate what
/// was derived from them, as [`refresh_workspace_files`] does.
pub fn apply_disk_changes(
    workspace: &Workspace,
    changes: Vec<(std::path::PathBuf, Option<String>)>,
) {
    if changes.is_empty() {
        return;
    }
    for (path, text) in changes {
        match text {
            Some(text) => workspace.file_index.add_file(path, text),
            None => workspace.file_index.remove_file(&path),
        }
    }
    forget_vanished_workspace_objects(workspace);
    workspace.symbols.invalidate_all_composed();
    workspace.invalidate_insight_graph();
    workspace.mark_generation_changed();
}

/// Drop the symbol entries of workspace objects no file declares any more.
///
/// They are re-registered when the call graph is next built; until then
/// `search` kept returning an object whose file had been deleted or renamed.
fn forget_vanished_workspace_objects(workspace: &Workspace) {
    workspace
        .symbols
        .retain_package_entries("workspace", |entry| {
            workspace
                .file_index
                .find_by_object_name(&entry.name)
                .is_some()
        });
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
mod source_cache_tests;
#[cfg(test)]
mod tests;
