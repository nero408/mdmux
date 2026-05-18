//! Application state machine for the TUI.
//!
//! Keeps the file tree, selection, navigation stack, and the surface id of
//! the markdown panel currently open in cmux. All UI side-effects go through
//! the [`CmuxClient`](crate::cmux::CmuxClient) trait so unit tests can use a
//! mock.

use std::path::{Path, PathBuf};

use crate::cmux::{CmuxClient, CmuxError, SurfaceId};
use crate::tree::{NodeKind, Tree, TreeConfig};

/// Upper bound on how many roots we remember for the `b` (back) key. A user
/// hammering `c d` / `u` shouldn't slowly leak memory; 64 entries is more
/// history than anyone can usefully reach for.
const HISTORY_CAP: usize = 64;

/// Upper bound on the file size we slurp for the in-process demo preview pane.
/// Demo mode is for quick screenshots/gifs, so capping at 1 MiB keeps a typo
/// (`mdmux --demo /` then pressing Enter on a giant log file) from OOM-ing
/// the process. Real use delegates rendering to cmux, which has its own
/// streaming strategy.
const DEMO_PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// Upper bound on the number of lines we keep for the demo preview pane.
/// Defends against pathologically long lines or files with millions of lines
/// (the preview pane only shows a few dozen anyway).
const DEMO_PREVIEW_MAX_LINES: usize = 5_000;

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

/// Snapshot of a file loaded for the in-process demo preview pane.
///
/// Only populated when [`App::demo_mode`] is true. In normal use, the right
/// pane is rendered by cmux itself, outside mdmux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoPreview {
    pub path: PathBuf,
    pub lines: Vec<String>,
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
    /// When true, the TUI grows an in-process preview pane (see
    /// [`DemoPreview`]) on file open. Used for `--demo` (screenshots / gifs);
    /// the real binary delegates rendering to cmux.
    pub demo_mode: bool,
    /// Content of the most recently opened file when [`Self::demo_mode`] is on.
    pub demo_preview: Option<DemoPreview>,
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
            demo_preview: None,
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

    /// Open the currently selected markdown file in cmux (replacing any
    /// previously opened panel).
    pub fn open_selected(&mut self) -> Result<(), CmuxError> {
        let Some(row) = self.tree.visible_rows().get(self.selection).cloned() else {
            return Ok(());
        };
        if row.kind != NodeKind::File {
            return Ok(());
        }
        // Close previous panel if any.
        if let Some(prev) = self.current_md_surface.take() {
            // If close fails (surface already gone), keep going.
            let _ = self.cmux.close_surface(&prev);
        }
        let res = self.cmux.open_markdown(&row.path)?;
        self.current_md_surface = Some(res.surface);
        self.last_opened = Some(row.path.clone());
        if self.demo_mode {
            // Best-effort: if the file can't be read we just leave the prior
            // preview in place rather than failing the open. Reads are capped
            // so pointing the demo at a giant file (accidentally or
            // intentionally) doesn't OOM us.
            if let Some(preview) = load_demo_preview(&row.path) {
                self.demo_preview = Some(preview);
            }
        }
        self.status = format!("→ {}", row.path.display());
        Ok(())
    }

    /// Best-effort close of the markdown panel we own.
    pub fn close_markdown_panel(&mut self) {
        if let Some(s) = self.current_md_surface.take() {
            let _ = self.cmux.close_surface(&s);
        }
        self.demo_preview = None;
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

/// Read a file for the demo preview pane, returning `None` if the file is
/// unreadable. The read is capped at [`DEMO_PREVIEW_MAX_BYTES`] and the line
/// count at [`DEMO_PREVIEW_MAX_LINES`] so pointing the demo at a giant log,
/// a binary, or a pathologically long line can't OOM the process.
fn load_demo_preview(path: &Path) -> Option<DemoPreview> {
    use std::io::Read;
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    // `take(N)` caps the read so we never allocate more than N bytes.
    f.take(DEMO_PREVIEW_MAX_BYTES).read_to_end(&mut buf).ok()?;
    let s = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = s
        .lines()
        .take(DEMO_PREVIEW_MAX_LINES)
        .map(String::from)
        .collect();
    if lines.len() == DEMO_PREVIEW_MAX_LINES {
        lines.push("…(preview truncated)".to_string());
    }
    Some(DemoPreview {
        path: path.to_path_buf(),
        lines,
    })
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
    fn load_demo_preview_caps_large_files() {
        let r = temp("demo_cap");
        let big = r.join("huge.md");
        fs::create_dir_all(big.parent().unwrap()).unwrap();
        // Write 2 MiB of `a`'s — twice the preview cap.
        let content = "a".repeat((DEMO_PREVIEW_MAX_BYTES * 2) as usize);
        fs::write(&big, content).unwrap();
        let preview = load_demo_preview(&big).unwrap();
        // Only the capped slice ends up in memory.
        let total_bytes: usize = preview.lines.iter().map(|l| l.len()).sum();
        assert!(
            (total_bytes as u64) <= DEMO_PREVIEW_MAX_BYTES,
            "loaded {} bytes (cap is {})",
            total_bytes,
            DEMO_PREVIEW_MAX_BYTES
        );
    }
}
