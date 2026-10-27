# Agent Install Guide

> **You are an AI agent that has been asked to install MemVault.** This is the
> machine-readable companion to [INSTALL.md](INSTALL.md): run it top-to-bottom
> rather than reading it for background, and stop at the first step that fails
> to report exactly what broke. Human-facing detail (full platform matrix, Web
> Dashboard, Docker, uninstall, troubleshooting) lives in [INSTALL.md](INSTALL.md)
> and [TROUBLESHOOTING.md](TROUBLESHOOTING.md) — follow those links when a step
> needs it; do not invent commands that are not in these docs.

MemVault gives every agent the user runs one shared, self-hosted memory store.
It is three binaries — `memvault-cli` (CLI), `memvault-mcp` (MCP server + REST
backend), `memvault-proxy` (optional) — backed by a single SQLite file at
`~/.memvault/data.db`. Nothing is uploaded; there is no account.

Your job has four steps: **install → verify → register this host → confirm recall**.

## 1. Install the binaries

Detect the OS first (`uname -s` on Unix, `$env:OS` on PowerShell).

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
export PATH="$HOME/.memvault/bin:$PATH"
```

**Windows (PowerShell):**

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

The installer downloads the matching prebuilt release, checks its SHA-256, and
installs to `~/.memvault/bin`. It also starts the local REST backend (the Web
Dashboard host); re-running it is a no-op if that backend is already up.

Do **not** treat a checksum failure as transient — it aborts for a reason. Fall
backs, in order of preference, if the one-liner cannot run here:

- **Homebrew (Apple Silicon):** `brew install dreamor/tap/memvault`
- **crates.io (needs a Rust 1.85+ toolchain):** `cargo install memvault-cli memvault-mcp`
- **Docker:** `docker pull ghcr.io/dreamor/memvault:latest`
- Prebuilt Linux binaries need **glibc ≥ 2.38** (Ubuntu 24.04+ / Fedora 39+ /
  Arch). On older Linux, use Docker or build from source. There are **no
  prebuilt Intel-macOS binaries** — build from source there.

## 2. Verify the install

```bash
memvault-cli --version     # expect: memvault 0.6.6
memvault-mcp --version     # expect: memvault-mcp 0.6.6
memvault-cli list          # empty list on a fresh install = database is healthy
```

If `memvault-cli` is "command not found", `~/.memvault/bin` (or `~/.cargo/bin`)
is not on `PATH` — re-export it for this shell, or call the binary by absolute
path.

## 3. Register this host as an MCP client

Pick the single row that matches the agent you are running inside. Set a
per-agent identity with `MEMVAULT_AGENT_ID` — hosts share one store, and the id
is how MemVault tells them apart (it is provenance, not a filter).

**Claude Code** — one-command native plugin (bundled MCP server, skills, slash
commands, SessionStart recall hook). Run these two prompts separately:

```
/plugin marketplace add dreamor/memvault
/plugin install memvault@memvault
```

**Codex CLI** — same plugin bundle:

```
codex plugin marketplace add dreamor/memvault
```

then install `memvault@memvault` from the plugin browser and **approve the
SessionStart / Stop hooks once in `/hooks`** (they do nothing until trusted).

**Gemini CLI / Antigravity:**

```
gemini extensions install https://github.com/dreamor/memvault
```

**DeepSeek Harness (dsh):** use the in-repo Cordis plugin — see
[INSTALL.md §2.5](INSTALL.md#25-deepseek-harness-dsh).

**Any other MCP client** (Claude Desktop, Cursor, Windsurf, Cline/Roo,
Continue, Zed, JetBrains/Junie, VS Code Copilot Chat) — merge this object into
the host's MCP config, with an **absolute** path to `memvault-mcp`:

```json
{
  "mcpServers": {
    "memvault": {
      "command": "/absolute/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"],
      "env": { "MEMVAULT_AGENT_ID": "<your-agent-id>" }
    }
  }
}
```

Per-host target file paths are in
[integrations/mcp-clients/README.md](../integrations/mcp-clients/README.md).
Two traps: Claude Desktop does not expand `~`, so use an absolute DB path there;
and Continue's config is **YAML**, so re-key the snippet rather than pasting it.

Claude Code without the plugin: `claude mcp add memvault -- /absolute/path/to/memvault-mcp --db ~/.memvault/data.db`.

## 4. Confirm recall is live

```bash
memvault-cli save --content "Smoke test from the agent install" --type fact
memvault-cli search --query "smoke test"                                  # the row comes back
memvault-cli session-start --agent-id <your-agent-id> --context "hello"   # what a host receives on connect
```

`session-start` prints the context that host is injected with on connect; any
MUST-priority memories appear there as instructions. Then **restart the host
agent (or open a new session)** so the MCP registration / SessionStart hook
takes effect. If the host has no injection hook, call the `session_start` MCP
tool yourself at the start of work.

## 5. What to save from here on

Use the `save_memory` MCP tool (or `memvault-cli save`) for durable facts:
explicit user preferences, project conventions, decisions and their rationale,
environment specifics. After a task, call `record_outcome`
(or `memvault-cli outcome`) so failures are distilled into lessons that
auto-inject next time. Prefer one atomic fact per memory — near-duplicates are
skipped and similar memories merge automatically.

## 6. When a step fails

- `memvault-cli status` — which features are degraded and why (e.g. no embedding provider).
- `memvault-cli doctor` — read-only hygiene lint (`--json` for machine output).
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) — symptom → cause → fix.
- [INSTALL.md](INSTALL.md) — the full platform / client matrix.

Report which step failed and paste that command's output verbatim; do not retry
a failed checksum or a refused dataset unchanged.
