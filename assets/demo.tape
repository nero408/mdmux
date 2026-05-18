# mdmux demo tape — render with:
#   vhs assets/demo.tape
#
# Requires:
#   - `mdmux` on PATH (e.g. `cargo install --path .` first)
#   - `vhs` (https://github.com/charmbracelet/vhs)
#
# Uses --demo, which substitutes a built-in fake cmux client AND grows an
# in-process preview pane so the gif can show "what cmux would render"
# without cmux installed. CI uses this on every push that touches src/ or
# the tape.

Output assets/demo.gif

Set Shell bash
Set FontSize 14
Set Width 1200
Set Height 680
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms
Set PlaybackSpeed 1.0
Set Padding 18

# Seed the fake notes tree off-screen so the gif is reproducible.
Hide
Type "bash assets/seed_demo.sh" Enter
Type "cd /tmp/mdmux-demo" Enter
Type "clear" Enter
Show

# Launch.
Type "mdmux --demo ."
Sleep 400ms
Enter
Sleep 1800ms

# Open the top-level README so the preview pane appears.
Enter Sleep 2400ms

# Walk into docs/api/, open endpoints.md.
Down Sleep 250ms                 # docs/
Right Sleep 350ms                # expand docs/
Down Sleep 250ms                 # docs/api/
Right Sleep 350ms                # expand docs/api/
Down Sleep 250ms                 # endpoints.md
Enter Sleep 2400ms

# Replace, don't stack — open errors.md next.
Down Sleep 350ms
Enter Sleep 2400ms

# And webhooks.md.
Down Sleep 350ms
Enter Sleep 2400ms

# Live filter.
Type "/" Sleep 400ms
Type "des" Sleep 1400ms
Escape Sleep 500ms

# Peek at help.
Type "?" Sleep 1800ms
Escape Sleep 500ms

# Quit.
Type "q" Sleep 600ms
