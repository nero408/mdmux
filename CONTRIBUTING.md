# Contributing to mdmux

Bug reports and PRs welcome — please open an issue first for anything
bigger than a typo so we can talk it through. `cargo test` should stay
green and new behavior gets a unit test (the `CmuxClient` trait makes the
state machine fully mockable in either render mode).

## Development

```sh
cargo test                       # unit tests
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

CI runs the same three commands on Linux + macOS for every PR.

## Commit messages

We use [conventional commits](https://www.conventionalcommits.org) so
`git-cliff` can group changelog entries automatically. Prefix every
commit message with one of:

| Prefix       | Section in CHANGELOG  |
|--------------|-----------------------|
| `feat:`      | Features              |
| `fix:`       | Bug Fixes             |
| `security:`  | Security              |
| `perf:`      | Performance           |
| `refactor:`  | Refactor              |
| `docs:`      | Documentation         |
| `test:`      | Tests                 |
| `ci:`        | CI                    |
| `build:`     | Build                 |
| `chore:`     | Chores                |

A commit body containing `BREAKING CHANGE:` — or a `!` after the type,
e.g. `feat!: …` — marks the entry as breaking. Anything else lands under
"Other"; nothing is silently dropped.

## Regenerating the demo gif

`assets/demo.gif` is rendered by [vhs](https://github.com/charmbracelet/vhs)
from `assets/demo.tape`. CI regenerates it automatically on every push
that touches `src/**` or the tape itself. To regenerate locally:

```sh
cargo install --path .          # put mdmux on PATH
brew install vhs                # or see vhs install docs
vhs assets/demo.tape            # writes assets/demo.gif
```

The tape uses `mdmux --demo`, which substitutes a built-in fake cmux
client so the gif can be produced on machines without cmux installed.

## Releasing (maintainers)

Cutting a release is one command:

```sh
cargo release patch --execute   # 0.1.0 → 0.1.1
cargo release minor --execute   # 0.1.0 → 0.2.0
cargo release major --execute   # 0.1.0 → 1.0.0
cargo release 0.3.0 --execute   # explicit version
```

[`cargo-release`](https://github.com/crate-ci/cargo-release) bumps
`Cargo.toml`, regenerates `CHANGELOG.md` via
[`git-cliff`](https://git-cliff.org) from the conventional-commit
history since the previous tag, commits the bump + changelog as
`chore(release): mdmux X.Y.Z`, tags it `vX.Y.Z`, publishes to crates.io,
and pushes the tag to GitHub. CI takes it from there.

Drop `--execute` to dry-run — cargo-release defaults to a no-op, so a
bare `cargo release patch` only prints what *would* happen.

The pushed tag triggers the [`release` workflow](.github/workflows/release.yml),
which:

1. Builds release binaries for `aarch64-apple-darwin`,
   `x86_64-apple-darwin`, and `x86_64-unknown-linux-gnu` on native
   runners.
2. Packages each as `mdmux-vX.Y.Z-<target>.tar.gz` + a `.sha256`
   sidecar.
3. Creates a GitHub Release with auto-generated notes and attaches the
   tarballs.
4. Pushes an updated `Formula/mdmux.rb` to the
   [`nero408/homebrew-tap`](https://github.com/nero408/homebrew-tap)
   repo so `brew install nero408/tap/mdmux` picks up the new version
   immediately.

### One-time setup

Maintainers need cargo-release + git-cliff installed:

```sh
cargo install cargo-release git-cliff --locked
cargo login                     # crates.io token, one-time
```

The Homebrew tap also needs one-time setup (only before the very first
release):

1. Create an empty public repo: `nero408/homebrew-tap`.
2. Generate a fine-grained PAT with `Contents: read and write` scope on
   that single repo.
3. Add it to `nero408/mdmux` as the `HOMEBREW_TAP_TOKEN` secret.

If the token is missing the workflow still publishes the GitHub
Release; it just skips the tap bump with a warning.

### Manual fallback

If cargo-release is unavailable for some reason, the old flow still
works:

```sh
# bump version in Cargo.toml, commit, then:
git tag v0.2.0
git push origin v0.2.0
cargo publish                   # crates.io
```
