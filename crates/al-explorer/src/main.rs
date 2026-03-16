mod types;
use types::{ObjectKind, SymbolEntry, SymbolIndex};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{
    error::Error,
    io,
    sync::Arc,
};

// ---------------------------------------------------------------------------
// Daemon client (inline — mirrors al-cli/src/client.rs, no al-* dep)
// ---------------------------------------------------------------------------

mod daemon {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Request {
        pub id: u64,
        pub method: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(default)]
        pub params: Option<serde_json::Value>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Response {
        pub id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(default)]
        pub result: Option<serde_json::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        #[serde(default)]
        pub error: Option<RpcError>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct RpcError {
        pub code: i32,
        pub message: String,
    }

    pub struct DaemonClient {
        reader: BufReader<UnixStream>,
        writer: UnixStream,
        next_id: u64,
    }

    impl DaemonClient {
        pub fn connect(project_root: &Path) -> Result<Self, String> {
            let sock_path = socket_path(project_root);
            if let Ok(stream) = UnixStream::connect(&sock_path) {
                return Self::from_stream(stream);
            }
            Self::start_daemon(project_root)?;
            Self::wait_for_daemon(&sock_path)?;
            let stream = UnixStream::connect(&sock_path)
                .map_err(|e| format!("Failed to connect after starting daemon: {e}"))?;
            Self::from_stream(stream)
        }

        fn from_stream(stream: UnixStream) -> Result<Self, String> {
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .map_err(|e| format!("Failed to set timeout: {e}"))?;
            let writer = stream
                .try_clone()
                .map_err(|e| format!("Failed to clone stream: {e}"))?;
            Ok(Self {
                reader: BufReader::new(stream),
                writer,
                next_id: 1,
            })
        }

        fn start_daemon(project_root: &Path) -> Result<(), String> {
            let al_lsp = find_al_lsp_binary()?;
            let _child = std::process::Command::new(&al_lsp)
                .arg("daemon")
                .arg("--project")
                .arg(project_root)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("Failed to start al-lsp daemon: {e}"))?;
            Ok(())
        }

        fn wait_for_daemon(sock_path: &Path) -> Result<(), String> {
            for _ in 0..50 {
                std::thread::sleep(Duration::from_millis(100));
                if UnixStream::connect(sock_path).is_ok() {
                    return Ok(());
                }
            }
            Err("Daemon did not start within 5 seconds".to_string())
        }

        pub fn request(
            &mut self,
            method: &str,
            params: Option<serde_json::Value>,
        ) -> Result<serde_json::Value, String> {
            self.send_request(method, &params)?;
            const MAX_RETRIES: u32 = 3;
            for retry in 0..=MAX_RETRIES {
                let response = self.read_response()?;
                if let Some(ref err) = response.error {
                    if err.message.contains("initializing") && retry < MAX_RETRIES {
                        std::thread::sleep(Duration::from_millis(500));
                        self.send_request(method, &params)?;
                        continue;
                    }
                    return Err(format!("{} (code {})", err.message, err.code));
                }
                return Ok(response.result.unwrap_or(serde_json::Value::Null));
            }
            Err("Workspace is initializing, try again".to_string())
        }

        fn send_request(&mut self, method: &str, params: &Option<serde_json::Value>) -> Result<(), String> {
            let id = self.next_id;
            self.next_id += 1;
            let req = Request { id, method: method.to_string(), params: params.clone() };
            let mut json = serde_json::to_string(&req)
                .map_err(|e| format!("Failed to serialize request: {e}"))?;
            json.push('\n');
            self.writer.write_all(json.as_bytes())
                .map_err(|e| format!("Failed to send request: {e}"))?;
            self.writer.flush()
                .map_err(|e| format!("Failed to flush: {e}"))?;
            Ok(())
        }

