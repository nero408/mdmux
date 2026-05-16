//! mdmux — entry point.
//!
//! Walks the current directory for markdown files, lets the user navigate a
//! tree, and on Enter renders the file in a [cmux](https://cmux.app) markdown
//! side-panel with live reload.

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
    MouseEvent, MouseEventKind, poll, read,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use mdmux::app::{Action, App, Mode};
use mdmux::cmux::{CliCmux, CmuxClient};
use mdmux::tree::NodeKind;
use mdmux::ui::draw;

#[derive(Parser, Debug)]
#[command(
    name = "mdmux",
    version,
    about = "Browse markdown files in a tree, render them in a cmux side-panel",
    long_about = "mdmux — a terminal UI for browsing markdown files in a directory tree.\n\
                  Pressing Enter on a file opens it in a cmux markdown panel split to the right\n\
                  with live reload. The panel is replaced on each new selection.\n\n\
                  Run inside a cmux session. Press `?` at any time for keybindings."
)]
struct Cli {
    /// Root directory to browse. Defaults to current working directory.
    #[arg()]
    root: Option<PathBuf>,

    /// Show hidden files / directories (dotfiles).
    #[arg(long)]
    hidden: bool,

    /// Don't respect .gitignore (show ignored markdown files too).
    #[arg(long)]
    no_gitignore: bool,

    /// List markdown files instead of running the TUI (debugging/scripting).
    #[arg(long)]
    list: bool,

    /// Maximum recursion depth.
    #[arg(long)]
    max_depth: Option<usize>,

    /// Override the cmux binary (useful for tests).
    #[arg(long, env = "CMUX_BIN")]
    cmux_bin: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let root = cli
        .root
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    if cli.list {
        return list_files(root, &cli);
    }

    let cmux: Box<dyn CmuxClient> = Box::new(CliCmux {
        binary: cli.cmux_bin.clone(),
    });
    let cmux_available = cmux.is_available();

    let mut app = App::new(root, cmux)?;
    app.config.show_hidden = cli.hidden;
    app.config.respect_gitignore = !cli.no_gitignore;
    app.config.max_depth = cli.max_depth;
    app.refresh()?;
    if !cmux_available {
        app.mode = Mode::Error {
            message: "cmux is not running or 'cmux' CLI is not on PATH.\n\
                      Start cmux and try again. The TUI will still let you browse,\n\
                      but Enter won't be able to open the markdown panel."
                .into(),
        };
    } else {
        app.status = format!("ready · {} files", app.tree.file_count());
    }

    run_tui(app)
}

fn list_files(root: PathBuf, cli: &Cli) -> anyhow::Result<()> {
    use mdmux::tree::{Tree, TreeConfig};
    let cfg = TreeConfig {
        show_hidden: cli.hidden,
        respect_gitignore: !cli.no_gitignore,
        max_depth: cli.max_depth,
    };
    let tree = Tree::build(&root, &cfg)?;
    for n in tree.nodes() {
        if n.kind == NodeKind::File {
            println!("{}", n.path.display());
        }
    }
    Ok(())
}

fn run_tui(mut app: App) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut app);

    // Always restore the terminal before exiting.
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Ok(Action::QuitAndClose) = result {
        app.close_markdown_panel();
    }
    result.map(|_| ())
}

fn event_loop<B: ratatui::backend::Backend + std::io::Write>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> anyhow::Result<Action> {
    let mut pending: Option<char> = None;
    loop {
        terminal.draw(|f| draw(f, app))?;
        if !poll(Duration::from_millis(250))? {
            continue;
        }
        match read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
                    continue;
                }
                // `Q` clears `current_md_surface` in the handler before
                // returning, so QuitAndClose becomes a no-op for the panel —
                // both branches end the loop, the difference is only whether
                // the surface is still tracked when run_tui calls close.
                match handle_key(app, key, &mut pending) {
                    Action::Quit | Action::QuitAndClose => {
                        return Ok(Action::QuitAndClose);
                    }
                    Action::Noop | Action::Redraw => {}
                }
            }
            Event::Mouse(m) => handle_mouse(app, m),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn handle_mouse(app: &mut App, m: MouseEvent) {
    match m.kind {
        MouseEventKind::ScrollDown => app.move_down(),
        MouseEventKind::ScrollUp => app.move_up(),
        _ => {}
    }
}

