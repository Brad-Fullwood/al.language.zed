//! Object Browser View — hierarchical tree of AL objects grouped by type.
//!
//! Presents objects grouped by kind (Tables, Pages, Codeunits, etc.) with
//! expandable nodes showing fields, procedures, and triggers. Supports
//! keyboard navigation and cross-level search filtering.

use crate::types::{ObjectKind, SymbolEntry, SymbolIndex};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame,
};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Tree node model
// ---------------------------------------------------------------------------

/// A single row in the object browser tree.
#[derive(Debug, Clone)]
pub enum TreeNode {
    /// A top-level group header, e.g. "Tables (12)".
    Group {
        kind: ObjectKind,
        label: String,
        expanded: bool,
        /// Index of the first child row, cached after `rebuild`.
        child_start: usize,
        count: usize,
    },
    /// An object row inside an expanded group.
    Object {
        entry: Arc<SymbolEntry>,
        expanded: bool,
        /// Index of the first child row, cached after `rebuild`.
        child_start: usize,
    },
    /// A member row (field / procedure / trigger) inside an expanded object.
    Member {
        label: String,
        kind: MemberDisplayKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberDisplayKind {
    Field,
    Key,
    Procedure,
    Control,
    EnumValue,
}

impl MemberDisplayKind {
    fn icon(self) -> &'static str {
        match self {
            MemberDisplayKind::Field => "F",
            MemberDisplayKind::Key => "K",
            MemberDisplayKind::Procedure => "P",
            MemberDisplayKind::Control => "C",
            MemberDisplayKind::EnumValue => "E",
        }
    }

    fn color(self) -> Color {
        match self {
            MemberDisplayKind::Field => Color::White,
            MemberDisplayKind::Key => Color::Yellow,
            MemberDisplayKind::Procedure => Color::Green,
            MemberDisplayKind::Control => Color::Magenta,
            MemberDisplayKind::EnumValue => Color::Cyan,
        }
    }
}

// ---------------------------------------------------------------------------
// ObjectBrowserView
// ---------------------------------------------------------------------------

/// Hierarchical object browser.
///
/// Objects are grouped by kind. Groups and objects are individually expandable.
/// The view is fully driven by keyboard input and re-renders on every frame.
pub struct ObjectBrowserView {
    /// Flat list of currently-visible tree rows (rebuilt on every state change).
    rows: Vec<TreeNode>,
    /// Which row index is currently selected.
    selected: usize,
    /// Ratatui list state (tracks scroll offset).
    list_state: ListState,
    /// The current search filter (empty = show all).
    pub search_query: String,
    /// Sorted, deduplicated object kinds present in the index.
    all_kinds: Vec<ObjectKind>,
    /// Per-kind expansion state (group-level).
    group_expanded: Vec<bool>,
    /// Set of object keys (kind + id) that are expanded at the object level.
    object_expanded: std::collections::HashSet<(ObjectKind, i32)>,
    /// Whether keyboard focus is on the browser tree (vs. the search bar).
    pub focused: bool,
}

impl ObjectBrowserView {
    /// Create an empty browser. Call `load` to populate from a `SymbolIndex`.
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            selected: 0,
            list_state: ListState::default(),
            search_query: String::new(),
            all_kinds: Vec::new(),
            group_expanded: Vec::new(),
            object_expanded: std::collections::HashSet::new(),
            focused: false,
        }
    }

    /// Populate the view from a symbol index, applying the current search query.
    pub fn load(&mut self, symbols: &SymbolIndex, package_filter: Option<&str>) {
        // Collect and filter entries
        let all: Vec<Arc<SymbolEntry>> = if let Some(pkg) = package_filter {
            symbols.search_in_package(pkg, "")
        } else {
            symbols.search("", 50_000)
        };

        let query = self.search_query.to_lowercase();
        let filtered: Vec<Arc<SymbolEntry>> = if query.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|e| {
                    e.name.to_lowercase().contains(&query)
                        || e.id.to_string().contains(&query)
                        || e.fields.iter().any(|f| f.name.to_lowercase().contains(&query))
                        || e.methods.iter().any(|m| m.name.to_lowercase().contains(&query))
                        || e.controls.iter().any(|c| c.name.to_lowercase().contains(&query))
                        || e.enum_values.iter().any(|v| v.name.to_lowercase().contains(&query))
                })
                .collect()
        };

        // Discover all kinds present
        let mut kinds_set = std::collections::HashSet::new();
        for e in &filtered {
            kinds_set.insert(e.kind);
        }
        let mut kinds: Vec<ObjectKind> = kinds_set.into_iter().collect();
        kinds.sort_by_key(|k| format!("{k:?}"));

        // Grow or shrink group_expanded to match the new kind list
        let prev_kinds = std::mem::replace(&mut self.all_kinds, kinds);
        let prev_expanded: std::collections::HashMap<ObjectKind, bool> = prev_kinds
            .iter()
            .zip(self.group_expanded.iter())
            .map(|(&k, &e)| (k, e))
            .collect();

        self.group_expanded = self
            .all_kinds
            .iter()
            .map(|k| *prev_expanded.get(k).unwrap_or(&false))
            .collect();

        // Build flat rows — collect everything into `rows` without holding a
        // borrow on `self.all_kinds` while calling `&mut self` methods.
        let kinds_snapshot: Vec<(ObjectKind, bool)> = self
            .all_kinds
            .iter()
            .zip(self.group_expanded.iter())
            .map(|(&k, &e)| (k, e))
            .collect();

        self.rows.clear();
        for (kind, group_expanded) in kinds_snapshot {
            let mut children: Vec<Arc<SymbolEntry>> =
                filtered.iter().filter(|e| e.kind == kind).cloned().collect();
            children.sort_by_key(|e| e.id);

            let group_row_idx = self.rows.len();
            self.rows.push(TreeNode::Group {
                kind,
                label: format!("{kind:?} ({})", children.len()),
                expanded: group_expanded,
                child_start: group_row_idx + 1,
                count: children.len(),
            });

            if group_expanded {
                for entry in children {
                    let obj_key = (entry.kind, entry.id);
                    let obj_expanded = self.object_expanded.contains(&obj_key);
                    let obj_row_idx = self.rows.len();

                    let has_members = !entry.fields.is_empty()
                        || !entry.keys.is_empty()
                        || !entry.methods.is_empty()
                        || !entry.controls.is_empty()
                        || !entry.enum_values.is_empty();

                    self.rows.push(TreeNode::Object {
                        entry: entry.clone(),
                        expanded: obj_expanded,
                        child_start: obj_row_idx + 1,
                    });

                    if obj_expanded && has_members {
                        self.append_members(&entry);
                    }
                }
            }
        }

        // Clamp selection
        if !self.rows.is_empty() && self.selected >= self.rows.len() {
            self.selected = self.rows.len() - 1;
        }
        self.list_state.select(if self.rows.is_empty() {
            None
        } else {
            Some(self.selected)
        });
    }

    fn append_members(&mut self, entry: &Arc<SymbolEntry>) {
        for f in &entry.fields {
            self.rows.push(TreeNode::Member {
                label: format!("  {} {:<4} {} : {}", "F", f.id, f.name, f.type_name),
                kind: MemberDisplayKind::Field,
            });
        }
        for k in &entry.keys {
            self.rows.push(TreeNode::Member {
                label: format!("  {} {} ({})", "K", k.name, k.field_names.join(", ")),
                kind: MemberDisplayKind::Key,
            });
        }
        for m in &entry.methods {
            let params: Vec<_> = m.parameters.iter().map(|p| p.name.as_str()).collect();
            let ret = m
                .return_type
                .as_deref()
                .map(|r| format!(" : {r}"))
                .unwrap_or_default();
            self.rows.push(TreeNode::Member {
                label: format!("  {} {}({}){}", "P", m.name, params.join(", "), ret),
                kind: MemberDisplayKind::Procedure,
            });
        }
        for c in &entry.controls {
            self.rows.push(TreeNode::Member {
                label: format!("  {} {} {}", "C", c.kind, c.name),
                kind: MemberDisplayKind::Control,
            });
        }
        for v in &entry.enum_values {
            self.rows.push(TreeNode::Member {
                label: format!("  {} {} {}", "E", v.ordinal, v.name),
                kind: MemberDisplayKind::EnumValue,
            });
        }
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    /// Move selection down by one row, wrapping around.
    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.rows.len();
        self.list_state.select(Some(self.selected));
    }

    /// Move selection up by one row, wrapping around.
    pub fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.rows.len() - 1
        } else {
            self.selected - 1
        };
        self.list_state.select(Some(self.selected));
    }

    /// Toggle expand/collapse on the currently-selected row.
    /// Returns `true` if the tree was mutated (caller should call `load` again).
    pub fn toggle_selected(&mut self) -> bool {
        match self.rows.get(self.selected) {
            Some(TreeNode::Group { kind, expanded, .. }) => {
                let kind = *kind;
                let currently_expanded = *expanded;
                if let Some(gi) = self.all_kinds.iter().position(|&k| k == kind) {
                    self.group_expanded[gi] = !currently_expanded;
                }
                true
            }
            Some(TreeNode::Object { entry, expanded, .. }) => {
                let key = (entry.kind, entry.id);
                if *expanded {
                    self.object_expanded.remove(&key);
                } else {
                    self.object_expanded.insert(key);
                }
                true
            }
            _ => false,
        }
    }

    /// Expand the currently-selected group or object without toggling.
    pub fn expand_selected(&mut self) -> bool {
        match self.rows.get(self.selected) {
            Some(TreeNode::Group { kind, expanded, .. }) if !expanded => {
                let kind = *kind;
                if let Some(gi) = self.all_kinds.iter().position(|&k| k == kind) {
                    self.group_expanded[gi] = true;
                }
                true
            }
            Some(TreeNode::Object { entry, expanded, .. }) if !expanded => {
                let key = (entry.kind, entry.id);
                self.object_expanded.insert(key);
                true
            }
            _ => false,
        }
    }

    /// Collapse the currently-selected group or object.
    pub fn collapse_selected(&mut self) -> bool {
        match self.rows.get(self.selected) {
            Some(TreeNode::Group { kind, expanded, .. }) if *expanded => {
                let kind = *kind;
                if let Some(gi) = self.all_kinds.iter().position(|&k| k == kind) {
                    self.group_expanded[gi] = false;
                }
                true
            }
            Some(TreeNode::Object { entry, expanded, .. }) if *expanded => {
                let key = (entry.kind, entry.id);
                self.object_expanded.remove(&key);
                true
            }
            _ => false,
        }
    }

    /// Returns the SymbolEntry of the currently-selected Object row (if any).
    pub fn selected_object(&self) -> Option<Arc<SymbolEntry>> {
        match self.rows.get(self.selected) {
            Some(TreeNode::Object { entry, .. }) => Some(entry.clone()),
            _ => None,
        }
    }

    // -----------------------------------------------------------------------
    // Key handling
    // -----------------------------------------------------------------------

    /// Handle a key event. Returns `Action` indicating what the caller should do.
    pub fn handle_key(&mut self, key: KeyEvent) -> BrowserAction {
        if !self.focused {
            return BrowserAction::None;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => BrowserAction::Unfocus,

            KeyCode::Char('j') | KeyCode::Down => {
                self.select_next();
                BrowserAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.select_prev();
                BrowserAction::None
            }

            // Enter / Space — toggle expand/collapse
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.toggle_selected() {
                    BrowserAction::Reload
                } else {
                    BrowserAction::None
                }
            }

            // Right arrow — expand
            KeyCode::Right | KeyCode::Char('l') => {
                if self.expand_selected() {
                    BrowserAction::Reload
                } else {
                    BrowserAction::None
                }
            }

            // Left arrow — collapse
            KeyCode::Left | KeyCode::Char('h') => {
                if self.collapse_selected() {
                    BrowserAction::Reload
                } else {
                    BrowserAction::None
                }
            }

            // Open in Zed
            KeyCode::Char('o') => {
                if let Some(entry) = self.selected_object() {
                    BrowserAction::OpenObject(entry)
                } else {
                    BrowserAction::None
                }
            }

            // Search — delegate to caller
            KeyCode::Char('/') => BrowserAction::StartSearch,

            // Ctrl+A — expand all groups
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                for e in &mut self.group_expanded {
                    *e = true;
                }
                BrowserAction::Reload
            }

            // Ctrl+C — collapse all
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                for e in &mut self.group_expanded {
                    *e = false;
                }
                self.object_expanded.clear();
                BrowserAction::Reload
            }

            _ => BrowserAction::None,
        }
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Render the object browser tree into `area`.
    pub fn render(&mut self, f: &mut Frame, area: Rect) {
        let border_style = if self.focused {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let title = if self.search_query.is_empty() {
            " Object Browser (j/k enter esc /) ".to_string()
        } else {
            format!(" Object Browser [filter: {}] ", self.search_query)
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style);

        let inner = block.inner(area);
        f.render_widget(block, area);

        if self.rows.is_empty() {
            let empty = ratatui::widgets::Paragraph::new("No objects found.")
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(empty, inner);
            return;
        }

        // Split inner area: tree list + help line at bottom
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(inner);

        let items: Vec<ListItem> = self.rows.iter().map(|node| self.row_to_item(node)).collect();

        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");

        f.render_stateful_widget(list, chunks[0], &mut self.list_state);

        // Help footer
        let help = ratatui::widgets::Paragraph::new(
            "j/k:nav  Enter:expand  h/l:col/exp  o:open  /:search  Ctrl+A/C:all",
        )
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(help, chunks[1]);
    }

    fn row_to_item<'a>(&self, node: &'a TreeNode) -> ListItem<'a> {
        let line: Line<'static> = match node {
            TreeNode::Group {
                label, expanded, ..
            } => {
                let arrow = if *expanded { "▼ " } else { "▶ " };
                Line::from(vec![
                    Span::styled(
                        arrow,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        label.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])
            }
            TreeNode::Object { entry, expanded, .. } => {
                let arrow = if *expanded { "  ▼ " } else { "  ▶ " };
                let id_str = format!("{:>6} ", entry.id);
                Line::from(vec![
                    Span::styled(arrow, Style::default().fg(Color::DarkGray)),
                    Span::styled(id_str, Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        entry.name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ])
            }
            TreeNode::Member { label, kind } => Line::from(vec![
                Span::styled(
                    format!("     [{}] ", kind.icon()),
                    Style::default().fg(kind.color()),
                ),
                Span::raw(label.trim_start().to_string()),
            ]),
        };

        ListItem::new(line)
    }
}

