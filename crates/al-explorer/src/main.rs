use al_symbols::{SymbolEntry, SymbolIndex, ObjectKind};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use std::{
    error::Error,
    io,
    sync::Arc,
};

#[derive(PartialEq, Clone, Copy)]
enum ActivePane {
    Search,
    Packages,
    Objects,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickTarget {
    Objects,
    Details,
}

#[derive(Debug, Clone)]
enum DetailTargetKind {
    Field,
    Key,
    Control(String),
    EnumValue,
    Procedure,
}

#[derive(Debug, Clone)]
struct DetailTarget {
    name: String,
    kind: DetailTargetKind,
}

impl DetailTarget {
    fn to_member_kind(&self) -> al_symbols::virtual_file::MemberKind {
        match &self.kind {
            DetailTargetKind::Field => al_symbols::virtual_file::MemberKind::Field,
            DetailTargetKind::Key => al_symbols::virtual_file::MemberKind::Key,
            DetailTargetKind::Control(kind) => al_symbols::virtual_file::MemberKind::Control(kind.clone()),
            DetailTargetKind::EnumValue => al_symbols::virtual_file::MemberKind::EnumValue,
            DetailTargetKind::Procedure => al_symbols::virtual_file::MemberKind::Procedure,
        }
    }
}

struct App {
    pub active_pane: ActivePane,
    pub search_query: String,
    pub global_search: bool,
    
    pub packages: Vec<String>,
    pub package_list_state: ListState,
    
    pub kinds: Vec<ObjectKind>,
    pub active_kind_index: usize,

    pub symbols: SymbolIndex,
    pub current_objects: Vec<Arc<SymbolEntry>>,
    pub object_list_state: ListState,
    
    pub details_list_state: ListState,
    pub details_items: Vec<(Option<DetailTarget>, Line<'static>)>,

    pub should_quit: bool,
    
    pub last_click_time: std::time::Instant,
    pub last_click_target: Option<ClickTarget>,
    pub last_click_index: usize,
}

impl App {
    fn new() -> App {
        App {
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
        }
    }

    fn init_workspace(&mut self) -> Result<(), Box<dyn Error>> {
        let root = std::env::current_dir()?;
        let packages_dir = root.join(".alpackages");
        let packages: Vec<std::path::PathBuf> = std::fs::read_dir(&packages_dir)
            .into_iter()
            .flat_map(|entries| entries.into_iter())
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("app")))
            .collect();
        if !packages.is_empty() {
            let loaded = self.symbols.load_packages(&packages);
            
            // Extract unique package names directly from loaded data
            let mut pkg_names: Vec<String> = loaded.into_iter().map(|p| p.name).collect();
            pkg_names.sort();
            pkg_names.dedup();
            self.packages = pkg_names;

            if !self.packages.is_empty() {
                self.package_list_state.select(Some(0));
                self.update_objects_list(true);
            }
        }
        Ok(())
    }

    fn update_objects_list(&mut self, reset_selection: bool) {
        if let Some(selected) = self.package_list_state.selected()
            && let Some(pkg_name) = self.packages.get(selected) {
                // Fetch items based on global search or package scope
                let results = if self.global_search && !self.search_query.is_empty() {
                    self.symbols.search(&self.search_query, 5000)
                } else {
                    self.symbols.search_in_package(pkg_name, "")
                };
                
                let query = self.search_query.to_lowercase();
                
                let mut filtered = Vec::new();
                let mut kinds_set = std::collections::HashSet::new();
                
                for r in results {
                    let matches_search = query.is_empty() 
                        || r.name.to_lowercase().contains(&query)
                        || r.id.to_string().contains(&query);
                        
                    if matches_search {
                        kinds_set.insert(r.kind);
                        filtered.push(r);
                    }
                }
                
                // Group the available kinds and sort them deterministically
                let mut kinds: Vec<_> = kinds_set.into_iter().collect();
                kinds.sort_by_key(|k| format!("{:?}", k));
                
                let current_kind = self.kinds.get(self.active_kind_index).copied();
                self.kinds = kinds;

                // Try to preserve the selected kind tab if it's still available in the new search results
                if let Some(k) = current_kind {
                    if let Some(new_idx) = self.kinds.iter().position(|&x| x == k) {
                        self.active_kind_index = new_idx;
                    } else {
                        self.active_kind_index = 0;
                    }
                } else {
                    self.active_kind_index = 0;
                }

                // Filter objects by the active kind
                let mut final_objects = Vec::new();
                if let Some(k) = self.kinds.get(self.active_kind_index) {
                    final_objects = filtered.into_iter().filter(|r| r.kind == *k).collect();
                    final_objects.sort_by_key(|e| e.id); // Sorted strictly by ID
                }
                
                self.current_objects = final_objects;
                
                if !self.current_objects.is_empty() {
                    if reset_selection {
                        self.object_list_state.select(Some(0));
                        self.details_list_state.select(Some(0));
                    } else {
                        let current = self.object_list_state.selected().unwrap_or(0);
                        let safe_idx = std::cmp::min(current, self.current_objects.len().saturating_sub(1));
                        self.object_list_state.select(Some(safe_idx));
                        self.details_list_state.select(Some(0));
                    }
                } else {
                    self.object_list_state.select(None);
                    self.details_list_state.select(None);
                }
        }
    }