fn handle_key(app: &mut App, key: KeyEvent, pending: &mut Option<char>) -> Action {
    // Handle modes that swallow input first.
    match app.mode.clone() {
        Mode::Error { .. } => {
            app.leave_help_or_dialog();
            return Action::Redraw;
        }
        Mode::Help => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                    app.leave_help_or_dialog();
                }
                _ => {}
            }
            return Action::Redraw;
        }
        Mode::Filter => {
            match key.code {
                KeyCode::Esc => {
                    app.clear_filter();
                    app.mode = Mode::Browse;
                }
                KeyCode::Enter => {
                    app.mode = Mode::Browse;
                }
                KeyCode::Backspace => app.pop_filter_char(),
                KeyCode::Char(c) => {
                    if c == 'u' && key.modifiers.contains(KeyModifiers::CONTROL) {
                        app.clear_filter();
                    } else {
                        app.push_filter_char(c);
                    }
                }
                _ => {}
            }
            return Action::Redraw;
        }
        Mode::GoTo { mut input } => {
            match key.code {
                KeyCode::Esc => app.mode = Mode::Browse,
                KeyCode::Enter => {
                    let path = expand_path(&input);
                    match app.change_root(path) {
                        Ok(_) => app.mode = Mode::Browse,
                        Err(e) => {
                            app.mode = Mode::Error {
                                message: format!("{}", e),
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    app.mode = Mode::GoTo { input };
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    app.mode = Mode::GoTo { input };
                }
                _ => {}
            }
            return Action::Redraw;
        }
        Mode::Browse => {}
    }

    // Two-key sequences (vim-style).
    if let Some(prev) = pending.take() {
        match (prev, key.code) {
            ('g', KeyCode::Char('g')) => {
                app.go_top();
                return Action::Redraw;
            }
            ('c', KeyCode::Char('d')) => {
                if let Some(path) = app.current_row_path()
                    && let Some(kind) = app.current_row_kind()
                    && kind == NodeKind::Dir
                    && let Err(e) = app.change_root(path)
                {
                    app.mode = Mode::Error {
                        message: format!("{}", e),
                    };
                }
                return Action::Redraw;
            }
            ('G', KeyCode::Char('p')) => {
                app.mode = Mode::GoTo {
                    input: String::new(),
                };
                return Action::Redraw;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Char('q') => return Action::Quit,
        KeyCode::Char('Q') => {
            // Keep panel open: clear the tracked surface so closing is a no-op.
            app.current_md_surface = None;
            return Action::QuitAndClose;
        }
        KeyCode::Char('?') => app.enter_help(),
        KeyCode::Char('/') => app.enter_filter(),
        KeyCode::Char('r') => {
            if let Err(e) = app.refresh() {
                app.mode = Mode::Error {
                    message: format!("{}", e),
                };
            }
        }
        KeyCode::Char('.') => {
            if let Err(e) = app.toggle_hidden() {
                app.mode = Mode::Error {
                    message: format!("{}", e),
                };
            }
        }
        KeyCode::Char('i') => {
            if let Err(e) = app.toggle_gitignore() {
                app.mode = Mode::Error {
                    message: format!("{}", e),
                };
            }
        }
        KeyCode::Char('a') => {
            app.auto_open = !app.auto_open;
            app.status = if app.auto_open {
                "auto-open ON".into()
            } else {
                "auto-open OFF".into()
            };
        }
        KeyCode::Char('x') => {
            app.close_markdown_panel();
            app.status = "panel closed".into();
        }
        KeyCode::Char('u') => match app.parent_root() {
            Ok(_) => {}
            Err(e) => {
                app.mode = Mode::Error {
                    message: format!("{}", e),
                };
            }
        },
        KeyCode::Char('b') => match app.back_root() {
            Ok(_) => {}
            Err(e) => {
                app.mode = Mode::Error {
                    message: format!("{}", e),
                };
            }
        },
        KeyCode::Char('~') => match dirs::home_dir() {
            Some(home) => {
                if let Err(e) = app.change_root(home) {
                    app.mode = Mode::Error {
                        message: format!("{}", e),
                    };
                }
            }
            None => {
                app.mode = Mode::Error {
                    message: "no $HOME".into(),
                };
            }
        },
        KeyCode::Char('E') => app.expand_all(),
        KeyCode::Char('C') => app.collapse_all(),
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::PageDown => app.page_down(),
        KeyCode::PageUp => app.page_up(),
        KeyCode::Home => app.go_top(),
        KeyCode::End => app.go_bottom(),
        KeyCode::Char('g') => *pending = Some('g'),
        KeyCode::Char('G') => *pending = Some('G'),
        KeyCode::Char('c') => *pending = Some('c'),
        KeyCode::Char(' ') => app.toggle_current(),
        KeyCode::Right | KeyCode::Char('l') => {
            if let Some(kind) = app.current_row_kind()
                && kind == NodeKind::Dir
                && let Some(p) = app.current_row_path()
            {
                app.tree.expand(&p);
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if let Some(kind) = app.current_row_kind() {
                let p = app.current_row_path();
                if let Some(p) = p {
                    if kind == NodeKind::Dir {
                        if app.tree.is_expanded(&p) {
                            app.tree.collapse(&p);
                        } else if let Some(parent) = p.parent()
                            && parent != app.tree.root()
                        {
                            app.select_path(parent);
                        }
                    } else if let Some(parent) = p.parent()
                        && parent != app.tree.root()
                    {
                        app.select_path(parent);
                    }
                }
            }
        }
        KeyCode::Enter | KeyCode::Char('o') => {
            if let Some(kind) = app.current_row_kind() {
                match kind {
                    NodeKind::File => {
                        if let Err(e) = app.open_selected() {
                            app.mode = Mode::Error {
                                message: format!("{}", e),
                            };
                        }
                    }
                    NodeKind::Dir => app.toggle_current(),
                }
            }
        }
        _ => {}
    }
    Action::Redraw
}

fn expand_path(s: &str) -> PathBuf {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    if s == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home;
    }
    PathBuf::from(s)
}
