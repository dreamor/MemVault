# Installation Guide

This document covers every install path for **all MemVault components**.
Pick what you need:

| Component | Purpose | Recommended install |
|-----------|---------|---------------------|
| CLI + MCP Server | command-line tooling / MCP stdio server | One-line installer, Homebrew (Apple Silicon), `cargo install`, or Docker |
| Web Dashboard | browser management UI | Release static bundle, or build from source (requires Node.js ≥ 22.7) |
| Obsidian plugin | manage memories inside the notebook app | Community plugin directory (listed) or BRAT |

---

## 0. One-line install (recommended, no Rust toolchain needed)

Downloads the prebuilt binaries for your platform from GitHub Releases and
verifies SHA-256 automatically.

**Linux / macOS (ARM64)**:

```bash
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
export PATH="$HOME/.memvault/bin:$PATH"
```

**Windows (PowerShell)**:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
# installs to %LOCALAPPDATA%\memvault\bin by default
```

> `install.sh` / `install.ps1` pull assets from the latest Release; each archive
> ships a `.sha256` and the Release provides a summary `SHA256SUMS`, and the
> hash is verified at install time. On macOS, if Gatekeeper blocks the unsigned
> prebuilt binary, right-click → Open once (same behavior as any unsigned
> GitHub binary).
> **There are no prebuilt binaries for Intel Macs (macOS x86_64)** — fastembed's
> bundled ONNX Runtime has no artifacts for that platform. Use the source build
> in §1 instead; `install.sh` prints the same guidance when run on an Intel Mac.
> The PowerShell installer has been reviewed statically but has not yet been
> exercised on a real Windows machine.

---

## 1. Core: CLI + MCP Server

### 1.1 Prerequisites

| Dependency | Required | Version | Notes |
|------------|----------|---------|-------|
| **Rust toolchain** | required | 1.85+ stable (edition 2024) | `rustup install stable` |
| **C compiler** | required | C11 | Xcode CLT on macOS; `build-essential` on Debian/Ubuntu; MSVC on Windows |
| **pkg-config** | required | any | used on Linux to locate OpenSSL |
| **OpenSSL dev libraries** | recommended | 1.1+ / 3.x | Linux `libssl-dev`; macOS `brew install openssl`; Windows vcpkg |
| **SQLite** | not required | 3.x | `rusqlite` uses the `bundled` feature; compiles without system SQLite |

> **macOS (Apple Silicon)**: run `xcode-select --install` before the first build.
> **Linux distro quick reference**:
> - Debian / Ubuntu: `sudo apt install build-essential pkg-config libssl-dev`
> - Fedora / RHEL: `sudo dnf install gcc gcc-c++ pkgconfig openssl-devel`
> - Arch / Manjaro: `sudo pacman -S base-devel openssl pkgconf`
> - Alpine: `sudo apk add musl-dev pkgconfig openssl-dev` (may need MUSL-compat patches)

### 1.2 Build from source

```bash
git clone https://github.com/dreamor/memvault.git
cd memvault
cargo build --release
```

> Binary locations
> - `target/release/memvault-cli`
> - `target/release/memvault-mcp`

**Speed up the build** (reuse locally compiled artifacts):

```bash
# Use the mold linker (macOS, Linux)
cargo install mold --locked
RUSTFLAGS="-C link-arg=-fuse-ld=mold" cargo build --release

# Use sccache as a compile cache
cargo install sccache --locked
export RUSTC_WRAPPER=sccache
cargo build --release
```

**Cross-compiling**: no cross-compile guide is published yet.

### 1.3 Install onto PATH (from source or crates.io)

```bash
# From a source build (macOS / Linux)
install -m 0755 target/release/memvault-{cli,mcp} ~/.local/bin/

