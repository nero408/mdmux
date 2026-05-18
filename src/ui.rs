//! ratatui rendering.
//!
//! Pure functions over the app state; we do not own the terminal here.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{App, DemoPreview, Mode};
use crate::tree::NodeKind;

/// Replace control characters in a string with a visible placeholder so they
/// can't reach the terminal and execute as escape sequences. Filenames,
/// markdown file contents in demo mode, and other "untrusted" text all need
/// to go through this before becoming a `Span`.
///
/// Filenames on Unix can contain anything but `/` and `\0`. A file named
/// `\x1b[2J\x1b[H` could otherwise clear the screen and reposition the
/// cursor when rendered. Tabs and newlines are also stripped because they
/// disrupt the single-line layout of the file tree.
pub(crate) fn sanitize_for_display(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{FFFD}' } else { c })
        .collect()
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(1),    // body (tree, or tree + preview in demo)
            Constraint::Length(1), // status / filter
            Constraint::Length(1), // hint
        ])
        .split(area);

    draw_title(f, app, chunks[0]);

    // In demo mode, once a file has been opened we split the body
    // horizontally and render its content on the right. In real use, the
    // right pane is owned by cmux.
    let preview_snapshot = app.demo_preview.clone();
    let tree_area = if let Some(ref preview) = preview_snapshot {
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
            .split(chunks[1]);
        draw_demo_preview(f, preview, body[1]);
        body[0]
    } else {
        chunks[1]
    };

    // Resize viewport based on actual tree height.
    let tree_height = tree_area.height.saturating_sub(2); // borders
    app.viewport_height = tree_height.max(1);

    draw_tree(f, app, tree_area);
    draw_status(f, app, chunks[2]);
    draw_hint(f, app, chunks[3]);

    match app.mode.clone() {
        Mode::Help => draw_help_overlay(f, area),
        Mode::GoTo { input } => draw_goto_overlay(f, area, &input),
        Mode::Error { message } => draw_error_overlay(f, area, &message),
        _ => {}
    }
}

fn draw_title(f: &mut Frame, app: &App, area: Rect) {
    let root = sanitize_for_display(&app.tree.root().display().to_string());
    let line = Line::from(vec![
        Span::styled(
            " mdmux ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(root, Style::default().fg(Color::White)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = app.tree.visible_rows().to_vec();
    let filter = app.tree.filter().to_string();
    let total = app.tree.file_count();
    let matched = app.tree.match_count();

    let title = if filter.is_empty() {
        format!(" Files ({}) ", total)
    } else {
        format!(" Files ({}/{}) ", matched, total)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(Color::DarkGray));

    if rows.is_empty() {
        let msg = if filter.is_empty() {
            "no markdown files found"
        } else {
            "no matches"
        };
        let para = Paragraph::new(format!("\n  {}", msg))
            .block(block)
            .style(Style::default().fg(Color::Gray));
        f.render_widget(para, area);
        return;
    }

    let items: Vec<ListItem> = rows
        .iter()
        .map(|r| {
            let indent: String = "  ".repeat(r.depth);
            let icon = match (&r.kind, r.expanded) {
                (NodeKind::Dir, true) => "▾ ",
                (NodeKind::Dir, false) => "▸ ",
                (NodeKind::File, _) => "  ",
            };
            let file_glyph = match r.kind {
                NodeKind::Dir => "📁 ",
                NodeKind::File => "📝 ",
            };
            let style = match r.kind {
                NodeKind::Dir => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                NodeKind::File => Style::default().fg(Color::White),
            };
            // Highlight filter substring for files. Filenames are sanitized
            // so terminal escape sequences embedded in a name can't hijack
            // the renderer.
            let spans: Vec<Span> = if !filter.is_empty() && r.kind == NodeKind::File {
                highlight_match(&r.name, &filter, style)
            } else {
                vec![Span::styled(sanitize_for_display(&r.name), style)]
            };
            let mut all = vec![
                Span::raw(indent),
                Span::styled(icon, Style::default().fg(Color::DarkGray)),
                Span::raw(file_glyph),
            ];
            all.extend(spans);
            ListItem::new(Line::from(all))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.selection));
    *state.offset_mut() = app.viewport_offset.min(rows.len().saturating_sub(1));

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::Indexed(238))
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, area, &mut state);
    // Update the viewport offset from list state for consistency.
    app.viewport_offset = state.offset();
}

/// Case-insensitive substring highlighter that operates on `char` indices so
/// it can't slice through a UTF-8 boundary.
///
/// The previous implementation byte-indexed into both the original string
/// (`name`) and its lowercased form. `String::to_lowercase` can change byte
/// length — e.g. Turkish `İ` (2 bytes) lowercases to `i\u{307}` (3 bytes) —
/// so reusing a byte offset from one to slice the other panics
/// (`byte index N is not a char boundary`). On a TUI in raw mode that panic
/// leaves the user's terminal mangled.
///
/// Going char-by-char is O(n*m) where n = name length and m = needle length,
/// but filenames are tiny so this is fine in practice.
fn highlight_match(raw_name: &str, needle: &str, base: Style) -> Vec<Span<'static>> {
    let name = sanitize_for_display(raw_name);
    let needle = sanitize_for_display(needle);
    if needle.is_empty() {
        return vec![Span::styled(name, base)];
    }
    let name_chars: Vec<char> = name.chars().collect();
    let needle_lower: Vec<char> = needle.chars().flat_map(|c| c.to_lowercase()).collect();

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < name_chars.len() {
        if let Some(match_len) = char_match_len(&name_chars[i..], &needle_lower) {
            if !buf.is_empty() {
                out.push(Span::styled(std::mem::take(&mut buf), base));
            }
            let hit: String = name_chars[i..i + match_len].iter().collect();
            out.push(Span::styled(hit, base.fg(Color::Black).bg(Color::Yellow)));
            i += match_len;
        } else {
            buf.push(name_chars[i]);
            i += 1;
        }
    }
    if !buf.is_empty() {
        out.push(Span::styled(buf, base));
    }
    out
}

