#![cfg(unix)]

use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::{
    App, MAX_INPUT_LEN, advance_list_selection, input_focused_style, truncate_with_ellipsis,
};

#[derive(Debug, Clone)]
struct HotspotRow {
    procedure: String,
    object: String,
    self_time_ms: f64,
    total_time_ms: f64,
    hit_count: u64,
}

pub(crate) struct ProfilerView {
    pub(crate) file_path: String,
    pub(crate) input_focused: bool,
    hotspots: Vec<HotspotRow>,
    list_state: ListState,
    status: String,
    /// Total session duration (ms).
    duration_ms: f64,
}

impl ProfilerView {
    pub(crate) fn new() -> Self {
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

        // Cap the number of nodes we iterate. A malformed or adversarial
        // profile could declare millions of nodes; iterating all of them in the
        // synchronous TUI would freeze the UI. Real BC CPU profiles are far
        // smaller than this bound.
        const MAX_PROFILE_NODES: usize = 500_000;
        let truncated = nodes.len() > MAX_PROFILE_NODES;
        let nodes = if truncated {
            &nodes[..MAX_PROFILE_NODES]
        } else {
            &nodes[..]
        };

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

        rows.sort_by(|a, b| {
            b.self_time_ms
                .partial_cmp(&a.self_time_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let count = rows.len();
        self.hotspots = rows;
        self.status = if count == 0 {
            "No hotspots found in profile (all hitCount=0?)".to_string()
        } else if truncated {
            format!(
                "{count} hotspots loaded (profile truncated to first {MAX_PROFILE_NODES} nodes) — duration {:.1}ms",
                self.duration_ms
            )
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

pub(crate) fn handle_profiler_key(app: &mut App, key: crossterm::event::KeyEvent) {
    let view = &mut app.profiler;
    if view.input_focused {
        match key.code {
            KeyCode::Char(c) => {
                if view.file_path.len() < MAX_INPUT_LEN {
                    view.file_path.push(c);
                }
            }
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

pub(crate) fn render_profiler(f: &mut Frame, area: Rect, view: &mut ProfilerView) {
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
        .title(" Profile Path (.alcpuprofile — Enter to load, Tab to navigate) ")
        .border_style(input_style);
    let input_inner = input_block.inner(chunks[0]);
    f.render_widget(input_block, chunks[0]);
    f.render_widget(
        Paragraph::new(format!("{}{}", view.file_path, cursor))
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

    let items: Vec<ListItem> = if view.hotspots.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No profile loaded — enter a path above and press Enter",
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
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

    f.render_widget(
        Paragraph::new(view.status.clone()).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_profile_handles_large_node_arrays() {
        // One real hotspot followed by many empty nodes; exercises the bounded
        // iteration path introduced by the node-count cap.
        let mut nodes = vec![serde_json::json!({
            "hitCount": 5u64,
            "callFrame": { "functionName": "DoWork", "url": "Cod50000.al" }
        })];
        for _ in 0..1000 {
            nodes.push(serde_json::json!({ "hitCount": 0u64 }));
        }
        let profile = serde_json::json!({
            "startTime": 0.0,
            "endTime": 1_000_000.0,
            "nodes": nodes,
        });

        let path = std::env::temp_dir().join(format!(
            "al-explorer-test-profile-{}-{}.alcpuprofile",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, serde_json::to_vec(&profile).unwrap()).unwrap();

        let mut view = ProfilerView::new();
        view.file_path = path.to_string_lossy().into_owned();
        view.load_profile();
        std::fs::remove_file(&path).ok();

        assert_eq!(view.hotspots.len(), 1);
        assert_eq!(view.hotspots[0].procedure, "DoWork");
        assert!(!view.status.contains("truncated"));
    }
}
