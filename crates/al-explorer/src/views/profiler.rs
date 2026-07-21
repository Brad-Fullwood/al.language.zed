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

/// Aggregate per-node self time (in **microseconds**) from a profile's
/// `samples` + `timeDeltas` arrays.
///
/// Canonical implementation: `al_bc::profiling::aggregate_self_time_us`
/// (`crates/al-bc/src/profiling.rs`). This is a deliberate, behaviour-identical
/// port — al-explorer is a pure TUI/CLI crate and pulling in `al-bc` would drag
/// the whole reqwest + tokio networking stack in just to reuse one synchronous
/// function, and al-bc's only public entry point (`analyze_profile`) would
/// discard this view's TUI safeguards (node cap, BOM strip, GC filter). Keep the
/// aggregation convention here in lock-step with al-bc.
///
/// Convention: the `i`-th recorded sample (`samples[i]`, a node id) is charged
/// the `i`-th time delta (`timeDeltas[i]`, microseconds); deltas are summed per
/// sampled node id so self time reflects on-CPU time, not sample count. The two
/// arrays are expected to be equal length; if a malformed profile gives them
/// different lengths we iterate the common prefix (`min(len)`) so we can't panic.
///
/// Returns an empty map when either array is absent or empty, in which case the
/// caller falls back to the legacy "1 ms per hit" estimate.
fn aggregate_self_time_us(json: &serde_json::Value) -> std::collections::HashMap<u64, f64> {
    let mut by_node: std::collections::HashMap<u64, f64> = std::collections::HashMap::new();

    let (samples, deltas) = match (
        json.get("samples").and_then(|v| v.as_array()),
        json.get("timeDeltas").and_then(|v| v.as_array()),
    ) {
        (Some(s), Some(d)) if !s.is_empty() && !d.is_empty() => (s, d),
        _ => return by_node,
    };

    // Mismatched lengths => malformed profile; aggregate over the common prefix
    // so we never index out of bounds (matches al-bc, which logs a warning here;
    // this view has no tracing subscriber so we degrade silently).
    let n = samples.len().min(deltas.len());
    for i in 0..n {
        let Some(node_id) = samples[i].as_u64() else {
            continue;
        };
        // timeDeltas are integer microseconds in practice; read as f64 defensively.
        let delta_us = deltas[i].as_f64().unwrap_or(0.0);
        *by_node.entry(node_id).or_insert(0.0) += delta_us;
    }

    by_node
}