        fn read_response(&mut self) -> Result<Response, String> {
            let mut line = String::new();
            let bytes = self.reader.read_line(&mut line)
                .map_err(|e| format!("Failed to read response: {e}"))?;
            if bytes == 0 {
                return Err("Connection closed by daemon (EOF)".to_string());
            }
            serde_json::from_str(line.trim())
                .map_err(|e| format!("Failed to parse response: {e}"))
        }
    }

    fn fnv1a64(bytes: &[u8]) -> u64 {
        const OFFSET: u64 = 0xcbf29ce484222325;
        const PRIME: u64 = 0x00000100000001b3;
        let mut hash = OFFSET;
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(PRIME);
        }
        hash
    }

    pub fn socket_path(project_root: &Path) -> PathBuf {
        let canonical = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
        let hash = format!("{:016x}", fnv1a64(canonical.as_os_str().as_encoded_bytes()));
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(format!("{runtime_dir}/al-lsp/{hash}.sock"))
    }

    fn find_al_lsp_binary() -> Result<PathBuf, String> {
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent() {
                let candidate = dir.join("al-lsp");
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        if let Ok(output) = std::process::Command::new("which").arg("al-lsp").output()
            && output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Ok(PathBuf::from(path));
                }
            }
        Err("Cannot find al-lsp binary. Install it or add it to PATH.".to_string())
    }
}

// ---------------------------------------------------------------------------
// View mode
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum ViewMode {
    ObjectBrowser,
    EventChain,
    CallGraph,
}

