# MemVault for Codex CLI

Codex has no SessionStart/Stop hooks for instruction injection, so this
integration is the "instruction-tier" shape: MCP + rules file + custom
prompts. Capabilities are intentionally advertised honestly:

| Capability | Status |
|---|---|
| MCP (18 tools + 2 resources) | ✅ register below |
| Session-start recall | ⚠️ agent-invoked via rules file (`session_start` tool), not hook-injected |
| End-of-session extraction | ❌ Codex exposes no transcript hook; use the review inbox via prompts, or run `memvault extract` on `~/.codex/sessions/*.jsonl` manually |

## Setup

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

Codex shares the same SQLite store as Claude Code and every other MemVault
integration, so preferences learned with one agent are available to the other
(via either the `session_start` tool or the synced `AGENTS.md` blocks).
