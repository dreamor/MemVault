# MemVault plugin for Claude Code and Codex

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

## Install (Codex)

Codex reads the same bundle through its own marketplace:

```bash
codex plugin marketplace add dreamor/memvault
```

then install `memvault@memvault` from the plugin browser, or enable it in the
project's `.codex/config.toml`:

```toml
[plugins."memvault@memvault"]
enabled = true
```

Two Codex-specific notes:

- **Trust the hooks once.** Codex runs plugin hooks only after they have been
  approved; an untrusted hook is listed but skipped ("hooks need review before
  they can run"). Approve the bundled `SessionStart`/`Stop` entries once in
  Codex's hooks view (`/hooks`). The review is pinned to the script hashes, so
  editing a hook asks again.
- **Portable manifest + hooks.** The bundle ships a portable Agent Plugins
  `plugin.json` (what ChatGPT/Codex read) alongside the
  `.claude-plugin/plugin.json` Claude Code reads. Codex discovers
  `hooks/hooks.json` by default as long as the portable manifest does not
  define `hooks` itself — an explicit `hooks` value (top-level or
  `extensions.com.openai.hooks`) would replace default-file discovery and
  `SessionStart` recall would silently stop. `tests/run-tests.sh` and CI
  enforce this.

## What you get

| Piece | What it does |
|---|---|
| `SessionStart` hook | Recalls relevant memories into every new session (startup, resume, clear, compact) — you don't have to ask. |
| `Stop` hook (opt-in) | With `MEMVAULT_HOOK_EXTRACT=1`, distills the session into unreviewed memory drafts in the review inbox. |
| `Stop` hook (opt-in, independent) | With `MEMVAULT_HOOK_OUTCOME=1`, infers a task outcome (success/partial/fail) from the session and records it as an episode for `bench` and lesson reflection — never bypasses review, only decides whether to record one. |
| `memvault` MCP server | 16 tools (`save_memory`, `search_memory`, `session_start`, `review_memory`, …) plus the `memory://user-profile` and `memory://project-context` resources. |
| Skills | `memvault-recall`, `memvault-save`, `memvault-review`, `memvault-sync` — invoked by the model when relevant. |
| Commands | `/memvault-review`, `/memvault-save`, `/memvault-sync`, `/memvault-doctor`. |

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `MEMVAULT_AGENT_ID` | `codex` under Codex, else `claude-code` | Agent identity for scoped injection and stamped drafts. The hook picks the host default from the environment; override it when the host is misdetected. The id must have an entry in `~/.memvault/agents.yaml` (see `agents.example.yaml`) — an unknown id falls back to `default`, whose `namespace_filter` is global-only, so **project memories would not be injected**. |
| `MEMVAULT_HOOK_EXTRACT` | `0` | `1` = extract memory drafts at session end (they go to the review inbox, never straight into context). |
| `MEMVAULT_HOOK_OUTCOME` | `0` | `1` = infer a task outcome at session end and record it as an episode. Independent of `MEMVAULT_HOOK_EXTRACT`. |
| `MEMVAULT_HTTP_URL` | `http://127.0.0.1:3777` | REST backend used when the CLI is unavailable (`memvault-mcp --transport http`). |
| `MEMVAULT_BIN` | — | Explicit path to the `memvault`/`memvault-cli` binary, for non-interactive shells missing `~/.memvault/bin`. |
| `MEMVAULT_HOOK_DEBUG` | `0` | `1` = append hook diagnostics to `${TMPDIR:-/tmp}/memvault-hook.log`. |

These are **hook-process** variables: set them in the environment your host
launches hooks with. `~/.memvault/.env` is only read by the CLI/MCP binaries,
not by the hook scripts, and Codex/Claude `hooks.json` has no per-hook `env`
block — so a GUI-launched Codex that misdetects its host needs the variable
exported at the app level (e.g. `launchctl setenv MEMVAULT_AGENT_ID codex` on
macOS).

Hooks **never fail your session**: if neither the CLI nor an HTTP backend
answers they exit 0 and the MCP server still works once you fix the
environment. They are no longer silent about it, though — one diagnostic line
goes to stderr on failure (`session-start failed (rc=…, bin=…)`), so an outage
is visible without `MEMVAULT_HOOK_DEBUG=1`.

## Notes

- The bundled MCP server runs `memvault-mcp --transport stdio`; it must be on
  your `PATH`. If your client does not pick it up, register it manually:
  `claude mcp add memvault -- memvault-mcp --transport stdio`.
- Rule files for hosts without hooks or MCP (`AGENTS.md`, `CLAUDE.md`, …) can
  be generated with `memvault-cli sync` — see the `memvault-sync` skill.
- Check your setup anytime with `/memvault-doctor`.
- After the bundle changes in this repo, refresh Codex's cached copy with
  `scripts/refresh-codex-plugin.sh` (idempotent; upgrades the marketplace
  snapshot and reinstalls `memvault@memvault`); changed hook scripts
  re-require `/hooks` trust.
- On Codex, confirm the hooks actually loaded before debugging anything else:
  they must appear (as plugin hooks) in the `/hooks` review list. If they are
  absent, upgrade the bundle to the current portable manifest — a `hooks`
  definition in `plugin.json` would also replace default `hooks/hooks.json`
  discovery; if they are listed as untrusted, approve them there.