    fn next_package(&mut self) {
        let i = match self.package_list_state.selected() {
            Some(i) => {
                if i >= self.packages.len().saturating_sub(1) { 0 } else { i + 1 }
            }
            None => 0,
        };
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    fn previous_package(&mut self) {
        let i = match self.package_list_state.selected() {
            Some(i) => {
                if i == 0 { self.packages.len().saturating_sub(1) } else { i - 1 }
            }
            None => 0,
        };
        self.package_list_state.select(Some(i));
        self.update_objects_list(true);
    }

    fn next_kind(&mut self) {
        if self.kinds.is_empty() { return; }
        self.active_kind_index = (self.active_kind_index + 1) % self.kinds.len();
        self.update_objects_list(true);
    }
    
    fn previous_kind(&mut self) {
        if self.kinds.is_empty() { return; }
        if self.active_kind_index == 0 {
            self.active_kind_index = self.kinds.len() - 1;
        } else {
            self.active_kind_index -= 1;
        }
        self.update_objects_list(true);
    }

    fn next_object(&mut self) {
        let i = match self.object_list_state.selected() {
            Some(i) => {
                if i >= self.current_objects.len().saturating_sub(1) { 0 } else { i + 1 }
            }
            None => 0,
        };
        if !self.current_objects.is_empty() {
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
        }
    }

    fn previous_object(&mut self) {
        let i = match self.object_list_state.selected() {
            Some(i) => {
                if i == 0 { self.current_objects.len().saturating_sub(1) } else { i - 1 }
            }
            None => 0,
        };
        if !self.current_objects.is_empty() {
            self.object_list_state.select(Some(i));
            self.details_list_state.select(Some(0));
        }
    }

    fn next_detail(&mut self) {
        let i = match self.details_list_state.selected() {
            Some(i) => {
                if i >= self.details_items.len().saturating_sub(1) { 0 } else { i + 1 }
            }
            None => 0,
        };
        if !self.details_items.is_empty() {
            self.details_list_state.select(Some(i));
        }
    }

    fn previous_detail(&mut self) {
        let i = match self.details_list_state.selected() {
            Some(i) => {
                if i == 0 { self.details_items.len().saturating_sub(1) } else { i - 1 }
            }
            None => 0,
        };
        if !self.details_items.is_empty() {
            self.details_list_state.select(Some(i));
        }
    }

    fn open_selected_object(&mut self) {
        // Find if they clicked a specific member
        let mut target_member: Option<DetailTarget> = None;
        if let Some(detail_idx) = self.details_list_state.selected()
            && let Some((Some(member), _)) = self.details_items.get(detail_idx) {
            target_member = Some(member.clone());
        }

        if let Some(selected) = self.object_list_state.selected()
            && let Some(entry) = self.current_objects.get(selected) {
            let app_path = self.symbols.app_path(&entry.package);
            if let Ok(path) = al_symbols::virtual_file::get_or_create(entry, app_path.as_deref())
                && let Ok(abs_path) = std::fs::canonicalize(&path)
                && let Some(path_str) = abs_path.to_str() {
                let mut zed_url = format!("zed://file{}", path_str);

                if let Some(member) = target_member {
                    let line = al_symbols::virtual_file::find_member_line_with_kind(
                        &abs_path,
                        &member.name,
                        member.to_member_kind(),
                    )
                    .or_else(|| al_symbols::virtual_file::find_member_line(&abs_path, &member.name));
                    if let Some(line) = line {
                        zed_url = format!("zed://file{}:{}:1", path_str, line + 1);
                    }
                }

                let _ = open::that(zed_url);
            }
        }
    }

    fn register_click(&mut self, target: ClickTarget, index: usize) -> bool {
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Enable Mouse Capture!
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    app.init_workspace()?;

    let res = run_app(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("{:?}", err);
    }

    Ok(())
}

fn run_app<B: Backend<Error = io::Error>>(terminal: &mut Terminal<B>, mut app: App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            let evt = event::read()?;
            match evt {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(());
                }

                // Global Tab bindings for type filters
                if key.code == KeyCode::Tab {
                    app.next_kind();
                    continue;
                }
                if key.code == KeyCode::BackTab {
                    app.previous_kind();
                    continue;
                }

                match app.active_pane {
                        ActivePane::Search => {
                            match key.code {
                                KeyCode::Esc => {
                                    app.search_query.clear();
                                    app.update_objects_list(true);
                                }
                                KeyCode::Backspace => {
                                    app.search_query.pop();
                                    app.update_objects_list(true);
                                }
                                KeyCode::Char(c) => {
                                    app.search_query.push(c);
                                    app.update_objects_list(true);
                                }
                                KeyCode::Down | KeyCode::Enter => {
                                    app.active_pane = ActivePane::Packages;
                                }
                                KeyCode::Right => {
                                    app.active_pane = ActivePane::Objects;
                                }
                                KeyCode::Tab => {
                                    app.global_search = !app.global_search;
                                    app.update_objects_list(true);
                                }
                                _ => {}
                            }
                        }
                        ActivePane::Packages => {
                            match key.code {
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
                            }
                        }
                        ActivePane::Objects => {
                            match key.code {
                                KeyCode::Down | KeyCode::Char('j') => app.next_object(),
                                KeyCode::Up | KeyCode::Char('k') => app.previous_object(),
                                KeyCode::Left | KeyCode::Char('h') => {
                                    app.active_pane = ActivePane::Packages;
                                }
                                KeyCode::Right | KeyCode::Char('l') => {
                                    app.active_pane = ActivePane::Details;
                                }
                                KeyCode::Enter => {
                                    app.details_list_state.select(None); // Ensure no specific detail member is passed
                                    app.open_selected_object();
                                }
                                KeyCode::Esc => {
                                    app.active_pane = ActivePane::Search;
                                }
                                _ => {}
                            }
                        }
                        ActivePane::Details => {
                            match key.code {
                                KeyCode::Down | KeyCode::Char('j') => app.next_detail(),
                                KeyCode::Up | KeyCode::Char('k') => app.previous_detail(),
                                KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => {
                                    app.active_pane = ActivePane::Objects;
                                }
                                KeyCode::Enter => app.open_selected_object(),
                                _ => {}
                            }
                        }
                    }
                }
                Event::Mouse(mouse_event) => {
                    if let Ok((width, height)) = crossterm::terminal::size() {
                        let layout = compute_layout(Rect { x: 0, y: 0, width, height });
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
                            .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
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
                _ => {}
            }
        }
        if app.should_quit {
            return Ok(());
        }
    }
}

