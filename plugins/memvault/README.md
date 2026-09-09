# MemVault plugin for Claude Code

Shared memory for AI agents: your preferences, corrections and project
decisions follow you into every new session — and flow between your agents.

> Install the binary first: `curl -fsSL
> https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh
> | bash`, then add `~/.memvault/bin` to your `PATH`.

## Install

```
/plugin marketplace add dreamor/memvault
```
```
/plugin install memvault@memvault
```

(Send the two commands as separate prompts.)

## What you get

| Piece | What it does |
|---|---|
| `SessionStart` hook | Recalls relevant memories into every new session (startup, resume, clear, compact) — you don't have to ask. |
| `Stop` hook (opt-in) | With `MEMVAULT_HOOK_EXTRACT=1`, distills the session into unreviewed memory drafts in the review inbox. |
| `memvault` MCP server | 16 tools (`save_memory`, `search_memory`, `session_start`, `review_memory`, …) plus the `memory://user-profile` and `memory://project-context` resources. |
| Skills | `memvault-recall`, `memvault-save`, `memvault-review`, `memvault-sync` — invoked by the model when relevant. |
| Commands | `/memvault-review`, `/memvault-sync`, `/memvault-doctor`. |

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `MEMVAULT_AGENT_ID` | `claude-code` | Agent identity for scoped injection and stamped drafts. Other agents on your machine use their own ids and share the same store. |
| `MEMVAULT_HOOK_EXTRACT` | `0` | `1` = extract memory drafts at session end (they go to the review inbox, never straight into context). |
| `MEMVAULT_HTTP_URL` | `http://127.0.0.1:3777` | REST backend used when the CLI is unavailable (`memvault-mcp --transport http`). |
| `MEMVAULT_BIN` | — | Explicit path to the `memvault`/`memvault-cli` binary, for non-interactive shells missing `~/.memvault/bin`. |
| `MEMVAULT_HOOK_DEBUG` | `0` | `1` = append hook diagnostics to `${TMPDIR:-/tmp}/memvault-hook.log`. |

Hooks **never fail your session**: if neither the CLI nor an HTTP backend
answers, they exit quietly and the MCP server still works after you fix the
environment. Endpoint parity for every host lives in
[docs/AGENT-PORTABILITY.md](../../docs/AGENT-PORTABILITY.md).

## Notes

- The bundled MCP server runs `memvault-mcp --transport stdio`; it must be on
  your `PATH`. If your client does not pick it up, register it manually:
  `claude mcp add memvault -- memvault-mcp --transport stdio`.
- Rule files for hosts without hooks or MCP (`AGENTS.md`, `CLAUDE.md`, …) can
  be generated with `memvault sync` — see the `memvault-sync` skill.
- Check your setup anytime with `/memvault-doctor`.
