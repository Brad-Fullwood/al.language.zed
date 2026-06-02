// `collapsible_match` would force `Event::Mouse(mouse_event)` arms into
// `Event::Mouse(mouse_event) if app.view_mode == ViewMode::X` style guards,
// which makes the per-mode dispatch table noticeably less skimmable. The
// expanded form is intentional in this file's TUI event dispatcher.
#![allow(clippy::collapsible_match)]

#[cfg(unix)]
mod cli;
#[cfg(unix)]
mod types;
#[cfg(unix)]
use clap::Parser;
#[cfg(unix)]
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
#[cfg(unix)]
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
#[cfg(unix)]
use std::process::ExitCode;
#[cfg(unix)]
use std::{error::Error, io, sync::Arc};
#[cfg(unix)]
use types::{ObjectKind, SymbolEntry, SymbolIndex};

#[cfg(unix)]
use al_protocol::DaemonClient;

// ---------------------------------------------------------------------------
// Navigation helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
/// Advance a wrap-around list index forward by one.
///
/// Returns the next index (wrapping from the last item back to 0).
/// If `current` is `None` (no selection), starts at index 0.
#[inline]
fn wrap_next(current: Option<usize>, len: usize) -> usize {
    match current {
        Some(i) if i >= len.saturating_sub(1) => 0,
        Some(i) => i + 1,
        None => 0,
    }
}

#[cfg(unix)]
/// Retreat a wrap-around list index backward by one.
///
/// Returns the previous index (wrapping from 0 to the last item).
/// If `current` is `None` (no selection), starts at index 0.
#[inline]
fn wrap_prev(current: Option<usize>, len: usize) -> usize {
    match current {
        Some(0) | None => len.saturating_sub(1),
        Some(i) => i - 1,
    }
}

/// Connect to the al-lsp daemon if not already connected, recording any
/// connection failure in `status`.
#[cfg(unix)]
fn ensure_daemon_client(
    client: &mut Option<DaemonClient>,
    project_root: &std::path::Path,
    status: &mut String,
) {
    if client.is_none() {
        match DaemonClient::connect(project_root) {
            Ok(c) => *client = Some(c),
            Err(e) => *status = format!("Cannot connect to daemon: {e}"),
        }
    }
}

/// Move a list selection one step (forward or backward) with wrap-around,
/// doing nothing when the list is empty.
#[cfg(unix)]
fn advance_list_selection(list_state: &mut ListState, len: usize, forward: bool) {
    if len == 0 {
        return;
    }
    let i = if forward {
        wrap_next(list_state.selected(), len)
    } else {
        wrap_prev(list_state.selected(), len)
    };
    list_state.select(Some(i));
}

/// Highlight style for a focused text input (bold yellow) vs. unfocused
/// (dark gray).
#[cfg(unix)]
fn input_focused_style(is_focused: bool) -> Style {
    if is_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Highlight style for the active pane (bold yellow) vs. inactive (dark gray).
#[cfg(unix)]
fn pane_style(is_active: bool) -> Style {
    if is_active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

// ---------------------------------------------------------------------------
// View mode
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[derive(PartialEq, Clone, Copy)]
enum ViewMode {
    ObjectBrowser,
    EventChain,
    CallGraph,
    Profiler,
    TestRunner,
}

// ---------------------------------------------------------------------------
// Object browser types
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[derive(PartialEq, Clone, Copy)]
enum ActivePane {
    Search,
    Packages,
    Objects,
    Details,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickTarget {
    Objects,
    Details,
}

#[cfg(unix)]
/// What kind of member a `DetailTarget` refers to.
/// Stored for future use (deep-link precision when virtual file support
/// is added in ISSUE-017 follow-up work).
#[allow(dead_code)]
#[derive(Debug, Clone)]
enum DetailTargetKind {
    Field,
    Key,
    Control(String),
    EnumValue,
    Procedure,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
struct DetailTarget {
    name: String,
    #[allow(dead_code)]
    kind: DetailTargetKind,
}

// ---------------------------------------------------------------------------
// Event chain view
// ---------------------------------------------------------------------------

#[cfg(unix)]
/// A single row shown in the event chain results list.
#[derive(Debug, Clone)]
struct TraceRow {
    depth: usize,
    edge_type: String,
    node_type: String,
    name: String,
    object: String,
}

#[cfg(unix)]
struct EventChainView {
    /// Current text in the search input.
    query: String,
    /// Whether the search input is focused (vs. the results list).
    input_focused: bool,
    /// Flattened trace rows from the daemon.
    rows: Vec<TraceRow>,
    list_state: ListState,
    /// Status/error message shown below the list.
    status: String,
    /// Daemon client (None if not connected).
    client: Option<DaemonClient>,
    project_root: std::path::PathBuf,
}

#[cfg(unix)]
impl EventChainView {
    fn new(project_root: std::path::PathBuf) -> Self {
        Self {
            query: String::new(),
            input_focused: true,
            rows: Vec::new(),
            list_state: ListState::default(),
            status: String::from("Type an event name and press Enter to trace"),
            client: None,
            project_root,
        }
    }

    fn ensure_client(&mut self) {
        ensure_daemon_client(&mut self.client, &self.project_root, &mut self.status);
    }

    fn run_trace(&mut self) {
        if self.query.trim().is_empty() {
            self.status = "Enter an event name to search".to_string();
            return;
        }
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };
        let params = serde_json::json!({ "event": self.query.trim(), "depth": 10 });
        match client.request("trace", Some(params)) {
            Ok(val) => {
                self.rows.clear();
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        self.rows.push(TraceRow {
                            depth: item.get("depth").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                            edge_type: item
                                .get("edgeType")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            node_type: item
                                .get("nodeType")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            name: item
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                            object: item
                                .get("object")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string(),
                        });
                    }
                    if self.rows.is_empty() {
                        self.status = format!("No event chain found for '{}'", self.query.trim());
                    } else {
                        self.status = format!(
                            "{} steps in event chain for '{}'",
                            self.rows.len(),
                            self.query.trim()
                        );
                        self.list_state.select(Some(0));
                    }
                } else {
                    self.status = "Unexpected response format from daemon".to_string();
                }
            }
            Err(e) => {
                // Connection may have dropped — reset so next query reconnects
                self.client = None;
                self.status = format!("Daemon error: {e}");
            }
        }
    }

    fn next_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.rows.len(), true);
    }

    fn prev_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.rows.len(), false);
    }
}

// ---------------------------------------------------------------------------
// Call graph view
// ---------------------------------------------------------------------------

#[cfg(unix)]
/// A single row shown in the call graph results list.
#[derive(Debug, Clone)]
struct CallRow {
    label: String,
    kind: CallRowKind,
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq)]
enum CallRowKind {
    Header,
    Entry,
}

#[cfg(unix)]
struct CallGraphView {
    /// Current text in the search input.
    query: String,
    /// Whether the search input is focused.
    input_focused: bool,
    rows: Vec<CallRow>,
    list_state: ListState,
    status: String,
    client: Option<DaemonClient>,
    project_root: std::path::PathBuf,
}

#[cfg(unix)]
impl CallGraphView {
    fn new(project_root: std::path::PathBuf) -> Self {
        Self {
            query: String::new(),
            input_focused: true,
            rows: Vec::new(),
            list_state: ListState::default(),
            status: String::from("Type a symbol name and press Enter to query"),
            client: None,
            project_root,
        }
    }

    fn ensure_client(&mut self) {
        ensure_daemon_client(&mut self.client, &self.project_root, &mut self.status);
    }

