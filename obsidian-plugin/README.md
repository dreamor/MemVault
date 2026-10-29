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

## Development

```bash
npm install
npm run build      # esbuild -> out/main.js
npm run typecheck
npm test           # vitest
```
