# MemVault

**Your agents forget between sessions. MemVault gives them one local, shared memory.**

Save a preference once. Claude Code, Codex, Cursor, and Obsidian can all recall it.
No account, no cloud dependency, no lock-in — the source of truth is one SQLite
file at `~/.memvault/data.db`.

![MemVault in 30 seconds](./assets/demo.gif)

## Start in 2 minutes

### 1. Install the binary

```bash
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
export PATH="$HOME/.memvault/bin:$PATH"
```

macOS/Linux installers also start the local REST backend. Verify it with
`memvault-cli status`.

### 2. Install the plugin

**Claude Code** (send these as two separate prompts):

```text
/plugin marketplace add dreamor/memvault
/plugin install memvault@memvault
```

**Codex**:

```bash
codex plugin marketplace add dreamor/memvault
```

Then install `memvault@memvault` from the plugin browser. On Codex, open
`/hooks` once and approve the `SessionStart` / `Stop` entries — hooks do not
run until trusted.

### 3. Prove recall works

In a session, say:

```text
Remember that I prefer pnpm over npm.
```

Open a **new** session and ask:

```text
What package manager do I prefer?
```

The new session should recall the preference before you mention it. If it does
not, run `/memvault-doctor` and follow its one-line fixes.

## What you get

| Piece | What it does |
|---|---|
| `SessionStart` hook | Recalls relevant memories into every new or resumed session automatically. |
| `Stop` hook (opt-in) | With `MEMVAULT_HOOK_EXTRACT=1`, distills durable learnings into the review inbox. Drafts never enter context until reviewed. |
| Outcome inference (opt-in) | With `MEMVAULT_HOOK_OUTCOME=1`, records whether a task succeeded, partially succeeded, or failed, so lessons can inform later work. |
| `SessionEnd` hook (on by default) | Closes the compliance rows the session opened: with an LLM extractor configured each injected memory is judged followed/violated/unknown, without one the rows are closed as `unknown`. Disable with `MEMVAULT_HOOK_COMPLIANCE=0`. |
| `memvault` MCP server | 22 tools (`save_memory`, `search_memory`, `session_start`, `review_memory`, `propose_memory_edit`, `withdraw_memory_edit`, …) plus the `memory://user-profile` and `memory://project-context` resources. |
| Skills | `memvault-recall`, `memvault-save`, `memvault-review`, `memvault-sync` — invoked by the model when relevant. |
| Commands | `/memvault-review`, `/memvault-save`, `/memvault-sync`, `/memvault-doctor`. |

## Your data and permissions

MemVault is deliberately boring about trust:

- **Reads:** the host's hook payload (session id, working directory, prompt
  metadata, or transcript path). When end-of-session extraction is enabled, it
  reads the transcript file the host passes to the `Stop` hook.
- **Writes:** `~/.memvault/data.db` (memories, review inbox, episodes and
  history), plus server PID/log files under `~/.memvault` when the local REST
  backend is running.
- **Does not:** create an account, upload your memory store, or require a
  cloud service. The REST backend binds to loopback and stores everything in
  the SQLite file above.
- **Turn it off:** set `MEMVAULT_HOOK_EXTRACT=0` / `MEMVAULT_HOOK_OUTCOME=0`
  (or unset them), or disable/remove the plugin. Existing data remains a local
  SQLite file you can inspect, back up, or delete.
- **Review boundary:** automatic extraction writes unreviewed drafts. Only
  approved memories are injected by the normal recall path.

## Commands

| Command | Purpose |
|---|---|
| `/memvault-doctor` | Check CLI, plugin version, MCP handshake, REST health, database, hooks and embeddings; report one fix per failure. |
| `/memvault-review` | Approve, edit or reject pending memory drafts. |
| `/memvault-save` | Save the current session's durable learnings. |
| `/memvault-sync` | Generate `AGENTS.md`, `CLAUDE.md`, or Copilot rule files for hosts without hooks. |

## Install details

### Claude Code

```text
/plugin marketplace add dreamor/memvault
/plugin install memvault@memvault
```

### Codex

Codex reads the same bundle through its own marketplace:

```bash
codex plugin marketplace add dreamor/memvault
```

Then install `memvault@memvault` from the plugin browser, or enable it in the
project's `.codex/config.toml`:

```toml
[plugins."memvault@memvault"]
enabled = true
```

Two Codex-specific notes:

- **Self-healing backend.** The `SessionStart` hook also runs
  `hooks/ensure-server.sh`, which calls the idempotent
  `memvault-cli server start --quiet` when the REST backend is not answering.
  A reboot fixes itself the next time you start a session. The probe also
  replaces a backend left over from an older release.
- **Host identity comes from the payload.** `MEMVAULT_AGENT_ID` wins, then
  `CODEX_*` in the environment, then the hook payload itself. Codex Desktop
  starts plugin hooks without `CODEX_*`, so the payload (`turn_id`, a Codex
  rollout transcript, or Codex-specific fields) is the reliable signal.
- **Trust the hooks once.** Codex runs plugin hooks only after approval. An
  untrusted hook is listed but skipped. Approve the bundled `SessionStart` /
  `Stop` entries in `/hooks`; the review is pinned to script hashes, so
  editing a hook asks again.
