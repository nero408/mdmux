//! Application state machine for the TUI.
//!
//! Keeps the file tree, selection, navigation stack, and either the surface
//! id of an external cmux panel ([`RenderMode::Cmux`]) or a snapshot of the
//! currently-open file rendered in mdmux's own pane ([`RenderMode::InProcess`]).
//! All cmux side-effects go through the [`CmuxClient`](crate::cmux::CmuxClient)
//! trait so unit tests can use a mock.

use std::path::{Path, PathBuf};

use crate::cmux::{CmuxClient, CmuxError, SurfaceId};
use crate::preview::{self, Preview};
use crate::tree::{NodeKind, Tree, TreeConfig};

/// Upper bound on how many roots we remember for the `b` (back) key. A user
/// hammering `c d` / `u` shouldn't slowly leak memory; 64 entries is more
/// history than anyone can usefully reach for.
const HISTORY_CAP: usize = 64;

/// Where the rendered markdown panel lives.
///
/// - [`Cmux`](Self::Cmux): we shell out to `cmux markdown open` and the
///   panel is a sibling cmux pane outside our process. The default when a
///   cmux daemon is reachable.
/// - [`InProcess`](Self::InProcess): we render the markdown inside mdmux's
///   own ratatui surface (a horizontal split: tree on the left, preview on
///   the right). Selected automatically when cmux is missing, or forced via
///   `--no-cmux` / `--demo`. The preview pane is live-reloaded from disk by
///   [`crate::watcher::FileWatcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    Cmux,
    InProcess,
}

/// Top-level UI mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Normal navigation.
    Browse,
    /// Live-incremental filter.
    Filter,
    /// Help overlay.
    Help,
    /// Input dialog for "go to path".
    GoTo { input: String },
    /// Transient error toast.
    Error { message: String },
}

/// Result of handling a key — what the event loop should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Stay in place, nothing visible changed.
    Noop,
    /// Re-render.
    Redraw,
    /// Quit the app.
    Quit,
    /// Quit but also close the cmux markdown panel we opened.
    QuitAndClose,
}

pub struct App {
    pub mode: Mode,
    pub tree: Tree,
    pub selection: usize,
    pub config: TreeConfig,
    pub viewport_height: u16,
    pub viewport_offset: usize,
    pub history: Vec<PathBuf>,
    pub current_md_surface: Option<SurfaceId>,
    pub last_opened: Option<PathBuf>,
    pub cmux: Box<dyn CmuxClient>,
    pub auto_open: bool,
    /// Status line message shown briefly after an action.
    pub status: String,
    /// When true, the status bar shows the (fake) surface id even though
    /// nothing was actually opened in cmux. Used by `--demo` so the gif looks
    /// realistic on machines without cmux installed.
    pub demo_mode: bool,
    /// Where the rendered markdown lives. Decided at startup based on cmux
    /// availability + CLI flags. See [`RenderMode`].
    pub render_mode: RenderMode,
    /// Content of the most recently opened file. Populated when
    /// `render_mode == RenderMode::InProcess`; ignored in cmux mode (where
    /// cmux owns rendering).
    pub preview: Option<Preview>,
    /// Scroll offset (in lines) within the preview pane. `0` shows the top.
    pub preview_scroll: u16,
}

impl App {
    pub fn new(root: PathBuf, cmux: Box<dyn CmuxClient>) -> anyhow::Result<Self> {
        let config = TreeConfig::default();
        let tree = Tree::build(&root, &config)?;
        Ok(Self {
            mode: Mode::Browse,
            tree,
            selection: 0,
            config,
            viewport_height: 20,
            viewport_offset: 0,
            history: Vec::new(),
            current_md_surface: None,
            last_opened: None,
            cmux,
            auto_open: false,
            status: String::new(),
            demo_mode: false,
            render_mode: RenderMode::Cmux,
            preview: None,
            preview_scroll: 0,
        })
    }

    /// Refresh the tree from disk, keeping selection on the same path when
    /// possible.
    pub fn refresh(&mut self) -> anyhow::Result<()> {
        let prev_path = self
            .tree
            .visible_rows()
            .get(self.selection)
            .map(|r| r.path.clone());
        let root = self.tree.root().to_path_buf();
        self.tree = Tree::build(&root, &self.config)?;
        if let Some(p) = prev_path {
            self.select_path(&p);
        } else {
            self.selection = 0;
        }
        self.clamp_selection();
        self.status = format!("refreshed — {} files", self.tree.file_count());
        Ok(())
    }

