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
//! ratatui rendering uses the `tui-markdown` crate so we get headings, lists,
//! blockquotes, fenced code blocks, and inline emphasis without writing a
//! markdown parser.

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
    pub fn render(&self) -> Text<'static> {
        let joined = self.lines.join("\n");
        // `tui_markdown::from_str` returns `Text<'_>` borrowed from the
        // input. `Text` has no public `into_owned`, but we can rebuild it
        // by deep-cloning each span's `Cow<str>` into an owned `String`.
        let borrowed = tui_markdown::from_str(&joined);
        to_owned_text(borrowed)
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

/// Deep-clone a borrowed `Text` into a `Text<'static>` so we can hand it to
/// a renderer that outlives the source string.
///
/// `ratatui::text::Text` doesn't expose `into_owned`, but its public fields
/// do — every `Span` is a `Cow<str>` we can replace with an owned `String`.
fn to_owned_text(t: Text<'_>) -> Text<'static> {
    use ratatui::text::{Line, Span};
    use std::borrow::Cow;
    let lines = t
        .lines
        .into_iter()
        .map(|line| {
            let spans = line
                .spans
                .into_iter()
                .map(|s| Span {
                    content: Cow::Owned(s.content.into_owned()),
                    style: s.style,
                })
                .collect();
            Line {
                spans,
                alignment: line.alignment,
                style: line.style,
            }
        })
        .collect();
    Text {
        lines,
        alignment: t.alignment,
        style: t.style,
    }
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
        // The whole point of `to_owned_text` is that the result outlives
        // the source string — compile-checking this in isolation is enough.
        let text: Text<'static> = preview.render();
        assert!(!text.lines.is_empty());
    }

    #[test]
    fn render_handles_empty_file() {
        let preview = Preview {
            path: PathBuf::from("empty.md"),
            lines: vec![],
            truncated: false,
        };
        let text = preview.render();
        // Empty markdown → empty text. Should not panic.
        let _ = text.lines.len();
    }
}
