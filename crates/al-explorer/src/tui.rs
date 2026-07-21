//! Terminal setup/teardown and the TUI event loop that drives [`App`] and
//! renders the active view.

use std::error::Error;
use std::io;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::views::call_graph::{handle_call_graph_key, render_call_graph};
use crate::views::event_chain::{handle_event_chain_key, render_event_chain};
use crate::views::object_browser::{
    handle_object_browser_key, handle_object_browser_mouse, render_object_browser,
};
use crate::views::profiler::{handle_profiler_key, render_profiler};
use crate::views::test_runner::{handle_test_runner_key, render_test_runner};
use crate::{App, ViewMode};

pub(crate) fn run_tui() -> Result<(), Box<dyn Error>> {
    // Pre-flight: refuse with a human-readable message instead of letting
    // crossterm propagate ENXIO (code 6) when stdin/stdout aren't a TTY.
    // Without this, `al-explorer | tee log` or running
    // in a CI step prints `Error: Os { code: 6 }` and exits non-zero with
    // no hint that the TUI cannot run headless.
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err("al-explorer TUI requires an interactive terminal — \
             stdin or stdout is not a TTY. Use `al-explorer <subcommand>` \
             for scripted output (run `al-explorer --help` for the CLI list)."
            .into());
    }

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stderr(), crossterm::terminal::LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    // Non-blocking: the first frame renders immediately with a
    // "Loading workspace…" status while the daemon starts and indexes
    // in the background.
    app.start_init_workspace();

    let res = run_app(&mut terminal, app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        eprintln!("{:?}", err);
    }

    Ok(())
}

fn run_app<B: Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    mut app: App,
) -> io::Result<()> {
    loop {
        app.poll_init();
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(std::time::Duration::from_millis(250))? {
            let evt = event::read()?;
            match evt {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return Ok(());
                    }

                    // Global view switching — F1..F5, with Alt+1..Alt+5 as
                    // equivalents for terminals where the host editor
                    // swallows the function keys (Zed binds F4/F5 to
                    // its own debugger commands and they never reach the
                    // embedded terminal).
                    let alt_digit = if key.modifiers.contains(KeyModifiers::ALT) {
                        match key.code {
                            KeyCode::Char(c @ '1'..='5') => Some(c as u8 - b'0'),
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let fkey = match key.code {
                        KeyCode::F(n @ 1..=5) => Some(n),
                        _ => alt_digit,
                    };
                    match fkey {
                        Some(1) => {
                            app.view_mode = ViewMode::ObjectBrowser;
                            continue;
                        }
                        Some(2) => {
                            app.view_mode = ViewMode::EventChain;
                            continue;
                        }
                        Some(3) => {
                            app.view_mode = ViewMode::CallGraph;
                            continue;
                        }
                        Some(4) => {
                            app.view_mode = ViewMode::Profiler;
                            continue;
                        }
                        Some(5) => {
                            app.view_mode = ViewMode::TestRunner;
                            app.test_runner.refresh_discovery();
                            continue;
                        }
                        _ => {}
                    }

                    match app.view_mode {
                        ViewMode::ObjectBrowser => handle_object_browser_key(&mut app, key),
                        ViewMode::EventChain => handle_event_chain_key(&mut app, key),
                        ViewMode::CallGraph => handle_call_graph_key(&mut app, key),
                        ViewMode::Profiler => handle_profiler_key(&mut app, key),
                        ViewMode::TestRunner => handle_test_runner_key(&mut app, key),
                    }
                }
                Event::Mouse(mouse_event) => {
                    if app.view_mode == ViewMode::ObjectBrowser {
                        handle_object_browser_mouse(&mut app, mouse_event);
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

fn ui(f: &mut Frame, app: &mut App) {
    let size = f.area();

    let top_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(size);

    render_mode_bar(f, top_split[0], app.view_mode);

    match app.view_mode {
        ViewMode::ObjectBrowser => render_object_browser(f, top_split[1], app),
        ViewMode::EventChain => render_event_chain(f, top_split[1], &mut app.event_chain),
        ViewMode::CallGraph => render_call_graph(f, top_split[1], &mut app.call_graph),
        ViewMode::Profiler => render_profiler(f, top_split[1], &mut app.profiler),
        ViewMode::TestRunner => render_test_runner(f, top_split[1], &mut app.test_runner),
    }
}

fn render_mode_bar(f: &mut Frame, area: Rect, mode: ViewMode) {
    // Alt+1..5 are equivalents for terminals where the host editor (e.g.
    // Zed's debugger keymap) swallows the function keys.
    let tabs = [
        (" F1|M-1: Objects ", ViewMode::ObjectBrowser),
        (" F2|M-2: Events ", ViewMode::EventChain),
        (" F3|M-3: CallGraph ", ViewMode::CallGraph),
        (" F4|M-4: Profiler ", ViewMode::Profiler),
        (" F5|M-5: Tests ", ViewMode::TestRunner),
    ];

    let spans: Vec<Span> = tabs
        .iter()
        .map(|(label, tab_mode)| {
            if *tab_mode == mode {
                Span::styled(
                    *label,
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(*label, Style::default().fg(Color::DarkGray))
            }
        })
        .collect();

    let quit_hint = Span::styled("  Ctrl+C: Quit", Style::default().fg(Color::DarkGray));
    let mut all_spans = spans;
    all_spans.push(quit_hint);

    f.render_widget(Paragraph::new(Line::from(all_spans)), area);
}