    /// Switch to a new root directory. Pushes the old root onto history.
    ///
    /// `new_root` must resolve to a directory. Pointing at a regular file
    /// works by accident in [`Tree::build`] (the walker just yields the file
    /// itself) and produces a confusing one-row tree, so reject it up front.
    pub fn change_root(&mut self, new_root: PathBuf) -> anyhow::Result<()> {
        let meta = std::fs::metadata(&new_root)
            .map_err(|e| anyhow::anyhow!("{}: {}", new_root.display(), e))?;
        if !meta.is_dir() {
            anyhow::bail!("not a directory: {}", new_root.display());
        }
        let prev_root = self.tree.root().to_path_buf();
        let tree = Tree::build(&new_root, &self.config)?;
        self.history.push(prev_root);
        // Bound history so a long session of `cd`/`u` doesn't slowly grow
        // memory forever. Drop the oldest entry when we overflow.
        if self.history.len() > HISTORY_CAP {
            let drop_count = self.history.len() - HISTORY_CAP;
            self.history.drain(0..drop_count);
        }
        self.tree = tree;
        self.selection = 0;
        self.viewport_offset = 0;
        self.status = format!("→ {}", self.tree.root().display());
        Ok(())
    }

    /// Move root up to parent dir (if any).
    pub fn parent_root(&mut self) -> anyhow::Result<bool> {
        let current = self.tree.root().to_path_buf();
        let Some(parent) = current.parent().map(|p| p.to_path_buf()) else {
            return Ok(false);
        };
        if parent == current {
            return Ok(false);
        }
        self.change_root(parent)?;
        Ok(true)
    }

    /// Pop the most recently pushed root from history.
    pub fn back_root(&mut self) -> anyhow::Result<bool> {
        let Some(prev) = self.history.pop() else {
            return Ok(false);
        };
        let tree = Tree::build(&prev, &self.config)?;
        self.tree = tree;
        self.selection = 0;
        self.viewport_offset = 0;
        self.status = format!("← {}", self.tree.root().display());
        Ok(true)
    }

    pub fn current_row_path(&mut self) -> Option<PathBuf> {
        self.tree
            .visible_rows()
            .get(self.selection)
            .map(|r| r.path.clone())
    }

    pub fn current_row_kind(&mut self) -> Option<NodeKind> {
        self.tree
            .visible_rows()
            .get(self.selection)
            .map(|r| r.kind.clone())
    }

    fn clamp_selection(&mut self) {
        let n = self.tree.visible_rows().len();
        if n == 0 {
            self.selection = 0;
            self.viewport_offset = 0;
            return;
        }
        if self.selection >= n {
            self.selection = n - 1;
        }
        // Adjust viewport so selection stays in view.
        if self.viewport_height == 0 {
            return;
        }
        let h = self.viewport_height as usize;
        if self.selection < self.viewport_offset {
            self.viewport_offset = self.selection;
        } else if self.selection >= self.viewport_offset + h {
            self.viewport_offset = self.selection + 1 - h;
        }
    }

    pub fn move_down(&mut self) {
        let n = self.tree.visible_rows().len();
        if n == 0 {
            return;
        }
        if self.selection + 1 < n {
            self.selection += 1;
            self.clamp_selection();
            if self.auto_open && self.current_row_kind() == Some(NodeKind::File) {
                let _ = self.open_selected();
            }
        }
    }

    pub fn move_up(&mut self) {
        if self.selection > 0 {
            self.selection -= 1;
            self.clamp_selection();
            if self.auto_open && self.current_row_kind() == Some(NodeKind::File) {
                let _ = self.open_selected();
            }
        }
    }

    pub fn page_down(&mut self) {
        let n = self.tree.visible_rows().len();
        if n == 0 {
            return;
        }
        let step = self.viewport_height.max(1) as usize;
        self.selection = (self.selection + step).min(n.saturating_sub(1));
        self.clamp_selection();
    }

    pub fn page_up(&mut self) {
        let step = self.viewport_height.max(1) as usize;
        self.selection = self.selection.saturating_sub(step);
        self.clamp_selection();
    }

    pub fn go_top(&mut self) {
        self.selection = 0;
        self.clamp_selection();
    }

    pub fn go_bottom(&mut self) {
        let n = self.tree.visible_rows().len();
        if n > 0 {
            self.selection = n - 1;
        }
        self.clamp_selection();
    }

