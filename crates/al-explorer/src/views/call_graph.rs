#![cfg(unix)]

use al_protocol::DaemonClient;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::{
    App, MAX_INPUT_LEN, advance_list_selection, ensure_daemon_client, input_focused_style,
};

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

pub(crate) struct CallGraphView {
    pub(crate) query: String,
    pub(crate) input_focused: bool,
    rows: Vec<CallRow>,
    list_state: ListState,
    status: String,
    client: Option<DaemonClient>,
    project_root: std::path::PathBuf,
}

impl CallGraphView {
    pub(crate) fn new(project_root: std::path::PathBuf) -> Self {
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
                        // FB-11: group by reference type and render each
                        // entry as readable text — kind, ID, name, and the
                        // field/procedure that creates the reference. The
                        // previous code dumped raw JSON for non-string
                        // entries.
                        let mut by_type: std::collections::BTreeMap<String, Vec<String>> =
                            std::collections::BTreeMap::new();
                        for entry in impacted {
                            if let Some(s) = entry.as_str() {
                                by_type
                                    .entry("other".to_string())
                                    .or_default()
                                    .push(s.to_string());
                                continue;
                            }
                            let kind = entry.get("k").and_then(|v| v.as_str()).unwrap_or("?");
                            let name = entry.get("n").and_then(|v| v.as_str()).unwrap_or("?");
                            let id = entry.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                            let ref_type = entry
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("reference")
                                .to_string();
                            let pkg = entry.get("package").and_then(|v| v.as_str()).unwrap_or("");
                            let mut label = if id > 0 {
                                format!("{kind} {id} \"{name}\"")
                            } else {
                                format!("{kind} \"{name}\"")
                            };
                            if let Some(field) = entry.get("field").and_then(|v| v.as_str()) {
                                label.push_str(&format!(" — field \"{field}\""));
                            }
                            if let Some(proc) = entry.get("proc").and_then(|v| v.as_str()) {
                                label.push_str(&format!(" — {proc}"));
                            }
                            if !pkg.is_empty() {
                                label.push_str(&format!("  [{pkg}]"));
                            }
                            by_type.entry(ref_type).or_default().push(label);
                        }
                        let total: usize = by_type.values().map(Vec::len).sum();
                        self.rows.push(CallRow {
                            label: format!("  {total} impacted symbols:"),
                            kind: CallRowKind::Header,
                        });
                        for (ref_type, labels) in by_type {
                            self.rows.push(CallRow {
                                label: format!("  {} ({}):", ref_type, labels.len()),
                                kind: CallRowKind::Header,
                            });
                            for label in labels {
                                self.rows.push(CallRow {
                                    label: format!("    {label}"),
                                    kind: CallRowKind::Entry,
                                });
                            }
                        }
                    }
                }
                self.status = format!(
                    "Impact query complete for '{}' — tip: use Object.Member \
                     (e.g. Customer.OnBeforePost) to narrow",
                    self.query.trim()
                );
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

pub(crate) fn handle_call_graph_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.call_graph;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => {
                if view.query.len() < MAX_INPUT_LEN {
                    view.query.push(c);
                }
            }
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

pub(crate) fn render_call_graph(f: &mut Frame, area: Rect, view: &mut CallGraphView) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

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

    f.render_widget(
        Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}
