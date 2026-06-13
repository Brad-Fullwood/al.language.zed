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

use crate::{App, ViewMode, advance_list_selection, ensure_daemon_client};

// ---------------------------------------------------------------------------
// Test runner view
// ---------------------------------------------------------------------------

/// Status of a single test method as reported by the daemon.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MethodStatus {
    NotRun,
    Pass { duration_ms: u64 },
    Fail { error: Option<String> },
    Skip,
}

/// A single row in the test runner tree — either a codeunit header or a method.
#[derive(Debug, Clone)]
pub(crate) enum TestRow {
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

pub(crate) struct TestRunnerView {
    rows: Vec<TestRow>,
    list_state: ListState,
    status: String,
    client: Option<DaemonClient>,
    project_root: std::path::PathBuf,
}

impl TestRunnerView {
    pub(crate) fn new(project_root: std::path::PathBuf) -> Self {
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
    pub(crate) fn refresh_discovery(&mut self) {
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

pub(crate) fn handle_test_runner_key(app: &mut App, key: crossterm::event::KeyEvent) {
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

pub(crate) fn render_test_runner(f: &mut Frame, area: Rect, view: &mut TestRunnerView) {
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