/// Roll up `total_time_ms` over the call tree: `total(node) = self(node) + Σ
/// total(child)` via an iterative post-order DFS (cycle/dangling-safe). Ported
/// from `al_bc::profiling::aggregate_total_time_ms` — see that canonical
/// copy; kept here to avoid pulling the heavy al-bc (reqwest/tokio) dep into this
/// TUI crate. `self_ms_by_node` must be in milliseconds and cover every node.
fn aggregate_total_time_ms(
    nodes: &[serde_json::Value],
    self_ms_by_node: &std::collections::HashMap<u64, f64>,
) -> std::collections::HashMap<u64, f64> {
    use std::collections::{HashMap, HashSet};
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut order: Vec<u64> = Vec::with_capacity(nodes.len());
    for node in nodes {
        let Some(id) = node.get("id").and_then(|v| v.as_u64()) else {
            continue;
        };
        order.push(id);
        let kids: Vec<u64> = node
            .get("children")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|c| c.as_u64()).collect())
            .unwrap_or_default();
        children.entry(id).or_default().extend(kids);
    }
    let mut total: HashMap<u64, f64> = HashMap::new();
    for &start in &order {
        if total.contains_key(&start) {
            continue;
        }
        let mut stack: Vec<(u64, bool)> = vec![(start, false)];
        let mut on_path: HashSet<u64> = HashSet::new();
        while let Some((id, expanded)) = stack.pop() {
            if expanded {
                on_path.remove(&id);
                let mut sum = self_ms_by_node.get(&id).copied().unwrap_or(0.0);
                if let Some(kids) = children.get(&id) {
                    for k in kids {
                        if let Some(t) = total.get(k) {
                            sum += *t;
                        }
                    }
                }
                total.insert(id, sum);
            } else {
                if total.contains_key(&id) || on_path.contains(&id) {
                    continue;
                }
                on_path.insert(id);
                stack.push((id, true));
                if let Some(kids) = children.get(&id) {
                    for &k in kids {
                        if !total.contains_key(&k) && !on_path.contains(&k) {
                            stack.push((k, false));
                        }
                    }
                }
            }
        }
    }
    total
}

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

        // Accurate self time: sum each node's sampled `timeDeltas` (µs). Empty
        // when the profile carries no `samples`/`timeDeltas`, in which case we
        // fall back to the legacy 1 ms-per-hit estimate below. Same convention
        // as al-bc — see `aggregate_self_time_us` above.
        let self_time_by_node = aggregate_self_time_us(&json);
        let have_time = !self_time_by_node.is_empty();

        // Per-node self-time in ms over ALL nodes (incl. root/idle/filtered and
        // hitCount==0), so the call-tree roll-up for total_time_ms is correct
        // even when a child is hidden from the displayed rows.
        let self_ms_by_node: std::collections::HashMap<u64, f64> = nodes
            .iter()
            .filter_map(|n| {
                let id = n.get("id").and_then(|v| v.as_u64())?;
                let ms = if have_time {
                    self_time_by_node.get(&id).copied().unwrap_or(0.0) / 1000.0
                } else {
                    n.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0) as f64
                };
                Some((id, ms))
            })
            .collect();
        let total_ms_by_node = aggregate_total_time_ms(nodes, &self_ms_by_node);

        let mut rows: Vec<HotspotRow> = Vec::new();
        for node in nodes {
            let hit_count = node.get("hitCount").and_then(|v| v.as_u64()).unwrap_or(0);
            if hit_count == 0 {
                continue;
            }
            let node_id = node.get("id").and_then(|v| v.as_u64());
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

            // Self time: aggregate this node's sampled `timeDeltas` (µs -> ms),
            // looked up by node id. hitCount alone is only the number of times
            // the sampler caught this node on top of the stack — accurate as ms
            // only when the sampling interval is exactly 1 ms. We fall back to
            // that legacy estimate only when the profile carries no
            // samples/timeDeltas to aggregate.
            let self_time_ms = if have_time {
                node_id
                    .and_then(|id| self_time_by_node.get(&id).copied())
                    .unwrap_or(0.0)
                    / 1000.0
            } else {
                hit_count as f64
            };
            rows.push(HotspotRow {
                procedure: function_name,
                object: url,
                self_time_ms,
                // Total = self + Σ descendants (call-tree roll-up), falling
                // back to self-time for id-less nodes not in the tree.
                total_time_ms: node_id
                    .and_then(|id| total_ms_by_node.get(&id).copied())
                    .unwrap_or(self_time_ms),
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

    /// Write `profile` to a unique temp `.alcpuprofile`, load it through the
    /// real `load_profile` parse path, then clean up.
    fn load_view(profile: &serde_json::Value) -> ProfilerView {
        let path = std::env::temp_dir().join(format!(
            "al-explorer-test-profile-{}-{}.alcpuprofile",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, serde_json::to_vec(profile).unwrap()).unwrap();
        let mut view = ProfilerView::new();
        view.file_path = path.to_string_lossy().into_owned();
        view.load_profile();
        std::fs::remove_file(&path).ok();
        view
    }

    #[test]
    fn time_based_self_time_outranks_hit_count() {
        // "Fast" is sampled often (10 hits) but each sample is cheap (100 µs =>
        // 1 ms total). "Slow" is sampled rarely (3 hits) but each sample is
        // expensive (5000 µs => 15 ms total). A hit-count ranking would put
        // Fast first; an accurate timeDeltas-based ranking must put Slow first.
        let mut samples: Vec<u64> = Vec::new();
        let mut time_deltas: Vec<u64> = Vec::new();
        for _ in 0..10 {
            samples.push(2);
            time_deltas.push(100);
        }
        for _ in 0..3 {
            samples.push(3);
            time_deltas.push(5000);
        }

        let profile = serde_json::json!({
            "startTime": 0.0,
            "endTime": 1_000_000.0,
            "nodes": [
                { "id": 1, "hitCount": 0u64, "callFrame": { "functionName": "(root)", "url": "" } },
                { "id": 2, "hitCount": 10u64, "callFrame": { "functionName": "Fast", "url": "Cod1.al" } },
                { "id": 3, "hitCount": 3u64, "callFrame": { "functionName": "Slow", "url": "Cod2.al" } },
            ],
            "samples": samples,
            "timeDeltas": time_deltas,
        });

        let view = load_view(&profile);

        assert_eq!(view.hotspots.len(), 2);
        // Time-based ranking: Slow (15 ms) beats Fast (1 ms) despite fewer hits.
        assert_eq!(view.hotspots[0].procedure, "Slow");
        assert!((view.hotspots[0].self_time_ms - 15.0).abs() < 1e-9);
        assert_eq!(view.hotspots[0].hit_count, 3);
        assert_eq!(view.hotspots[1].procedure, "Fast");
        assert!((view.hotspots[1].self_time_ms - 1.0).abs() < 1e-9);
        assert_eq!(view.hotspots[1].hit_count, 10);
    }

    #[test]
    fn mismatched_samples_and_time_deltas_lengths_handled() {
        // samples has 4 entries, timeDeltas only 2 — a malformed profile. The
        // aggregation must iterate the common prefix (min len = 2) without
        // panicking: node 2 is charged the two 3000 µs deltas => 6 ms.
        let profile = serde_json::json!({
            "startTime": 0.0,
            "endTime": 1_000_000.0,
            "nodes": [
                { "id": 2, "hitCount": 4u64, "callFrame": { "functionName": "Maybe", "url": "Cod3.al" } },
            ],
            "samples": [2u64, 2u64, 2u64, 2u64],
            "timeDeltas": [3000u64, 3000u64],
        });

        let view = load_view(&profile);

        assert_eq!(view.hotspots.len(), 1);
        assert_eq!(view.hotspots[0].procedure, "Maybe");
        assert!((view.hotspots[0].self_time_ms - 6.0).abs() < 1e-9);
        assert_eq!(view.hotspots[0].hit_count, 4);
    }

    #[test]
    fn falls_back_to_hit_count_without_samples() {
        // No samples/timeDeltas at all: self time falls back to 1 ms per hit.
        let profile = serde_json::json!({
            "startTime": 0.0,
            "endTime": 1_000_000.0,
            "nodes": [
                { "id": 2, "hitCount": 7u64, "callFrame": { "functionName": "Legacy", "url": "Cod4.al" } },
            ],
        });

        let view = load_view(&profile);

        assert_eq!(view.hotspots.len(), 1);
        assert_eq!(view.hotspots[0].procedure, "Legacy");
        assert!((view.hotspots[0].self_time_ms - 7.0).abs() < 1e-9);
    }

    #[test]
    fn total_time_rolls_up_the_call_tree() {
        // root(1) -> A(2) -> B(3). A self=1ms, B self=4ms.
        // A.total = 1 + 4 = 5ms; B.total = 4ms (leaf). B outranks A by self-time.
        let profile = serde_json::json!({
            "startTime": 0.0,
            "endTime": 1_000_000.0,
            "nodes": [
                { "id": 1, "hitCount": 0u64, "children": [2u64], "callFrame": { "functionName": "(root)", "url": "" } },
                { "id": 2, "hitCount": 1u64, "children": [3u64], "callFrame": { "functionName": "A", "url": "Cod1.al" } },
                { "id": 3, "hitCount": 1u64, "callFrame": { "functionName": "B", "url": "Cod2.al" } },
            ],
            "samples": [2u64, 3u64],
            "timeDeltas": [1000u64, 4000u64],
        });

        let view = load_view(&profile);
        let a = view
            .hotspots
            .iter()
            .find(|h| h.procedure == "A")
            .expect("A");
        let b = view
            .hotspots
            .iter()
            .find(|h| h.procedure == "B")
            .expect("B");
        assert!((a.self_time_ms - 1.0).abs() < 1e-9);
        assert!(
            (a.total_time_ms - 5.0).abs() < 1e-9,
            "A.total should roll up B: {}",
            a.total_time_ms
        );
        assert!((b.self_time_ms - 4.0).abs() < 1e-9);
        assert!(
            (b.total_time_ms - 4.0).abs() < 1e-9,
            "B is a leaf: {}",
            b.total_time_ms
        );
    }
}
