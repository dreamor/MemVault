# MemVault Obsidian plugin

The GUI client for browsing and capturing MemVault memories inside an
Obsidian vault: a sidebar panel to list/search/delete memories, command
palette actions to save a selection as a memory (or a MUST rule) or extract
memories from it, and `memvault-cli sync`-style vault sync. Management
operations (review inbox, supersede, export/import, decay, promote) live in
the Web Dashboard and CLI instead — this plugin doesn't duplicate them.

It talks to MemVault over REST. Start the managed backend with
`memvault-cli dashboard` (default `http://127.0.0.1:3777`), then install the
plugin from the Obsidian community directory. **Desktop only:** the default
server address is loopback, so Obsidian on a phone cannot reach a backend
running on your computer.

If the panel cannot connect, open **Settings → MemVault → Detect server**.
Detection probes `127.0.0.1:3777` first and the legacy `:8080` port second.

Full install (community directory / BRAT / from source), configuration, and
command reference: [`docs/INSTALL.md` §4](../docs/INSTALL.md#4-obsidian-plugin).

This package releases on its own version/schedule into the separate
[`dreamor/memvault-obsidian`](https://github.com/dreamor/memvault-obsidian)
repo (source of truth is `src/` here) — see
[`docs/RELEASING.md` §2](../docs/RELEASING.md#2-obsidian-plugin-releases-dreamormemvault-obsidian).

## Synced notes

Vault sync writes one Markdown file per memory under
`MemVault/{10-Daily,20-Entities,30-Memories,40-Skills}/`, chosen by memory
type. Sync is one-way (DB → vault); a note is rewritten wholesale on update, so
edits made in Obsidian do not survive the next sync.

`memvault_id` is the authoritative match key — the filename is derived from the
content and may be renamed freely.

Every note carries a `status` property: `active`, `expired`, `archived`,
`superseded`, or `proposal`. It is derived, not stored, from the status-bearing
properties written alongside it (`expires_at`, `archived_at`, `occurred_at`,
`superseded_by`, `proposes_supersede_of`), each of which appears only when set:

```yaml
---
memvault_id: mem_abc12345
memvault_updated_at: 2026-08-14T09:00:00Z
status: expired
priority: MUST
type: preference
layer: L3
tags: ["coding", "python"]
namespace: global
human_reviewed: true
expires_at: 2026-08-20T00:00:00Z
---
```

`status` is a snapshot taken at sync time. A memory that expires later still
reads `active` until the next sync runs — which the sync itself detects, since
a note whose recorded status no longer matches is rewritten even when
`updated_at` has not moved (expiry is a comparison against the clock, and
archive/supersede are written by some *other* memory's approval, so neither
bumps it). To compute liveness live in a Dataview query, use `expires_at`
rather than `status`.

A note that is not `active` also gets a callout above its content, because
Obsidian collapses properties by default and hides them entirely in reading
mode.

## Development

```bash
npm install
npm run build      # esbuild -> out/main.js
npm run typecheck
npm test           # vitest
```
