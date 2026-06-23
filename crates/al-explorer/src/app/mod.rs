//! The object-browser `App` state plus its core list/selection logic.
//!
//! Details rendering (`update_details_items`), member hydration, and
//! "open in editor" live in the sibling [`details`] module.

mod details;

use std::sync::Arc;

use ratatui::text::Line;
use ratatui::widgets::ListState;

use al_protocol::DaemonClient;

use crate::types::{self, ObjectKind, SymbolEntry, SymbolIndex};
use crate::views::call_graph::CallGraphView;
use crate::views::event_chain::EventChainView;
use crate::views::profiler::ProfilerView;
use crate::views::test_runner::TestRunnerView;
use crate::{ActivePane, ClickTarget, DetailTarget, ViewMode, wrap_next, wrap_prev};

/// Payload handed from the background workspace-init thread to the event
/// loop: the connected daemon client plus the full symbol listing, or a
/// human-readable error.
type InitResult = Result<(DaemonClient, Vec<types::SymbolEntry>), String>;

pub(crate) struct App {
    pub(crate) view_mode: ViewMode,

    pub(crate) active_pane: ActivePane,
    pub(crate) search_query: String,
    pub(crate) global_search: bool,

    pub(crate) packages: Vec<String>,
    pub(crate) package_list_state: ListState,

    pub(crate) kinds: Vec<ObjectKind>,
    pub(crate) active_kind_index: usize,

    pub(crate) symbols: SymbolIndex,
    pub(crate) current_objects: Vec<Arc<SymbolEntry>>,
    pub(crate) object_list_state: ListState,

    pub(crate) details_list_state: ListState,
    pub(crate) details_items: Vec<(Option<DetailTarget>, Line<'static>)>,

    pub(crate) should_quit: bool,

    pub(crate) last_click_time: std::time::Instant,
    pub(crate) last_click_target: Option<ClickTarget>,
    pub(crate) last_click_index: usize,

    pub(crate) event_chain: EventChainView,
    pub(crate) call_graph: CallGraphView,
    pub(crate) profiler: ProfilerView,
    pub(crate) test_runner: TestRunnerView,

    // Persistent daemon connection for open_selected_object
    pub(crate) daemon_client: Option<DaemonClient>,

    // Background workspace-init handoff. `Some` while the init thread is
    // still running; the event loop polls it every tick so the first frame
    // renders immediately ("Loading workspace…") instead of blocking the
    // terminal for the whole daemon cold-start (FB-1).
    pub(crate) init_rx: Option<std::sync::mpsc::Receiver<InitResult>>,
    // Human-readable init state shown in the object browser while loading,
    // or the error if init failed.
    pub(crate) init_status: Option<String>,

    // Project root resolved once at startup. Subsequent code paths must use
    // this field rather than calling current_dir() again — the user can `cd`
    // after launching al-explorer, which would otherwise drift the daemon
    // socket key.
    pub(crate) project_root: std::path::PathBuf,
}

impl App {
    pub(crate) fn new() -> App {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        App {
            view_mode: ViewMode::ObjectBrowser,
            active_pane: ActivePane::Search,
            search_query: String::new(),
            global_search: false,
            packages: Vec::new(),
            package_list_state: ListState::default(),
            kinds: Vec::new(),
            active_kind_index: 0,
            symbols: SymbolIndex::new(),
            current_objects: Vec::new(),
            object_list_state: ListState::default(),
            details_list_state: ListState::default(),
            details_items: Vec::new(),
            should_quit: false,
            last_click_time: std::time::Instant::now() - std::time::Duration::from_secs(10),
            last_click_target: None,
            last_click_index: 0,
            event_chain: EventChainView::new(project_root.clone()),
            call_graph: CallGraphView::new(project_root.clone()),
            profiler: ProfilerView::new(),
            test_runner: TestRunnerView::new(project_root.clone()),
            daemon_client: None,
            init_rx: None,
            init_status: None,
            project_root,
        }
    }