- **Two manifests, one bundle.** Claude Code reads
  `.claude-plugin/plugin.json`; Codex reads the native
  `.codex-plugin/plugin.json`. The Codex manifest mirrors the version and
  listing metadata, declares `skills`, and points `mcpServers` at the
  portable plugin-root `mcp.json`. It deliberately omits `hooks`, so Codex
  discovers the shared `hooks/hooks.json`; Codex exports both
  `${PLUGIN_ROOT}` and `${CLAUDE_PLUGIN_ROOT}` to hook processes. Do not add a
  plugin-root `plugin.json`: on Codex `0.163.0-alpha.11.1` its presence stops
  default `hooks/hooks.json` discovery. `tests/run-tests.sh` and CI enforce
  all of this.

## Environment

| Variable | Default | Meaning |
|---|---|---|
| `MEMVAULT_AGENT_ID` | `codex` under Codex, else `claude-code` | Agent identity for scoped injection and stamped drafts. The id must have an entry in `~/.memvault/agents.yaml`; an unknown id falls back to `default`, whose `namespace_filter` is global-only, so project memories would not be injected. |
| `MEMVAULT_HOOK_EXTRACT` | `0` | `1` = extract memory drafts at session end (they go to the review inbox, never straight into context). |
| `MEMVAULT_HOOK_OUTCOME` | `0` | `1` = infer a task outcome at session end and record it as an episode. Independent of `MEMVAULT_HOOK_EXTRACT`. |
| `MEMVAULT_HOOK_COMPLIANCE` | `1` | `1` = at `SessionEnd`, finalize the pending compliance rows for this session (judged followed/violated when an LLM extractor is configured, otherwise `unknown`). `0` leaves them pending. |
| `MEMVAULT_HTTP_URL` | `http://127.0.0.1:3777` | REST backend used when the CLI is unavailable (`memvault-mcp --transport http`). |
| `MEMVAULT_API_KEY` | — | API key for the REST fallback when this agent has a registered key. Sent as `X-MemVault-Api-Key` with `MEMVAULT_AGENT_ID`. The CLI path does not need it. |
| `MEMVAULT_BIN` | — | Explicit path to the `memvault`/`memvault-cli` binary, for non-interactive shells missing `~/.memvault/bin`. |
| `MEMVAULT_MCP_BIN` | — | Explicit path to `memvault-mcp` for the bundled launcher. Takes precedence over PATH and the install prefix. |
| `MEMVAULT_CODEX_BIN` | — | Explicit path to the `codex` CLI for the automatic plugin-cache refresh. Normally found on PATH; set this when the Codex app launches hooks with a reduced PATH. |
| `MEMVAULT_HOOK_PLUGIN_UPGRADE` | `1` | On Codex, `1` lets SessionStart queue a background marketplace refresh when the installed CLI is newer than the cached plugin. `0` disables the self-heal. |
| `MEMVAULT_AGENT_<ID>_API_KEY` | — | Server-side MCP credential bound to one agent id (uppercase, non-alphanumerics → `_`). Lets writes such as `fact_write=resolve` authenticate without a per-call `api_key`; a mismatched id never receives it. `MEMVAULT_AGENT_API_KEY` is the same fallback for the single agent named by `MEMVAULT_AGENT_ID`. |
| `MEMVAULT_HOOK_DEBUG` | `0` | `1` = append hook diagnostics to `${TMPDIR:-/tmp}/memvault-hook.log`. |

These are **hook-process** variables: set them in the environment your host
launches hooks with. `~/.memvault/.env` is only read by the CLI/MCP binaries,
not by the hook scripts, and Codex/Claude `hooks.json` has no per-hook `env`
block — so a GUI-launched Codex that misdetects its host needs the variable
exported at the app level (e.g. `launchctl setenv MEMVAULT_AGENT_ID codex` on
macOS).

Hooks **never fail your session**: if neither the CLI nor an HTTP backend
answers they exit 0 and the MCP server still works once you fix the environment.
They are not silent, though — one diagnostic line goes to stderr on failure, so
an outage is visible without `MEMVAULT_HOOK_DEBUG=1`.

## Notes

- The bundle launches MCP through `bin/memvault-mcp-launcher`, which resolves
  `$MEMVAULT_MCP_BIN`, then `memvault-mcp` on `PATH`, then
  `~/.memvault/bin/memvault-mcp` and the Homebrew prefixes. A host launched
  without your login shell therefore still finds the install; if the binary is
  somewhere else, set `MEMVAULT_MCP_BIN`. Manual registration still works:
  `claude mcp add memvault -- memvault-mcp --transport stdio`.
- Rule files for hosts without hooks or MCP (`AGENTS.md`, `CLAUDE.md`, …) can
  be generated with `memvault-cli sync` — see the `memvault-sync` skill.
- On Codex, SessionStart self-heals a stale plugin cache: when the installed
  CLI reports a newer version than the plugin manifest, it queues an idempotent
  `codex plugin marketplace upgrade memvault` in the background (throttled to
  one attempt per six hours). The current session keeps running the old bundle;
  the next Codex session loads the refreshed one. Disable with
  `MEMVAULT_HOOK_PLUGIN_UPGRADE=0`.
- After changing this bundle from a source checkout, use
  `scripts/refresh-codex-plugin.sh` (idempotent; upgrades the marketplace
  snapshot and reinstalls `memvault@memvault`); changed hook scripts re-require
  `/hooks` trust. The manual fallback for end users remains
  `codex plugin marketplace upgrade memvault` plus a reinstall from the plugin
  browser if the cache is still stale.
- On Codex, confirm the hooks actually loaded before debugging anything else:
  they must appear as plugin hooks in `/hooks`. If they are absent, verify the
  installed bundle contains `.claude-plugin/plugin.json` and
  `hooks/hooks.json` but **no** plugin-root `plugin.json`.
