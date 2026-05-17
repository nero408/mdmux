# mdmux: a tiny TUI that turns cmux into a markdown IDE

*Published 2026-05-17 · 4 min read · [github.com/nero408/mdmux](https://github.com/nero408/mdmux)*

I take a lot of notes in markdown. Design docs, meeting notes, half-baked
ideas, code review walkthroughs — all `.md` files scattered across a
directory tree on disk. The two ways I'd been reading them were both bad:

1. **Editor preview panes** (VS Code, Obsidian). Great rendering, but they
   pull me out of the terminal where I actually live.
2. **`cat`, `bat`, `less`, `glow`**. Stays in the terminal, but I either
   lose the rich rendering or I lose the *tree* — the ability to skim a
   folder, hop between files, and watch them update as I edit.

What I actually wanted was an `nnn`/`yazi`-style file picker on the left,
a live-rendered markdown panel on the right, no mouse, no Electron, no
context switch. So I built one.

**`mdmux`** is a 1,500-line Rust TUI that does exactly that — and it gets
the rendering for free by piggy-backing on [cmux](https://cmux.app).

```
 mdmux  /home/me/notes
┌ Files (12) ────────────────────────┐
│  📝 README.md                      │
│▾ 📁 docs                           │
│  ▾ 📁 api                          │      ┌────────────────────────┐
│      📝 endpoints.md               │ ───▶ │ Rendered markdown      │
│      📝 errors.md                  │      │ shows up in the cmux   │
│      📝 webhooks.md                │      │ panel to the right,    │
│    📝 getting-started.md           │      │ with live reload.      │
│▾ 📁 notes                          │      └────────────────────────┘
│    📝 2026-05-13-design.md         │
│    📝 random.md                    │
└────────────────────────────────────┘
 open  /home/me/notes/docs/api/endpoints.md  [surface:438]
```

## The trick: don't render markdown, let cmux do it

cmux ships a `cmux markdown open <path>` command. It splits the current
pane, renders the file with rich formatting, and watches it for changes
on disk. It prints something like `surface:438` to stdout — a handle to
the panel it just opened.

That's the whole rendering pipeline I needed. So mdmux is *not* a
markdown renderer. It's a file picker that talks to cmux:

1. Walk the directory tree, respecting `.gitignore` (via `ignore`, the
   same crate ripgrep uses). Keep only `.md` files and their ancestor
   directories.
2. Render the tree in [ratatui](https://ratatui.rs/) with Vim-style keys.
3. On `Enter`, shell out to `cmux markdown open <selected>`, parse
   `surface:NNN` from its output, remember it.
4. On the *next* `Enter`, call `cmux close-surface --surface <previous>`
   *before* opening the new one. The panel is replaced, not stacked.
5. On `q`, close the tracked panel. On `Q`, leave it open.

That's it. ~150 lines of glue between a tree walker and a CLI wrapper.

The "replace, don't stack" rule is the one design decision I'd defend to
the death. Stacked panels make a file picker feel like a tab hoarder.
Replacement makes it feel like a preview pane.

## Why a separate tool, why not a cmux feature?

Three reasons:

- **Boundaries.** cmux is a terminal multiplexer. Knowing which markdown
  file to open next is a notes-app concern, not a multiplexer concern.
- **Composition.** `mdmux --list` prints matched paths and exits — pipe
  it into `fzf`, `xargs`, your own scripts. The TUI is one front-end
  among many.
- **Testability.** Every cmux call goes through a `CmuxClient` trait
  with a mock impl. The state machine is unit-tested without ever
  spawning a real cmux process. `cargo test` runs in <1s.

## What I learned building it

A few things were surprisingly easy:

- **`ignore` is a gift.** Two function calls and I get ripgrep-grade
  `.gitignore` handling, including nested ignores and global excludes.
- **ratatui's stateful widgets** make tree selection trivial once you
  separate "the model" (a flat `Vec<Entry>` with depth) from "the
  rendering."
- **Trait-based side effects** kept me honest. Every time I was tempted
  to call `Command::new("cmux")` directly, the trait reminded me to
  route it through the wrapper. The state machine never knew cmux
  existed.

A few were surprisingly hard:

- **Edge keys.** `g g` for "jump to top" needs a pending-prefix state
  machine that times out and forgets, exactly like Vim. Most TUI key
  handlers I'd seen ignored this; getting it right took a refactor.
- **Surface lifecycle.** "Quit while a panel is open" is the kind of
  case you forget until a user's pane is stuck open after `Ctrl+C`. The
  fix was a `Drop` impl that closes any tracked surface, plus an
  explicit `Q` to opt out.
- **Filter + tree.** Live `/`-filtering has to keep parent directories
  visible even when *they* don't match — otherwise matched files become
  orphans with no path context. Solved by collecting matches first,
  then back-filling ancestors.

## Install

```sh
brew install nero408/tap/mdmux
# or
cargo install mdmux
```

You need [cmux](https://cmux.app) installed and running. macOS and Linux
are supported. Rust 1.83+ to build from source.

## Where it's headed

Short list of things I'm thinking about:

- **`s`-search across file *contents*** (ripgrep-style), not just names.
- **Bookmarks** — pin a directory to the top of the tree.
- **A `--watch` mode** that re-walks on disk events instead of on `r`.
- **Plugin hook** for "open with" so people can wire it to glow / mdcat
  / their editor instead of cmux when they want.

If any of those sound useful — or you have a use case I haven't thought
of — open an issue. PRs welcome; `cargo test` should stay green and new
behavior gets a unit test against the `CmuxClient` trait.

## Try it

```sh
brew install nero408/tap/mdmux
cd ~/notes
mdmux
```

That should be all it takes. If it isn't, [tell me what broke](https://github.com/nero408/mdmux/issues).

— [@nero408](https://github.com/nero408)
