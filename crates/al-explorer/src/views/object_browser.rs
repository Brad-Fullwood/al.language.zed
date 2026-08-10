use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::{
    ActivePane, App, ClickTarget, MAX_INPUT_LEN, display_object_id, pad_center, pane_style,
    truncate_with_ellipsis,
};

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

pub(crate) fn handle_object_browser_key(app: &mut App, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

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
                if app.search_query.len() < MAX_INPUT_LEN {
                    app.search_query.push(c);
                    app.update_objects_list(true);
                }
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

pub(crate) fn handle_object_browser_mouse(
    app: &mut App,
    mouse_event: crossterm::event::MouseEvent,
) {
    // Use the area the object browser was ACTUALLY last rendered into
    // (recorded by `render_object_browser`) rather than recomputing it from
    // `crossterm::terminal::size()`. Immediately after a terminal resize —
    // before the next ~250ms redraw tick — the live terminal size and the
    // pane rectangles on screen disagree, so hit-testing against the live
    // size would dispatch clicks/scrolls against stale rectangles. No area
    // has been rendered yet on the very first event, in which case there is
    // nothing sensible to hit-test against.
    if let Some(content_area) = app.object_browser_area {
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

pub(crate) fn render_object_browser(f: &mut Frame, area: Rect, app: &mut App) {
    // Record the exact area this frame renders into so mouse hit-testing
    // (`handle_object_browser_mouse`) uses the pane rectangles that are
    // actually on screen, not a value recomputed from a possibly-stale
    // `crossterm::terminal::size()`.
    app.object_browser_area = Some(area);
    let layout = compute_layout(area);

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

    let pkg_style = pane_style(app.active_pane == ActivePane::Packages);

    let packages: Vec<ratatui::widgets::ListItem> = app
        .packages
        .iter()
        .map(|i| ratatui::widgets::ListItem::new(Line::from(vec![Span::raw(i.clone())])))
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

    if let Some(status) = &app.init_status {
        // Workspace still loading (or failed): show the status where the
        // objects will appear instead of a silently empty pane.
        let status_lower = status.to_ascii_lowercase();
        let style = if status_lower.contains("failed") || status_lower.contains("error") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };
        let loading = Paragraph::new(status.as_str())
            .style(style)
            .wrap(ratatui::widgets::Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(obj_style),
            );
        f.render_widget(loading, layout.middle_column[1]);
    } else {
        let objects: Vec<ListItem> = app
            .current_objects
            .iter()
            .map(|entry| {
                // Object IDs: AL interfaces (and a few other kinds) have no
                // developer-visible ID — symbol packages carry an internal
                // compiler hash there. Render blank instead of the hash /
                // `-1` sentinel.
                let display = match display_object_id(entry) {
                    Some(id) => format!("{} {}", id, entry.name),
                    None => entry.name.clone(),
                };
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
    }

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

#[cfg(test)]
mod mouse_hit_testing_tests {
    use super::*;
    use crate::App;

    fn area(x: u16, y: u16, width: u16, height: u16) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn click_uses_the_last_rendered_area_not_live_terminal_size() {
        let mut app = App::new();
        app.packages = vec!["Pkg1".to_string(), "Pkg2".to_string(), "Pkg3".to_string()];
        // Simulate a render at a small, atypical area — deliberately distinct
        // from whatever the real (headless test-process) terminal size is,
        // proving the handler reads this stored area and not
        // `crossterm::terminal::size()`.
        let rendered_area = area(0, 1, 100, 40);
        app.object_browser_area = Some(rendered_area);

        let layout = compute_layout(rendered_area);
        let packages_inner = inner_area(layout.left_column[1]);
        let click_row = packages_inner.y + 1; // second package row (0-indexed offset 1)
        let event = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: packages_inner.x,
            row: click_row,
            modifiers: crossterm::event::KeyModifiers::empty(),
        };

        handle_object_browser_mouse(&mut app, event);

        assert!(matches!(app.active_pane, ActivePane::Packages));
        assert_eq!(app.package_list_state.selected(), Some(1));
    }

    #[test]
    fn mouse_event_before_any_render_is_a_noop() {
        let mut app = App::new();
        assert!(app.object_browser_area.is_none());
        let event = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: crossterm::event::KeyModifiers::empty(),
        };

        // Must not panic, and must not guess at a pane to activate.
        handle_object_browser_mouse(&mut app, event);
        assert!(matches!(app.active_pane, ActivePane::Search));
    }
}
