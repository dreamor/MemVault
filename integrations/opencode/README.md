# MemVault for OpenCode

OpenCode plugins are plain JS, so this is the most capable integration after
Claude Code's:

| Capability | Status |
|---|---|
| MCP (16 tools + 2 resources) | ✅ |
| Session-start recall | ✅ hook-injected every session (`experimental.chat.system.transform`) |
| End-of-session extraction | ✅ on `session.idle`, drafts land in the review inbox (opt-in: `MEMVAULT_HOOK_EXTRACT=1`) |
| Slash commands | ✅ `/memvault-review`, `/memvault-save` |

## Setup

1. Install the binary (`scripts/install.sh`), then make sure `memvault-mcp`
   is on your `PATH` (or run `memvault-mcp --transport http` for REST).
2. Merge [opencode.json](opencode.json) into your project's `opencode.json`,
   replacing the `plugin` entry with an absolute path to
   `plugins/memvault.mjs` from this checkout.
3. Copy `command/*.md` into `.opencode/command/` for the slash commands.
4. Optional, project-level: `export MEMVAULT_HOOK_EXTRACT=1` before launching
   OpenCode to enable idle-time extraction.

Tunables (all optional): `MEMVAULT_HTTP_URL` (default
`http://127.0.0.1:3777`), `MEMVAULT_AGENT_ID` (default `opencode`),
`MEMVAULT_HOOK_EXTRACT`.

Verification status: authored against the OpenCode plugin API shape
(`experimental.chat.system.transform`, `event` with `session.idle`,
`client.session.messages`). Re-verify hook names against
[the plugin docs](https://opencode.ai/docs/plugins) when OpenCode ships
breaking changes; the plugin fails closed (no injection, no extraction)
rather than failing your session.
