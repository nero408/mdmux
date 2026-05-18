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

use mdmux::app::{Action, App, Mode, RenderMode};
use mdmux::cmux::{CliCmux, CmuxClient, mock::MockCmux};
use mdmux::tree::NodeKind;
use mdmux::ui::draw;
use mdmux::watcher::FileWatcher;

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

    /// Run with a built-in fake cmux client. cmux calls are no-ops; the TUI
    /// reports a fake `surface:N` in the status line. Used for screenshots,
    /// gifs, and CI smoke tests on machines without cmux installed.
    #[arg(long)]
    demo: bool,

    /// Render the markdown panel inside mdmux itself instead of asking cmux
    /// for a sibling pane. Useful if you don't run cmux, or if you want to
    /// preview without spawning an external pane. Default is to use cmux
    /// when it's available and fall back to in-process automatically.
    #[arg(long)]
    no_cmux: bool,
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

    // Pick a cmux client first so we can probe for availability. If the user
    // asked for `--demo`, swap in the in-memory mock (the demo gif and CI
    // smoke tests don't have a real cmux to talk to).
    let cmux: Box<dyn CmuxClient> = if cli.demo {
        Box::new(MockCmux::new())
    } else {
        Box::new(CliCmux {
            binary: cli.cmux_bin.clone(),
        })
    };
    let cmux_available = cmux.is_available();

    // Render mode decision:
    //   --demo                          → InProcess (gif-friendly preview)
    //   --no-cmux                       → InProcess (user opted out)
    //   cmux not reachable              → InProcess (graceful fallback)
    //   otherwise                       → Cmux (default, current behavior)
    let render_mode = if cli.demo || cli.no_cmux || !cmux_available {
        RenderMode::InProcess
    } else {
        RenderMode::Cmux
    };

    let mut app = App::new(root, cmux)?;
    app.config.show_hidden = cli.hidden;
    app.config.respect_gitignore = !cli.no_gitignore;
    app.config.max_depth = cli.max_depth;
    app.demo_mode = cli.demo;
    app.render_mode = render_mode;
    app.refresh()?;
    app.status = match render_mode {
        RenderMode::Cmux => format!("ready · {} files · cmux", app.tree.file_count()),
        RenderMode::InProcess => {
            if cli.demo {
                format!("ready · {} files · demo preview", app.tree.file_count())
            } else if cli.no_cmux {
                format!("ready · {} files · in-process", app.tree.file_count())
            } else {
                // We fell back because cmux is missing. Tell the user
                // softly via the status line; don't pop a modal error.
                format!(
                    "ready · {} files · cmux not running → in-process preview",
                    app.tree.file_count()
                )
            }
        }
    };

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
    install_panic_hook();
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    // Only spin up a filesystem watcher when we actually own the rendering
    // (in-process mode). In cmux mode the cmux daemon does its own
    // live-reload and our watcher would just burn descriptors.
    let watcher = if app.render_mode == RenderMode::InProcess {
        FileWatcher::new()
    } else {
        None
    };

    let result = event_loop(&mut terminal, &mut app, watcher);

    // Always restore the terminal before exiting.
    restore_terminal(&mut terminal)?;

    if let Ok(Action::QuitAndClose) = result {
        app.close_markdown_panel();
    }
    result.map(|_| ())
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Make sure a panic inside the event loop or the renderer doesn't leave the
/// user with a wedged terminal (raw mode, alt screen, hidden cursor).
///
/// Crossterm leaves the tty in raw mode + alternate screen as soon as we
/// enable them; if the process unwinds without explicit cleanup the user
/// loses local echo, line discipline, and visible cursor. We restore the
/// terminal first, then re-raise the panic so the message still reaches the
/// scrollback.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen, DisableMouseCapture);
        previous(info);
    }));
}

