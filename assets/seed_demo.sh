#!/usr/bin/env bash
# Build the deterministic notes tree the demo tape walks through.
# Called from assets/demo.tape's Hide block. Safe to run by hand too.

set -euo pipefail

ROOT="${1:-/tmp/mdmux-demo}"
rm -rf "$ROOT"
mkdir -p "$ROOT/docs/api" "$ROOT/notes"

cat > "$ROOT/README.md" <<'EOF'
# mdmux

A terminal browser for markdown files. Tree on the left,
live-rendered file on the right.

## Why

- Stay in the terminal — no editor preview pane
- Live reload on disk changes
- Vim keys, `.gitignore`-aware, replaces (not stacks) panels

> "Finally a markdown viewer that doesn't open Electron."

## Install

```sh
brew install nero408/tap/mdmux
mdmux ~/notes
```
EOF

cat > "$ROOT/docs/getting-started.md" <<'EOF'
# Getting started

mdmux walks the current directory for markdown files and renders
the selected one in a side panel.

## First run

```sh
cd ~/notes
mdmux
```

- `Enter` opens the file
- `/` filters incrementally
- `q` quits and closes the panel

See `docs/api/` for the endpoint reference.
EOF

cat > "$ROOT/docs/api/endpoints.md" <<'EOF'
# API endpoints

The service exposes three REST endpoints.

## GET /users

Returns a paginated list of users.

- `200` — body matches `User[]`
- `401` — token missing or invalid
- `429` — rate limited

## POST /users

Creates a user.

```json
{ "email": "ada@example.com", "name": "Ada" }
```
EOF

cat > "$ROOT/docs/api/errors.md" <<'EOF'
# Error catalog

## 4xx — client errors

- `400` bad request body
- `401` missing or invalid token
- `403` authenticated but not authorized
- `404` resource not found

## 5xx — server errors

- `500` internal — see `/var/log/api/error.log`
- `503` upstream unavailable, retry with backoff
EOF

cat > "$ROOT/docs/api/webhooks.md" <<'EOF'
# Webhooks

Outbound delivery is at-least-once.

> Always treat webhook bodies as untrusted until verified.

```http
POST /your-endpoint
X-MDMUX-Signature: sha256=...
```
EOF

cat > "$ROOT/notes/2026-05-13-design.md" <<'EOF'
# Design — 2026-05-13

## Question

Should mdmux render markdown itself, or delegate to cmux?

## Decision

**Delegate.** cmux already renders markdown with live reload.
mdmux is the file picker.

---

- ~150 lines of glue
- Trait-based cmux client so we can mock the side effects
- Replace, not stack, on subsequent opens
EOF

cat > "$ROOT/notes/random.md" <<'EOF'
# Random thoughts

- TUIs age really well — same keys for a decade
- `vhs` is the right answer for project demos
- A good launch gif is worth 1000 README words
EOF

echo "seeded ${ROOT}"