struct UiLayout {
    main_columns: [Rect; 3],
    left_column: [Rect; 2],
    middle_column: [Rect; 2],
}

fn compute_layout(size: Rect) -> UiLayout {
    let main_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(20),
            Constraint::Percentage(30),
            Constraint::Percentage(50),
        ])
        .split(size);

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

fn ui(f: &mut Frame, app: &mut App) {
    let size = f.area();
    let layout = compute_layout(size);

    // ==========================================
    // 1. Search Bar (Left Top)
    // ==========================================
    let search_style = if app.active_pane == ActivePane::Search {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    
    let search_title = " Search ";
    let search_block = Block::default()
        .borders(Borders::ALL)
        .title(search_title)
        .border_style(search_style);
        
    let search_inner = search_block.inner(layout.left_column[0]);
    f.render_widget(search_block, layout.left_column[0]);

    let search_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(5)].as_ref())
        .split(search_inner);

    let cursor = if app.active_pane == ActivePane::Search { "█" } else { "" };
    let search_text_str = format!("{}{}", app.search_query, cursor);
    
    let search_text = Paragraph::new(search_text_str)
        .style(Style::default().fg(Color::White));
    f.render_widget(search_text, search_chunks[0]);

    let filter_icon = if app.global_search { "[ALL]" } else { "[PKG]" };
    let icon_p = Paragraph::new(filter_icon)
        .alignment(ratatui::layout::Alignment::Right);
    f.render_widget(icon_p, search_chunks[1]);


    // ==========================================
    // 2. Packages List (Left Bottom)
    // ==========================================
    let pkg_style = if app.active_pane == ActivePane::Packages {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    
    let packages: Vec<ListItem> = app.packages.iter()
        .map(|i| ListItem::new(Line::from(vec![Span::raw(i.clone())])))
        .collect();

    let packages_list = List::new(packages)
        .block(Block::default().borders(Borders::ALL).title(" Packages ").border_style(pkg_style))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol(">> ");
    f.render_stateful_widget(packages_list, layout.left_column[1], &mut app.package_list_state);


    // ==========================================
    // 3. Types Tabs (Middle Top)
    // ==========================================
    let obj_style = if app.active_pane == ActivePane::Objects {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let tabs_block = Block::default()
        .borders(Borders::ALL)
        .title(" Types (Press Tab) ")
        .border_style(obj_style);
        
    let tabs_inner_area = tabs_block.inner(layout.middle_column[0]);
    f.render_widget(tabs_block, layout.middle_column[0]);

    if !app.kinds.is_empty() {
        let total = app.kinds.len();
        let idx = app.active_kind_index;
        
        let prev_idx = if idx == 0 { total.saturating_sub(1) } else { idx - 1 };
        let next_idx = if idx == total.saturating_sub(1) { 0 } else { idx + 1 };

        let arrow_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
            .split(tabs_inner_area);

        let left_arrow = if total > 1 { "<" } else { " " };
        let right_arrow = if total > 1 { ">" } else { " " };
        let arrow_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM);
        f.render_widget(Paragraph::new(left_arrow).alignment(ratatui::layout::Alignment::Center).style(arrow_style), arrow_chunks[0]);
        f.render_widget(Paragraph::new(right_arrow).alignment(ratatui::layout::Alignment::Center).style(arrow_style), arrow_chunks[2]);

        let middle_width = arrow_chunks[1].width as usize;
        if middle_width > 0 {
            let center_width = std::cmp::min(std::cmp::max(10, middle_width / 2), middle_width);
            let side_total = middle_width.saturating_sub(center_width);
            let left_width = side_total / 2;
            let right_width = side_total.saturating_sub(left_width);

            let prev_label = if total > 1 { format!("{:?}", app.kinds[prev_idx]) } else { "".to_string() };
            let next_label = if total > 1 {
                let display_idx = if total == 2 { prev_idx } else { next_idx };
                format!("{:?}", app.kinds[display_idx])
            } else {
                "".to_string()
            };
            let active_label = format!("{:?}", app.kinds[idx]);

            let left_text = pad_center(truncate_with_ellipsis(&prev_label, left_width), left_width);
            let right_text = pad_center(truncate_with_ellipsis(&next_label, right_width), right_width);
            let center_text = pad_center(truncate_with_ellipsis(&active_label, center_width), center_width);

            let spans = vec![
                Span::styled(left_text, Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)),
                Span::styled(center_text, Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(right_text, Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)),
            ];
            let p = Paragraph::new(Line::from(spans))
                .alignment(ratatui::layout::Alignment::Left);
            f.render_widget(p, arrow_chunks[1]);
        }
    }


    // ==========================================
    // 4. Objects List (Middle Bottom)
    // ==========================================
    let objects: Vec<ListItem> = app.current_objects.iter()
        .map(|entry| {
            let display = format!("{} {}", entry.id, entry.name);
            ListItem::new(Line::from(vec![Span::raw(display)]))
        })
        .collect();

    let objects_list = List::new(objects)
        .block(Block::default().borders(Borders::ALL).border_style(obj_style))
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD))
        .highlight_symbol(">> ");
    f.render_stateful_widget(objects_list, layout.middle_column[1], &mut app.object_list_state);


    // ==========================================
    // 5. Details Pane (Right Full Column)
    // ==========================================
    let detail_style = if app.active_pane == ActivePane::Details {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    app.details_items.clear();
    
    if let Some(selected) = app.object_list_state.selected()
        && let Some(entry) = app.current_objects.get(selected) {
        app.details_items.push((None, Line::from(vec![
                Span::styled(format!("{:?} ", entry.kind), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(entry.id.to_string(), Style::default().fg(Color::Cyan)),
                Span::raw(" ".to_string()),
                Span::styled(entry.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
            ])));
            
            if let Some(extends) = &entry.extends {
                app.details_items.push((None, Line::from(vec![Span::styled("Extends: ".to_string(), Style::default().add_modifier(Modifier::BOLD)), Span::raw(extends.clone())])));
            }
            
            app.details_items.push((None, Line::from(vec![Span::styled("Package: ".to_string(), Style::default().add_modifier(Modifier::BOLD)), Span::raw(entry.package.clone())])));
            
            if !entry.properties.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled("Properties:".to_string(), Style::default().add_modifier(Modifier::BOLD)))));
                for p in entry.properties.iter() {
                    app.details_items.push((None, Line::from(vec![
                        Span::raw("    ".to_string()),
                        Span::styled(format!("{:<20}", p.name), Style::default().fg(Color::DarkGray)),
                        Span::raw(" = ".to_string()),
                        Span::raw(p.value.clone()),
                    ])));
                }
            }

            if !entry.keys.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Keys ({}):", entry.keys.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for k in entry.keys.iter() {
                    let fields = k.field_names.join(", ");
                    app.details_items.push((Some(DetailTarget { name: k.name.clone(), kind: DetailTargetKind::Key }), Line::from(vec![
                        Span::raw("    ".to_string()),
                        Span::styled(format!("{:<20}", k.name), Style::default().fg(Color::Cyan)),
                        Span::raw(format!(" ({})", fields)),
                    ])));
                }
            }
            
            if !entry.fields.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Fields ({}):", entry.fields.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for f in entry.fields.iter() {
                    app.details_items.push((Some(DetailTarget { name: f.name.clone(), kind: DetailTargetKind::Field }), Line::from(vec![
                        Span::styled(format!("    {:<4} ", f.id), Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{:<30}", f.name), Style::default().fg(Color::White)),
                        Span::styled(format!(" : {}", f.type_name), Style::default().fg(Color::Cyan)),
                    ])));
                }
            }

            if !entry.controls.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Controls/Actions ({}):", entry.controls.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for c in entry.controls.iter() {
                    app.details_items.push((Some(DetailTarget { name: c.name.clone(), kind: DetailTargetKind::Control(c.kind.clone()) }), Line::from(vec![
                        Span::raw("    ".to_string()),
                        Span::styled(format!("{:<15}", c.kind), Style::default().fg(Color::Magenta)),
                        Span::raw(format!(" {}", c.name)),
                    ])));
                }
            }
            
            if !entry.enum_values.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Values ({}):", entry.enum_values.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for v in entry.enum_values.iter() {
                    app.details_items.push((Some(DetailTarget { name: v.name.clone(), kind: DetailTargetKind::EnumValue }), Line::from(vec![
                        Span::styled(format!("    {:<4} ", v.ordinal), Style::default().fg(Color::DarkGray)),
                        Span::raw(v.name.clone()),
                    ])));
                }
            }

            if !entry.methods.is_empty() {
                app.details_items.push((None, Line::from("".to_string())));
                app.details_items.push((None, Line::from(Span::styled(format!("Procedures ({}):", entry.methods.len()), Style::default().add_modifier(Modifier::BOLD)))));
                for m in entry.methods.iter() {
                    let mut spans = vec![Span::raw("    ".to_string())];
                    if m.is_local {
                        spans.push(Span::styled("local ", Style::default().fg(Color::DarkGray)));
                    } else {
                        spans.push(Span::raw("      ".to_string())); // Align with local
                    }
                    spans.push(Span::styled(m.name.clone(), Style::default().fg(Color::Green)));
                    spans.push(Span::raw("(".to_string()));
                    
                    let params = m.parameters.iter().map(|p| p.name.to_string()).collect::<Vec<_>>().join(", ");
                    spans.push(Span::raw(params));
                    
                    spans.push(Span::raw(")".to_string()));
                    
                    if let Some(ret) = &m.return_type {
                        spans.push(Span::styled(format!(" : {}", ret), Style::default().fg(Color::Cyan)));
                    }
                    
                    app.details_items.push((Some(DetailTarget { name: m.name.clone(), kind: DetailTargetKind::Procedure }), Line::from(spans)));
                }
        }
    }

    if app.details_items.is_empty() {
        app.details_items.push((None, Line::from("No object selected".to_string())));
    }

    let list_items: Vec<ListItem> = app.details_items.iter().map(|(_, line)| ListItem::new(line.clone())).collect();

    let details_list = List::new(list_items)
        .block(Block::default().borders(Borders::ALL).title(" Details (Scroll/Click) ").border_style(detail_style))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
        
    f.render_stateful_widget(details_list, layout.main_columns[2], &mut app.details_list_state);
}

fn truncate_with_ellipsis(s: &str, width: usize) -> String {
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

fn pad_center(s: String, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    format!("{:^width$}", s, width = width)
}
