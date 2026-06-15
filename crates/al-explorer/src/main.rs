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
mod views;
#[cfg(unix)]
use clap::Parser;
#[cfg(unix)]
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
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
    widgets::{ListState, Paragraph},
};
#[cfg(unix)]
use std::process::ExitCode;
#[cfg(unix)]
use std::{error::Error, io, sync::Arc};
#[cfg(unix)]
use types::{ObjectKind, SymbolEntry, SymbolIndex};

#[cfg(unix)]
use al_protocol::DaemonClient;

#[cfg(unix)]
use views::call_graph::{CallGraphView, handle_call_graph_key, render_call_graph};
#[cfg(unix)]
use views::event_chain::{EventChainView, handle_event_chain_key, render_event_chain};
#[cfg(unix)]
use views::object_browser::{
    handle_object_browser_key, handle_object_browser_mouse, render_object_browser,
};
#[cfg(unix)]
use views::profiler::{ProfilerView, handle_profiler_key, render_profiler};
#[cfg(unix)]
use views::test_runner::{TestRunnerView, handle_test_runner_key, render_test_runner};

/// Upper bound on the length (in bytes) of any single-line text input field
/// driven by `KeyCode::Char` events (search query, event-chain / call-graph
/// query, profiler file path). Without a cap, holding down a key would grow
/// these `String`s without limit — for `search_query` that also re-filters the
/// whole symbol set on every keystroke — eventually exhausting memory. No real
/// query or path approaches this length.
#[cfg(unix)]
pub(crate) const MAX_INPUT_LEN: usize = 4096;


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
pub(crate) fn ensure_daemon_client(
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
pub(crate) fn advance_list_selection(list_state: &mut ListState, len: usize, forward: bool) {
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
pub(crate) fn input_focused_style(is_focused: bool) -> Style {
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
pub(crate) fn pane_style(is_active: bool) -> Style {
    if is_active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}


#[cfg(unix)]
#[derive(PartialEq, Clone, Copy)]
pub(crate) enum ViewMode {
    ObjectBrowser,
    EventChain,
    CallGraph,
    Profiler,
    TestRunner,
}


#[cfg(unix)]
#[derive(PartialEq, Clone, Copy)]
pub(crate) enum ActivePane {
    Search,
    Packages,
    Objects,
    Details,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClickTarget {
    Objects,
    Details,
}

#[cfg(unix)]
/// What kind of member a `DetailTarget` refers to.
/// Stored for future use (deep-link precision when virtual file support
/// is added in ISSUE-017 follow-up work).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum DetailTargetKind {
    Field,
    Key,
    Control(String),
    EnumValue,
    Procedure,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
pub(crate) struct DetailTarget {
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) kind: DetailTargetKind,
}


/// Payload handed from the background workspace-init thread to the event
/// loop: the connected daemon client plus the full symbol listing, or a
/// human-readable error.
#[cfg(unix)]
type InitResult = Result<(DaemonClient, Vec<types::SymbolEntry>), String>;

#[cfg(unix)]
pub(crate) struct App {
    pub(crate) view_mode: ViewMode,

    // Object browser state
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

    // Event chain, call graph, profiler, and test runner views
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
            init_rx: None,
            init_status: None,
            project_root,
        }
    }

    /// Kick off workspace init on a background thread so the UI renders
    /// immediately. `poll_init` integrates the result on the event loop.
    fn start_init_workspace(&mut self) {
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
    fn poll_init(&mut self) {
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
            let i = wrap_next(self.object_list_state.selected(), self.current_objects.len());
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
            self.update_details_items();
        }
    }

    pub(crate) fn previous_object(&mut self) {
        if !self.current_objects.is_empty() {
            let i = wrap_prev(self.object_list_state.selected(), self.current_objects.len());
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
            self.update_details_items();
        }
    }

    /// FB-1: the startup symbol dump is slim (no member arrays — a full
    /// dump is ~60 MB JSON). Hydrate the selected object's members from the
    /// daemon on demand, replacing the slim entry in place.
    fn hydrate_selected_object(&mut self) {
        let Some(selected) = self.object_list_state.selected() else {
            return;
        };
        let needs_members = match self.current_objects.get(selected) {
            Some(e) => {
                e.methods.is_empty()
                    && e.fields.is_empty()
                    && e.controls.is_empty()
                    && e.enum_values.is_empty()
                    && e.keys.is_empty()
                    && e.properties.is_empty()
            }
            None => false,
        };
        if !needs_members {
            return;
        }
        let (kind, name, package) = match self.current_objects.get(selected) {
            Some(e) => (e.kind, e.name.clone(), e.package.clone()),
            None => return,
        };
        if self.daemon_client.is_none() {
            self.daemon_client = DaemonClient::connect(&self.project_root).ok();
        }
        let Some(client) = self.daemon_client.as_mut() else {
            return;
        };
        let result = client.request(
            "object",
            Some(serde_json::json!({
                "kind": format!("{:?}", kind),
                "name": name,
            })),
        );
        let Ok(val) = result else {
            self.daemon_client = None;
            return;
        };
        let full: Vec<types::SymbolEntry> = match serde_json::from_value(val) {
            Ok(v) => v,
            Err(_) => return,
        };
        // `object` returns every match for (kind, name) — prefer the entry
        // from the same package as the slim one we're hydrating.
        if let Some(hydrated) = full
            .iter()
            .find(|e| e.package == package)
            .or_else(|| full.first())
        {
            self.current_objects[selected] = Arc::new(hydrated.clone());
        }
    }

    pub(crate) fn update_details_items(&mut self) {
        self.hydrate_selected_object();
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
                    Span::styled(
                        display_object_id(entry)
                            .map(|id| id.to_string())
                            .unwrap_or_default(),
                        Style::default().fg(Color::Cyan),
                    ),
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
        if !self.details_items.is_empty() {
            let i = wrap_next(self.details_list_state.selected(), self.details_items.len());
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

    pub(crate) fn open_selected_object(&mut self) {
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
    // Non-blocking: the first frame renders immediately with a
    // "Loading workspace…" status while the daemon starts and indexes
    // in the background (FB-1).
    app.start_init_workspace();

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
        app.poll_init();
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

                    // Global view switching — F1..F5, with Alt+1..Alt+5 as
                    // equivalents for terminals where the host editor
                    // swallows the function keys (FB-5: Zed binds F4/F5 to
                    // its own debugger commands and they never reach the
                    // embedded terminal).
                    let alt_digit = if key.modifiers.contains(KeyModifiers::ALT) {
                        match key.code {
                            KeyCode::Char(c @ '1'..='5') => Some(c as u8 - b'0'),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let fkey = match key.code {
                        KeyCode::F(n @ 1..=5) => Some(n),
                        _ => alt_digit,
                    };
                    match fkey {
                        Some(1) => {
                            app.view_mode = ViewMode::ObjectBrowser;
                            continue;
                        }
                        Some(2) => {
                            app.view_mode = ViewMode::EventChain;
                            continue;
                        }
                        Some(3) => {
                            app.view_mode = ViewMode::CallGraph;
                            continue;
                        }
                        Some(4) => {
                            app.view_mode = ViewMode::Profiler;
                            continue;
                        }
                        Some(5) => {
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

/// Object ID for display: `None` when the kind has no developer-visible ID
/// in AL syntax, or when the entry carries a sentinel/synthetic ID (≤ 0).
#[cfg(unix)]
pub(crate) fn display_object_id(entry: &SymbolEntry) -> Option<i32> {
    if entry.kind.has_numeric_id() && entry.id > 0 {
        Some(entry.id)
    } else {
        None
    }
}

#[cfg(unix)]
fn render_mode_bar(f: &mut Frame, area: Rect, mode: ViewMode) {
    // Alt+1..5 are equivalents for terminals where the host editor (e.g.
    // Zed's debugger keymap) swallows the function keys (FB-5).
    let tabs = [
        (" F1|M-1: Objects ", ViewMode::ObjectBrowser),
        (" F2|M-2: Events ", ViewMode::EventChain),
        (" F3|M-3: CallGraph ", ViewMode::CallGraph),
        (" F4|M-4: Profiler ", ViewMode::Profiler),
        (" F5|M-5: Tests ", ViewMode::TestRunner),
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
pub(crate) fn truncate_with_ellipsis(s: &str, width: usize) -> String {
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
pub(crate) fn pad_center(s: String, width: usize) -> String {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::views::call_graph::handle_call_graph_key;
    use crate::views::event_chain::handle_event_chain_key;
    use crate::views::object_browser::handle_object_browser_key;
    use crate::views::profiler::handle_profiler_key;
    use crossterm::event::KeyEvent;
    use types::{ObjectKind, SymbolEntry};

    fn sym(kind: ObjectKind, id: i32, name: &str, package: &str) -> SymbolEntry {
        SymbolEntry {
            kind,
            id,
            name: name.to_string(),
            extends: None,
            package: package.to_string(),
            methods: Vec::new(),
            fields: Vec::new(),
            controls: Vec::new(),
            enum_values: Vec::new(),
            keys: Vec::new(),
            properties: Vec::new(),
        }
    }

    #[test]
    fn search_query_is_length_capped() {
        let mut app = App::new();
        // Feed far more characters than the cap allows.
        for _ in 0..(MAX_INPUT_LEN + 500) {
            handle_object_browser_key(&mut app, KeyEvent::from(KeyCode::Char('a')));
        }
        assert_eq!(app.search_query.len(), MAX_INPUT_LEN);
        // One more keystroke must not grow it further.
        handle_object_browser_key(&mut app, KeyEvent::from(KeyCode::Char('b')));
        assert_eq!(app.search_query.len(), MAX_INPUT_LEN);
    }

    #[test]
    fn profiler_file_path_is_length_capped() {
        let mut app = App::new();
        app.profiler.input_focused = true;
        for _ in 0..(MAX_INPUT_LEN + 500) {
            handle_profiler_key(&mut app, KeyEvent::from(KeyCode::Char('x')));
        }
        assert_eq!(app.profiler.file_path.len(), MAX_INPUT_LEN);
    }

    #[test]
    fn event_chain_and_call_graph_queries_are_length_capped() {
        let mut app = App::new();
        app.event_chain.input_focused = true;
        app.call_graph.input_focused = true;
        for _ in 0..(MAX_INPUT_LEN + 100) {
            handle_event_chain_key(&mut app, KeyEvent::from(KeyCode::Char('e')));
            handle_call_graph_key(&mut app, KeyEvent::from(KeyCode::Char('c')));
        }
        assert_eq!(app.event_chain.query.len(), MAX_INPUT_LEN);
        assert_eq!(app.call_graph.query.len(), MAX_INPUT_LEN);
    }

    #[test]
    fn object_kind_as_str_matches_debug() {
        for k in [
            ObjectKind::Table,
            ObjectKind::Codeunit,
            ObjectKind::Enum,
            ObjectKind::Entitlement,
        ] {
            assert_eq!(k.as_str(), format!("{k:?}"));
        }
    }

    #[test]
    fn update_objects_list_empty_query_keeps_all_package_objects() {
        let mut app = App::new();
        app.symbols.load(vec![
            sym(ObjectKind::Table, 1, "Customer", "Base"),
            sym(ObjectKind::Codeunit, 2, "Mgt", "Base"),
            sym(ObjectKind::Page, 3, "CustCard", "Other"),
        ]);
        app.packages = app.symbols.package_names();
        let base_idx = app.packages.iter().position(|p| p == "Base").unwrap();
        app.package_list_state.select(Some(base_idx));

        app.update_objects_list(true);
        // Empty query -> all kinds present for the Base package.
        assert!(app.kinds.contains(&ObjectKind::Table));
        assert!(app.kinds.contains(&ObjectKind::Codeunit));
        // The "Other" package's Page must not appear.
        assert!(!app.kinds.contains(&ObjectKind::Page));
    }

    #[test]
    fn update_objects_list_filters_by_query_in_package() {
        let mut app = App::new();
        app.symbols.load(vec![
            sym(ObjectKind::Table, 1, "Customer", "Base"),
            sym(ObjectKind::Table, 2, "Vendor", "Base"),
        ]);
        app.packages = app.symbols.package_names();
        let base_idx = app.packages.iter().position(|p| p == "Base").unwrap();
        app.package_list_state.select(Some(base_idx));
        app.search_query = "vend".to_string();

        app.update_objects_list(true);
        // Only "Vendor" matches; it is a Table, selected automatically.
        assert_eq!(app.current_objects.len(), 1);
        assert_eq!(app.current_objects[0].name, "Vendor");
    }
}
