# mdmux

[![CI](https://github.com/nero408/mdmux/actions/workflows/ci.yml/badge.svg)](https://github.com/nero408/mdmux/actions/workflows/ci.yml)
[![demo gif](https://github.com/nero408/mdmux/actions/workflows/demo.yml/badge.svg)](https://github.com/nero408/mdmux/actions/workflows/demo.yml)
[![Crates.io](https://img.shields.io/crates/v/mdmux.svg)](https://crates.io/crates/mdmux)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A terminal UI for browsing markdown files in a directory tree and rendering
the selected file in a [cmux](https://cmux.app) side-panel with live reload.

<p align="center">
  <img src="assets/demo.gif" alt="mdmux demo — file tree on the left, rendered markdown on the right" width="900">
</p>

> The gif is recorded with `mdmux --demo`, which renders an in-process
> preview pane so the demo works on machines without cmux. In normal use
> the right pane is cmux's own native renderer (with full live-reload).

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
- macOS or Linux with [cmux](https://cmux.app) installed and running
- Rust ≥ 1.83 to build from source (uses edition 2024)

## Usage

```sh
mdmux                  # browse the current directory
mdmux ~/notes          # browse a specific directory
mdmux --hidden         # include dotfiles
mdmux --no-gitignore   # show ignored markdown files too
mdmux --max-depth 3    # cap recursion
mdmux --list           # print markdown paths and exit (scripting)
mdmux --demo           # run with a fake cmux client (screenshots / gifs / CI)
```

Run it inside a cmux pane. Pressing **Enter** on a markdown file splits the
current pane to the right and shows the file in cmux's built-in markdown
viewer (with rich formatting and live file watching). Selecting another file
**replaces** the panel — no stacked tabs.

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
| `Enter` / `o`   | open selected markdown in cmux side panel               |
| `a`             | toggle auto-open while navigating                       |
| `x`             | close the current cmux markdown panel                   |
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

cmux already has a native `cmux markdown open <path>` command that renders
markdown into a `[markdown]` surface with live reload. mdmux is the
file-browsing front-end:

1. Walks the directory tree (respecting `.gitignore` by default, like
   ripgrep) and collects markdown files plus their ancestor directories.
2. On `Enter`, shells out to `cmux markdown open <path>`, parses the
   `surface:NNN` id from its output, and remembers it.
3. On the next `Enter`, calls `cmux close-surface --surface <prev>` before
   opening the new one. Result: the panel is **replaced**, not stacked.
4. On quit (`q`), closes the tracked panel. `Q` skips the close.

## Architecture

```
src/
├── main.rs   — CLI, terminal setup, key dispatch
├── app.rs    — application state machine (tree + selection + panel state)
├── tree.rs   — directory walker + tree model + filter
├── cmux.rs   — cmux CLI wrapper (CmuxClient trait + mock)
└── ui.rs     — ratatui rendering
```

All side-effecting calls to cmux go through the `CmuxClient` trait so the
state machine is fully unit-testable.

```sh
cargo test
```

## Contributing

Bug reports and PRs welcome — open an issue first for anything bigger than
a typo so we can talk it through. `cargo test` should stay green; new
behavior gets a unit test against the `CmuxClient` trait.

### Regenerating the demo gif

`assets/demo.gif` is rendered by [vhs](https://github.com/charmbracelet/vhs)
from `assets/demo.tape`. CI regenerates it automatically on every push that
touches `src/**` or the tape itself. To regenerate locally:

```sh
cargo install --path .            # put mdmux on PATH
brew install vhs                  # or see vhs install docs
vhs assets/demo.tape              # writes assets/demo.gif
```

The tape uses `mdmux --demo`, which substitutes a built-in fake cmux client
so the gif can be produced on machines without cmux installed.

### Releasing (maintainers)

Cutting a release is one command — push a `v*` tag. CI handles the rest:

```sh
# bump version in Cargo.toml, commit, then:
git tag v0.2.0
git push origin v0.2.0
```

The [`release` workflow](.github/workflows/release.yml) then:

1. Builds release binaries for `aarch64-apple-darwin`, `x86_64-apple-darwin`,
   and `x86_64-unknown-linux-gnu` on native runners.
2. Packages each as `mdmux-vX.Y.Z-<target>.tar.gz` + a `.sha256` sidecar.
3. Creates a GitHub Release with auto-generated notes and attaches the
   tarballs.
4. Pushes an updated `Formula/mdmux.rb` to the
   [`nero408/homebrew-tap`](https://github.com/nero408/homebrew-tap) repo
   so `brew install nero408/tap/mdmux` picks up the new version
   immediately.

**One-time setup** (only needed before the very first release):

1. Create an empty public repo: `nero408/homebrew-tap`.
2. Generate a fine-grained PAT with `Contents: read and write` scope on
   that single repo.
3. Add it to `nero408/mdmux` as the `HOMEBREW_TAP_TOKEN` secret.

If the token is missing the workflow still publishes the GitHub Release;
it just skips the tap bump with a warning.

## License

[MIT](LICENSE) © nero408