    fn run_query(&mut self) {
        if self.query.trim().is_empty() {
            self.status = "Enter a symbol name to search".to_string();
            return;
        }
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };
        let params = serde_json::json!({ "symbol": self.query.trim() });
        match client.request("impact", Some(params)) {
            Ok(val) => {
                self.rows.clear();
                let symbol = val
                    .get("symbol")
                    .and_then(|v| v.as_str())
                    .unwrap_or(self.query.trim());
                self.rows.push(CallRow {
                    label: format!("Impact analysis for: {symbol}"),
                    kind: CallRowKind::Header,
                });
                if let Some(impacted) = val.get("impacted").and_then(|v| v.as_array()) {
                    if impacted.is_empty() {
                        self.rows.push(CallRow {
                            label: "  (no impacted symbols found)".to_string(),
                            kind: CallRowKind::Entry,
                        });
                    } else {
                        self.rows.push(CallRow {
                            label: format!("  Impacted symbols ({}):", impacted.len()),
                            kind: CallRowKind::Header,
                        });
                        for entry in impacted {
                            let display =
                                entry.as_str().map(|s| s.to_string()).unwrap_or_else(|| {
                                    serde_json::to_string(entry).unwrap_or_default()
                                });
                            self.rows.push(CallRow {
                                label: format!("    {display}"),
                                kind: CallRowKind::Entry,
                            });
                        }
                    }
                }
                self.status = format!("Impact query complete for '{}'", self.query.trim());
                if !self.rows.is_empty() {
                    self.list_state.select(Some(0));
                }
            }
            Err(e) => {
                self.client = None;
                self.status = format!("Daemon error: {e}");
            }
        }
    }

    fn next_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.rows.len(), true);
    }

    fn prev_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.rows.len(), false);
    }
}

// ---------------------------------------------------------------------------
// Profiler view
// ---------------------------------------------------------------------------

#[cfg(unix)]
/// A single hotspot row parsed from a `.alcpuprofile` file.
#[derive(Debug, Clone)]
struct HotspotRow {
    procedure: String,
    object: String,
    self_time_ms: f64,
    total_time_ms: f64,
    hit_count: u64,
}

#[cfg(unix)]
struct ProfilerView {
    /// File path input typed by the user.
    file_path: String,
    /// Whether the file path input is focused.
    input_focused: bool,
    /// Parsed hotspot rows.
    hotspots: Vec<HotspotRow>,
    list_state: ListState,
    /// Status/error message.
    status: String,
    /// Total session duration (ms).
    duration_ms: f64,
}

#[cfg(unix)]
impl ProfilerView {
    fn new() -> Self {
        Self {
            file_path: String::new(),
            input_focused: true,
            hotspots: Vec::new(),
            list_state: ListState::default(),
            status: String::from("Enter path to .alcpuprofile and press Enter to load"),
            duration_ms: 0.0,
        }
    }

    /// Parse a Chrome-style `.alcpuprofile` JSON file and populate `hotspots`.
    fn load_profile(&mut self) {
        let path = self.file_path.trim().to_string();
        if path.is_empty() {
            self.status = "No file path entered".to_string();
            return;
        }
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("Cannot read file: {e}");
                return;
            }
        };
        // Strip UTF-8 BOM if present
        let data = if data.starts_with(&[0xEF, 0xBB, 0xBF]) {
            &data[3..]
        } else {
            &data[..]
        };

        let json: serde_json::Value = match serde_json::from_slice(data) {
            Ok(v) => v,
            Err(e) => {
                self.status = format!("JSON parse error: {e}");
                return;
            }
        };

        // Compute session duration
        let start = json
            .get("startTime")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let end = json.get("endTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
        // Chrome profiles use microseconds
        self.duration_ms = (end - start) / 1000.0;

        let nodes = match json.get("nodes").and_then(|v| v.as_array()) {
            Some(n) => n,
            None => {
                self.status = "No 'nodes' array found in profile".to_string();
                return;
            }
        };

        // Build a map: node id -> (functionName, url, hitCount)
        let mut rows: Vec<HotspotRow> = Vec::new();
        for node in nodes {
            let hit_count = node.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
            if hit_count == 0 {
                continue;
            }
            let call_frame = node.get("callFrame").unwrap_or(&serde_json::Value::Null);
            let function_name = call_frame
                .get("functionName")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)")
                .to_string();
            let url = call_frame
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Skip internal/empty nodes
            if function_name == "(root)"
                || function_name == "(idle)"
                || function_name == "(garbage collector)"
            {
                continue;
            }

            // hitCount in Chrome's CPU profile format is the number of times
            // the sampler observed this node at the top of the stack. It is
            // NOT a millisecond duration — the actual durations live in the
            // top-level `timeDeltas` array, which we don't aggregate yet.
            //
            // Treating hit_count as ms is a deliberately rough approximation
            // that's only accurate when the sampling interval happens to be
            // 1 ms (BC's default in the alcpuprofile producer). It's good
            // enough for ranking hotspots — which is all this view shows —
            // but mis-reports raw "self_time_ms" for any other interval.
            // TODO(profiler): aggregate timeDeltas per node for true ms.
            let self_time_ms = hit_count as f64;
            rows.push(HotspotRow {
                procedure: function_name,
                object: url,
                self_time_ms,
                total_time_ms: self_time_ms, // simplified: no call tree aggregation
                hit_count,
            });
        }

        // Sort descending by self_time_ms
        rows.sort_by(|a, b| {
            b.self_time_ms
                .partial_cmp(&a.self_time_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let count = rows.len();
        self.hotspots = rows;
        self.status = if count == 0 {
            "No hotspots found in profile (all hitCount=0?)".to_string()
        } else {
            format!(
                "{count} hotspots loaded — duration {:.1}ms",
                self.duration_ms
            )
        };
        if !self.hotspots.is_empty() {
            self.list_state.select(Some(0));
            self.input_focused = false;
        }
    }

    fn next_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.hotspots.len(), true);
    }

    fn prev_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.hotspots.len(), false);
    }
}

// ---------------------------------------------------------------------------
// Test runner view
// ---------------------------------------------------------------------------

#[cfg(unix)]
/// Status of a single test method as reported by the daemon.
#[derive(Debug, Clone, PartialEq)]
enum MethodStatus {
    NotRun,
    Pass { duration_ms: u64 },
    Fail { error: Option<String> },
    Skip,
}

#[cfg(unix)]
/// A single row in the test runner tree — either a codeunit header or a method.
#[derive(Debug, Clone)]
enum TestRow {
    Codeunit {
        name: String,
        id: i32,
    },
    Method {
        codeunit_id: i32,
        name: String,
        status: MethodStatus,
    },
}

#[cfg(unix)]
struct TestRunnerView {
    rows: Vec<TestRow>,
    list_state: ListState,
    status: String,
    client: Option<DaemonClient>,
    project_root: std::path::PathBuf,
}

#[cfg(unix)]
impl TestRunnerView {
    fn new(project_root: std::path::PathBuf) -> Self {
        Self {
            rows: Vec::new(),
            list_state: ListState::default(),
            status: String::from("Press 'r' to run selected, 'R' to run all"),
            client: None,
            project_root,
        }
    }

    fn ensure_client(&mut self) {
        ensure_daemon_client(&mut self.client, &self.project_root, &mut self.status);
    }

    /// Discover tests and load last results, populating `rows`.
    fn refresh_discovery(&mut self) {
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };

        // Discover test codeunits / methods.
        let discovered = match client.request("tests.discover", None) {
            Ok(v) => v,
            Err(e) => {
                self.client = None;
                self.status = format!("Daemon error (discover): {e}");
                return;
            }
        };

        // Optionally load last results.
        let last_results = client
            .request("tests.last_results", None)
            .unwrap_or(serde_json::Value::Null);

        self.rows.clear();
        let Some(codeunits) = discovered.as_array() else {
            self.status = "No test codeunits found".to_string();
            return;
        };

        for cu in codeunits {
            let cu_name = cu
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("(unknown)")
                .to_string();
            let cu_id = cu.get("id").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            self.rows.push(TestRow::Codeunit {
                name: cu_name,
                id: cu_id,
            });

            if let Some(tests) = cu.get("tests").and_then(|v| v.as_array()) {
                for t in tests {
                    let method_name = t
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)")
                        .to_string();
                    // Look up status from last_results.
                    let status = find_method_status(&last_results, cu_id, &method_name);
                    self.rows.push(TestRow::Method {
                        codeunit_id: cu_id,
                        name: method_name,
                        status,
                    });
                }
            }
        }

        if self.rows.is_empty() {
            self.status = "No test codeunits discovered".to_string();
        } else {
            self.status = format!("{} row(s) loaded", self.rows.len());
            self.list_state.select(Some(0));
        }
    }

    /// Run the currently-selected codeunit (if the selected row is a Codeunit or Method).
    fn run_selected(&mut self) {
        let codeunit_id = match self.list_state.selected().and_then(|i| self.rows.get(i)) {
            Some(TestRow::Codeunit { id, .. }) => *id,
            Some(TestRow::Method { codeunit_id, .. }) => *codeunit_id,
            None => {
                self.status = "Nothing selected".to_string();
                return;
            }
        };
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };
        let params = serde_json::json!({ "codeunitIds": [codeunit_id] });
        match client.request("tests.run_batch", Some(params)) {
            Ok(_) => {
                self.status = format!("Run complete for codeunit {codeunit_id}. Refreshing…");
            }
            Err(e) => {
                self.client = None;
                self.status = format!("Daemon error (run_batch): {e}");
                return;
            }
        }
        self.refresh_discovery();
    }

    /// Run all discovered tests.
    fn run_all(&mut self) {
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match client.request("tests.run_auto", None) {
            Ok(_) => {
                self.status = "Run all complete. Refreshing…".to_string();
            }
            Err(e) => {
                self.client = None;
                self.status = format!("Daemon error (run_auto): {e}");
                return;
            }
        }
        self.refresh_discovery();
    }

    fn next_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.rows.len(), true);
    }

    fn prev_row(&mut self) {
        advance_list_selection(&mut self.list_state, self.rows.len(), false);
    }

    /// Return the error message of the currently-selected method (if any).
    fn selected_error(&self) -> Option<&str> {
        match self.list_state.selected().and_then(|i| self.rows.get(i)) {
            Some(TestRow::Method {
                status: MethodStatus::Fail { error: Some(e) },
                ..
            }) => Some(e.as_str()),
            _ => None,
        }
    }
}

