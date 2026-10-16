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
| Session-start recall | ✅ documented-compatible, trust once — Codex bundles plugin hooks from the same `hooks/hooks.json` with identical `SessionStart`/`Stop` event names; it sets `CLAUDE_PLUGIN_ROOT` for hook compatibility, forwards the same stdin fields (`session_id`, `cwd`, …) and consumes the same `hookSpecificOutput.additionalContext` envelope. Hooks stay skipped until reviewed once via `/hooks`; trust is pinned to the script hash, so changed scripts re-require review |
| End-of-session extraction | ✅ documented-compatible, trust once + opt-in — Codex runs the same `Stop` hook semantics, and `memvault-cli extract` parses Codex's `rollout.jsonl` natively (developer app-context, tool-call and UI-event rows stay out). Still gated behind `MEMVAULT_HOOK_EXTRACT=1` and the friction gate; the rollout format is officially unstable, so drift degrades to noisy drafts, never silence |
| Rules file (`AGENTS.md`) | ✅ same as before, via `memvault-cli sync` |

## Memory identity

Codex and Claude Code load the *same* plugin and hit the *same* store, so they
must not report the same agent id. The hook scripts detect the host from the
environment (Codex exports `CODEX_*` into hook processes) and default to
`codex` under Codex, `claude-code` everywhere else — no per-host env needs to
be set in `hooks.json`, which Codex shares with Claude Code.

Two things must line up for project-scoped memories to be injected:

1. **A `codex` profile in `~/.memvault/agents.yaml`** — copy the `codex` block
   from [`agents.example.yaml`](../../agents.example.yaml). An id with no
   profile falls back to `default`, whose `namespace_filter` is `["global"]`,
   so `project:*` memories are silently dropped.
2. **The right id at runtime** — verify with
   `MEMVAULT_HOOK_DEBUG=1` (see `${TMPDIR}/memvault-hook.log`) or by checking
   that a hook-saved memory is stamped `source_agent.id: codex`.

To pin the id explicitly (e.g. the detection is wrong for your launcher), set
`MEMVAULT_AGENT_ID=codex` in the environment Codex launches hooks with.

## Stale installs

If an older `memvault-cli` shadows `PATH`, Codex's SessionStart hook calls it
with flags it does not understand (`--format hook-json`, `--hook-input`), so
nothing gets injected. The hook no longer hides that: it prints one stderr line
naming the failing binary, its exit code and the CLI's own message. Fix it by
keeping exactly one install channel — see
[`docs/TROUBLESHOOTING.md` §5.3](../../docs/TROUBLESHOOTING.md#53-multiple-installs-shadow-each-other).

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
   memvault-cli sync --out <project-dir>   # writes/refreshes AGENTS.md
   ```

3. Optional: copy `prompts/*.md` into `~/.codex/prompts/` to get the
   `/memvault-review`, `/memvault-save` and `/memvault-status` custom prompts.

With the plugin installed these prompts are redundant: the four plugin skills
(`memvault-recall`, `memvault-review`, `memvault-save`, `memvault-sync`) cover
the same workflows via Codex's `$`-mention syntax.

Codex shares the same SQLite store as Claude Code and every other MemVault
integration, so preferences learned with one agent are available to the other.