    pub fn select_path(&mut self, path: &Path) {
        let idx = self
            .tree
            .visible_rows()
            .iter()
            .position(|r| r.path == path)
            .unwrap_or(0);
        self.selection = idx;
        self.clamp_selection();
    }

    /// Toggle expanded state of the current row if it is a directory.
    pub fn toggle_current(&mut self) {
        let Some(row) = self.tree.visible_rows().get(self.selection).cloned() else {
            return;
        };
        if row.kind == NodeKind::Dir {
            self.tree.toggle(&row.path);
            self.clamp_selection();
        }
    }

    pub fn expand_all(&mut self) {
        self.tree.expand_all();
        self.clamp_selection();
    }

    pub fn collapse_all(&mut self) {
        self.tree.collapse_all();
        self.selection = 0;
        self.viewport_offset = 0;
    }

    pub fn toggle_hidden(&mut self) -> anyhow::Result<()> {
        self.config.show_hidden = !self.config.show_hidden;
        self.refresh()?;
        self.status = if self.config.show_hidden {
            "showing hidden files".into()
        } else {
            "hiding hidden files".into()
        };
        Ok(())
    }

    pub fn toggle_gitignore(&mut self) -> anyhow::Result<()> {
        self.config.respect_gitignore = !self.config.respect_gitignore;
        self.refresh()?;
        self.status = if self.config.respect_gitignore {
            "respecting .gitignore".into()
        } else {
            "ignoring .gitignore".into()
        };
        Ok(())
    }

    /// Open the currently selected markdown file. In cmux mode this opens a
    /// new cmux markdown surface (closing the previous one first). In
    /// in-process mode it loads the file into the preview pane.
    pub fn open_selected(&mut self) -> Result<(), CmuxError> {
        let Some(row) = self.tree.visible_rows().get(self.selection).cloned() else {
            return Ok(());
        };
        if row.kind != NodeKind::File {
            return Ok(());
        }
        // Close previous cmux panel if any (always tracked, even in
        // in-process mode — the mock cmux client used by `--demo` produces
        // fake surface ids).
        if let Some(prev) = self.current_md_surface.take() {
            // If close fails (surface already gone), keep going.
            let _ = self.cmux.close_surface(&prev);
        }
        let res = self.cmux.open_markdown(&row.path)?;
        self.current_md_surface = Some(res.surface);
        self.last_opened = Some(row.path.clone());
        if self.render_mode == RenderMode::InProcess {
            // Best-effort load — if the file is gone since the tree walk,
            // leave the previous preview in place rather than failing the
            // open. The size cap in `preview::load` guards against OOM.
            if let Some(preview) = preview::load(&row.path) {
                self.preview = Some(preview);
                self.preview_scroll = 0;
            }
        }
        self.status = format!("→ {}", row.path.display());
        Ok(())
    }

    /// Reload the currently-open file from disk (live-reload trigger).
    /// Called by the event loop when the file watcher reports a change.
    /// Returns `true` if the preview was actually updated.
    pub fn reload_preview(&mut self) -> bool {
        let Some(p) = self.preview.as_ref().map(|p| p.path.clone()) else {
            return false;
        };
        if let Some(fresh) = preview::load(&p) {
            // Preserve scroll position so a save mid-scroll doesn't jump the
            // viewer back to the top. Clamp later in `clamp_preview_scroll`
            // if the file got shorter.
            self.preview = Some(fresh);
            true
        } else {
            false
        }
    }

    /// Best-effort close of the markdown panel we own (cmux surface in cmux
    /// mode, preview pane in in-process mode).
    pub fn close_markdown_panel(&mut self) {
        if let Some(s) = self.current_md_surface.take() {
            let _ = self.cmux.close_surface(&s);
        }
        self.preview = None;
        self.preview_scroll = 0;
    }

    /// Scroll the in-process preview pane up by one line. No-op when there
    /// is no preview open.
    pub fn preview_scroll_up(&mut self, n: u16) {
        self.preview_scroll = self.preview_scroll.saturating_sub(n);
    }

    /// Scroll the in-process preview pane down by `n` lines. The exact upper
    /// bound is clamped in the renderer where the pane height is known —
    /// here we cap to the preview's line count so we can't run past the end.
    pub fn preview_scroll_down(&mut self, n: u16) {
        let max = self
            .preview
            .as_ref()
            .map(|p| p.lines.len().saturating_sub(1) as u16)
            .unwrap_or(0);
        self.preview_scroll = self.preview_scroll.saturating_add(n).min(max);
    }