    /// Kick off workspace init on a background thread so the UI renders
    /// immediately. `poll_init` integrates the result on the event loop.
    pub(crate) fn start_init_workspace(&mut self) {
        let root = self.project_root.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.init_rx = Some(rx);
        self.init_status = Some("Loading workspace symbols…".to_string());
        std::thread::spawn(move || {
            let _ = tx.send(Self::load_workspace_entries(&root));
        });
    }

    /// Connect to the daemon (auto-starting it) and fetch the full symbol
    /// listing. `request()` itself waits through "Workspace is initializing"
    /// for up to 60s, so no blind sleeps are needed here. A brief
    /// empty-result re-poll remains as a belt-and-suspenders for the window
    /// where the daemon answers before its package load has produced
    /// entries (ISSUE-071).
    fn load_workspace_entries(root: &std::path::Path) -> InitResult {
        let mut client = DaemonClient::connect(root)
            .map_err(|e| format!("Cannot connect to al-lsp daemon: {e}"))?;

        let mut entries: Vec<types::SymbolEntry> = Vec::new();
        for attempt in 0..20usize {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            let result = client
                .request(
                    "search",
                    Some(serde_json::json!({
                        "query": "",
                        "limit": 100_000,
                        // FB-1: slim entries (no member arrays) — a full
                        // dump is ~60 MB JSON. Members hydrate lazily per
                        // selected object (`hydrate_selected_object`).
                        "summary": true
                    })),
                )
                .map_err(|e| format!("search request failed: {e}"))?;

            let parsed: Vec<types::SymbolEntry> = serde_json::from_value(result)
                .map_err(|e| format!("Failed to deserialize symbol entries from daemon: {e}"))?;
            if !parsed.is_empty() {
                entries = parsed;
                break;
            }
        }

        if entries.is_empty() {
            return Err("Daemon returned no symbols. \
                Run 'al-lsp daemon --project .' first, then relaunch al-explorer."
                .to_string());
        }
        Ok((client, entries))
    }