// ---------------------------------------------------------------------------
// Object browser types
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum ActivePane {
    Search,
    Packages,
    Objects,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickTarget {
    Objects,
    Details,
}

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

#[derive(Debug, Clone)]
struct DetailTarget {
    name: String,
    #[allow(dead_code)]
    kind: DetailTargetKind,
}


// ---------------------------------------------------------------------------
// Event chain view
// ---------------------------------------------------------------------------

/// A single row shown in the event chain results list.
#[derive(Debug, Clone)]
struct TraceRow {
    depth: usize,
    edge_type: String,
    node_type: String,
    name: String,
    object: String,
}

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
    client: Option<daemon::DaemonClient>,
    project_root: std::path::PathBuf,
}

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
        if self.client.is_none() {
            match daemon::DaemonClient::connect(&self.project_root) {
                Ok(c) => self.client = Some(c),
                Err(e) => self.status = format!("Cannot connect to daemon: {e}"),
            }
        }
    }

    fn run_trace(&mut self) {
        if self.query.trim().is_empty() {
            self.status = "Enter an event name to search".to_string();
            return;
        }
        self.ensure_client();
        let Some(client) = self.client.as_mut() else { return; };
        let params = serde_json::json!({ "event": self.query.trim(), "depth": 10 });
        match client.request("trace", Some(params)) {
            Ok(val) => {
                self.rows.clear();
                if let Some(arr) = val.as_array() {
                    for item in arr {
                        self.rows.push(TraceRow {
                            depth: item.get("depth").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                            edge_type: item.get("edgeType").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            node_type: item.get("nodeType").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            name: item.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                            object: item.get("object").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        });
                    }
                    if self.rows.is_empty() {
                        self.status = format!("No event chain found for '{}'", self.query.trim());
                    } else {
                        self.status = format!("{} steps in event chain for '{}'", self.rows.len(), self.query.trim());
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
        if self.rows.is_empty() { return; }
        let i = match self.list_state.selected() {
            Some(i) => if i >= self.rows.len().saturating_sub(1) { 0 } else { i + 1 },
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn prev_row(&mut self) {
        if self.rows.is_empty() { return; }
        let i = match self.list_state.selected() {
            Some(i) => if i == 0 { self.rows.len().saturating_sub(1) } else { i - 1 },
            None => 0,
        };
        self.list_state.select(Some(i));
    }
}

// ---------------------------------------------------------------------------
// Call graph view
// ---------------------------------------------------------------------------

/// A single row shown in the call graph results list.
#[derive(Debug, Clone)]
struct CallRow {
    label: String,
    kind: CallRowKind,
}

#[derive(Debug, Clone, PartialEq)]
enum CallRowKind {
    Header,
    Entry,
}

struct CallGraphView {
    /// Current text in the search input.
    query: String,
    /// Whether the search input is focused.
    input_focused: bool,
    rows: Vec<CallRow>,
    list_state: ListState,
    status: String,
    client: Option<daemon::DaemonClient>,
    project_root: std::path::PathBuf,
}

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
        if self.client.is_none() {
            match daemon::DaemonClient::connect(&self.project_root) {
                Ok(c) => self.client = Some(c),
                Err(e) => self.status = format!("Cannot connect to daemon: {e}"),
            }
        }
    }

    fn run_query(&mut self) {
        if self.query.trim().is_empty() {
            self.status = "Enter a symbol name to search".to_string();
            return;
        }
        self.ensure_client();
        let Some(client) = self.client.as_mut() else { return; };
        let params = serde_json::json!({ "symbol": self.query.trim() });
        match client.request("impact", Some(params)) {
            Ok(val) => {
                self.rows.clear();
                let symbol = val.get("symbol").and_then(|v| v.as_str()).unwrap_or(self.query.trim());
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
                            let display = entry.as_str().map(|s| s.to_string())
                                .unwrap_or_else(|| serde_json::to_string(entry).unwrap_or_default());
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
        if self.rows.is_empty() { return; }
        let i = match self.list_state.selected() {
            Some(i) => if i >= self.rows.len().saturating_sub(1) { 0 } else { i + 1 },
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn prev_row(&mut self) {
        if self.rows.is_empty() { return; }
        let i = match self.list_state.selected() {
            Some(i) => if i == 0 { self.rows.len().saturating_sub(1) } else { i - 1 },
            None => 0,
        };
        self.list_state.select(Some(i));
    }
}

// ---------------------------------------------------------------------------
// Main application
// ---------------------------------------------------------------------------

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

    // Event chain and call graph views
    pub event_chain: EventChainView,
    pub call_graph: CallGraphView,
}

impl App {
    fn new() -> App {
        let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
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
            call_graph: CallGraphView::new(project_root),
        }
    }

    fn init_workspace(&mut self) -> Result<(), Box<dyn Error>> {
        let root = std::env::current_dir()?;
        let mut client = daemon::DaemonClient::connect(&root)
            .map_err(|e| format!("Cannot connect to al-lsp daemon: {e}"))?;

        // Load all symbols from the daemon's search endpoint (empty query = all)
        let result = client.request("search", Some(serde_json::json!({
            "query": "",
            "limit": 100_000
        }))).map_err(|e| format!("search request failed: {e}"))?;

        let entries: Vec<types::SymbolEntry> = serde_json::from_value(result)
            .map_err(|e| format!("Failed to deserialize symbol entries from daemon: {e}"))?;

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
            && let Some(pkg_name) = self.packages.get(selected) {
                let results = if self.global_search && !self.search_query.is_empty() {
                    self.symbols.search(&self.search_query, 5000)
                } else {
                    self.symbols.search_in_package(pkg_name, "")
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
                        let safe_idx = std::cmp::min(current, self.current_objects.len().saturating_sub(1));
                        self.object_list_state.select(Some(safe_idx));
                        self.details_list_state.select(Some(0));
                    }
                } else {
                    self.object_list_state.select(None);
                    self.details_list_state.select(None);
                }
        }
    }

    fn next_package(&mut self) {
        let i = match self.package_list_state.selected() {
            Some(i) => {
                if i >= self.packages.len().saturating_sub(1) { 0 } else { i + 1 }
            }
            None => 0,
        };
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    fn previous_package(&mut self) {
        let i = match self.package_list_state.selected() {
            Some(i) => {
                if i == 0 { self.packages.len().saturating_sub(1) } else { i - 1 }
            }
            None => 0,
        };
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    fn next_kind(&mut self) {
        if self.kinds.is_empty() { return; }
        self.active_kind_index = (self.active_kind_index + 1) % self.kinds.len();
        self.update_objects_list(true);
    }

    fn previous_kind(&mut self) {
        if self.kinds.is_empty() { return; }
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
                if i >= self.current_objects.len().saturating_sub(1) { 0 } else { i + 1 }
            }
            None => 0,
        };
        if !self.current_objects.is_empty() {
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
        }
    }

    fn previous_object(&mut self) {
        let i = match self.object_list_state.selected() {
            Some(i) => {
                if i == 0 { self.current_objects.len().saturating_sub(1) } else { i - 1 }
            }
            None => 0,
        };
        if !self.current_objects.is_empty() {
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
        }
    }

    fn next_detail(&mut self) {
        let i = match self.details_list_state.selected() {
            Some(i) => {
                if i >= self.details_items.len().saturating_sub(1) { 0 } else { i + 1 }
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
                if i == 0 { self.details_items.len().saturating_sub(1) } else { i - 1 }
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
            && let Some(entry) = self.current_objects.get(selected) {
            // Ask the daemon for the virtual file path for this object.
            let root = std::env::current_dir().unwrap_or_default();
            if let Ok(mut client) = daemon::DaemonClient::connect(&root) {
                let vf_result = client.request("virtualFile", Some(serde_json::json!({
                    "kind": format!("{:?}", entry.kind),
                    "id": entry.id,
                    "name": entry.name,
                    "package": entry.package,
                })));
                if let Ok(val) = vf_result
                    && let Some(path_str) = val.as_str()
                    && let Ok(abs_path) = std::fs::canonicalize(path_str)
                    && let Some(abs_str) = abs_path.to_str() {
                    let mut zed_url = format!("zed://file{}", abs_str);
                    if let Some(member) = target_member
                        && let Some(line) = find_member_line_in_file(&abs_path, &member.name) {
                        zed_url = format!("zed://file{}:{}:1", abs_str, line + 1);
                    }
                    let _ = open::that(zed_url);
                    return;
                }
            }
            // Fallback: open by object name
            let query = format!("{:?} {}", entry.kind, entry.name);
            let _ = open::that(format!("zed://symbol/{}", urlencoding_encode(&query)));
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
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
        execute!(
            io::stderr(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
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
        println!("{:?}", err);
    }

    Ok(())
}

fn run_app<B: Backend<Error = io::Error>>(terminal: &mut Terminal<B>, mut app: App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            let evt = event::read()?;
            match evt {
                Event::Key(key) => {
                    // Global quit
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        return Ok(());
                    }

                    // Global view switching — F1/F2/F3
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
                        _ => {}
                    }

                    match app.view_mode {
                        ViewMode::ObjectBrowser => handle_object_browser_key(&mut app, key),
                        ViewMode::EventChain => handle_event_chain_key(&mut app, key),
                        ViewMode::CallGraph => handle_call_graph_key(&mut app, key),
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
        ActivePane::Search => {
            match key.code {
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
            }
        }
        ActivePane::Packages => {
            match key.code {
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
            }
        }
        ActivePane::Objects => {
            match key.code {
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
            }
        }
        ActivePane::Details => {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => app.next_detail(),
                KeyCode::Up | KeyCode::Char('k') => app.previous_detail(),
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                    app.active_pane = ActivePane::Objects;
                }
                KeyCode::Enter => app.open_selected_object(),
                _ => {}
            }
        }
    }
}

fn handle_event_chain_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.event_chain;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => view.query.push(c),
            KeyCode::Backspace => { view.query.pop(); }
            KeyCode::Esc => { view.query.clear(); view.rows.clear(); view.status = "Cleared".to_string(); }
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

fn handle_call_graph_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.call_graph;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => view.query.push(c),
            KeyCode::Backspace => { view.query.pop(); }
            KeyCode::Esc => { view.query.clear(); view.rows.clear(); view.status = "Cleared".to_string(); }
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

fn handle_object_browser_mouse(app: &mut App, mouse_event: crossterm::event::MouseEvent) {
    if let Ok((width, height)) = crossterm::terminal::size() {
        // Account for the mode bar at the top (1 line)
        let content_area = Rect { x: 0, y: 1, width, height: height.saturating_sub(1) };
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
            .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
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

struct UiLayout {
    main_columns: [Rect; 3],
    left_column: [Rect; 2],
    middle_column: [Rect; 2],
}

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

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn inner_area(rect: Rect) -> Rect {
    Block::default().borders(Borders::ALL).inner(rect)
}

// ---------------------------------------------------------------------------
// UI rendering
// ---------------------------------------------------------------------------

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
    }
}

fn render_mode_bar(f: &mut Frame, area: Rect, mode: ViewMode) {
    let tabs = [(" F1: Objects ", ViewMode::ObjectBrowser),
        (" F2: Events  ", ViewMode::EventChain),
        (" F3: CallGraph ", ViewMode::CallGraph)];

    let spans: Vec<Span> = tabs.iter().map(|(label, tab_mode)| {
        if *tab_mode == mode {
            Span::styled(*label, Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(*label, Style::default().fg(Color::DarkGray))
        }
    }).collect();

    let quit_hint = Span::styled("  Ctrl+C: Quit", Style::default().fg(Color::DarkGray));
    let mut all_spans = spans;
    all_spans.push(quit_hint);

    f.render_widget(Paragraph::new(Line::from(all_spans)), area);
}

fn render_object_browser(f: &mut Frame, area: Rect, app: &mut App) {
    let layout = compute_layout(area);

    // ==========================================
    // 1. Search Bar (Left Top)
    // ==========================================
    let search_style = if app.active_pane == ActivePane::Search {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

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

    let cursor = if app.active_pane == ActivePane::Search { "█" } else { "" };
    let search_text_str = format!("{}{}", app.search_query, cursor);

    let search_text = Paragraph::new(search_text_str)
        .style(Style::default().fg(Color::White));
    f.render_widget(search_text, search_chunks[0]);

    let filter_icon = if app.global_search { "[ALL]" } else { "[PKG]" };
    let icon_p = Paragraph::new(filter_icon)
        .alignment(ratatui::layout::Alignment::Right);
    f.render_widget(icon_p, search_chunks[1]);

    // ==========================================
    // 2. Packages List (Left Bottom)
    // ==========================================
    let pkg_style = if app.active_pane == ActivePane::Packages {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let packages: Vec<ListItem> = app.packages.iter()
        .map(|i| ListItem::new(Line::from(vec![Span::raw(i.clone())])))
        .collect();

    let packages_list = List::new(packages)
        .block(Block::default().borders(Borders::ALL).title(" Packages ").border_style(pkg_style))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol(">> ");
    f.render_stateful_widget(packages_list, layout.left_column[1], &mut app.package_list_state);

    // ==========================================
    // 3. Types Tabs (Middle Top)
    // ==========================================
    let obj_style = if app.active_pane == ActivePane::Objects {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let tabs_block = Block::default()
        .borders(Borders::ALL)
        .title(" Types (Press Tab) ")
        .border_style(obj_style);

    let tabs_inner_area = tabs_block.inner(layout.middle_column[0]);
    f.render_widget(tabs_block, layout.middle_column[0]);

    if !app.kinds.is_empty() {
        let total = app.kinds.len();
        let idx = app.active_kind_index;

        let prev_idx = if idx == 0 { total.saturating_sub(1) } else { idx - 1 };
        let next_idx = if idx == total.saturating_sub(1) { 0 } else { idx + 1 };

        let arrow_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
            .split(tabs_inner_area);

        let left_arrow = if total > 1 { "<" } else { " " };
        let right_arrow = if total > 1 { ">" } else { " " };
        let arrow_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);
        f.render_widget(Paragraph::new(left_arrow).alignment(ratatui::layout::Alignment::Center).style(arrow_style), arrow_chunks[0]);
        f.render_widget(Paragraph::new(right_arrow).alignment(ratatui::layout::Alignment::Center).style(arrow_style), arrow_chunks[2]);

        let middle_width = arrow_chunks[1].width as usize;
        if middle_width > 0 {
            let center_width = std::cmp::min(std::cmp::max(10, middle_width / 2), middle_width);
            let side_total = middle_width.saturating_sub(center_width);
            let left_width = side_total / 2;
            let right_width = side_total.saturating_sub(left_width);

            let prev_label = if total > 1 { format!("{:?}", app.kinds[prev_idx]) } else { "".to_string() };
            let next_label = if total > 1 {
                let display_idx = if total == 2 { prev_idx } else { next_idx };
                format!("{:?}", app.kinds[display_idx])
            } else {
                "".to_string()
            };
            let active_label = format!("{:?}", app.kinds[idx]);

            let left_text = pad_center(truncate_with_ellipsis(&prev_label, left_width), left_width);
            let right_text = pad_center(truncate_with_ellipsis(&next_label, right_width), right_width);
            let center_text = pad_center(truncate_with_ellipsis(&active_label, center_width), center_width);

            let spans = vec![
                Span::styled(left_text, Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)),
                Span::styled(center_text, Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(right_text, Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)),
            ];
            let p = Paragraph::new(Line::from(spans))
                .alignment(ratatui::layout::Alignment::Left);
            f.render_widget(p, arrow_chunks[1]);
        }
    }

    // ==========================================
    // 4. Objects List (Middle Bottom)
    // ==========================================
    let objects: Vec<ListItem> = app.current_objects.iter()
        .map(|entry| {
            let display = format!("{} {}", entry.id, entry.name);
            ListItem::new(Line::from(vec![Span::raw(display)]))
        })
        .collect();

    let objects_list = List::new(objects)
        .block(Block::default().borders(Borders::ALL).border_style(obj_style))
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD))
        .highlight_symbol(">> ");
    f.render_stateful_widget(objects_list, layout.middle_column[1], &mut app.object_list_state);

    // ==========================================
    // 5. Details Pane (Right Full Column)
    // ==========================================
    let detail_style = if app.active_pane == ActivePane::Details {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    app.details_items.clear();

    if let Some(selected) = app.object_list_state.selected()
        && let Some(entry) = app.current_objects.get(selected) {
        app.details_items.push((None, Line::from(vec![
                Span::styled(format!("{:?} ", entry.kind), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(entry.id.to_string(), Style::default().fg(Color::Cyan)),
                Span::raw(" ".to_string()),
                Span::styled(entry.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ])));

            if let Some(extends) = &entry.extends {
                app.details_items.push((None, Line::from(vec![Span::styled("Extends: ".to_string(), Style::default().add_modifier(Modifier::BOLD)), Span::raw(extends.clone())])));
            }

            app.details_items.push((None, Line::from(vec![Span::styled("Package: ".to_string(), Style::default().add_modifier(Modifier::BOLD)), Span::raw(entry.package.clone())])));

            if !entry.properties.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled("Properties:".to_string(), Style::default().add_modifier(Modifier::BOLD)))));
                for p in entry.properties.iter() {
                    app.details_items.push((None, Line::from(vec![
                        Span::raw("    ".to_string()),
                        Span::styled(format!("{:<20}", p.name), Style::default().fg(Color::DarkGray)),
                        Span::raw(" = ".to_string()),
                        Span::raw(p.value.clone()),
                    ])));
                }
            }

            if !entry.keys.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Keys ({}):", entry.keys.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for k in entry.keys.iter() {
                    let fields = k.field_names.join(", ");
                    app.details_items.push((Some(DetailTarget { name: k.name.clone(), kind: DetailTargetKind::Key }), Line::from(vec![
                        Span::raw("    ".to_string()),
                        Span::styled(format!("{:<20}", k.name), Style::default().fg(Color::Cyan)),
                        Span::raw(format!(" ({})", fields)),
                    ])));
                }
            }

            if !entry.fields.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Fields ({}):", entry.fields.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for f in entry.fields.iter() {
                    app.details_items.push((Some(DetailTarget { name: f.name.clone(), kind: DetailTargetKind::Field }), Line::from(vec![
                        Span::styled(format!("    {:<4} ", f.id), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{:<30}", f.name), Style::default().fg(Color::White)),
                        Span::styled(format!(" : {}", f.type_name), Style::default().fg(Color::Cyan)),
                    ])));
                }
            }

            if !entry.controls.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Controls/Actions ({}):", entry.controls.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for c in entry.controls.iter() {
                    app.details_items.push((Some(DetailTarget { name: c.name.clone(), kind: DetailTargetKind::Control(c.kind.clone()) }), Line::from(vec![
                        Span::raw("    ".to_string()),
                        Span::styled(format!("{:<15}", c.kind), Style::default().fg(Color::Magenta)),
                        Span::raw(format!(" {}", c.name)),
                    ])));
                }
            }

            if !entry.enum_values.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Values ({}):", entry.enum_values.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for v in entry.enum_values.iter() {
                    app.details_items.push((Some(DetailTarget { name: v.name.clone(), kind: DetailTargetKind::EnumValue }), Line::from(vec![
                        Span::styled(format!("    {:<4} ", v.ordinal), Style::default().fg(Color::DarkGray)),
                        Span::raw(v.name.clone()),
                    ])));
                }
            }

            if !entry.methods.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Procedures ({}):", entry.methods.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for m in entry.methods.iter() {
                    let mut spans = vec![Span::raw("    ".to_string())];
                    if m.is_local {
                        spans.push(Span::styled("local ", Style::default().fg(Color::DarkGray)));
                    } else {
                        spans.push(Span::raw("      ".to_string()));
                    }
                    spans.push(Span::styled(m.name.clone(), Style::default().fg(Color::Green)));
                    spans.push(Span::raw("(".to_string()));

                    let params = m.parameters.iter().map(|p| p.name.to_string()).collect::<Vec<_>>().join(", ");
                    spans.push(Span::raw(params));

                    spans.push(Span::raw(")".to_string()));

                    if let Some(ret) = &m.return_type {
                        spans.push(Span::styled(format!(" : {}", ret), Style::default().fg(Color::Cyan)));
                    }

                    app.details_items.push((Some(DetailTarget { name: m.name.clone(), kind: DetailTargetKind::Procedure }), Line::from(spans)));
                }
        }
    }

    if app.details_items.is_empty() {
        app.details_items.push((None, Line::from("No object selected".to_string())));
    }

    let list_items: Vec<ListItem> = app.details_items.iter().map(|(_, line)| ListItem::new(line.clone())).collect();

    let details_list = List::new(list_items)
        .block(Block::default().borders(Borders::ALL).title(" Details (Scroll/Click) ").border_style(detail_style))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));

    f.render_stateful_widget(details_list, layout.main_columns[2], &mut app.details_list_state);
}

