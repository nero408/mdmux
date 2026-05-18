# mdmux — Markdown Multiplexer

[![CI](https://github.com/nero408/mdmux/actions/workflows/ci.yml/badge.svg)](https://github.com/nero408/mdmux/actions/workflows/ci.yml)
[![demo gif](https://github.com/nero408/mdmux/actions/workflows/demo.yml/badge.svg)](https://github.com/nero408/mdmux/actions/workflows/demo.yml)
[![Crates.io](https://img.shields.io/crates/v/mdmux.svg)](https://crates.io/crates/mdmux)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A terminal UI for browsing markdown files in a directory tree and rendering
the selected file in a [cmux](https://cmux.app) side-panel with live reload.

<p align="center">
  <img src="assets/demo.gif" alt="mdmux demo — file tree on the left, rendered markdown on the right" width="900">
</p>

> The gif is recorded with `mdmux --demo`. The same in-process preview
> pane shown in the gif is what you get on any machine where cmux isn't
> running — mdmux falls back automatically. With cmux running, the right
> pane is cmux's own native renderer with full live-reload.

```
 mdmux  /home/me/notes
┌ Files (12) ────────────────────────┐
│  📝 README.md                      │
│▾ 📁 docs                           │      ┌────────────────────────┐
│  ▾ 📁 api                          │      │ Rendered markdown      │
│      📝 endpoints.md               │ ───▶ │ shows up in the cmux   │
│      📝 errors.md                  │      │ panel to the right,    │
│      📝 webhooks.md                │      │ with live reload.      │
│    📝 getting-started.md           │      └────────────────────────┘
│▾ 📁 notes                          │
│    📝 2026-05-13-design.md         │
│    📝 random.md                    │
└────────────────────────────────────┘
 open  /home/me/notes/docs/api/endpoints.md  [surface:438]
↑/↓ move · enter open · ←/→ collapse/expand · / filter · u up · cd enter dir · ? help · q quit
```

## Install

### Homebrew (macOS / Linux)

```sh
brew install nero408/tap/mdmux
```

### Cargo

```sh
cargo install mdmux
```

### From source

```sh
git clone https://github.com/nero408/mdmux
cd mdmux
cargo install --path .
```

Drops the binary at `~/.cargo/bin/mdmux`.

Requires:
- macOS or Linux
- Rust ≥ 1.85 to build from source (uses edition 2024)
- [cmux](https://cmux.app) is optional — if it's not running, mdmux
  renders the markdown panel itself inside its own pane (see below)

## Usage

```sh
mdmux                  # browse the current directory
mdmux ~/notes          # browse a specific directory
mdmux --hidden         # include dotfiles
mdmux --no-gitignore   # show ignored markdown files too
mdmux --max-depth 3    # cap recursion
mdmux --list           # print markdown paths and exit (scripting)
mdmux --no-cmux        # always render the preview pane in-process,
                       #   even if cmux is available
mdmux --demo           # run with a fake cmux client (screenshots / gifs / CI)
```

### With cmux

Run mdmux inside a cmux pane. Pressing **Enter** on a markdown file splits
the current pane to the right and shows the file in cmux's built-in
markdown viewer (with rich formatting and live file watching). Selecting
another file **replaces** the panel — no stacked tabs.

### Without cmux

If cmux isn't running (or you pass `--no-cmux`), mdmux automatically
falls back to rendering the markdown pane itself. The TUI splits
horizontally: file tree on the left, rendered markdown on the right.
The preview is powered by [tui-markdown](https://crates.io/crates/tui-markdown)
(headings, lists, blockquotes, fenced code blocks with syntax highlighting,
emphasis), with live reload via [notify](https://crates.io/crates/notify).

## Keys

| Key             | Action                                                  |
|-----------------|---------------------------------------------------------|
| `↑` / `k`       | move selection up                                       |
| `↓` / `j`       | move selection down                                     |
| `PgUp` / `PgDn` | page up / page down                                     |
| `g g` / `G`     | jump to top / bottom                                    |
| `→` / `l`       | expand directory                                        |
| `←` / `h`       | collapse directory, or jump to parent                   |
| `Space`         | toggle current directory                                |
| `E` / `C`       | expand all / collapse all                               |
| `Enter` / `o`   | open selected markdown (cmux panel or in-process pane)  |
| `a`             | toggle auto-open while navigating                       |
| `x`             | close the open panel                                    |
| `J` / `K`       | scroll preview down / up (in-process mode)              |
| `Ctrl-D` / `Ctrl-U` | scroll preview half a page (in-process mode)        |
| `c d`           | make selected directory the new root                    |
| `u`             | move root up to parent directory                        |
| `b`             | go back to the previous root (history)                  |
| `~`             | jump to `$HOME`                                         |
| `:` / `g p`     | open "go to path" prompt                                |
| `.`             | toggle hidden files                                     |
| `i`             | toggle `.gitignore` respect                             |
| `/`             | live filter (`Esc` clears)                              |
| `r`             | refresh (re-walk the tree)                              |
| `?`             | toggle help screen                                      |
| `q`             | quit (closes the cmux panel)                            |
| `Q`             | quit but keep the cmux panel open                       |

## How it works

mdmux is, fundamentally, the file-browsing front-end. The markdown
*rendering* is delegated. There are two delegate options:

### Cmux mode (default when cmux is running)

cmux has a native `cmux markdown open <path>` command that renders
markdown into a `[markdown]` surface with live reload. mdmux:

1. Walks the directory tree (respecting `.gitignore` by default, like
   ripgrep) and collects markdown files plus their ancestor directories.
2. On `Enter`, shells out to `cmux markdown open <path>`, parses the
   `surface:NNN` id from its output, and remembers it.
3. On the next `Enter`, calls `cmux close-surface --surface <prev>` before
   opening the new one. Result: the panel is **replaced**, not stacked.
4. On quit (`q`), closes the tracked panel. `Q` skips the close.

### In-process mode (no cmux, or `--no-cmux`)

When `cmux ping` fails at startup, mdmux automatically falls back to
rendering the panel itself:

1. The TUI grows a second pane on the right (40/60 split).
2. On `Enter`, the file is loaded (capped at 1 MiB) and rendered via
   [tui-markdown](https://crates.io/crates/tui-markdown). Headings,
   lists, blockquotes, fenced code blocks with syntax highlighting via
   [syntect](https://github.com/trishume/syntect), and inline emphasis
   all work.
3. A [notify](https://crates.io/crates/notify) watcher tracks the open
   file and reloads on disk changes — same live-reload experience as
   cmux, just rendered inside mdmux's own pane.
4. `J` / `K` / `Ctrl-D` / `Ctrl-U` scroll the preview.

Force this mode with `--no-cmux` if you'd rather not spawn external cmux
panes even when cmux is available.

## Architecture

```
src/
├── main.rs     — CLI, terminal setup, key dispatch
├── app.rs      — state machine (tree + selection + render mode + preview)
├── tree.rs     — directory walker + tree model + filter
├── cmux.rs     — cmux CLI wrapper (CmuxClient trait + mock + timeout)
├── preview.rs  — in-process preview loader + tui-markdown render
├── watcher.rs  — notify-based live-reload for in-process mode
└── ui.rs       — ratatui rendering
```

All side-effecting calls to cmux go through the `CmuxClient` trait, so the
state machine is fully unit-testable in either render mode.

```sh
cargo test
```

## Contributing

Bug reports and PRs welcome — open an issue first for anything bigger
than a typo so we can talk it through. See
[CONTRIBUTING.md](CONTRIBUTING.md) for development setup, commit
conventions, and the release process.

## License

[MIT](LICENSE) © nero408
