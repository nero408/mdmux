//! In-process markdown preview pane.
//!
//! When the TUI is run *inside* a cmux session, pressing Enter on a file
//! delegates rendering to cmux — see [`crate::cmux`]. When cmux is missing
//! (or the user passed `--no-cmux` / `--demo`) we keep the markdown panel
//! inside mdmux's own ratatui surface instead. That's what lives here.
//!
//! [`Preview`] owns a snapshot of the currently-opened file (raw lines plus a
//! pre-rendered ratatui [`Text`]) and a scroll offset. [`load`] reads a file
//! from disk with a size cap and produces a fresh [`Preview`]. The actual
//! ratatui rendering uses the `ratkit` crate's markdown widget so we get
//! headings, lists, blockquotes, fenced code blocks (with syntect-based
//! syntax highlighting), inline emphasis, **and** pipe tables with column
//! alignment — without writing a markdown parser.

use std::io::Read;
use std::path::{Path, PathBuf};

use ratatui::text::Text;

/// Upper bound on how many bytes we load into the preview pane. Pointing
/// `mdmux` at a directory whose markdown files happen to be huge (logs,
/// `.md` exports of databases) shouldn't OOM us — the user only sees a few
/// dozen rows at a time anyway.
pub const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;

/// Upper bound on the number of lines we keep. Defends against pathologically
/// long single-line files.
pub const PREVIEW_MAX_LINES: usize = 5_000;

/// Snapshot of a file loaded for the in-process preview pane.
#[derive(Debug, Clone)]
pub struct Preview {
    pub path: PathBuf,
    /// The raw markdown source, line-split. Kept separately from the
    /// rendered `Text` so live-reload can compare cheaply ("did the file
    /// actually change?") and so we can fall back to a plain dump if
    /// rendering needs to be disabled.
    pub lines: Vec<String>,
    /// Whether the file got truncated by the byte/line cap.
    pub truncated: bool,
}

impl Preview {
    /// Render the markdown to a ratatui `Text`. We re-render on every draw
    /// because the cost is dominated by the file size, not the frame rate,
    /// and the file size is capped.
    ///
    /// `max_width` is the inner content width of the preview pane (i.e.
    /// `area.width` minus the block borders). It's forwarded to ratkit so
    /// table rendering and full-width backgrounds size to the actual pane
    /// rather than ratkit's 120-column default. `None` falls back to that
    /// default — fine for tests, suboptimal for rendering.
    pub fn render(&self, max_width: Option<usize>) -> Text<'static> {
        let joined = self.lines.join("\n");
        ratkit::widgets::markdown_preview::render_markdown(&joined, max_width)
    }
}

/// Open `path`, slurp up to [`PREVIEW_MAX_BYTES`], cap at [`PREVIEW_MAX_LINES`]
/// lines, and return a [`Preview`]. Returns `None` if the file can't be opened
/// (permission denied, gone since the tree walk, etc.) — the caller should
/// leave the previous preview in place rather than fail the open.
pub fn load(path: &Path) -> Option<Preview> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    // `take(N)` caps the read so we never allocate more than N bytes.
    f.take(PREVIEW_MAX_BYTES).read_to_end(&mut buf).ok()?;
    let s = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = s
        .lines()
        .take(PREVIEW_MAX_LINES)
        .map(String::from)
        .collect();
    let truncated = lines.len() == PREVIEW_MAX_LINES;
    if truncated {
        lines.push("…(preview truncated)".to_string());
    }
    Some(Preview {
        path: path.to_path_buf(),
        lines,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_file(name: &str, body: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        p.push(format!("mdmux-preview-{}-{}.md", name, stamp));
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn load_returns_lines_for_short_file() {
        let p = tmp_file("short", "# hi\n\nhello\n");
        let preview = load(&p).unwrap();
        assert_eq!(preview.path, p);
        assert!(!preview.truncated);
        assert!(preview.lines.iter().any(|l| l == "# hi"));
    }

    #[test]
    fn load_caps_huge_file() {
        let body = "a".repeat((PREVIEW_MAX_BYTES * 2) as usize);
        let p = tmp_file("huge", &body);
        let preview = load(&p).unwrap();
        let bytes: usize = preview.lines.iter().map(|l| l.len()).sum();
        assert!(
            (bytes as u64) <= PREVIEW_MAX_BYTES + 64,
            "loaded {bytes} bytes, cap {PREVIEW_MAX_BYTES}",
        );
    }

    #[test]
    fn load_returns_none_for_missing_file() {
        let p = std::env::temp_dir().join("mdmux-preview-does-not-exist-9999.md");
        assert!(load(&p).is_none());
    }

    #[test]
    fn render_produces_owned_text() {
        let preview = Preview {
            path: PathBuf::from("test.md"),
            lines: vec!["# title".into(), "".into(), "**bold** text".into()],
            truncated: false,
        };
        // ratkit::render_markdown returns `Text<'static>` directly; this
        // assignment is the compile-time proof.
        let text: Text<'static> = preview.render(None);
        assert!(!text.lines.is_empty());
    }

    #[test]
    fn render_handles_empty_file() {
        let preview = Preview {
            path: PathBuf::from("empty.md"),
            lines: vec![],
            truncated: false,
        };
        let text = preview.render(None);
        // Empty markdown → empty text. Should not panic.
        let _ = text.lines.len();
    }

    /// Regression guard: the in-process preview must render markdown tables.
    /// Switching off `tui-markdown` (which never supported tables) onto
    /// `ratkit` was the whole point of this renderer — pin the contract so a
    /// future swap can't silently drop the feature again.
    #[test]
    fn render_includes_table_cell_content() {
        let preview = Preview {
            path: PathBuf::from("table.md"),
            lines: vec![
                "| Col A | Col B |".into(),
                "|-------|-------|".into(),
                "| cell-alpha | cell-beta |".into(),
            ],
            truncated: false,
        };
        let text = preview.render(Some(80));

        // Flatten every span on every line into one string. We don't care
        // about the exact layout (borders, padding) — just that the cell
        // contents survive the renderer instead of being dropped on the
        // floor the way tui-markdown silently did.
        let flat: String = text
            .lines
            .iter()
            .flat_map(|line| line.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        for needle in ["Col A", "Col B", "cell-alpha", "cell-beta"] {
            assert!(
                flat.contains(needle),
                "rendered table missing {needle:?}; full text = {flat:?}",
            );
        }
    }
}
