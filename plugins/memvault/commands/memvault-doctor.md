---
description: Diagnose the MemVault integration and return one fix per failure
---

Run a read-only MemVault integration check. Do not change configuration, start
the server, or write memories unless the user explicitly asks; report what is
broken and the exact command that fixes it.

Run these checks in order and capture the real output:

1. **CLI on PATH** — `command -v memvault-cli && memvault-cli --version`.
   If missing, check `~/.memvault/bin/memvault-cli` before reporting it absent.
2. **Version alignment** — read the plugin manifest version from
   `${CLAUDE_PLUGIN_ROOT}/.claude-plugin/plugin.json` (fall back to this
   bundle's `plugin.json` path) and compare it with `memvault-cli --version`
   and `memvault-mcp --version`. Report a mismatch instead of assuming it is
   cosmetic.
3. **MCP binary** — `command -v memvault-mcp`. Then send one MCP `initialize`
   request over stdio:
   `printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"doctor","version":"1"}}}' | memvault-mcp --transport stdio`
   The response must be valid JSON-RPC and its instructions/server identity
   must mention MemVault.
4. **REST backend identity** — probe
   `curl -fsS --max-time 2 "${MEMVAULT_HTTP_URL:-http://127.0.0.1:3777}/api/health"`
   and require JSON with `"service":"memvault-mcp"`. Do not use bare `/health`
   for identity: it only returns `ok`. If nothing answers, report the command
   `memvault-cli dashboard` (or `memvault-cli server start --quiet`) as the fix.
5. **Database and pipeline** — run `memvault-cli status`, `memvault-cli list`
   and `memvault-cli doctor`. The database is usable if `status` reports a
   schema and `list` exits 0. Summarize doctor warning counts and the review
   backlog; do not print memory contents.
6. **Embedding provider** — read the first lines of `memvault-cli status`.
   If degraded, report the configured provider and whether `OPENAI_API_KEY`
   is relevant; never print the key.
7. **Hooks** — report whether this session contains a `MEMORY CONTEXT` block
   (recall is live). Report host-level `MEMVAULT_HOOK_EXTRACT` /
   `MEMVAULT_HOOK_OUTCOME` values without changing them. On Codex/Claude Code,
   state that hook trust must be checked in `/hooks`; a hook can be installed
   but still skipped until approved.

Return a compact table:

| Check | Result | Evidence | Fix |
|---|---|---|---|

Rules:

- Use `PASS`, `WARN`, or `FAIL` in Result.
- Put exactly one copy-pasteable command in Fix for each failed check; use `—`
  when no action is needed.
- Distinguish “CLI/MCP works without the REST server” from “Obsidian and the
  dashboard require the REST server”.
- End with the single highest-impact next action. Never claim success from an
  install message alone.