#[cfg(unix)]
/// Look up the run status for `(codeunit_id, method_name)` in a JSON
/// last-results response.  Returns `NotRun` if not found.
fn find_method_status(
    last_results: &serde_json::Value,
    codeunit_id: i32,
    method_name: &str,
) -> MethodStatus {
    let arr = match last_results.as_array() {
        Some(a) => a,
        None => return MethodStatus::NotRun,
    };
    let lower = method_name.to_lowercase();
    let matching = arr.iter().find(|r| {
        r.get("codeunitId").and_then(|v| v.as_i64()) == Some(codeunit_id as i64)
            && r.get("methodName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase())
                .as_deref()
                == Some(&lower)
    });
    let Some(r) = matching else {
        return MethodStatus::NotRun;
    };
    let status_str = r.get("status").and_then(|v| v.as_str()).unwrap_or("");
    match status_str {
        "pass" | "Pass" => MethodStatus::Pass {
            duration_ms: r.get("durationMs").and_then(|v| v.as_u64()).unwrap_or(0),
        },
        "fail" | "Fail" => MethodStatus::Fail {
            error: r
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        },
        "skip" | "Skip" => MethodStatus::Skip,
        _ => MethodStatus::NotRun,
    }
}

// ---------------------------------------------------------------------------
// Main application
// ---------------------------------------------------------------------------

#[cfg(unix)]
struct App {
    pub view_mode: ViewMode,

    // Object browser state
    pub active_pane: ActivePane,
    pub search_query: String,
    pub global_search: bool,

    pub packages: Vec<String>,
    pub package_list_state: ListState,

    pub kinds: Vec<ObjectKind>,
    pub active_kind_index: usize,

    pub symbols: SymbolIndex,
    pub current_objects: Vec<Arc<SymbolEntry>>,
    pub object_list_state: ListState,

    pub details_list_state: ListState,
    pub details_items: Vec<(Option<DetailTarget>, Line<'static>)>,

    pub should_quit: bool,

    pub last_click_time: std::time::Instant,
    pub last_click_target: Option<ClickTarget>,
    pub last_click_index: usize,

    // Event chain, call graph, profiler, and test runner views
    pub event_chain: EventChainView,
    pub call_graph: CallGraphView,
    pub profiler: ProfilerView,
    pub test_runner: TestRunnerView,

    // Persistent daemon connection for open_selected_object
    pub daemon_client: Option<DaemonClient>,

    // Project root resolved once at startup. Subsequent code paths must use
    // this field rather than calling current_dir() again — the user can `cd`
    // after launching al-explorer, which would otherwise drift the daemon
    // socket key.
    pub project_root: std::path::PathBuf,
}

#[cfg(unix)]
impl App {
    fn new() -> App {
        // Resolve project_root ONCE here. al-explorer is long-running and the
        // user can `cd` after launch, so we must not re-call current_dir() in
        // later code paths or the daemon socket key would drift.
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
            project_root,
        }
    }

    fn init_workspace(&mut self) -> Result<(), Box<dyn Error>> {
        let root = self.project_root.clone();
        let mut client = DaemonClient::connect(&root)
            .map_err(|e| format!("Cannot connect to al-lsp daemon: {e}"))?;

        // ISSUE-071: daemon may still be loading packages at startup.
        // Retry up to 5 times with 800ms delay if the result is empty.
        let mut entries: Vec<types::SymbolEntry> = Vec::new();
        for attempt in 0..5usize {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_millis(800));
            }
            let result = client
                .request(
                    "search",
                    Some(serde_json::json!({
                        "query": "",
                        "limit": 100_000
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
            return Err("Daemon returned no symbols after 5 attempts. \
                Run 'al-lsp daemon --project .' first, then relaunch al-explorer."
                .into());
        }

        self.daemon_client = Some(client);
        self.symbols.load(entries);
        self.packages = self.symbols.package_names();

        if !self.packages.is_empty() {
            self.package_list_state.select(Some(0));
            self.update_objects_list(true);
        }
        Ok(())
    }

    fn update_objects_list(&mut self, reset_selection: bool) {
        if let Some(selected) = self.package_list_state.selected()
            && let Some(pkg_name) = self.packages.get(selected)
        {
            let results = if self.global_search && !self.search_query.is_empty() {
                self.symbols.search(&self.search_query, 5000)
            } else {
                self.symbols.search_in_package(pkg_name)
            };

            let query = self.search_query.to_lowercase();

            let mut filtered = Vec::new();
            let mut kinds_set = std::collections::HashSet::new();

            for r in results {
                let matches_search = query.is_empty()
                    || r.name.to_lowercase().contains(&query)
                    || r.id.to_string().contains(&query);

                if matches_search {
                    kinds_set.insert(r.kind);
                    filtered.push(r);
                }
            }

            let mut kinds: Vec<_> = kinds_set.into_iter().collect();
            kinds.sort_by_key(|k| format!("{:?}", k));

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

    fn next_package(&mut self) {
        let i = wrap_next(self.package_list_state.selected(), self.packages.len());
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    fn previous_package(&mut self) {
        let i = wrap_prev(self.package_list_state.selected(), self.packages.len());
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    fn next_kind(&mut self) {
        if self.kinds.is_empty() {
            return;
        }
        self.active_kind_index = (self.active_kind_index + 1) % self.kinds.len();
        self.update_objects_list(true);
    }

    fn previous_kind(&mut self) {
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

    fn next_object(&mut self) {
        let i = match self.object_list_state.selected() {
            Some(i) => {
                if i >= self.current_objects.len().saturating_sub(1) {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        if !self.current_objects.is_empty() {
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
            self.update_details_items();
        }
    }

    fn previous_object(&mut self) {
        let i = match self.object_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.current_objects.len().saturating_sub(1)
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        if !self.current_objects.is_empty() {
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
            self.update_details_items();
        }
    }

    fn update_details_items(&mut self) {
        self.details_items.clear();

        if let Some(selected) = self.object_list_state.selected()
            && let Some(entry) = self.current_objects.get(selected)
        {
            self.details_items.push((
                None,
                Line::from(vec![
                    Span::styled(
                        format!("{:?} ", entry.kind),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(entry.id.to_string(), Style::default().fg(Color::Cyan)),
                    Span::raw(" ".to_string()),
                    Span::styled(
                        entry.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]),
            ));

            if let Some(extends) = &entry.extends {
                self.details_items.push((
                    None,
                    Line::from(vec![
                        Span::styled(
                            "Extends: ".to_string(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(extends.clone()),
                    ]),
                ));
            }

            self.details_items.push((
                None,
                Line::from(vec![
                    Span::styled(
                        "Package: ".to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(entry.package.clone()),
                ]),
            ));

            if !entry.properties.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        "Properties:".to_string(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for p in entry.properties.iter() {
                    self.details_items.push((
                        None,
                        Line::from(vec![
                            Span::raw("    ".to_string()),
                            Span::styled(
                                format!("{:<20}", p.name),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::raw(" = ".to_string()),
                            Span::raw(p.value.clone()),
                        ]),
                    ));
                }
            }

            if !entry.keys.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Keys ({}):", entry.keys.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for k in entry.keys.iter() {
                    let fields = k.field_names.join(", ");
                    self.details_items.push((
                        Some(DetailTarget {
                            name: k.name.clone(),
                            kind: DetailTargetKind::Key,
                        }),
                        Line::from(vec![
                            Span::raw("    ".to_string()),
                            Span::styled(
                                format!("{:<20}", k.name),
                                Style::default().fg(Color::Cyan),
                            ),
                            Span::raw(format!(" ({})", fields)),
                        ]),
                    ));
                }
            }

            if !entry.fields.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Fields ({}):", entry.fields.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for f in entry.fields.iter() {
                    self.details_items.push((
                        Some(DetailTarget {
                            name: f.name.clone(),
                            kind: DetailTargetKind::Field,
                        }),
                        Line::from(vec![
                            Span::styled(
                                format!("    {:<4} ", f.id),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::styled(
                                format!("{:<30}", f.name),
                                Style::default().fg(Color::White),
                            ),
                            Span::styled(
                                format!(" : {}", f.type_name),
                                Style::default().fg(Color::Cyan),
                            ),
                        ]),
                    ));
                }
            }

            if !entry.controls.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Controls/Actions ({}):", entry.controls.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for c in entry.controls.iter() {
                    self.details_items.push((
                        Some(DetailTarget {
                            name: c.name.clone(),
                            kind: DetailTargetKind::Control(c.kind.clone()),
                        }),
                        Line::from(vec![
                            Span::raw("    ".to_string()),
                            Span::styled(
                                format!("{:<15}", c.kind),
                                Style::default().fg(Color::Magenta),
                            ),
                            Span::raw(format!(" {}", c.name)),
                        ]),
                    ));
                }
            }

            if !entry.enum_values.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Values ({}):", entry.enum_values.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for v in entry.enum_values.iter() {
                    self.details_items.push((
                        Some(DetailTarget {
                            name: v.name.clone(),
                            kind: DetailTargetKind::EnumValue,
                        }),
                        Line::from(vec![
                            Span::styled(
                                format!("    {:<4} ", v.ordinal),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::raw(v.name.clone()),
                        ]),
                    ));
                }
            }

            if !entry.methods.is_empty() {
                self.details_items.push((None, Line::from("".to_string())));
                self.details_items.push((
                    None,
                    Line::from(Span::styled(
                        format!("Procedures ({}):", entry.methods.len()),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
                ));
                for m in entry.methods.iter() {
                    let mut spans = vec![Span::raw("    ".to_string())];
                    if m.is_local {
                        spans.push(Span::styled("local ", Style::default().fg(Color::DarkGray)));
                    } else {
                        spans.push(Span::raw("      ".to_string()));
                    }
                    spans.push(Span::styled(
                        m.name.clone(),
                        Style::default().fg(Color::Green),
                    ));
                    spans.push(Span::raw("(".to_string()));

                    let params = m
                        .parameters
                        .iter()
                        .map(|p| p.name.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    spans.push(Span::raw(params));

                    spans.push(Span::raw(")".to_string()));

                    if let Some(ret) = &m.return_type {
                        spans.push(Span::styled(
                            format!(" : {}", ret),
                            Style::default().fg(Color::Cyan),
                        ));
                    }

                    self.details_items.push((
                        Some(DetailTarget {
                            name: m.name.clone(),
                            kind: DetailTargetKind::Procedure,
                        }),
                        Line::from(spans),
                    ));
                }
            }
        }

        if self.details_items.is_empty() {
            self.details_items
                .push((None, Line::from("No object selected".to_string())));
        }
    }

    fn next_detail(&mut self) {
        let i = match self.details_list_state.selected() {
            Some(i) => {
                if i >= self.details_items.len().saturating_sub(1) {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        if !self.details_items.is_empty() {
            self.details_list_state.select(Some(i));
        }
    }

    fn previous_detail(&mut self) {
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

    fn open_selected_object(&mut self) {
        let target_member: Option<DetailTarget> = self
            .details_list_state
            .selected()
            .and_then(|idx| self.details_items.get(idx))
            .and_then(|(m, _)| m.clone());

        if let Some(selected) = self.object_list_state.selected()
            && let Some(entry) = self.current_objects.get(selected)
        {
            // Ask the daemon for the workspace file path for this object.
            // Reconnect if the persistent client has been dropped.
            if self.daemon_client.is_none() {
                match DaemonClient::connect(&self.project_root) {
                    Ok(c) => self.daemon_client = Some(c),
                    Err(e) => {
                        // App has no status bar field (status lives on
                        // sub-views). Write to stderr so the user sees the
                        // cause after the TUI exits — otherwise the
                        // double-click silently does nothing and a missing
                        // daemon looks indistinguishable from a missing
                        // workspace path.
                        eprintln!(
                            "al-explorer: daemon connect failed (project_root={}): {e}",
                            self.project_root.display()
                        );
                    }
                }
            }
            if let Some(client) = self.daemon_client.as_mut() {
                let loc_result = client.request(
                    "location",
                    Some(serde_json::json!({
                        "name": entry.name,
                        "kind": format!("{:?}", entry.kind),
                        "id": entry.id,
                    })),
                );
                match loc_result {
                    Ok(val) => {
                        if let Some(path_str) = val.get("path").and_then(|v| v.as_str()) {
                            let abs_path = std::path::Path::new(path_str);
                            let line = if let Some(member) = &target_member {
                                find_member_line_in_file(abs_path, &member.name)
                                    .map(|l| l + 1)
                                    .unwrap_or(1)
                            } else {
                                1
                            };
                            // ISSUE-078: use `zed <path>:<line>:<col>` CLI instead of
                            // zed:// URL which is unreliable on Linux.
                            let file_spec = format!("{}:{}:1", path_str, line);
                            if let Err(e) =
                                std::process::Command::new("zed").arg(&file_spec).spawn()
                            {
                                eprintln!("al-explorer: failed to spawn 'zed {file_spec}': {e}");
                            }
                        }
                    }
                    Err(_) => {
                        // Connection may have dropped; reset so next call reconnects.
                        self.daemon_client = None;
                    }
                }
            }
            // No fallback for .app package symbols -- they have no workspace file.
        }
    }

    fn register_click(&mut self, target: ClickTarget, index: usize) -> bool {
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

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

// al-explorer talks to the al-lsp daemon over a Unix-domain socket
// (`al_protocol::DaemonClient` is `#[cfg(unix)]`), so the whole binary is
// Unix-only. The Zed extension does NOT need al-explorer — it spawns the
// portable `al-lsp` server directly — so on non-Unix targets we compile a small
// stub that exits with an actionable message instead of failing to build. This
// is what keeps `cargo build --workspace` (and the Windows release job) green.
#[cfg(not(unix))]
fn main() {
    eprintln!(
        "al-explorer is not supported on this platform.\n\
         \n\
         It is a developer TUI/CLI that talks to the al-lsp daemon over a \
         Unix-domain socket, which only exists on Unix-like systems (Linux, \
         macOS).\n\
         \n\
         The AL language server (al-lsp) itself runs on this platform and is all \
         the Zed extension needs to edit AL — you do not need al-explorer."
    );
    std::process::exit(1);
}

#[cfg(unix)]
fn main() -> ExitCode {
    // Default behaviour with no arguments is the TUI. Any subcommand
    // (`al-explorer search ...`, `al-explorer hover ...`) goes to the CLI.
    if std::env::args().nth(1).is_some() {
        let cli_args = cli::Cli::parse();
        return cli::run(cli_args);
    }
    match run_tui() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(unix)]
fn run_tui() -> Result<(), Box<dyn Error>> {
    // Pre-flight: refuse with a human-readable message instead of letting
    // crossterm propagate ENXIO (code 6) when stdin/stdout aren't a TTY
    // (T033 / RT-001). Without this, `al-explorer | tee log` or running
    // in a CI step prints `Error: Os { code: 6 }` and exits non-zero with
    // no hint that the TUI cannot run headless.
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("al-explorer TUI requires an interactive terminal — \
             stdin or stdout is not a TTY. Use `al-explorer <subcommand>` \
             for scripted output (run `al-explorer --help` for the CLI list)."
            .into());
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stderr(), crossterm::terminal::LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let init_result = app.init_workspace();

    // Always restore terminal state before propagating any error — failure to
    // do so leaves the terminal in raw mode + alternate screen.
    let cleanup = || -> io::Result<()> {
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
        Ok(())
    };

    if let Err(e) = init_result {
        let _ = cleanup();
        return Err(e);
    }

    let res = run_app(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("{:?}", err);
    }

    Ok(())
}

#[cfg(unix)]
fn run_app<B: Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    mut app: App,
) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            let evt = event::read()?;
            match evt {
                Event::Key(key) => {
                    // Global quit
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return Ok(());
                    }

                    // Global view switching — F1/F2/F3/F4/F5
                    match key.code {
                        KeyCode::F(1) => {
                            app.view_mode = ViewMode::ObjectBrowser;
                            continue;
                        }
                        KeyCode::F(2) => {
                            app.view_mode = ViewMode::EventChain;
                            continue;
                        }
                        KeyCode::F(3) => {
                            app.view_mode = ViewMode::CallGraph;
                            continue;
                        }
                        KeyCode::F(4) => {
                            app.view_mode = ViewMode::Profiler;
                            continue;
                        }
                        KeyCode::F(5) => {
                            app.view_mode = ViewMode::TestRunner;
                            app.test_runner.refresh_discovery();
                            continue;
                        }
                        _ => {}
                    }

                    match app.view_mode {
                        ViewMode::ObjectBrowser => handle_object_browser_key(&mut app, key),
                        ViewMode::EventChain => handle_event_chain_key(&mut app, key),
                        ViewMode::CallGraph => handle_call_graph_key(&mut app, key),
                        ViewMode::Profiler => handle_profiler_key(&mut app, key),
                        ViewMode::TestRunner => handle_test_runner_key(&mut app, key),
                    }
                }
                Event::Mouse(mouse_event) => {
                    if app.view_mode == ViewMode::ObjectBrowser {
                        handle_object_browser_mouse(&mut app, mouse_event);
                    }
                }
                _ => {}
            }
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

#[cfg(unix)]
fn handle_object_browser_key(app: &mut App, key: crossterm::event::KeyEvent) {
    // Tab bindings for type filters
    if key.code == KeyCode::Tab {
        app.next_kind();
        return;
    }
    if key.code == KeyCode::BackTab {
        app.previous_kind();
        return;
    }

    match app.active_pane {
        ActivePane::Search => match key.code {
            KeyCode::Esc => {
                app.search_query.clear();
                app.update_objects_list(true);
            }
            KeyCode::Backspace => {
                app.search_query.pop();
                app.update_objects_list(true);
            }
            KeyCode::Char(c) => {
                app.search_query.push(c);
                app.update_objects_list(true);
            }
            KeyCode::Down | KeyCode::Enter => {
                app.active_pane = ActivePane::Packages;
            }
            KeyCode::Right => {
                app.active_pane = ActivePane::Objects;
            }
            _ => {}
        },
        ActivePane::Packages => match key.code {
            KeyCode::Down | KeyCode::Char('j') => app.next_package(),
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(0) = app.package_list_state.selected() {
                    app.active_pane = ActivePane::Search;
                } else {
                    app.previous_package();
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                app.active_pane = ActivePane::Objects;
            }
            _ => {}
        },
        ActivePane::Objects => match key.code {
            KeyCode::Down | KeyCode::Char('j') => app.next_object(),
            KeyCode::Up | KeyCode::Char('k') => app.previous_object(),
            KeyCode::Left | KeyCode::Char('h') => {
                app.active_pane = ActivePane::Packages;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                app.active_pane = ActivePane::Details;
            }
            KeyCode::Enter => {
                app.details_list_state.select(None);
                app.open_selected_object();
            }
            KeyCode::Esc => {
                app.active_pane = ActivePane::Search;
            }
            _ => {}
        },
        ActivePane::Details => match key.code {
            KeyCode::Down | KeyCode::Char('j') => app.next_detail(),
            KeyCode::Up | KeyCode::Char('k') => app.previous_detail(),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                app.active_pane = ActivePane::Objects;
            }
            KeyCode::Enter => app.open_selected_object(),
            _ => {}
        },
    }
}

#[cfg(unix)]
fn handle_event_chain_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.event_chain;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => view.query.push(c),
            KeyCode::Backspace => {
                view.query.pop();
            }
            KeyCode::Esc => {
                view.query.clear();
                view.rows.clear();
                view.status = "Cleared".to_string();
            }
            KeyCode::Enter => {
                view.run_trace();
                if !view.rows.is_empty() {
                    view.input_focused = false;
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                if !view.rows.is_empty() {
                    view.input_focused = false;
                    view.list_state.select(Some(0));
                }
            }
            _ => {}
        }
    } else {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => view.next_row(),
            KeyCode::Char('k') | KeyCode::Up => view.prev_row(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::BackTab => {
                view.input_focused = true;
            }
            KeyCode::Enter => {
                // Re-run trace with current query
                view.input_focused = true;
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
fn handle_call_graph_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.call_graph;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => view.query.push(c),
            KeyCode::Backspace => {
                view.query.pop();
            }
            KeyCode::Esc => {
                view.query.clear();
                view.rows.clear();
                view.status = "Cleared".to_string();
            }
            KeyCode::Enter => {
                view.run_query();
                if !view.rows.is_empty() {
                    view.input_focused = false;
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                if !view.rows.is_empty() {
                    view.input_focused = false;
                    view.list_state.select(Some(0));
                }
            }
            _ => {}
        }
    } else {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => view.next_row(),
            KeyCode::Char('k') | KeyCode::Up => view.prev_row(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::BackTab => {
                view.input_focused = true;
            }
            KeyCode::Enter => {
                view.input_focused = true;
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
fn handle_profiler_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.profiler;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => view.file_path.push(c),
            KeyCode::Backspace => {
                view.file_path.pop();
            }
            KeyCode::Esc => {
                view.file_path.clear();
                view.hotspots.clear();
                view.status = "Cleared".to_string();
            }
            KeyCode::Enter => {
                view.load_profile();
            }
            KeyCode::Down | KeyCode::Tab => {
                if !view.hotspots.is_empty() {
                    view.input_focused = false;
                    view.list_state.select(Some(0));
                }
            }
            _ => {}
        }
    } else {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => view.next_row(),
            KeyCode::Char('k') | KeyCode::Up => view.prev_row(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::BackTab => {
                view.input_focused = true;
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
fn handle_test_runner_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.test_runner;
    match key.code {
        KeyCode::Char('j') | KeyCode::Down => view.next_row(),
        KeyCode::Char('k') | KeyCode::Up => view.prev_row(),
        KeyCode::Char('r') => view.run_selected(),
        KeyCode::Char('R') => view.run_all(),
        KeyCode::Esc | KeyCode::Char('q') => {
            app.view_mode = ViewMode::ObjectBrowser;
        }
        _ => {}
    }
}

#[cfg(unix)]
fn handle_object_browser_mouse(app: &mut App, mouse_event: crossterm::event::MouseEvent) {
    if let Ok((width, height)) = crossterm::terminal::size() {
        // Account for the mode bar at the top (1 line)
        let content_area = Rect {
            x: 0,
            y: 1,
            width,
            height: height.saturating_sub(1),
        };
        let layout = compute_layout(content_area);
        let (col, row) = (mouse_event.column, mouse_event.row);

        let search_area = layout.left_column[0];
        let packages_area = layout.left_column[1];
        let types_area = layout.middle_column[0];
        let objects_area = layout.middle_column[1];
        let details_area = layout.main_columns[2];

        let packages_inner = inner_area(packages_area);
        let objects_inner = inner_area(objects_area);
        let details_inner = inner_area(details_area);
        let search_inner = inner_area(search_area);
        let search_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(5)])
            .split(search_inner);
        let tabs_inner = inner_area(types_area);
        let arrow_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(tabs_inner);

        match mouse_event.kind {
            MouseEventKind::ScrollDown => {
                if rect_contains(packages_inner, col, row) {
                    app.next_package();
                } else if rect_contains(details_inner, col, row) {
                    app.next_detail();
                } else if rect_contains(objects_inner, col, row) {
                    app.next_object();
                }
            }
            MouseEventKind::ScrollUp => {
                if rect_contains(packages_inner, col, row) {
                    app.previous_package();
                } else if rect_contains(details_inner, col, row) {
                    app.previous_detail();
                } else if rect_contains(objects_inner, col, row) {
                    app.previous_object();
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if rect_contains(search_area, col, row) {
                    app.active_pane = ActivePane::Search;
                    if rect_contains(search_chunks[1], col, row) {
                        app.global_search = !app.global_search;
                        app.update_objects_list(true);
                    }
                } else if rect_contains(packages_inner, col, row) {
                    app.active_pane = ActivePane::Packages;
                    let offset = app.package_list_state.offset();
                    let clicked_idx = row.saturating_sub(packages_inner.y) as usize;
                    let target = offset + clicked_idx;
                    if target < app.packages.len() {
                        app.package_list_state.select(Some(target));
                        app.update_objects_list(true);
                    }
                } else if rect_contains(types_area, col, row) {
                    app.active_pane = ActivePane::Objects;
                    if rect_contains(arrow_chunks[0], col, row) {
                        app.previous_kind();
                    } else if rect_contains(arrow_chunks[2], col, row) {
                        app.next_kind();
                    }
                } else if rect_contains(objects_inner, col, row) {
                    app.active_pane = ActivePane::Objects;
                    let offset = app.object_list_state.offset();
                    let clicked_idx = row.saturating_sub(objects_inner.y) as usize;
                    let target = offset + clicked_idx;
                    if target < app.current_objects.len() {
                        app.object_list_state.select(Some(target));
                        app.details_list_state.select(Some(0));
                        app.update_details_items();
                        let is_double = app.register_click(ClickTarget::Objects, target);
                        if is_double {
                            app.details_list_state.select(None);
                            app.open_selected_object();
                        }
                    }
                } else if rect_contains(details_inner, col, row) {
                    app.active_pane = ActivePane::Details;
                    let offset = app.details_list_state.offset();
                    let clicked_idx = row.saturating_sub(details_inner.y) as usize;
                    let target = offset + clicked_idx;
                    if target < app.details_items.len() {
                        app.details_list_state.select(Some(target));
                        let is_double = app.register_click(ClickTarget::Details, target);
                        if is_double {
                            app.open_selected_object();
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
struct UiLayout {
    main_columns: [Rect; 3],
    left_column: [Rect; 2],
    middle_column: [Rect; 2],
}

#[cfg(unix)]
fn compute_layout(area: Rect) -> UiLayout {
    let main_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(30),
            Constraint::Percentage(50),
        ])
        .split(area);

    let left_column = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(main_columns[0]);

    let middle_column = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(main_columns[1]);

    UiLayout {
        main_columns: [main_columns[0], main_columns[1], main_columns[2]],
        left_column: [left_column[0], left_column[1]],
        middle_column: [middle_column[0], middle_column[1]],
    }
}

#[cfg(unix)]
fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

#[cfg(unix)]
fn inner_area(rect: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(rect)
}

// ---------------------------------------------------------------------------
// UI rendering
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn ui(f: &mut Frame, app: &mut App) {
    let size = f.area();

    // Mode bar at top (1 line)
    let top_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(size);

    render_mode_bar(f, top_split[0], app.view_mode);

    match app.view_mode {
        ViewMode::ObjectBrowser => render_object_browser(f, top_split[1], app),
        ViewMode::EventChain => render_event_chain(f, top_split[1], &mut app.event_chain),
        ViewMode::CallGraph => render_call_graph(f, top_split[1], &mut app.call_graph),
        ViewMode::Profiler => render_profiler(f, top_split[1], &mut app.profiler),
        ViewMode::TestRunner => render_test_runner(f, top_split[1], &mut app.test_runner),
    }
}

#[cfg(unix)]
fn render_mode_bar(f: &mut Frame, area: Rect, mode: ViewMode) {
    let tabs = [
        (" F1: Objects ", ViewMode::ObjectBrowser),
        (" F2: Events  ", ViewMode::EventChain),
        (" F3: CallGraph ", ViewMode::CallGraph),
        (" F4: Profiler ", ViewMode::Profiler),
        (" F5: Tests ", ViewMode::TestRunner),
    ];

    let spans: Vec<Span> = tabs
        .iter()
        .map(|(label, tab_mode)| {
            if *tab_mode == mode {
                Span::styled(
                    *label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(*label, Style::default().fg(Color::DarkGray))
            }
        })
        .collect();

    let quit_hint = Span::styled("  Ctrl+C: Quit", Style::default().fg(Color::DarkGray));
    let mut all_spans = spans;
    all_spans.push(quit_hint);

    f.render_widget(Paragraph::new(Line::from(all_spans)), area);
}

#[cfg(unix)]
fn render_object_browser(f: &mut Frame, area: Rect, app: &mut App) {
    let layout = compute_layout(area);

    // ==========================================
    // 1. Search Bar (Left Top)
    // ==========================================
    let search_style = pane_style(app.active_pane == ActivePane::Search);

    let search_block = Block::default()
        .borders(Borders::ALL)
        .title(" Search ")
        .border_style(search_style);

    let search_inner = search_block.inner(layout.left_column[0]);
    f.render_widget(search_block, layout.left_column[0]);

    let search_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(5)].as_ref())
        .split(search_inner);

    let cursor = if app.active_pane == ActivePane::Search {
        "█"
    } else {
        ""
    };
    let search_text_str = format!("{}{}", app.search_query, cursor);

    let search_text = Paragraph::new(search_text_str).style(Style::default().fg(Color::White));
    f.render_widget(search_text, search_chunks[0]);

    let filter_icon = if app.global_search { "[ALL]" } else { "[PKG]" };
    let icon_p = Paragraph::new(filter_icon).alignment(ratatui::layout::Alignment::Right);
    f.render_widget(icon_p, search_chunks[1]);

    // ==========================================
    // 2. Packages List (Left Bottom)
    // ==========================================
    let pkg_style = pane_style(app.active_pane == ActivePane::Packages);

    let packages: Vec<ListItem> = app
        .packages
        .iter()
        .map(|i| ListItem::new(Line::from(vec![Span::raw(i.clone())])))
        .collect();

    let packages_list = List::new(packages)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Packages ")
                .border_style(pkg_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(
        packages_list,
        layout.left_column[1],
        &mut app.package_list_state,
    );

    // ==========================================
    // 3. Types Tabs (Middle Top)
    // ==========================================
    let obj_style = pane_style(app.active_pane == ActivePane::Objects);

    let tabs_block = Block::default()
        .borders(Borders::ALL)
        .title(" Types (Press Tab) ")
        .border_style(obj_style);

    let tabs_inner_area = tabs_block.inner(layout.middle_column[0]);
    f.render_widget(tabs_block, layout.middle_column[0]);

    if !app.kinds.is_empty() {
        let total = app.kinds.len();
        let idx = app.active_kind_index;

        let prev_idx = if idx == 0 {
            total.saturating_sub(1)
        } else {
            idx - 1
        };
        let next_idx = if idx == total.saturating_sub(1) {
            0
        } else {
            idx + 1
        };

        let arrow_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(tabs_inner_area);

        let left_arrow = if total > 1 { "<" } else { " " };
        let right_arrow = if total > 1 { ">" } else { " " };
        let arrow_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM);
        f.render_widget(
            Paragraph::new(left_arrow)
                .alignment(ratatui::layout::Alignment::Center)
                .style(arrow_style),
            arrow_chunks[0],
        );
        f.render_widget(
            Paragraph::new(right_arrow)
                .alignment(ratatui::layout::Alignment::Center)
                .style(arrow_style),
            arrow_chunks[2],
        );

        let middle_width = arrow_chunks[1].width as usize;
        if middle_width > 0 {
            let center_width = std::cmp::min(std::cmp::max(10, middle_width / 2), middle_width);
            let side_total = middle_width.saturating_sub(center_width);
            let left_width = side_total / 2;
            let right_width = side_total.saturating_sub(left_width);

            let prev_label = if total > 1 {
                format!("{:?}", app.kinds[prev_idx])
            } else {
                "".to_string()
            };
            let next_label = if total > 1 {
                let display_idx = if total == 2 { prev_idx } else { next_idx };
                format!("{:?}", app.kinds[display_idx])
            } else {
                "".to_string()
            };
            let active_label = format!("{:?}", app.kinds[idx]);

            let left_text = pad_center(truncate_with_ellipsis(&prev_label, left_width), left_width);
            let right_text = pad_center(
                truncate_with_ellipsis(&next_label, right_width),
                right_width,
            );
            let center_text = pad_center(
                truncate_with_ellipsis(&active_label, center_width),
                center_width,
            );

            let spans = vec![
                Span::styled(
                    left_text,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    center_text,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    right_text,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ),
            ];
            let p = Paragraph::new(Line::from(spans)).alignment(ratatui::layout::Alignment::Left);
            f.render_widget(p, arrow_chunks[1]);
        }
    }

    // ==========================================
    // 4. Objects List (Middle Bottom)
    // ==========================================
    let objects: Vec<ListItem> = app
        .current_objects
        .iter()
        .map(|entry| {
            let display = format!("{} {}", entry.id, entry.name);
            ListItem::new(Line::from(vec![Span::raw(display)]))
        })
        .collect();

    let objects_list = List::new(objects)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(obj_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(
        objects_list,
        layout.middle_column[1],
        &mut app.object_list_state,
    );

    // ==========================================
    // 5. Details Pane (Right Full Column)
    // ==========================================
    let detail_style = pane_style(app.active_pane == ActivePane::Details);

    let list_items: Vec<ListItem> = app
        .details_items
        .iter()
        .map(|(_, line)| ListItem::new(line.clone()))
        .collect();

    let details_list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Details (Scroll/Click) ")
                .border_style(detail_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(
        details_list,
        layout.main_columns[2],
        &mut app.details_list_state,
    );
}

#[cfg(unix)]
fn render_event_chain(f: &mut Frame, area: Rect, view: &mut EventChainView) {
    // Split: search input (3 lines) + results list + status bar (1 line)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    // Search input
    let input_style = input_focused_style(view.input_focused);
    let cursor = if view.input_focused { "█" } else { "" };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" Event Name (Enter to trace, Tab to navigate results) ")
        .border_style(input_style);
    let input_inner = input_block.inner(chunks[0]);
    f.render_widget(input_block, chunks[0]);
    f.render_widget(
        Paragraph::new(format!("{}{}", view.query, cursor))
            .style(Style::default().fg(Color::White)),
        input_inner,
    );

    // Results list
    let list_style = if !view.input_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = view
        .rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let (prefix_style, name_style) = match row.node_type.as_str() {
                "event" => (
                    Style::default().fg(Color::Magenta),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                "subscriber" => (
                    Style::default().fg(Color::Cyan),
                    Style::default().fg(Color::Green),
                ),
                _ => (
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::White),
                ),
            };
            let edge_label = match row.edge_type.as_str() {
                "origin" => "EVENT",
                "subscribes_to" => "SUBS",
                "publishes" => "PUB",
                other => other,
            };
            let line = Line::from(vec![
                Span::raw(indent),
                Span::styled(format!("[{edge_label:<6}] "), prefix_style),
                Span::styled(row.object.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw("."),
                Span::styled(row.name.clone(), name_style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = if view.rows.is_empty() {
        " Event Chain (no results) "
    } else {
        " Event Chain (j/k navigate, Esc back to search) "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(list_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(list, chunks[1], &mut view.list_state);

    // Status bar
    f.render_widget(
        Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

#[cfg(unix)]
fn render_call_graph(f: &mut Frame, area: Rect, view: &mut CallGraphView) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    // Search input
    let input_style = input_focused_style(view.input_focused);
    let cursor = if view.input_focused { "█" } else { "" };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" Symbol Name (Enter to query impact, Tab to navigate results) ")
        .border_style(input_style);
    let input_inner = input_block.inner(chunks[0]);
    f.render_widget(input_block, chunks[0]);
    f.render_widget(
        Paragraph::new(format!("{}{}", view.query, cursor))
            .style(Style::default().fg(Color::White)),
        input_inner,
    );

    // Results list
    let list_style = if !view.input_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = view
        .rows
        .iter()
        .map(|row| {
            let style = match row.kind {
                CallRowKind::Header => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                CallRowKind::Entry => Style::default().fg(Color::White),
            };
            ListItem::new(Line::from(Span::styled(row.label.clone(), style)))
        })
        .collect();

    let title = if view.rows.is_empty() {
        " Call Graph / Impact (no results) "
    } else {
        " Call Graph / Impact (j/k navigate, Esc back to search) "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(list_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(list, chunks[1], &mut view.list_state);

    // Status bar
    f.render_widget(
        Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

#[cfg(unix)]
fn render_profiler(f: &mut Frame, area: Rect, view: &mut ProfilerView) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

    // File path input
    let input_style = input_focused_style(view.input_focused);
    let cursor = if view.input_focused { "█" } else { "" };
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" Profile Path (.alcpuprofile — Enter to load, Tab to navigate) ")
        .border_style(input_style);
    let input_inner = input_block.inner(chunks[0]);
    f.render_widget(input_block, chunks[0]);
    f.render_widget(
        Paragraph::new(format!("{}{}", view.file_path, cursor))
            .style(Style::default().fg(Color::White)),
        input_inner,
    );

    // Hotspot table
    let list_style = if !view.input_focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = if view.hotspots.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No profile loaded — enter a path above and press Enter",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        // Header row
        let header_text = format!(
            "{:<40} {:<30} {:>10} {:>10} {:>8}",
            "Procedure", "Object/File", "Self(ms)", "Total(ms)", "Hits"
        );
        let mut rows: Vec<ListItem> = vec![ListItem::new(Line::from(Span::styled(
            header_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))];

        for h in &view.hotspots {
            let procedure = truncate_with_ellipsis(&h.procedure, 39);
            let object = truncate_with_ellipsis(&h.object, 29);
            let line = Line::from(vec![
                Span::styled(
                    format!("{:<40} ", procedure),
                    Style::default().fg(Color::Green),
                ),
                Span::styled(
                    format!("{:<30} ", object),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{:>10.1} ", h.self_time_ms),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(
                    format!("{:>10.1} ", h.total_time_ms),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("{:>8}", h.hit_count),
                    Style::default().fg(Color::Magenta),
                ),
            ]);
            rows.push(ListItem::new(line));
        }
        rows
    };

    let title = if view.hotspots.is_empty() {
        " Hotspots "
    } else {
        " Hotspots (j/k navigate, Esc back to input) "
    };

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(list_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    f.render_stateful_widget(list, chunks[1], &mut view.list_state);

    // Status bar
    f.render_widget(
        Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

#[cfg(unix)]
fn render_test_runner(f: &mut Frame, area: Rect, view: &mut TestRunnerView) {
    // Split vertically: list (left 60%) | error detail (right 40%).
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    // Status bar at bottom of left column.
    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(columns[0]);

    // Build list items.
    let items: Vec<ListItem> = view
        .rows
        .iter()
        .map(|row| match row {
            TestRow::Codeunit { name, id } => ListItem::new(Line::from(vec![Span::styled(
                format!(" {name} ({id})"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )])),
            TestRow::Method { name, status, .. } => {
                let (icon, color) = match status {
                    MethodStatus::NotRun => ("○", Color::DarkGray),
                    MethodStatus::Pass { .. } => ("✓", Color::Green),
                    MethodStatus::Fail { .. } => ("✗", Color::Red),
                    MethodStatus::Skip => ("⊘", Color::Yellow),
                };
                let duration = if let MethodStatus::Pass { duration_ms } = status {
                    format!(" ({duration_ms}ms)")
                } else {
                    String::new()
                };
                ListItem::new(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(icon, Style::default().fg(color)),
                    Span::raw(format!(" {name}{duration}")),
                ]))
            }
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Tests [r: run selected | R: run all | j/k: navigate | q: back] "),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    f.render_stateful_widget(list, left_rows[0], &mut view.list_state);

    // Status bar.
    let status_p = Paragraph::new(view.status.as_str()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status_p, left_rows[1]);

    // Right pane: error detail for selected Fail row.
    let error_text = view
        .selected_error()
        .unwrap_or("(select a failed test to see error)");
    let detail = Paragraph::new(error_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Error Detail "),
        )
        .wrap(ratatui::widgets::Wrap { trim: false });
    f.render_widget(detail, columns[1]);
}

// ---------------------------------------------------------------------------
// String helpers
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn truncate_with_ellipsis(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let len = s.chars().count();
    if len <= width {
        return s.to_string();
    }
    if width <= 2 {
        return s.chars().take(width).collect();
    }
    let mut out: String = s.chars().take(width - 2).collect();
    out.push_str("..");
    out
}

#[cfg(unix)]
fn pad_center(s: String, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    format!("{:^width$}", s, width = width)
}

#[cfg(unix)]
/// Scan a text file for the first line containing `member_name` as a whole word.
///
/// Match must be surrounded by non-identifier characters (or start/end of line)
/// so a 1-character field name doesn't accidentally match every line that
/// happens to contain that letter.
fn find_member_line_in_file(path: &std::path::Path, member_name: &str) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let lower = member_name.to_lowercase();
    let is_ident_byte = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for (i, line) in content.lines().enumerate() {
        let line_lower = line.to_lowercase();
        let bytes = line_lower.as_bytes();
        let mut start = 0;
        while let Some(found) = line_lower[start..].find(&lower) {
            let abs = start + found;
            let before_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
            let end = abs + lower.len();
            let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
            if before_ok && after_ok {
                // Saturate rather than silently wrap: `i as u32` would truncate
                // modulo 2^32 for a (pathological) >4-billion-line file, handing
                // the editor a bogus line number. Clamp to u32::MAX instead.
                return Some(u32::try_from(i).unwrap_or(u32::MAX));
            }
            start = abs + 1;
        }
    }
    None
}