# Or install the published crates into ~/.cargo/bin/
cargo install memvault-cli --locked
cargo install memvault-mcp --locked
```

Make sure `~/.local/bin` or `~/.cargo/bin` is on `$PATH`.

### 1.4 Docker image

Pull the published image (built by CI on every release, mirrored to two
registries):

```bash
docker pull ghcr.io/dreamor/memvault:latest
docker run --rm -it -v memvault-data:/home/memvault/.memvault ghcr.io/dreamor/memvault:latest --help
```

Or build locally:

```bash
git clone https://github.com/dreamor/memvault.git
cd memvault
docker build -t memvault:local .
```

> The data volume `/home/memvault/.memvault` holds SQLite and the Agent
> Registry. Full configuration reference: [`docs/DOCKER.md`](DOCKER.md).

### 1.5 Homebrew (Apple Silicon)

```bash
brew install dreamor/tap/memvault
```

> Served from the `dreamor/homebrew-tap` tap; `scripts/update-homebrew-formula.sh`
> generates the formula from release assets. The formula is Apple-Silicon-only
> (`depends_on arch: :arm64`) — there are no prebuilt Intel macOS binaries.

### 1.6 Verify the install

```bash
memvault-cli --version    # prints memvault 0.3.0
memvault-mcp --version    # prints memvault-mcp 0.3.0
memvault-cli list         # lists saved memories (proves the DB works)
```

### 1.7 MCP Proxy (optional, advanced)

The third binary, `memvault-proxy`, sits between an agent and one or more
upstream MCP servers: it merges the upstream tool surface, transparently
injects memories, and tracks compliance.

```bash
memvault-proxy --config ~/.memvault/proxy.yaml          # stdio (default)
memvault-proxy --transport sse --port 3778              # client connects to http://127.0.0.1:3778/mcp
```

Upstream topology (which MCP server it forwards to) lives in
`~/.memvault/proxy.yaml` (`--config` to override). When both MCP and proxy
injection are active, set `inject_channel` (`mcp` / `proxy` / `sync`) in
`agents.yaml` so the same memory is never injected twice — see §2.6. Full flag
list: `memvault-proxy --help`.

---

## 2. MCP client integration

Once the CLI and MCP Server are installed, configure **one** of your MCP
clients from the sections below.

### 2.1 Claude Desktop

Edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS),
`%APPDATA%\Claude\claude_desktop_config.json` (Windows), or
`~/.config/Claude/claude_desktop_config.json` (Linux):

```json
{
  "mcpServers": {
    "memvault": {
      "command": "/absolute/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"]
    }
  }
}
```

> The path **must** be absolute — Claude Desktop does not expand `~`.

Restart Claude Desktop; under *Settings → Developer* the `memvault` server
should list 16 tools / 2 resources.

### 2.2 Claude Code

```bash
claude mcp add memvault -- /absolute/path/to/memvault-mcp --db ~/.memvault/data.db
```

Verify with `claude mcp list` — `memvault` should appear.

### 2.3 Cline / Continue / Cursor

Cline / Continue / Cursor all accept the standard `mcpServers` JSON — same
shape as §2.1 — in their respective config files.

### 2.4 SSE / HTTP remote MCP

Start the server with `--transport sse --port 3777` and point the client at:

```json
{
  "mcpServers": {
    "memvault": {
      "url": "http://127.0.0.1:3777/mcp"
    }
  }
}
```

### 2.5 DeepSeek Harness (dsh)

[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) (`dsh`) is
an open-source agent harness built on the **Cordis** plugin meta-framework
("everything is a plugin"). Both integration paths below were validated against
the real dsh source and a real runtime — pick one.

**Option A: zero code, as long as tools are callable**

dsh ships the MCP client plugin `@deepseek-ai/dsh-mcp-client`. **Each upstream
MCP server maps to one plugin instance** (not a `mcpServers` list like Claude
Desktop), and tools get registered as `mcp__<serverName>__<toolName>` (e.g.
`mcp__memvault__save_memory`). Add an entry to the dsh profile directory
(`$DSH_HOME/profiles/<name>/cordis.patch.yml`):

```yaml
- insert:
    - id: memvault-mcp
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        transport: stdio
        serverName: memvault
        command: /absolute/path/to/memvault-proxy   # or memvault-mcp
        args: []
```

Or connect to an already-running HTTP instance:

```yaml
- insert:
    - id: memvault-mcp
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        transport: streamable-http
        serverName: memvault
        url: http://127.0.0.1:3778/mcp