    /// Poll the background init thread; returns once per tick from the
    /// event loop. Populates the browser on success, records the error on
    /// failure (shown in the objects pane).
    pub(crate) fn poll_init(&mut self) {
        let Some(rx) = self.init_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok((client, entries))) => {
                self.init_rx = None;
                self.init_status = None;
                self.daemon_client = Some(client);
                self.symbols.load(entries);
                self.packages = self.symbols.package_names();
                if !self.packages.is_empty() {
                    self.package_list_state.select(Some(0));
                    self.update_objects_list(true);
                }
            }
            Ok(Err(e)) => {
                self.init_rx = None;
                self.init_status = Some(format!("Workspace load failed: {e}"));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.init_rx = None;
                self.init_status = Some("Workspace load thread died unexpectedly".to_string());
            }
        }
    }

    pub(crate) fn update_objects_list(&mut self, reset_selection: bool) {
        if let Some(selected) = self.package_list_state.selected()
            && let Some(pkg_name) = self.packages.get(selected)
        {
            // `search()` already applies the name/id query filter, so its
            // results are pre-filtered; `search_in_package()` returns the whole
            // package and still needs filtering when a query is present.
            let global = self.global_search && !self.search_query.is_empty();
            let results = if global {
                self.symbols.search(&self.search_query, 5000)
            } else {
                self.symbols.search_in_package(pkg_name)
            };

            let query = self.search_query.to_lowercase();

            // We only need to re-filter for the per-package branch with a
            // non-empty query — the global branch is already query-filtered, and
            // an empty query matches everything. Avoiding the redundant
            // `to_lowercase()`/`contains()` pass matters because this runs on
            // every keystroke over up to 5000 results.
            let needs_filter = !global && !query.is_empty();

            let mut filtered = Vec::new();
            let mut kinds_set = std::collections::HashSet::new();

            if needs_filter {
                for r in results {
                    let matches_search =
                        r.name.to_lowercase().contains(&query) || r.id.to_string().contains(&query);
                    if matches_search {
                        kinds_set.insert(r.kind);
                        filtered.push(r);
                    }
                }
            } else {
                for r in results {
                    kinds_set.insert(r.kind);
                    filtered.push(r);
                }
            }

            let mut kinds: Vec<_> = kinds_set.into_iter().collect();
            kinds.sort_by_key(|k| k.as_str());

            let current_kind = self.kinds.get(self.active_kind_index).copied();
            self.kinds = kinds;

            if let Some(k) = current_kind {
                if let Some(new_idx) = self.kinds.iter().position(|&x| x == k) {
                    self.active_kind_index = new_idx;
                } else {
                    self.active_kind_index = 0;
                }
            } else {
                self.active_kind_index = 0;
            }

            let mut final_objects = Vec::new();
            if let Some(k) = self.kinds.get(self.active_kind_index) {
                final_objects = filtered.into_iter().filter(|r| r.kind == *k).collect();
                final_objects.sort_by_key(|e| e.id);
            }

            self.current_objects = final_objects;

            if !self.current_objects.is_empty() {
                if reset_selection {
                    self.object_list_state.select(Some(0));
                    self.details_list_state.select(Some(0));
                } else {
                    let current = self.object_list_state.selected().unwrap_or(0);
                    let safe_idx =
                        std::cmp::min(current, self.current_objects.len().saturating_sub(1));
                    self.object_list_state.select(Some(safe_idx));
                    self.details_list_state.select(Some(0));
                }
            } else {
                self.object_list_state.select(None);
                self.details_list_state.select(None);
            }
        }
        self.update_details_items();
    }

    pub(crate) fn next_package(&mut self) {
        let i = wrap_next(self.package_list_state.selected(), self.packages.len());
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    pub(crate) fn previous_package(&mut self) {
        let i = wrap_prev(self.package_list_state.selected(), self.packages.len());
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    pub(crate) fn next_kind(&mut self) {
        if self.kinds.is_empty() {
            return;
        }
        self.active_kind_index = (self.active_kind_index + 1) % self.kinds.len();
        self.update_objects_list(true);
    }

    pub(crate) fn previous_kind(&mut self) {
        if self.kinds.is_empty() {
            return;
        }
        if self.active_kind_index == 0 {
            self.active_kind_index = self.kinds.len() - 1;
        } else {
            self.active_kind_index -= 1;
        }
        self.update_objects_list(true);
    }

    pub(crate) fn next_object(&mut self) {
        if !self.current_objects.is_empty() {
            let i = wrap_next(
                self.object_list_state.selected(),
                self.current_objects.len(),
            );
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
            self.update_details_items();
        }
    }

    pub(crate) fn previous_object(&mut self) {
        if !self.current_objects.is_empty() {
            let i = wrap_prev(
                self.object_list_state.selected(),
                self.current_objects.len(),
            );
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
            self.update_details_items();
        }
    }

    pub(crate) fn next_detail(&mut self) {
        if !self.details_items.is_empty() {
            let i = wrap_next(self.details_list_state.selected(), self.details_items.len());
            self.details_list_state.select(Some(i));
        }
    }

    pub(crate) fn previous_detail(&mut self) {
        let i = match self.details_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.details_items.len().saturating_sub(1)
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        if !self.details_items.is_empty() {
            self.details_list_state.select(Some(i));
        }
    }

    pub(crate) fn register_click(&mut self, target: ClickTarget, index: usize) -> bool {
        let now = std::time::Instant::now();
        let is_double = self.last_click_target == Some(target)
            && self.last_click_index == index
            && now.duration_since(self.last_click_time) < std::time::Duration::from_millis(450);
        self.last_click_time = now;
        self.last_click_target = Some(target);
        self.last_click_index = index;
        is_double
    }
}