/// If `hay[..]` starts with a case-insensitive match for `needle_lower`,
/// returns the number of `char`s in `hay` that the match consumed. Lowercase
/// folding can change char count (`İ` → `i \u{307}`) so the consumed length
/// is determined against `hay`, not `needle_lower`.
fn char_match_len(hay: &[char], needle_lower: &[char]) -> Option<usize> {
    let mut hay_idx = 0;
    let mut needle_idx = 0;
    while needle_idx < needle_lower.len() {
        if hay_idx >= hay.len() {
            return None;
        }
        let folded: Vec<char> = hay[hay_idx].to_lowercase().collect();
        for f in &folded {
            if needle_idx >= needle_lower.len() || *f != needle_lower[needle_idx] {
                return None;
            }
            needle_idx += 1;
        }
        hay_idx += 1;
    }
    Some(hay_idx)
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let line: Line = match &app.mode {
        Mode::Filter => Line::from(vec![
            Span::styled(
                " filter ",
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                sanitize_for_display(app.tree.filter()),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                "▏",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ),
        ]),
        _ => {
            let opened = app
                .last_opened
                .as_ref()
                .map(|p| sanitize_for_display(&p.display().to_string()))
                .unwrap_or_else(|| "(nothing opened yet)".to_string());
            let surface = app
                .current_md_surface
                .as_ref()
                .map(|s| sanitize_for_display(s.as_arg()))
                .unwrap_or_default();
            let mut spans = vec![
                Span::styled(" open ", Style::default().bg(Color::Cyan).fg(Color::Black)),
                Span::raw(" "),
                Span::styled(opened, Style::default().fg(Color::Gray)),
            ];
            if !surface.is_empty() {
                spans.push(Span::raw("  ["));
                spans.push(Span::styled(surface, Style::default().fg(Color::Magenta)));
                spans.push(Span::raw("]"));
            }
            if !app.status.is_empty() {
                spans.push(Span::raw("  · "));
                spans.push(Span::styled(
                    sanitize_for_display(&app.status),
                    Style::default().fg(Color::Green),
                ));
            }
            Line::from(spans)
        }
    };
    f.render_widget(Paragraph::new(line), area);
}

