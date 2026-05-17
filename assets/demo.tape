# mdmux demo tape — render with:
#   vhs assets/demo.tape
#
# Requires:
#   - `mdmux` on PATH (e.g. `cargo install --path .` first)
#   - `vhs` (https://github.com/charmbracelet/vhs)
#
# CI regenerates this on every push that touches the tape or src/.

Output assets/demo.gif

Set Shell zsh
Set FontSize 14
Set Width 1100
Set Height 620
Set Theme "Catppuccin Mocha"
Set TypingSpeed 70ms
Set PlaybackSpeed 1.0
Set Padding 18

# Build a deterministic notes tree off-screen so the gif is reproducible.
Hide
Type "rm -rf /tmp/mdmux-demo && mkdir -p /tmp/mdmux-demo/docs/api /tmp/mdmux-demo/notes" Enter
Type "cd /tmp/mdmux-demo" Enter
Type "printf '# mdmux\n\nA terminal markdown browser.\n' > README.md" Enter
Type "printf '# Getting started\n' > docs/getting-started.md" Enter
Type "printf '# Endpoints\n' > docs/api/endpoints.md" Enter
Type "printf '# Errors\n' > docs/api/errors.md" Enter
Type "printf '# Webhooks\n' > docs/api/webhooks.md" Enter
Type "printf '# Design notes — 2026-05-13\n' > notes/2026-05-13-design.md" Enter
Type "printf '# Random thoughts\n' > notes/random.md" Enter
Type "clear" Enter
Show

# Launch.
Type "mdmux --demo ."
Sleep 400ms
Enter
Sleep 1600ms

# Walk down into docs/api/, open a file.
Down Sleep 300ms
Right Sleep 350ms          # expand docs/
Down Sleep 250ms
Right Sleep 350ms          # expand docs/api/
Down Sleep 250ms
Enter Sleep 1600ms         # open endpoints.md

# Hop to another file — panel is replaced, not stacked.
Down Sleep 350ms
Enter Sleep 1600ms         # open errors.md

# Show the live filter.
Type "/" Sleep 400ms
Type "des" Sleep 1200ms
Escape Sleep 500ms

# Peek at the help screen.
Type "?" Sleep 1600ms
Escape Sleep 500ms

# Quit.
Type "q" Sleep 500ms
