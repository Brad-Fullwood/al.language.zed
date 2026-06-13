#![cfg(unix)]

use al_protocol::DaemonClient;
use crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::{
    App, MAX_INPUT_LEN, advance_list_selection, ensure_daemon_client, input_focused_style,
};

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

pub(crate) struct EventChainView {
    /// Current text in the search input.
    pub(crate) query: String,
    /// Whether the search input is focused (vs. the results list).
    pub(crate) input_focused: bool,
    /// Flattened trace rows from the daemon.
    rows: Vec<TraceRow>,
    list_state: ListState,
    /// Live event-name suggestions for the current query (FB-6:
    /// search-as-you-type). Refreshed on each keystroke, rendered in the
    /// results area until a trace is run.
    suggestions: Vec<String>,
    suggestion_state: ListState,
    /// Status/error message shown below the list.
    status: String,
    /// Daemon client (None if not connected).
    client: Option<DaemonClient>,
    project_root: std::path::PathBuf,
}

impl EventChainView {
    pub(crate) fn new(project_root: std::path::PathBuf) -> Self {
        Self {
            query: String::new(),
            input_focused: true,
            rows: Vec::new(),
            list_state: ListState::default(),
            suggestions: Vec::new(),
            suggestion_state: ListState::default(),
            status: String::from("Type an event name — matches appear as you type"),
            client: None,
            project_root,
        }
    }

    fn ensure_client(&mut self) {
        ensure_daemon_client(&mut self.client, &self.project_root, &mut self.status);
    }

    /// FB-6: refresh the search-as-you-type suggestion list from the
    /// daemon's `events` substring search. Cheap (index-backed) and
    /// synchronous — runs on each keystroke.
    fn refresh_suggestions(&mut self) {
        self.suggestions.clear();
        self.suggestion_state.select(None);
        let q = self.query.trim().to_string();
        if q.len() < 2 {
            self.status = String::from("Type an event name — matches appear as you type");
            return;
        }
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match client.request("events", Some(serde_json::json!({ "name": q }))) {
            Ok(val) => {
                if let Some(arr) = val.as_array() {
                    let mut seen = std::collections::HashSet::new();
                    for item in arr {
                        let obj = item
                            .get("objectName")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        let method = item
                            .get("methodName")
                            .and_then(|v| v.as_str())
                            .unwrap_or("?");
                        if seen.insert((obj.to_string(), method.to_string())) {
                            self.suggestions.push(format!("{obj}::{method}"));
                        }
                        if self.suggestions.len() >= 100 {
                            break;
                        }
                    }
                }
                self.status = format!(
                    "{} matching events — ↓ to select, Enter to trace",
                    self.suggestions.len()
                );
            }
            Err(e) => {
                self.client = None;
                self.status = format!("Daemon error: {e}");
            }
        }
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

pub(crate) fn handle_event_chain_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.event_chain;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => {
                if view.query.len() < MAX_INPUT_LEN {
                    view.query.push(c);
                    view.refresh_suggestions();
                }
            }
            KeyCode::Backspace => {
                view.query.pop();
                view.refresh_suggestions();
            }
            KeyCode::Esc => {
                view.query.clear();
                view.rows.clear();
                view.suggestions.clear();
                view.status = "Cleared".to_string();
            }
            KeyCode::Enter => {
                view.run_trace();
                if !view.rows.is_empty() {
                    view.input_focused = false;
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                // Prefer the live suggestion list when present (FB-6),
                // otherwise fall back to the trace rows.
                if view.rows.is_empty() && !view.suggestions.is_empty() {
                    view.input_focused = false;
                    view.suggestion_state.select(Some(0));
                } else if !view.rows.is_empty() {
                    view.input_focused = false;
                    view.list_state.select(Some(0));
                }
            }
            _ => {}
        }
    } else if view.rows.is_empty() && !view.suggestions.is_empty() {
        // Navigating the suggestion list.
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                advance_list_selection(&mut view.suggestion_state, view.suggestions.len(), true);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                advance_list_selection(&mut view.suggestion_state, view.suggestions.len(), false);
            }
            KeyCode::Enter => {
                if let Some(s) = view
                    .suggestion_state
                    .selected()
                    .and_then(|sel| view.suggestions.get(sel))
                {
                    // Suggestion format is "Object::Event" — trace by
                    // the event name.
                    let event = s.rsplit("::").next().unwrap_or(s).to_string();
                    view.query = event;
                    view.run_trace();
                    if !view.rows.is_empty() {
                        view.list_state.select(Some(0));
                    }
                }
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::BackTab => {
                view.input_focused = true;
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

pub(crate) fn render_event_chain(f: &mut Frame, area: Rect, view: &mut EventChainView) {
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

    // FB-6: before a trace has run, the results area shows live
    // search-as-you-type matches for the current query.
    if view.rows.is_empty() && !view.suggestions.is_empty() {
        let items: Vec<ListItem> = view
            .suggestions
            .iter()
            .map(|s| {
                let (obj, evt) = s.rsplit_once("::").unwrap_or(("", s.as_str()));
                ListItem::new(Line::from(vec![
                    Span::styled(obj.to_string(), Style::default().fg(Color::DarkGray)),
                    Span::raw("::"),
                    Span::styled(
                        evt.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Matching events (↓ then Enter to trace) ")
                    .border_style(list_style),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");
        f.render_stateful_widget(list, chunks[1], &mut view.suggestion_state);

        f.render_widget(
            Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
            chunks[2],
        );
        return;
    }

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