```

> The `insert:` wrapper is mandatory — a bare `- id: memvault-mcp ...` entry
> means "overwrite an existing entry" and errors out with
> `patch: entry "memvault-mcp" not found` for a not-yet-existing `id`.
> `transport` accepts only `stdio` / `streamable-http`; there is no `sse`
> value (easily confused with MemVault's own `--transport sse` — different
> naming layers).

**Option B: deep integration, want auto-injection + auto-extraction**

Option A only makes the tools visible — whether MUST memories get read and
whether `notify_response` fires each turn is still up to the agent. For MUST
memories to **automatically** appear in the system prompt and extraction to
fire **automatically** at turn end, use [`dsh-plugin/`](../dsh-plugin/README.md)
(`@dreamor/dsh-memvault`) — a real Cordis plugin that hooks
`ctx.systemPrompt.section()` and `session/event` listeners directly. The four
pitfalls discovered while building it (patch semantics, embedding-provider env
leaks, a startup race, a memoization bug after failed connections) and their
fixes are documented in [`dsh-plugin/README.md`](../dsh-plugin/README.md).

**Install into a dsh profile**: run `npm install && npm run build` inside
`dsh-plugin/`, then `npx @deepseek-ai/dsh plugin --profile <name> add "$PWD"` —
`dsh plugin add` writes the package into that profile's `dsh.profile.bundles`
with its default config (including `cordis.patch.yml`: `mode: spawn` +
`embeddingProvider: native`). To override fields locally (e.g. point
`binaryPath` at your own build), add a bare-id override patch (without
`insert`) in the profile's `cordis.patch.yml` and rewrite the whole `config`
object — overrides replace it wholesale, they are not merged field-by-field.
Full steps in [`dsh-plugin/README.md`](../dsh-plugin/README.md).

> **One pitfall both options hit**: when you spawn `memvault-proxy` /
> `memvault-mcp` via `command`/`binaryPath`, the child inherits dsh's
> `OPENAI_API_KEY` (likely set if dsh uses an OpenAI-compatible model; the old
> `OPENAI_API_BASE` fallback is no longer honored). MemVault would use that key
> as its fallback embedding credential and get 401s. Setting `env` (Option A)
> or `embeddingProvider` (Option B) to `native` avoids the inherited key and
> works offline. **This no longer needs a manual workaround**: when
> `MEMVAULT_EMBEDDING_PROVIDER` is unset, MemVault validates an inherited key
> with a test embedding call at startup and falls back to `native`
> automatically if the call fails. Setting `embeddingProvider` explicitly still
> skips one network round-trip and remains the clearer option.

### 2.6 REST API (required by the Obsidian plugin)

**The Obsidian plugin does not speak MCP** — it talks to the backend over the
HTTP REST API (`/api/*`). That imposes a transport requirement that is easy to
miss:

| Transport | Endpoints exposed | Usable by the Obsidian plugin? |
|-----------|-------------------|--------------------------------|
| `stdio` (default) | none (no HTTP) | ❌ |
| `sse` | `/mcp` only (MCP-over-HTTP) | ❌ |
| `http` / `rest` | full REST routes (`/api/*`) | ✅ |

Start it with:

```bash
memvault-mcp --db ~/.memvault/data.db --transport http --port 8080
```

The Server URL in Obsidian settings defaults to `http://127.0.0.1:8080`,
matching the command above.

**Key endpoints** (full list in the root README "MCP Server" section):

- `GET /api/memories`, `POST /api/memories` (create): generates an int8 vector
  when an embedder is available; the response contains `"embedded":bool`.
  `PUT /api/memories/{id}` (general partial update — content / priority /
  tags / namespace / layer / skill_trigger / …), `DELETE /api/memories/{id}`
- `POST /api/search`: `mode` = `keyword` (default) / `semantic` / `hybrid`;
  per-hit `search_mode` and `hit_sources` (e.g. `["kw#1","vec#1"]`, matching
  the MCP `search_memory` tool)
- `POST /api/extract`: returns `{ memories, coverage }` where `coverage`
  buckets `input_lines` / `empty_lines` / `extracted_lines` /
  `no_signal_lines` (mutually exclusive, summing to the input line count)
- `GET/POST /api/inbox/*` (review queue)
- `POST /api/dedup`, `POST /api/decay`, `POST /api/promote`
- `GET /api/compliance/session|summary`

**Admin authentication (optional)**: besides per-`agent_id` auth for
`save_memory` / `search` / `session_start`, the management endpoints
(list / delete / edit / review queue / dedup / decay / promote / compliance)
authenticate as a single "admin" agent via headers:

```
X-MemVault-Agent-Id: admin      # optional, defaults to "admin"
X-MemVault-Api-Key: <your-key>
```

If `agents.yaml` has no `api_key` for `admin`, these endpoints stay
unauthenticated (backwards compatible). To enable auth, add:

```yaml
agents:
  - id: admin
    agent_type: general-assistant
    description: "Dashboard / Obsidian management operations"
    api_key: "your-secret-key"
```

The API Key field in Obsidian settings is sent as `X-MemVault-Api-Key`.

**Injection-channel dedup (optional)**: an agent may receive memories through
several channels — the MCP `session_start` tool, transparent injection by
`memvault-proxy`, and the instruction files written by `sync` — causing
duplicates. Set `inject_channel` (`mcp` / `proxy` / `sync`) in `agents.yaml`
to designate the canonical channel for an agent; automatic injection through
the other channels is skipped:

```yaml
agents:
  - id: claude-code
    agent_type: coding-assistant
    inject_channel: proxy   # only proxy transparent-injects for this agent
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
```

Omitting `inject_channel` leaves all channels unrestricted (default, fully
backwards compatible).

---

## 3. Web Dashboard (optional)

### 3.1 Prerequisites

| Dependency | Version | Notes |
|------------|---------|-------|
| Node.js | ≥ 22.7 | frontend build (vitest 4 requirement) |

> The backend is still `memvault-mcp`, built with Rust per §1. The Web
> Dashboard is a purely static frontend — no desktop shell, no packaging /
> signing / notarization per platform.

### 3.2 Development mode

Start the REST backend first:

```bash
cargo build --release -p memvault-mcp
./target/release/memvault-mcp --db ~/.memvault/data.db --transport http --port 3777
```

Then the frontend dev server (it proxies `/api`, `/health`, `/metrics` to
`127.0.0.1:3777`):

```bash
cd dashboard
npm install
npm run dev             # open http://localhost:1420
```

### 3.3 Production: let the backend host the frontend

Build the static bundle and serve it from the REST port via `--serve-web`
(same origin, no CORS):

```bash
cd dashboard && npm ci && npm run build     # output: dashboard/dist/
./target/release/memvault-mcp --db ~/.memvault/data.db \
  --transport http --port 3777 --serve-web ./dashboard/dist
```

Open `http://127.0.0.1:3777`. The GitHub Release asset
`memvault-dashboard-<version>.tar.gz` is exactly the packaged `dist/` — unpack
it and pass the directory to `--serve-web`.

---

## 4. Obsidian plugin

> **Prerequisite**: the plugin talks REST, so start
> `memvault-mcp --transport http` first (see §2.6).

### 4.1 Install from the community directory

The plugin is listed: in Obsidian's community-plugin browser, search for
`MemVault` and install it.

### 4.2 Install via BRAT (fastest tracking of new releases)

1. Install `BRAT` from the community plugins
2. BRAT Settings → Add Beta Plugin → enter the repo URL and version
3. Enable the `MemVault` plugin

### 4.3 Install from source

```bash
cd obsidian-plugin
npm install
npm run build
mkdir -p <your-vault>/.obsidian/plugins/memvault
cp main.js manifest.json styles.css <your-vault>/.obsidian/plugins/memvault/
```

### 4.4 Verify

Obsidian Settings → Community plugins → enable `MemVault` → a sidebar icon
appears.

### 4.5 Common commands

- **Command palette** (⌘/Ctrl+P, type `MemVault:`): Open memory panel, Search
  memories, Search and insert memory, Save selection as memory, Save selection
  as MUST rule, Extract memories from selection, Mark memory as read, Sync
  memories to vault
- **Sidebar panel** (Memories): browse the memory list, supports delete
- **Management operations** (review inbox / approve / reject / supersede /
  edit / stats / export / import / backup / checkpoints / dedup / decay /
  promote) live in the Web Dashboard and CLI — the Obsidian plugin does not
  duplicate them

---

## 5. Upgrade & uninstall

### 5.1 Upgrade

```bash
# From source
cd memvault && git pull && cargo build --release
# From crates.io
cargo install memvault-cli --locked --force
cargo install memvault-mcp --locked --force
# Docker
docker pull ghcr.io/dreamor/memvault:latest
# Homebrew
brew upgrade dreamor/tap/memvault
```

Run `memvault-cli backup` before upgrading; afterwards `memvault-cli list`
should read your data normally.

### 5.2 Uninstall

```bash
# Binary installs
cargo uninstall memvault-cli memvault-mcp
rm -rf ~/.memvault        # data
rm ~/.local/bin/memvault-{cli,mcp}

# Docker
docker rm -f memvault
docker volume rm memvault-data
```

Uninstall the Obsidian plugin from Obsidian's plugin panel.

---

## 6. Layered memories: L0–L3, promote, skills, proxy extraction

### 6.1 Layered memories (MemoryLayer)

Memories carry four layers:

| Layer | Meaning | Auto-assigned to |
|-------|---------|------------------|
| L3 | core persona | MUST-priority memories |
| L2 | scenario summaries | REFERENCE memories |
| L1 | atomic facts | BACKGROUND memories / extraction output |
| L0 | raw archive | source memories after promote |

```bash
# Save with an explicit layer
memvault-cli save --content "User prefers Python" --priority MUST --layer L3

# list shows the layer
memvault-cli list
# [Must|L3] mem_xxx — User prefers Python
```

### 6.2 Promote pipeline

Condenses low-layer memories upward:

```bash
# Run promote (L1→L2, L2→L3)
memvault-cli promote

# Custom thresholds (default: 3 L1 memories merge into L2, 2 L2 promote to L3)
memvault-cli promote --min-l1 5 --min-l2 3
```

### 6.3 Structured skills

Skill-type memories carry trigger/steps/verification:

```bash
memvault-cli save --content "Deployment procedure" --type skill \
  --skill-trigger "deploy,release,ship" \
  --skill-steps "build,test,push,verify" \
  --skill-verification "health check passes"
```

MCP tool call:

```json
{
  "tool": "save_memory",
  "arguments": {
    "content": "Deployment procedure",
    "type": "skill",
    "skill_trigger": "deploy",
    "skill_steps": ["build", "test", "push"],
    "skill_verification": "health check passes"
  }
}
```

### 6.4 Proxy extraction loop

The MCP Proxy exposes `notify_response`; call it after each agent turn to
auto-extract memories into the review inbox:

```json
{
  "tool": "notify_response",
  "arguments": {
    "response_text": "Noted, you prefer the FastAPI framework",
    "agent_id": "claude-code"
  }
}
```

Extraction policy:

- whitelist: only `preference` / `fact` / `skill` types are extracted
- confidence threshold: ≥ 0.6
- per-call cap: 5 memories
- saved with `human_reviewed=false` (awaiting inbox review)

### 6.5 Tiered injection

`session_start` uses a tiered injection strategy:

- MUST memories: injected verbatim (unchanged)
- REFERENCE memories: full text within the token budget; beyond it, summaries
- closing hint: "N more related memories are available via search_memory"

---

## 7. Troubleshooting

See [`docs/TROUBLESHOOTING.md`](TROUBLESHOOTING.md). Quick coverage:

| Symptom | Section |
|---------|---------|
| `failed to bind` | §1.1 port / permissions |
| `OPENAI_API_KEY invalid` | §2 embedding |
| MCP server unreachable, but the binary runs | §2.1 stdio config path |

---

## 8. Agent plugin integrations (first batch)

Beyond the generic MCP configs above, one-command native plugins exist for:

- **Claude Code** (recommended, full-featured): run
  `/plugin marketplace add dreamor/memvault`, then
  `/plugin install memvault@memvault` (two separate commands). A SessionStart
  hook auto-injects memories; `MEMVAULT_HOOK_EXTRACT=1` turns on
  end-of-session auto-extraction (drafts go to the Review Inbox) — this does
  **not** extract on every Stop: extraction runs only when the session
  accumulated friction signals (repeated tool errors/retries, rejected tool
  calls, correction/interruption wording) up to
  `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` (default `1`); long friction-free
  sessions are quietly skipped. Set it to `0` to disable the gate and extract
  every time. The plugin also ships skills (recall / save / review / sync) and
  the `/memvault-review`, `/memvault-sync`, `/memvault-doctor` commands.
  Environment variables: `MEMVAULT_AGENT_ID` (default `claude-code`),
  `MEMVAULT_HTTP_URL` (default `http://127.0.0.1:3777`), `MEMVAULT_BIN`
  (explicit path to `~/.memvault/bin/memvault-cli` when it is not on PATH).
- **OpenCode**: merge the `integrations/opencode/opencode.json` template into
  your project `opencode.json` (`plugin` points at the absolute path of
  `integrations/opencode/plugins/memvault.mjs`).
- **Codex**: `codex plugin marketplace add dreamor/memvault`, then install
  `memvault@memvault` from the plugin browser (or enable it in the project
  `.codex/config.toml`). This installs the same plugin Claude Code uses.
  Details plus a manual pre-plugin fallback: `integrations/codex/README.md`.
- **Gemini CLI**: `gemini extensions install https://github.com/dreamor/memvault`.
- **Cursor / Windsurf / Cline / Continue / Zed / JetBrains / VS Code / Claude
  Desktop**: paste the matching snippet from `integrations/mcp-clients/`. Each
  engine distinguishes identity via `MEMVAULT_AGENT_ID` while sharing the same
  memory store.
