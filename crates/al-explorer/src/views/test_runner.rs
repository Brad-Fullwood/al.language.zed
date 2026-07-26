use al_protocol::DaemonClient;
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::cli::commands::request_checked;
use crate::{App, ViewMode, advance_list_selection, ensure_daemon_client};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MethodStatus {
    NotRun,
    Pass { duration_ms: Option<u64> },
    Fail { error: Option<String> },
    Skip,
}

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

    pub(crate) fn refresh_discovery(&mut self) {
        self.rows.clear();
        self.list_state.select(None);
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };

        let discovered = match request_checked(client, "tests.discover", None) {
            Ok(v) => v,
            Err(e) => {
                self.client = None;
                self.status = format!("Daemon error (discover): {e}");
                return;
            }
        };

        let last_results = match request_checked(client, "tests.last_results", None) {
            Ok(value) => value,
            Err(error) => {
                self.client = None;
                self.status = format!("Daemon error (last results): {error}");
                return;
            }
        };
        self.rows = match parse_test_rows(&discovered, &last_results) {
            Ok(rows) => rows,
            Err(error) => {
                self.client = None;
                self.status = format!("Invalid daemon test response: {error}");
                return;
            }
        };

        if self.rows.is_empty() {
            self.status = "No test codeunits discovered".to_string();
        } else {
            self.status = format!("{} row(s) loaded", self.rows.len());
            self.list_state.select(Some(0));
        }
    }

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
        match request_checked(client, "tests.run_batch", Some(params)) {
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

    fn run_all(&mut self) {
        self.ensure_client();
        let Some(client) = self.client.as_mut() else {
            return;
        };
        match request_checked(client, "tests.run_auto", None) {
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
    last_results: &[serde_json::Value],
    codeunit_id: i32,
    method_name: &str,
) -> Result<MethodStatus, String> {
    let lower = method_name.to_lowercase();
    // History is append-only. Walk newest-to-oldest so the icon represents the
    // latest run, not the first run ever recorded for this method.
    let matching = last_results.iter().rev().find(|r| {
        r.get("codeunitId").and_then(|v| v.as_i64()) == Some(codeunit_id as i64)
            && r.get("methodName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase())
                .as_deref()
                == Some(&lower)
    });
    let Some(r) = matching else {
        return Ok(MethodStatus::NotRun);
    };
    let status_str = r
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!("history for codeunit {codeunit_id} method {method_name:?} has no status")
        })?;
    match status_str {
        "pass" => Ok(MethodStatus::Pass {
            duration_ms: r.get("durationMs").and_then(|v| v.as_u64()),
        }),
        "fail" => Ok(MethodStatus::Fail {
            error: r
                .get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }),
        "skip" => Ok(MethodStatus::Skip),
        other => Err(format!(
            "history for codeunit {codeunit_id} method {method_name:?} has unknown status {other:?}"
        )),
    }
}

fn parse_test_rows(
    discovered: &serde_json::Value,
    last_results: &serde_json::Value,
) -> Result<Vec<TestRow>, String> {
    let codeunits = discovered
        .as_array()
        .ok_or_else(|| "tests.discover is not an array".to_string())?;
    let history = last_results
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "tests.last_results.results is not an array".to_string())?;
    let mut rows = Vec::new();
    for (codeunit_index, codeunit) in codeunits.iter().enumerate() {
        let name = codeunit
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("tests.discover[{codeunit_index}].name is not a string"))?
            .to_string();
        let id = codeunit
            .get("id")
            .and_then(serde_json::Value::as_i64)
            .ok_or_else(|| format!("tests.discover[{codeunit_index}].id is not an integer"))
            .and_then(|id| {
                i32::try_from(id)
                    .map_err(|_| format!("tests.discover[{codeunit_index}].id is outside i32"))
            })?;
        rows.push(TestRow::Codeunit { name, id });
        let tests = codeunit
            .get("tests")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("tests.discover[{codeunit_index}].tests is not an array"))?;
        for (test_index, test) in tests.iter().enumerate() {
            let method_name = test
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    format!(
                        "tests.discover[{codeunit_index}].tests[{test_index}].name is not a string"
                    )
                })?
                .to_string();
            let status = find_method_status(history, id, &method_name)?;
            rows.push(TestRow::Method {
                codeunit_id: id,
                name: method_name,
                status,
            });
        }
    }
    Ok(rows)
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
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let left_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(columns[0]);

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
                let duration = match status {
                    MethodStatus::Pass {
                        duration_ms: Some(duration_ms),
                    } => format!(" ({duration_ms}ms)"),
                    _ => String::new(),
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

    let status_p = Paragraph::new(view.status.as_str()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status_p, left_rows[1]);

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

#[cfg(test)]
mod tests {
    use super::{MethodStatus, TestRow, parse_test_rows};

    fn discovered() -> serde_json::Value {
        serde_json::json!([{
            "name": "Example Tests",
            "id": 50100,
            "file": "ExampleTests.Codeunit.al",
            "tests": [{
                "name": "DoesWork",
                "line": 7,
                "handlerFunctions": []
            }],
            "testInitializers": [],
            "testCleanups": []
        }])
    }

    #[test]
    fn persisted_history_envelope_uses_the_latest_matching_result() {
        let history = serde_json::json!({
            "results": [
                {
                    "timestamp": 1,
                    "codeunitId": 50100,
                    "codeunitName": "Example Tests",
                    "methodName": "DoesWork",
                    "status": "fail",
                    "error": "old failure"
                },
                {
                    "timestamp": 2,
                    "codeunitId": 50100,
                    "codeunitName": "Example Tests",
                    "methodName": "DoesWork",
                    "status": "pass",
                    "durationMs": 14
                }
            ]
        });
        let rows = parse_test_rows(&discovered(), &history).expect("valid test responses");
        assert!(matches!(
            &rows[1],
            TestRow::Method {
                status: MethodStatus::Pass {
                    duration_ms: Some(14)
                },
                ..
            }
        ));
    }

    #[test]
    fn malformed_history_envelope_is_not_treated_as_not_run() {
        let error = parse_test_rows(&discovered(), &serde_json::json!([]))
            .expect_err("malformed history must be visible");
        assert!(error.contains("tests.last_results.results"), "{error}");
    }
}