fn render_event_chain(f: &mut Frame, area: Rect, view: &mut EventChainView) {
    // Split: search input (3 lines) + results list + status bar (1 line)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    // Search input
    let input_style = if view.input_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
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
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = view.rows.iter().map(|row| {
        let indent = "  ".repeat(row.depth);
        let (prefix_style, name_style) = match row.node_type.as_str() {
            "event" => (
                Style::default().fg(Color::Magenta),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
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
    }).collect();

    let title = if view.rows.is_empty() {
        " Event Chain (no results) "
    } else {
        " Event Chain (j/k navigate, Esc back to search) "
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title).border_style(list_style))
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD))
        .highlight_symbol(">> ");
    f.render_stateful_widget(list, chunks[1], &mut view.list_state);

    // Status bar
    f.render_widget(
        Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn render_call_graph(f: &mut Frame, area: Rect, view: &mut CallGraphView) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    // Search input
    let input_style = if view.input_focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
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
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let items: Vec<ListItem> = view.rows.iter().map(|row| {
        let style = match row.kind {
            CallRowKind::Header => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            CallRowKind::Entry => Style::default().fg(Color::White),
        };
        ListItem::new(Line::from(Span::styled(row.label.clone(), style)))
    }).collect();

    let title = if view.rows.is_empty() {
        " Call Graph / Impact (no results) "
    } else {
        " Call Graph / Impact (j/k navigate, Esc back to search) "
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title).border_style(list_style))
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD))
        .highlight_symbol(">> ");
    f.render_stateful_widget(list, chunks[1], &mut view.list_state);

    // Status bar
    f.render_widget(
        Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

// ---------------------------------------------------------------------------
// String helpers
// ---------------------------------------------------------------------------

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

fn pad_center(s: String, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    format!("{:^width$}", s, width = width)
}

/// Scan a text file for the first line whose content (lowercased) contains `member_name`.
fn find_member_line_in_file(path: &std::path::Path, member_name: &str) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    let lower = member_name.to_lowercase();
    for (i, line) in content.lines().enumerate() {
        if line.to_lowercase().contains(&lower) {
            return Some(i as u32);
        }
    }
    None
}

/// Percent-encode a string for use in a URL path segment.
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}
