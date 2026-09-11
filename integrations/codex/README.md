# MemVault for Codex

Since Codex adopted the same plugin + marketplace system, it can install the
exact same plugin directory Claude Code uses (`plugins/memvault/`): the repo
root doubles as a Codex marketplace and every Codex host reads
`.agents/plugins/marketplace.json`. Capabilities are intentionally advertised
honestly:

| Capability | Status |
|---|---|
| MCP (18 tools + 2 resources) | ✅ bundled via the plugin's `mcp.json` |
| Skills (recall / review / save / sync) | ✅ discovered from the plugin's `skills/` |
| Session-start recall | ⚠️ the plugin's `hooks/hooks.json` provides a `SessionStart` hook (session-start.sh); Codex reuses `hooks/hooks.json` and sets `CLAUDE_PLUGIN_ROOT`, but bundled hooks must be trusted once before they run |
| End-of-session extraction | ⚠️ the plugin also registers a `Stop` hook (session-extract.sh); the underlying command is the same one Claude Code runs, but Stop-hook semantics on Codex are not yet verified there |
| Rules file (`AGENTS.md`) | ✅ same as before, via `memvault sync` |

## Install (marketplace)

Prerequisite: install the MemVault binaries first (see the [root
README](../../README.md)); the hook scripts and the bundled MCP server both
expect `memvault` / `memvault-mcp` on `PATH`.

```bash
codex plugin marketplace add dreamor/memvault
```

Then install the plugin from the Codex plugin browser (or the Plugins tab in
the ChatGPT desktop app), or enable it for a repository directly in the
project's `.codex/config.toml`:

```toml
[plugins."memvault@memvault"]
enabled = true
```

Useful commands:

```bash
codex plugin marketplace list          # marketplace root Codex resolves
codex plugin list --marketplace memvault --available
codex plugin remove memvault@memvault  # uninstall
```

## Manual fallback (no plugin support)

If your Codex build predates the plugin system, the old three-step setup still
works:

1. Register the MCP server in `~/.codex/config.toml`:

   ```toml
   [mcp_servers.memvault]
   command = "memvault-mcp"
   args = ["--transport", "stdio"]
   ```

2. Project memory into the rules file Codex always reads:

   ```bash
   memvault sync --out <project-dir>   # writes/refreshes AGENTS.md
   ```

3. Optional: copy `prompts/*.md` into `~/.codex/prompts/` to get the
   `/memvault-review`, `/memvault-save` and `/memvault-status` custom prompts.

With the plugin installed these prompts are redundant: the four plugin skills
(`memvault-recall`, `memvault-review`, `memvault-save`, `memvault-sync`) cover
the same workflows via Codex's `$`-mention syntax.

Codex shares the same SQLite store as Claude Code and every other MemVault
integration, so preferences learned with one agent are available to the other.