fn draw_hint(f: &mut Frame, app: &App, area: Rect) {
    let hint = match &app.mode {
        Mode::Browse => {
            "↑/↓ move · enter open · ←/→ collapse/expand · / filter · u up · cd enter dir · ? help · q quit"
        }
        Mode::Filter => "enter accept · esc cancel · backspace delete",
        Mode::Help => "esc close help",
        Mode::GoTo { .. } => "enter go · esc cancel",
        Mode::Error { .. } => "press any key to dismiss",
    };
    let para = Paragraph::new(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(para, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area)[1];
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v)[1]
}

fn draw_help_overlay(f: &mut Frame, area: Rect) {
    let area = centered_rect(70, 80, area);
    f.render_widget(Clear, area);
    let lines: Vec<Line> = HELP_LINES
        .iter()
        .map(|(key, desc)| {
            if key.is_empty() && desc.is_empty() {
                Line::from("")
            } else if key.is_empty() {
                Line::from(Span::styled(
                    desc.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("  {:<14}", key),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(desc.to_string(), Style::default().fg(Color::White)),
                ])
            }
        })
        .collect();
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Keybindings ")
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .style(Style::default().bg(Color::Indexed(235)));
    f.render_widget(para, area);
}

fn draw_goto_overlay(f: &mut Frame, area: Rect, input: &str) {
    let area = centered_rect(60, 20, area);
    f.render_widget(Clear, area);
    let para = Paragraph::new(vec![
        Line::from(Span::styled(
            "Go to path",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("› ", Style::default().fg(Color::Cyan)),
            Span::styled(
                sanitize_for_display(input),
                Style::default().fg(Color::White),
            ),
            Span::styled("▏", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  enter — go · esc — cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Navigate "),
    )
    .style(Style::default().bg(Color::Indexed(235)));
    f.render_widget(para, area);
}

fn draw_error_overlay(f: &mut Frame, area: Rect, message: &str) {
    let area = centered_rect(60, 25, area);
    f.render_widget(Clear, area);
    // Sanitize the message so cmux/IO error strings can't smuggle escape
    // sequences (filenames, command output) into the overlay.
    let sanitized = sanitize_for_display(message);
    let mut lines = vec![
        Line::from(Span::styled(
            "Error",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];
    for l in sanitized.lines() {
        lines.push(Line::from(l.to_string()));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  press any key to dismiss",
        Style::default().fg(Color::DarkGray),
    )));
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title(" Error "),
        )
        .style(Style::default().bg(Color::Indexed(235)));
    f.render_widget(para, area);
}

const HELP_LINES: &[(&str, &str)] = &[
    ("", "Navigation"),
    ("↑ / k", "move selection up"),
    ("↓ / j", "move selection down"),
    ("pgup / pgdn", "page up / down"),
    ("g g / G", "jump to top / bottom"),
    ("", ""),
    ("", "Tree"),
    ("→ / l", "expand directory"),
    ("← / h", "collapse directory (or jump to parent)"),
    ("space", "toggle directory"),
    ("E", "expand all"),
    ("C", "collapse all"),
    ("", ""),
    ("", "Opening files"),
    ("enter / o", "open selected markdown in cmux panel"),
    ("a", "toggle auto-open while navigating"),
    ("x", "close current cmux markdown panel"),
    ("", ""),
    ("", "Change directory"),
    ("cd", "make selected directory the new root"),
    ("u", "move root up to parent"),
    ("b", "back to previous root (history)"),
    ("~", "go to $HOME"),
    (".", "toggle hidden files"),
    ("i", "toggle .gitignore respect"),
    (": / g p", "open 'go to path' prompt"),
    ("", ""),
    ("", "Search / filter"),
    ("/", "filter incrementally (Esc to clear)"),
    ("", ""),
    ("", "Misc"),
    ("r", "refresh (re-walk the tree)"),
    ("?", "show / hide this help"),
    ("q", "quit (closes cmux panel)"),
    ("Q", "quit (keep cmux panel open)"),
];

fn draw_demo_preview(f: &mut Frame, preview: &DemoPreview, area: Rect) {
    let filename = preview
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?");
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(vec![
            Span::raw(" 📝 "),
            Span::styled(
                sanitize_for_display(filename),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  · live reload (demo)  ",
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    let para = Paragraph::new(render_markdown_lines(&preview.lines))
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

/// Lightly-styled markdown → ratatui lines. Not a full parser — handles the
/// constructs that make a 10-second demo gif look right (headings, fenced
/// code blocks, list items, blockquotes, horizontal rules). Anything else
/// passes through unchanged.
///
/// Every line is sanitized first: a `.md` file is just bytes and can contain
/// embedded escape sequences. We never want those to reach the terminal raw.
fn render_markdown_lines(lines: &[String]) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::with_capacity(lines.len());
    let mut in_code = false;
    for raw in lines {
        let raw = sanitize_for_display(raw);
        let line = raw.as_str();
        if let Some(rest) = line.strip_prefix("```") {
            in_code = !in_code;
            let label = if in_code && !rest.is_empty() {
                format!("  ─── {} ───", rest.trim())
            } else {
                "  ─────────".to_string()
            };
            out.push(Line::from(Span::styled(
                label,
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }
        if in_code {
            out.push(Line::from(Span::styled(
                format!("    {}", line),
                Style::default().fg(Color::LightGreen),
            )));
            continue;
        }
        if let Some(rest) = line.strip_prefix("### ") {
            out.push(Line::from(Span::styled(
                format!("  {}", rest),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(rest) = line.strip_prefix("## ") {
            out.push(Line::from(Span::styled(
                format!(" {}", rest),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if let Some(rest) = line.strip_prefix("# ") {
            out.push(Line::from(Span::styled(
                rest.to_string(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            )));
        } else if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            out.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(Color::Cyan)),
                Span::raw(rest.to_string()),
            ]));
        } else if let Some(rest) = line.strip_prefix("> ") {
            out.push(Line::from(vec![
                Span::styled("┃ ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    rest.to_string(),
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        } else if line.trim() == "---" || line.trim() == "***" {
            out.push(Line::from(Span::styled(
                "─".repeat(40),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            out.push(Line::from(Span::raw(raw.clone())));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for the panic where byte-indexing `name` with offsets
    /// derived from `name.to_lowercase()` crashed on names like `İ.md`. With
    /// the char-based implementation, this case must return cleanly even when
    /// the lowercase form has a different char count.
    #[test]
    fn highlight_match_handles_unicode_case_folding_without_panic() {
        let style = Style::default();
        let spans = highlight_match("İ.md", "i", style);
        // We expect at least one styled span back; the exact split doesn't
        // matter as long as it doesn't panic.
        assert!(!spans.is_empty());
    }

    #[test]
    fn highlight_match_with_empty_needle_returns_whole_name() {
        let style = Style::default();
        let spans = highlight_match("anything.md", "", style);
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn highlight_match_finds_ascii_substring() {
        let style = Style::default();
        let spans = highlight_match("hello.md", "ll", style);
        // "he" + "ll" + "o.md" → 3 spans.
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn highlight_match_is_case_insensitive() {
        let style = Style::default();
        let spans = highlight_match("HELLO.MD", "ll", style);
        assert!(spans.len() >= 2);
    }

    #[test]
    fn sanitize_strips_terminal_escape_sequences() {
        // ESC + [2J would clear the screen if rendered raw.
        let dirty = "\x1b[2Jevil";
        let clean = sanitize_for_display(dirty);
        // The escape and bracket disappear; only `evil` (plus placeholders for
        // the escape itself) remains visible. Critically: no raw ESC byte.
        assert!(!clean.contains('\x1b'));
        assert!(clean.ends_with("evil"));
    }

    #[test]
    fn sanitize_replaces_newlines_so_layout_is_preserved() {
        let dirty = "line1\nline2\tend";
        let clean = sanitize_for_display(dirty);
        assert!(!clean.contains('\n'));
        assert!(!clean.contains('\t'));
    }

    #[test]
    fn sanitize_preserves_normal_unicode() {
        let s = "Ångström — naïve résumé İstanbul";
        assert_eq!(sanitize_for_display(s), s);
    }
}