fn event_loop<B: ratatui::backend::Backend + std::io::Write>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut watcher: Option<FileWatcher>,
) -> anyhow::Result<Action> {
    let mut pending: Option<char> = None;
    // Path the watcher is currently tuned to. Tracked here so we can detect
    // when the user opens a different file and need to re-arm the watch.
    let mut watched_path: Option<std::path::PathBuf> = None;

    // Always draw on entry. After that, redraw only when something happens.
    terminal.draw(|f| draw(f, app))?;
    loop {
        // If the preview is showing a file, make sure the watcher is tuned
        // to it. Re-arming is cheap and idempotent on the same path.
        if let (Some(w), Some(p)) = (
            watcher.as_mut(),
            app.preview.as_ref().map(|p| p.path.clone()),
        ) && watched_path.as_ref() != Some(&p)
        {
            let _ = w.watch(&p);
            watched_path = Some(p);
        }

        // Pick a poll timeout. When the file watcher is active and a file is
        // open we want a tight loop (200ms) so live-reload feels instant.
        // Otherwise block for a long time — there's nothing else that should
        // wake us up between key presses.
        let want_watch = watcher.is_some() && app.preview.is_some();
        let timeout = if want_watch {
            Duration::from_millis(200)
        } else {
            Duration::from_secs(60 * 60 * 24)
        };

        if !poll(timeout)? {
            // Timeout expired. If we're watching a file, drain the watcher
            // and reload on a hit; otherwise just go back to sleep.
            if let (Some(w), Some(p)) = (
                watcher.as_mut(),
                app.preview.as_ref().map(|p| p.path.clone()),
            ) && w.drain_for(&p)
                && app.reload_preview()
            {
                terminal.draw(|f| draw(f, app))?;
            }
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
                    Action::Noop => continue,
                    Action::Redraw => {}
                }
            }
            Event::Mouse(m) => handle_mouse(app, m),
            Event::Resize(_, _) => {}
            _ => continue,
        }
        terminal.draw(|f| draw(f, app))?;
    }
}

fn handle_mouse(app: &mut App, m: MouseEvent) {
    match m.kind {
        MouseEventKind::ScrollDown => app.move_down(),
        MouseEventKind::ScrollUp => app.move_up(),
        _ => {}
    }
}

/// A reasonable "half-page" magnitude for the in-process preview's
/// shift-J / shift-K (and Ctrl-D / Ctrl-U) scroll. We don't have access
/// to the live pane height at the keypress site, so we use a fixed value
/// that matches the typical 24–32 row terminal pretty well.
fn half_screen() -> u16 {
    12
}

/// "Full-page" magnitude for `j` / `k` when a preview is open. Same
/// caveat as [`half_screen`] — we don't see the live pane height here,
/// so we pick a constant that feels like a page on a normal-sized
/// terminal and leaves a couple of rows of overlap with the previous
/// view.
fn page_size() -> u16 {
    22
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

    // Two-key sequences (vim-style). A non-matching second key falls through
    // so the second key is interpreted on its own — this matches `vim`'s
    // behavior for partial sequences and matters because we want `g`
    // followed by an arrow key to mean "deselect the prefix, process the
    // arrow key".
    if let Some(prev) = pending.take() {
        match (prev, key.code) {
            ('g', KeyCode::Char('g')) => {
                app.go_top();
                return Action::Redraw;
            }
            ('g', KeyCode::Char('p')) => {
                app.mode = Mode::GoTo {
                    input: String::new(),
                };
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
        // Ctrl-U scrolls the preview half a page. Has to come before the
        // plain `u` arm so the guard wins.
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.preview_scroll_up(half_screen())
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
        // Preview pane scrolling. When a preview is showing, j/k scroll a
        // full page, and shift-J/K (plus Ctrl-D/Ctrl-U) scroll half a page.
        // When no preview is open (cmux mode, or in-process mode before
        // anything was opened) j/k fall through to tree navigation — see
        // the unguarded arms below. Ctrl-U is matched higher up next to the
        // plain `u` arm so the guarded variant wins.
        KeyCode::Char('j') if app.preview.is_some() => app.preview_scroll_down(page_size()),
        KeyCode::Char('k') if app.preview.is_some() => app.preview_scroll_up(page_size()),
        KeyCode::Char('J') => app.preview_scroll_down(half_screen()),
        KeyCode::Char('K') => app.preview_scroll_up(half_screen()),
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.preview_scroll_down(half_screen())
        }
        KeyCode::Char('j') | KeyCode::Down => app.move_down(),
        KeyCode::Char('k') | KeyCode::Up => app.move_up(),
        KeyCode::PageDown => app.page_down(),
        KeyCode::PageUp => app.page_up(),
        KeyCode::Home => app.go_top(),
        KeyCode::End | KeyCode::Char('G') => app.go_bottom(),
        KeyCode::Char(':') => {
            app.mode = Mode::GoTo {
                input: String::new(),
            };
        }
        KeyCode::Char('g') => *pending = Some('g'),
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
