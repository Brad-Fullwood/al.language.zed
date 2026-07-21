// `collapsible_match` would force `Event::Mouse(mouse_event)` arms in the TUI
// event dispatcher (see `tui::run_app`) into `Event::Mouse(mouse_event) if
// app.view_mode == ViewMode::X` style guards, which makes the per-mode dispatch
// table noticeably less skimmable. The expanded form is intentional.
#![allow(clippy::collapsible_match)]

pub mod cli;
pub mod types;
pub mod views;

mod app;
mod tui;

// The TUI `App` lives in `app` but several `views` modules (and the unit tests
// below) refer to it as `crate::App`; re-export it at the crate root so those
// paths keep resolving unchanged.
pub(crate) use app::App;

use al_protocol::DaemonClient;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::ListState;
use types::SymbolEntry;

/// Upper bound on the length (in bytes) of any single-line text input field
/// driven by `KeyCode::Char` events (search query, event-chain / call-graph
/// query, profiler file path). Without a cap, holding down a key would grow
/// these `String`s without limit — for `search_query` that also re-filters the
/// whole symbol set on every keystroke — eventually exhausting memory. No real
/// query or path approaches this length.
pub(crate) const MAX_INPUT_LEN: usize = 4096;

/// Advance a wrap-around list index forward by one.
///
/// Returns the next index (wrapping from the last item back to 0).
/// If `current` is `None` (no selection), starts at index 0.
#[inline]
pub(crate) fn wrap_next(current: Option<usize>, len: usize) -> usize {
    match current {
        Some(i) if i >= len.saturating_sub(1) => 0,
        Some(i) => i + 1,
        None => 0,
    }
}

/// Retreat a wrap-around list index backward by one.
///
/// Returns the previous index (wrapping from 0 to the last item).
/// If `current` is `None` (no selection), starts at index 0.
#[inline]
pub(crate) fn wrap_prev(current: Option<usize>, len: usize) -> usize {
    match current {
        Some(0) | None => len.saturating_sub(1),
        Some(i) => i - 1,
    }
}

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
pub(crate) fn pane_style(is_active: bool) -> Style {
    if is_active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

#[derive(PartialEq, Clone, Copy)]
pub(crate) enum ViewMode {
    ObjectBrowser,
    EventChain,
    CallGraph,
    Profiler,
    TestRunner,
}

#[derive(PartialEq, Clone, Copy)]
pub(crate) enum ActivePane {
    Search,
    Packages,
    Objects,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClickTarget {
    Objects,
    Details,
}

#[derive(Debug, Clone)]
pub(crate) struct DetailTarget {
    pub(crate) name: String,
}

/// Object ID for display: `None` when the kind has no developer-visible ID
/// in AL syntax, or when the entry carries a sentinel/synthetic ID (≤ 0).
pub(crate) fn display_object_id(entry: &SymbolEntry) -> Option<i32> {
    if entry.kind.has_numeric_id() && entry.id > 0 {
        Some(entry.id)
    } else {
        None
    }
}

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

pub(crate) fn pad_center(s: String, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    format!("{:^width$}", s, width = width)
}

pub fn run() -> std::process::ExitCode {
    use clap::Parser;
    use std::process::ExitCode;

    // Default behaviour with no arguments is the TUI. Any subcommand
    // (`al-explorer search ...`, `al-explorer hover ...`) goes to the CLI.
    if std::env::args().nth(1).is_some() {
        let cli_args = cli::Cli::parse();
        return cli::run(cli_args);
    }
    match tui::run_tui() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::call_graph::handle_call_graph_key;
    use crate::views::event_chain::handle_event_chain_key;
    use crate::views::object_browser::handle_object_browser_key;
    use crate::views::profiler::handle_profiler_key;
    use crossterm::event::{KeyCode, KeyEvent};
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
        for _ in 0..(MAX_INPUT_LEN + 500) {
            handle_object_browser_key(&mut app, KeyEvent::from(KeyCode::Char('a')));
        }
        assert_eq!(app.search_query.len(), MAX_INPUT_LEN);
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
        assert!(app.kinds.contains(&ObjectKind::Table));
        assert!(app.kinds.contains(&ObjectKind::Codeunit));
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
        assert_eq!(app.current_objects.len(), 1);
        assert_eq!(app.current_objects[0].name, "Vendor");
    }
}