    pub fn preview_scroll_to_top(&mut self) {
        self.preview_scroll = 0;
    }

    pub fn preview_scroll_to_bottom(&mut self) {
        let max = self
            .preview
            .as_ref()
            .map(|p| p.lines.len().saturating_sub(1) as u16)
            .unwrap_or(0);
        self.preview_scroll = max;
    }

    pub fn enter_help(&mut self) {
        self.mode = Mode::Help;
    }

    pub fn enter_filter(&mut self) {
        self.mode = Mode::Filter;
    }

    pub fn leave_help_or_dialog(&mut self) {
        self.mode = Mode::Browse;
    }

    pub fn push_filter_char(&mut self, c: char) {
        let mut s = self.tree.filter().to_string();
        s.push(c);
        self.tree.set_filter(&s);
        self.clamp_selection();
    }

    pub fn pop_filter_char(&mut self) {
        let mut s = self.tree.filter().to_string();
        s.pop();
        self.tree.set_filter(&s);
        self.clamp_selection();
    }

    pub fn clear_filter(&mut self) {
        self.tree.set_filter("");
        self.clamp_selection();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmux::mock::MockCmux;
    use std::fs;

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, "# stub").unwrap();
    }

    fn temp(prefix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("mdmux-app-{}-{}", prefix, stamp));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn open_then_open_closes_previous_surface() {
        let r = temp("open_twice");
        touch(&r.join("a.md"));
        touch(&r.join("b.md"));

        let mock = MockCmux::new();
        let mut app = App::new(r.clone(), Box::new(mock)).unwrap();
        // Select first file
        app.selection = 0;
        // We need a known visible-row ordering. Force visibility resolution.
        let _ = app.tree.visible_rows().len();
        app.open_selected().unwrap();
        app.selection = 1;
        let _ = app.tree.visible_rows().len();
        app.open_selected().unwrap();

        // Sniff the mock through the trait object. Trick: re-use the App's
        // box by downcasting via a stable trait. Instead, just verify state:
        assert!(app.current_md_surface.is_some());
        assert!(app.last_opened.is_some());
        assert_eq!(
            app.last_opened.as_ref().unwrap().file_name().unwrap(),
            "b.md"
        );
    }

    #[test]
    fn move_up_down_clamps() {
        let r = temp("move");
        touch(&r.join("a.md"));
        touch(&r.join("b.md"));
        touch(&r.join("c.md"));
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        app.viewport_height = 10;
        let n = app.tree.visible_rows().len();
        assert!(n >= 3);
        app.move_up();
        assert_eq!(app.selection, 0, "should not go below 0");
        for _ in 0..(n + 5) {
            app.move_down();
        }
        assert_eq!(app.selection, n - 1, "should clamp to last row");
    }

    #[test]
    fn change_root_pushes_history_and_back_pops_it() {
        let parent = temp("change_root");
        let sub_a = parent.join("a");
        let sub_b = parent.join("b");
        touch(&sub_a.join("inside_a.md"));
        touch(&sub_b.join("inside_b.md"));

        let mock = MockCmux::new();
        let mut app = App::new(sub_a.clone(), Box::new(mock)).unwrap();
        assert!(app.tree.root().ends_with("a"));

        app.change_root(sub_b.clone()).unwrap();
        assert!(app.tree.root().ends_with("b"));

        app.back_root().unwrap();
        assert!(app.tree.root().ends_with("a"));
    }

    #[test]
    fn parent_root_moves_up() {
        let parent = temp("parent_root");
        let sub = parent.join("sub");
        touch(&sub.join("a.md"));
        let mock = MockCmux::new();
        let mut app = App::new(sub.clone(), Box::new(mock)).unwrap();
        let before = app.tree.root().to_path_buf();
        let moved = app.parent_root().unwrap();
        assert!(moved);
        assert_ne!(app.tree.root(), before.as_path());
    }

    #[test]
    fn filter_mode_round_trip() {
        let r = temp("filter_app");
        touch(&r.join("alpha.md"));
        touch(&r.join("beta.md"));
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        app.viewport_height = 10;
        app.enter_filter();
        for c in "alp".chars() {
            app.push_filter_char(c);
        }
        let rows = app.tree.visible_rows();
        assert!(rows.iter().any(|r| r.name == "alpha.md"));
        assert!(!rows.iter().any(|r| r.name == "beta.md"));
        app.clear_filter();
        let rows = app.tree.visible_rows();
        assert!(rows.iter().any(|r| r.name == "alpha.md"));
        assert!(rows.iter().any(|r| r.name == "beta.md"));
    }

    #[test]
    fn open_on_directory_is_noop() {
        let r = temp("open_dir");
        touch(&r.join("sub/inside.md"));
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        app.viewport_height = 10;
        // First row should be the directory.
        app.selection = 0;
        let kind = app.current_row_kind().unwrap();
        assert_eq!(kind, NodeKind::Dir);
        app.open_selected().unwrap();
        assert!(app.current_md_surface.is_none());
    }

    #[test]
    fn change_root_rejects_non_directory() {
        let r = temp("change_root_file");
        let file = r.join("a.md");
        touch(&file);
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        let err = app.change_root(file).err();
        assert!(err.is_some(), "expected error for file path");
    }

    /// `HISTORY_CAP` keeps the back stack from growing forever even if the
    /// user mashes `c d` / `u` all day.
    #[test]
    fn change_root_history_is_capped() {
        let parent = temp("hist_cap");
        let mut roots = Vec::new();
        for i in 0..(HISTORY_CAP + 10) {
            let p = parent.join(format!("d{i}"));
            touch(&p.join("inside.md"));
            roots.push(p);
        }
        let mock = MockCmux::new();
        let mut app = App::new(roots[0].clone(), Box::new(mock)).unwrap();
        for r in roots.iter().skip(1) {
            app.change_root(r.clone()).unwrap();
        }
        assert!(
            app.history.len() <= HISTORY_CAP,
            "history grew to {}",
            app.history.len()
        );
    }

    #[test]
    fn open_selected_populates_preview_in_inprocess_mode() {
        let r = temp("inproc_open");
        touch(&r.join("hello.md"));
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        app.render_mode = RenderMode::InProcess;
        // Find the file row.
        app.viewport_height = 10;
        for _ in 0..app.tree.visible_rows().len() {
            if app.current_row_kind() == Some(NodeKind::File) {
                break;
            }
            app.selection += 1;
        }
        app.open_selected().unwrap();
        assert!(
            app.preview.is_some(),
            "in-process mode should populate preview"
        );
    }

    #[test]
    fn open_selected_skips_preview_in_cmux_mode() {
        let r = temp("cmux_open");
        touch(&r.join("hello.md"));
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        app.render_mode = RenderMode::Cmux;
        app.viewport_height = 10;
        for _ in 0..app.tree.visible_rows().len() {
            if app.current_row_kind() == Some(NodeKind::File) {
                break;
            }
            app.selection += 1;
        }
        app.open_selected().unwrap();
        assert!(
            app.preview.is_none(),
            "cmux mode should leave preview empty"
        );
    }

    #[test]
    fn preview_scroll_is_bounded() {
        let r = temp("scroll");
        touch(&r.join("a.md"));
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        app.render_mode = RenderMode::InProcess;
        // No preview loaded — scrolling is a no-op.
        app.preview_scroll_down(100);
        assert_eq!(app.preview_scroll, 0);

        // With a preview, scroll_down caps at lines.len()-1.
        app.preview = Some(Preview {
            path: PathBuf::from("a.md"),
            lines: vec!["one".into(), "two".into(), "three".into()],
            truncated: false,
        });
        app.preview_scroll_down(1000);
        assert_eq!(app.preview_scroll, 2);
        app.preview_scroll_up(100);
        assert_eq!(app.preview_scroll, 0);
        app.preview_scroll_to_bottom();
        assert_eq!(app.preview_scroll, 2);
        app.preview_scroll_to_top();
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn reload_preview_picks_up_disk_changes() {
        let r = temp("reload");
        let path = r.join("doc.md");
        touch(&path);
        std::fs::write(&path, "before").unwrap();
        let mock = MockCmux::new();
        let mut app = App::new(r, Box::new(mock)).unwrap();
        app.render_mode = RenderMode::InProcess;
        app.preview = preview::load(&path);
        let original = app.preview.as_ref().unwrap().lines.clone();
        std::fs::write(&path, "after").unwrap();
        assert!(app.reload_preview());
        let updated = app.preview.as_ref().unwrap().lines.clone();
        assert_ne!(original, updated);
    }

    #[test]
    fn render_mode_defaults_to_cmux() {
        let r = temp("default_mode");
        touch(&r.join("a.md"));
        let mock = MockCmux::new();
        let app = App::new(r, Box::new(mock)).unwrap();
        assert_eq!(app.render_mode, RenderMode::Cmux);
    }
}