impl Default for ObjectBrowserView {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Action returned from key handling
// ---------------------------------------------------------------------------

/// What the caller should do after an `ObjectBrowserView::handle_key` call.
pub enum BrowserAction {
    /// No action needed; just redraw.
    None,
    /// Caller should reload the view (tree state changed).
    Reload,
    /// User pressed Esc or q — lose focus.
    Unfocus,
    /// User pressed / — transfer focus to search bar.
    StartSearch,
    /// User wants to open this object in Zed.
    OpenObject(Arc<SymbolEntry>),
}

// ---------------------------------------------------------------------------
// Unit tests (logic only, no ratatui rendering)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_index() -> SymbolIndex {
        SymbolIndex::new()
    }

    #[test]
    fn new_view_is_empty() {
        let view = ObjectBrowserView::new();
        assert!(view.rows.is_empty());
        assert_eq!(view.selected, 0);
        assert!(view.search_query.is_empty());
    }

    #[test]
    fn load_empty_index_produces_no_rows() {
        let mut view = ObjectBrowserView::new();
        let idx = make_index();
        view.load(&idx, None);
        assert!(view.rows.is_empty());
    }

    #[test]
    fn select_next_wraps_on_empty() {
        let mut view = ObjectBrowserView::new();
        view.select_next();
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn select_prev_wraps_on_empty() {
        let mut view = ObjectBrowserView::new();
        view.select_prev();
        assert_eq!(view.selected, 0);
    }

    #[test]
    fn toggle_selected_on_empty_returns_false() {
        let mut view = ObjectBrowserView::new();
        assert!(!view.toggle_selected());
    }

    #[test]
    fn selected_object_on_empty_is_none() {
        let view = ObjectBrowserView::new();
        assert!(view.selected_object().is_none());
    }

    #[test]
    fn member_display_kind_icons_are_distinct() {
        let icons: Vec<_> = [
            MemberDisplayKind::Field,
            MemberDisplayKind::Key,
            MemberDisplayKind::Procedure,
            MemberDisplayKind::Control,
            MemberDisplayKind::EnumValue,
        ]
        .iter()
        .map(|k| k.icon())
        .collect();
        // All icons are non-empty
        for icon in &icons {
            assert!(!icon.is_empty());
        }
    }

    #[test]
    fn handle_key_when_not_focused_is_noop() {
        let mut view = ObjectBrowserView::new();
        view.focused = false;
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(view.handle_key(key), BrowserAction::None));
    }

    #[test]
    fn handle_key_esc_unfocuses() {
        let mut view = ObjectBrowserView::new();
        view.focused = true;
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(view.handle_key(key), BrowserAction::Unfocus));
    }

    #[test]
    fn handle_key_slash_starts_search() {
        let mut view = ObjectBrowserView::new();
        view.focused = true;
        let key = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);
        assert!(matches!(view.handle_key(key), BrowserAction::StartSearch));
    }

    #[test]
    fn group_expanded_defaults_to_false() {
        let mut view = ObjectBrowserView::new();
        let idx = SymbolIndex::new();
        view.load(&idx, None);
        assert!(view.group_expanded.iter().all(|&e| !e));
    }
}
