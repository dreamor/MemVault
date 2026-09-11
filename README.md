<div align="center">

<img src="assets/memvault-logo.png" alt="MemVault — a round hamster mascot hugging its memory nut, honey gold and brown on cream" width="360" />

# MemVault

### The Shared Memory Layer for Every AI Agent You Run

> Not "agent learns to search memory" — memory finds the agent.

**MCP Native &nbsp;·&nbsp; Hybrid Retrieval &nbsp;·&nbsp; Auto-Injection &nbsp;·&nbsp; Zero-Config Sync**

**Open Source &nbsp;·&nbsp; Self-Hosted &nbsp;·&nbsp; Private &nbsp;·&nbsp; MIT Licensed**

[![Crates.io](https://img.shields.io/crates/v/memvault-cli.svg?style=flat-square)](https://crates.io/crates/memvault-cli) [![GitHub Release](https://img.shields.io/github/v/release/dreamor/memvault?style=flat-square)](https://github.com/dreamor/memvault/releases) [![CI](https://img.shields.io/github/actions/workflow/status/dreamor/memvault/ci.yml?style=flat-square&branch=master)](https://github.com/dreamor/memvault/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE) [![Rust](https://img.shields.io/badge/Rust-stable-orange.svg?style=flat-square)](https://www.rust-lang.org) [![MCP](https://img.shields.io/badge/MCP-compatible-blue.svg?style=flat-square)](https://modelcontextprotocol.io) [![Status](https://img.shields.io/badge/status-beta-yellow.svg?style=flat-square)](#project-status)

**English** &nbsp;·&nbsp; [简体中文](README.zh-CN.md)

<div align="center">

If MemVault solves a real problem for you, a star helps others find it.

**[⭐ Star on GitHub](https://github.com/dreamor/memvault)** &nbsp;·&nbsp; **[Report Bug](https://github.com/dreamor/memvault/issues/new)**

</div>

```bash
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
```

</div>

---

Every AI agent session starts from scratch. Claude Desktop doesn't know what Cursor just learned. DeepSeek Harness (dsh) doesn't know what you told Claude Code yesterday. Whatever agent you're running — international or domestic, IDE plugin or CLI harness — it forgets your preferences every time you start a new conversation.

You've been manually repeating context — project conventions, personal preferences, past decisions — across agents that should already know. This isn't a limitation of the models. It's a missing infrastructure layer.

MemVault is that layer. A lightweight, self-hosted memory router that sits between your agents and their context. It speaks plain MCP — no MemVault-specific SDK, no per-agent API integration. **Any MCP-compatible agent, from any vendor, automatically shares the same persistent memory the moment it connects.**

**Who it's for:**

- **Anyone running an MCP-capable agent** — Claude Code, Claude Desktop, Cursor, Cline, Continue, DeepSeek Harness (dsh), or any other MCP client, domestic or international — who wants preferences, project context, and past decisions to persist across sessions without repeating yourself
- **Multi-agent power users** running several of the above side by side, on different models, from different vendors — all sharing the same memory without configuration
- **Platform teams** deploying AI-assisted workflows where consistency matters across a mixed agent fleet: code review conventions, architecture decisions, project-specific preferences
- **Anyone tired of telling their AI the same thing twice** — MemVault works the way your brain should: you say it once, it's there when you need it, no matter which agent is asking

**[Quick Start](#quick-start)** &nbsp;·&nbsp; **[How It Works](#how-it-works)** &nbsp;·&nbsp; **[What MemVault Gives You](#what-memvault-gives-you)** &nbsp;·&nbsp; **[Why MemVault](#why-memvault)** &nbsp;·&nbsp; **[MCP Server](#mcp-server)** &nbsp;·&nbsp; **[CLI Reference](#cli-reference)** &nbsp;·&nbsp; **[Integrations](#integrations)** &nbsp;·&nbsp; **[Architecture](#architecture)** &nbsp;·&nbsp; **[Project Status](#project-status)** &nbsp;·&nbsp; **[Testing](#testing)** &nbsp;·&nbsp; **[Documentation](#documentation)** &nbsp;·&nbsp; **[Contributing](#contributing)** &nbsp;·&nbsp; **[License](#license)**

---

## Quick Start

```bash
# Install (Linux / macOS): official script, auto-verifies SHA-256
curl -fsSL https://raw.githubusercontent.com/dreamor/memvault/master/scripts/install.sh | bash
export PATH="$HOME/.memvault/bin:$PATH"

# Windows (PowerShell):
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1

# Homebrew (Apple Silicon):
#   brew install dreamor/tap/memvault

# Or from crates.io:
#   cargo install memvault-cli memvault-mcp

# Save a MUST-level preference (injected as instruction, agent must follow)
memvault save --content "User prefers Python" --priority MUST --type preference \
  --instruction "Use Python for code, not Java" --tags "coding,python"

# Search across all memory (hybrid: keyword + semantic when embedding is enabled)
memvault search --query "Python"

# See what context gets injected when a specific agent connects
memvault session-start --agent-id claude-desktop --context "Help me write code"

# Extract structured memories from free text
memvault extract --text "I prefer dark mode. Our project uses Rust." --save

# Auto-generate agent instruction files from memory
memvault sync

# All in one: dedup, decay, archive stale memories
memvault dedup && memvault decay
```

**Verify your install in 5 seconds:**

```bash
memvault-cli --version
# memvault 0.3.0

# sanity check: list saved memories (verifies DB is healthy)
memvault-cli list
```

### Local Ollama Demo (Zero Cost, Stays on Your Machine)

MemVault is plug-and-play with a local Ollama: unconfigured LLM extraction (full-text understanding / failure reflection / relation extraction) auto-detects a local Ollama; embeddings use the local model with the `ollama` or `auto` provider.

```bash
# 1. Install and start Ollama
brew install ollama && brew services start ollama    # or the official installer

# 2. Pull models
ollama pull nomic-embed-text        # embeddings, 768-dim (default for ollama/auto)
ollama pull qwen2.5:3b-instruct     # chat: LLM extraction/reflection (default qwen2.5:7b, use 3b on small machines)

# 3. (Optional) Pin the providers explicitly — persist them in ~/.memvault/.env
#    (shell exports also work — env vars take precedence over the file — but the file survives reboots)
mkdir -p ~/.memvault
cat >> ~/.memvault/.env <<'EOF'
MEMVAULT_EMBEDDING_PROVIDER=ollama
MEMVAULT_LLM_EXTRACTION_PROVIDER=ollama
MEMVAULT_LLM_EXTRACTION_MODEL=qwen2.5:3b-instruct
EOF

# 4. Verify
memvault status     # Embedding provider: configured and reachable
memvault save --content "Build server IP is 10.20.30.40"   # output (embedded int8)
memvault outcome --task "Deploy trading service" --status failure --cause "Disk space insufficient" --task-type deploy
#   → Lesson (Llm): ... means failure reflection ran through the local LLM (not the rule-based fallback)
```

With no configuration at all (neither env vars nor `~/.memvault/.env`), LLM extraction auto-detects a local Ollama and enables itself (default model `qwen2.5:7b`; pull it in advance with `ollama pull qwen2.5:7b`, or point `MEMVAULT_LLM_EXTRACTION_MODEL` at an installed model). Embeddings still default to the in-process native embedder; set `MEMVAULT_EMBEDDING_PROVIDER=auto` in `~/.memvault/.env` to prefer Ollama and fall back to native when it isn't running.

---

## How It Works

MemVault is a pipeline, not a single script. Every stage below is a shipping module:

```
Agent connects (MCP stdio/SSE)
        │
        ▼
┌───────────────────┐
│  Agent Router      │  ← match agent type/tag → filter relevant memory
│  (Agent Registry)  │
└────────┬──────────┘
         │
         ▼
┌───────────────────┐
│  Memory Retrieval  │  ← keyword (BM25) + vector (embedding) + RRF fusion
│  (3 search modes)  │     synonym expansion · scoring · soft filtering
└────────┬──────────┘
         │
         ▼
┌───────────────────┐
│  Auto-Injection    │  ← MUST-level → instruction prompt
│                    │     REFERENCE → context resource
│                    │     NORMAL    → search result
└────────┬──────────┘
         │
         ▼
  Agent receives context ──→ makes better decisions
```

- **Storage:** SQLite with bundled FTS5 (full-text search); embeddings stored int8-quantized (~1/4 the size of f32 at near-identical ranking quality, legacy f32 rows still readable)
- **Retrieval:** BM25 keyword search over FTS5 with CJK bigram tokenization (Chinese two-character words match correctly) and tiered match fallback (strict → relaxed unigram → synonym OR; relaxations are reported, never silent), local-first embedding (in-process native by default — switch to local Ollama or any OpenAI-compatible model), RRF fusion with per-result recall provenance (`kw#2`/`vec#5`), synonym expansion, relevance scoring, soft intent filtering
- **Pipeline:** Automatic entity extraction, delta-write on save (near-duplicates skipped, similar memories absorb only the residual), semantic deduplication, time-based decay, archive of stale memories
- **Sync:** Zero-invasion file generation — `memvault sync` produces CLAUDE.md, AGENTS.md, etc. directly from database contents

---

## What MemVault Gives You

- **Auto-Injected Context:** Session start automatically pulls relevant memory by agent identity — MUST-level rules land as instructions, not just chat history
- **Hybrid Retrieval:** BM25 + vector + RRF fusion with synonym expansion, relevance scoring, and per-result provenance (which path recalled each memory, at what rank) — available via CLI, MCP tool, and REST API
- **Explainable Injection:** every candidate dropped on the way into an agent's context is recorded with a reason (budget, caps, intent/type penalties) — "why didn't the agent get this memory?" always has an answer
- **MUST Enforcement:** MUST-priority memories are never filtered or truncated. Always in context, always obeyed — trust comes from provenance (human-authored/reviewed), with an opt-in fallback for memories independently corroborated by multiple identity-verified agents (`MEMVAULT_CORROBORATION_GATE`), so a single spoofed/compromised agent can't unilaterally inject a binding MUST
- **Multi-Agent Awareness:** Agent Registry with type/tag-based soft filtering (score demotion, not hard exclusion)
- **MCP Proxy:** Transparent proxy that injects memory into ANY upstream MCP server's responses — zero client changes
- **Compliance Tracking:** `inject_session_id` traces what was injected and measures follow-through rate
- **Cross-Platform:** CLI + MCP Server (stdio & SSE) + Web Dashboard (browser) + Obsidian Plugin
- **Zero-Invasion Sync:** Generate AGENTS.md / CLAUDE.md from memory — no per-agent config files to edit
- **Contextual Extraction, Local-First:** Rule-based keyword extraction by default; optionally understands a full user+assistant exchange via an LLM, auto-detecting a local Ollama for free before ever touching a remote API
- **History & Rollback:** Every update/delete is snapshotted into `memory_history` — `memvault checkpoints` + `memvault restore` roll one memory back without touching the rest
- **Self-Diagnostics:** `memvault status` reports exactly which features are degraded when no embedding provider is configured, plus a schema fingerprint (migration version + checksum) for cross-database comparison
- **Episodic Memory:** `record_outcome` records task results; failures are distilled into lessons and auto-injected next time (REFERENCE → MUST only with human approval), so the same trap isn't hit twice
- **Procedural Skill Activation:** skills whose `trigger` matches intent are injected as structured `[SKILL]` blocks with success-rate stats (shown after ≥3 runs); failures flag the skill for revision (`version++` on human edit), repeated successes auto-draft new skills into the review inbox
- **Semantic Knowledge Links:** lightweight relation triples, repeated facts consolidated into a linked semantic fact with provenance, and superseded facts archived (never re-injected, still listable & restorable)
- **Team Shared Pool & SOP Import:** memories marked `shared` are injected into every session (capped at 20); Markdown SOPs can be batch-imported as verifiable skills
- **Delta-Write on Save:** every save is checked against its namespace first — near-duplicates are skipped, similar memories absorb only the *residual* (what's genuinely new) and get their strength refreshed, so the library converges instead of accumulating near-copies; `--force` / `force_insert` bypasses
- **Task-Level Evaluation:** `memvault bench` samples your own failure history (episodes that distilled a lesson) and measures lesson retrieval/injection rates — and with `--judge`, an LLM scores "plan without vs. with memory" against the known failure cause, so you see *task-success* lift, not just retrieval recall
- **Two-Phase Injection (never blocks):** MUST rules resolve deterministically with zero embedding calls and are served immediately; the semantic pipeline prefetches in the background and lands within a short window (250ms) — if it doesn't, the deterministic baseline is served and the request moves on (two-phase design)
- **Conversation-N-Gram Retrieval:** retrieval keys are conditioned on the recent turn window, weighted by recency so the current focus dominates — not a single flat query
- **Single Canonical Injection Channel:** per-agent `inject_channel` (`mcp` / `proxy` / `sync` in `agents.yaml`) restricts automatic injection to one delivery path, so the same memory is never sent to the same agent twice
- **Data You Own:** Single SQLite file. Full export/import. No cloud dependency. Your data, your machine.

---

## Why MemVault

| Feature | Plain CLAUDE.md | Vector DB + RAG | **MemVault** |
|---|---|---|---|
| **Context injection** | Manual edits | Query-time only | Auto on session start |
| **Multi-agent sharing** | Copy-paste | Separate indexes | Single shared store |
| **MUST enforcement** | None | None | Instruction-layer injection |
| **Search modes** | File grep | Embedding only | BM25 + Vector + Hybrid |
| **Synonym expansion** | No | No | Built-in |
| **Deduplication** | No | No | Semantic dedup pipeline |
| **Write-time delta merge** | No | No | Near-dupes skip, similar absorb the residual at save |
| **Task-level evaluation** | None | Recall metrics only | `bench`: with-vs-without-memory task success delta |
| **Decay / archival** | No | No | Time-based + auto archive |
| **Memory extraction** | Manual | N/A | Rule-based by default; optional local-first LLM extraction |
| **MCP native** | No | No | stdio + SSE + Proxy |
| **Agent differentiation** | Global file | Query filter | Type/tag registry |
| **Compliance tracking** | None | None | inject_session_id + rate |
| **Self-hosted** | Yes | Varies | Single binary, no cloud |

MemVault complements your existing agent setup rather than replacing it. Keep your LLM, your IDE, and your workflow exactly as they are. MemVault adds the memory layer underneath.

---

## MCP Server

> Tier-1 agents (Claude Code, OpenCode, dsh, Gemini CLI, Codex) have one-command plugin installs — see [Installing into your agents](#installing-into-your-agents) first. Everything below is the universal fallback for any other MCP client.

### stdio (any standard MCP client)

MemVault speaks plain MCP stdio — the same `mcpServers` JSON works verbatim in Claude Desktop, Cursor, Cline, Continue, and any other client that reads this format:

```json
{
  "mcpServers": {
    "memvault": {
      "command": "/path/to/memvault-mcp",
      "args": ["--db", "~/.memvault/data.db"],
      "env": { "OPENAI_API_KEY": "sk-..." }
    }
  }
}
```

A couple of clients use their own one-liner instead of hand-editing JSON:

```bash
# Claude Code
claude mcp add memvault /path/to/memvault-mcp -- --db ~/.memvault/data.db
```

**DeepSeek Harness (dsh)** — a domestic (China) agent harness — gets deeper treatment than a generic stdio config: a native Cordis plugin (`dsh-plugin/`) that auto-injects memory into the system prompt and auto-extracts at turn end, with no per-turn cooperation required from the agent. See [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh) for both the zero-code MCP route and the deep-integration plugin.

Other MCP-compatible agents — international or domestic, IDE plugin or CLI harness — should work the same way: any client implementing standard MCP stdio/SSE can connect without MemVault-side changes. The ones above are the ones we've actually verified; if you get MemVault working with another one, a PR to this list is welcome.

### SSE (multi-client, network-accessible)

```bash
memvault-mcp --transport sse --port 3777
# Clients connect at http://127.0.0.1:3777/mcp
```

SSE features: multi-client simultaneous connections, auto-triggered embedding backfill on initialization, HTTP remote access.

> **Note:** `--transport sse` only mounts the MCP-over-HTTP endpoint (`/mcp`) — it does **not** expose the REST API (`/api/*`). The Web Dashboard is served by the REST backend (`memvault-mcp --transport http --serve-web <dist>`), and the Obsidian plugin also uses the REST API and requires `--transport http` instead. See [docs/INSTALL.md §2.6](docs/INSTALL.md#26-rest-apiobsidian-客户端专用).

### 18 MCP Tools

| Tool | Description |
|------|-------------|
| `save_memory` | Save with auto-embedding; delta-write by default (near-dupes skip, similar merge) — `force_insert` to bypass |
| `record_outcome` | Record a task outcome (episodic memory); failures reflect into lessons |
| `import_skills` | Import skills from a Markdown SOP (headings → skills, list items → steps) |
| `search_memory` | Keyword / semantic / hybrid |
| `session_start` | Agent-aware context injection; honors the agent's `inject_channel` (skips with an explanation when another channel is canonical) |
| `review_memory` | Approve / reject / edit |
| `delete_memory` | Remove a memory |
| `extract_memories` | Structured extraction from text |
| `run_dedup` | Deduplication scan |
| `run_decay` | Decay + auto-archive |
| `confirm_read` | Mark read (updates access_count) |
| `list_inbox` | List memories pending human review |
| `run_promote` | Promote pipeline (L1→L2→L3), archive sources to L0 |
| `report_compliance` | Report follow/violate status for an injected session |
| `get_compliance_report` | Compliance rates per session or aggregate |
| `add_evidence` | Record evidence relations: supports / contradicts / sourced_from |
| `get_memory_evidence` | Get the raw-evidence chain a memory was distilled from (its L0 trace rows) plus its evidence profile — read-only grounding, so an agent can quote the original session text and name its sources |
| `get_effectiveness_report` | Automatic effectiveness judgments for injected memories (useful/neutral/harmful/insufficient-context rates, judged from `record_outcome`) — independent of the manual `report_compliance` flow |

### 2 MCP Resources

| URI | Content |
|-----|---------|
| `memory://user-profile` | MUST-level rules, auto-loaded on connect |
| `memory://project-context` | REFERENCE-level project context |

### Configuration (.env file & environment variables)

Copy [.env.example](.env.example) to `~/.memvault/.env` and uncomment what you need — it is also the single source of truth documenting every key.

Precedence (high → low): **CLI flags > process environment > `~/.memvault/.env` > built-in defaults**. Every binary loads the env file first thing at startup; `--env-file <path>` or `MEMVAULT_ENV_FILE` points elsewhere, and a missing file is silently skipped. `memvault status` prints where each setting came from (env / file / default).

Two groups are intentionally not in the table below: the host installation contract (`MEMVAULT_AGENT_ID`, `MEMVAULT_HOOK_EXTRACT`, … — per-agent values authored in each host's plugin/mcpServers config), and the proxy's `upstreams` topology (structured data, lives in `~/.memvault/proxy.yaml`).

| Variable | Purpose | Default |
|----------|---------|---------|
| `MEMVAULT_EMBEDDING_PROVIDER` | Provider: `native` (in-process, default), `auto` (Ollama-first, native fallback), `ollama`/`local`, `openai`, or `openai-compatible` (any OpenAI-compatible endpoint) | `native` |
| `MEMVAULT_EMBEDDING_API_KEY` (legacy fallback: `OPENAI_API_KEY`) | API key for remote providers (not needed for local Ollama); the default `native` provider needs no key | (not needed — `native` local model) |
| `MEMVAULT_EMBEDDING_API_BASE` | Any OpenAI-compatible base URL (OpenAI / Azure / vLLM / gateway...). For `ollama`/`local` the embedder uses Ollama's native endpoint `http://localhost:11434/api` | `https://api.openai.com/v1` / `http://localhost:11434/api` (Ollama) |
| `MEMVAULT_EMBEDDING_MODEL` | Embedding model: `bge-small-zh` (zh, ~95MB) / `multilingual`/`e5-base` for native; `nomic-embed-text` (768-dim) for Ollama; or any model name for API providers | `bge-small-zh` (native) / `nomic-embed-text` (Ollama) / `text-embedding-3-small` (API) |
| `MEMVAULT_EMBEDDING_DIM` | Vector dimensions | `768` (local/Ollama) / `1536` (API) |
| `MEMVAULT_LLM_EXTRACTION_PROVIDER` | Optional: enables LLM-based *contextual* memory extraction (understands a full user+assistant exchange, not just keyword lines). Unset/`auto` → **local-first**: auto-detects a running local Ollama and uses it for free, no config needed; falls back to rule-based if none is running. `openai`/`openai-compatible`/custom → explicit remote provider (never auto-enabled just because an API key exists elsewhere — remote calls cost money and carry hallucination risk). `off`/`disabled`/`none` → force pure rule-based, even if local Ollama is running | (unset — local-first, rule-based if no local Ollama) |
| `MEMVAULT_LLM_EXTRACTION_API_KEY` (falls back to `OPENAI_API_KEY`) / `MEMVAULT_LLM_EXTRACTION_API_BASE` / `MEMVAULT_LLM_EXTRACTION_MODEL` | Chat-completions endpoint config for LLM extraction | local: `http://localhost:11434/v1` / `qwen2.5:7b` (no key) — remote: `https://api.openai.com/v1` / `gpt-4o-mini` |
| `MEMVAULT_RELATIONS` | Opt-in LLM relation extraction: `true` makes `extract_memories` (mode=llm) also persist `supports`/`contradicts`/`sourced_from` triples | (unset / false) |
| `MEMVAULT_DELTA_WRITE` | Delta-write on save: dedup within the same namespace first — near-duplicates skipped, similar memories absorb the residual. `false` turns it off; per-save bypass via `--force` / `force_insert` | true |
| `MEMVAULT_CONTEXT_NGRAM_WINDOW` | How many recent observed turns build the recency-weighted retrieval key used by proxy auto-injection | `5` |
| `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` | Minimum friction score (tool retries, rejected tool calls, mid-session corrections) a Stop-hook-triggered `extract` must reach before it saves anything. `0` disables the gate — extracts on every Stop, as before this existed | `1` |
| `MEMVAULT_IDENTITY_VERIFICATION` | Record whether a `save_memory` call's `agent_id` actually had a registered `agents.yaml` API key checked (`Memory.identity_verified`), vs. running unauthenticated. `false` stops recording it; recording alone never changes trust decisions | true |
| `MEMVAULT_CORROBORATION_GATE` | Opt-in MUST trust path: a MUST memory independently corroborated by enough distinct identity-verified agents (see `MEMVAULT_CORROBORATION_MIN_AGENTS`) is treated as trusted even without human review. `true` turns it on — off by default, so `is_trusted` output is unchanged unless you opt in | false |
| `MEMVAULT_CORROBORATION_MIN_AGENTS` | Minimum distinct identity-verified agents required for the corroboration gate above | `2` |
| `MEMVAULT_DB_POOL_SIZE` | SQLite connection pool size | `5` |
| `MEMVAULT_CORS_ORIGIN` | Comma-separated allowed CORS origins for REST (unset = localhost only) | (localhost only) |
| `MEMVAULT_DB` | SQLite database path | `~/.memvault/data.db` |
| `RUST_LOG` | Log verbosity | `info` |
| `MEMVAULT_HOME` | Base directory: where `.env` lives, plus the model cache (`~/.memvault/models`) and `agents.yaml`. Environment-only — it can't be set inside the `.env` file itself (a file can't define its own location) | `~/.memvault` |
| `HF_ENDPOINT` | HuggingFace endpoint override for native model downloads (e.g. `https://hf-mirror.com` on CN networks) | (HuggingFace default) |
| `MEMVAULT_EXTRACT_ASSISTANT` | Proxy-path contextual extraction of agent-produced text: on by default but saved downgraded (`review:required`, lowered confidence — nothing agent-produced is trusted before human review). `off`/`disabled`/`false`/`0` disables extracting from agent responses entirely | (on — downgraded) |

---

## CLI Reference

`save` · `outcome` · `search` · `list` · `review` · `delete` · `session-start` · `resource` · `extract` · `dedup` · `decay` · `doctor` · `promote` · `backup` · `export` · `import` · `import-skills` · `import-agent` · `ingest` · `confirm-read` · `sync` · `checkpoints` · `restore` · `supersede` · `status` · `bench` · `eval-history`

```bash
memvault <command> --help   # detailed usage per command
```

### Key Commands

| Command | What It Does |
|---------|--------------|
| `save` | Save a memory with priority, type, optional instruction. Delta-write by default: near-duplicates are skipped, similar memories absorb the residual; `--force` to bypass |
| `outcome` | Record a task result (success/failure/partial); failures are distilled into lessons that auto-inject into similar future tasks |
| `search` | Hybrid retrieval with relevance scoring; flags: `--query`, `--top-k`, `--namespace` |
| `session-start` | Simulate what context an agent receives on connect; a multi-line `--context` is treated as a turn sequence and weighted by recency |
| `extract` | Parse free text, extract structured memories |
| `import-skills` | Import skills from a Markdown SOP (`# / ##` headings → skills, list items → steps); enters the review inbox unless `--approve` |
| `import-agent` | Cold-start import from another agent's native memory files: Claude Code/Desktop (`CLAUDE.md`/auto-memory), Codex CLI (`AGENTS.md`), Hermes Agent (`USER.md`/`MEMORY.md`/skills), Qoder (`.qoder/rules`), OpenClaw (experimental); `--scan` to detect-only, `--path` to override, `--paste`/stdin as a generic fallback for any other agent, enters the review inbox unless `--approve` |
| `ingest` | Incrementally ingest agent session transcripts (Claude Code / Codex / Hermes) into memory: turns with extractable signal are kept as L0 evidence rows, and their extracted candidates carry `source_trace_ids` back to that evidence; a per-session watermark means each turn is processed once. `--dry-run` to preview, `--approve` to skip the review inbox, `--agent`/`--home`/`--max-sessions` to scope |
| `sync` | Generate agent instruction files (AGENTS.md / CLAUDE.md / MEMORY-INDEX.md, …) from memory (with `--watch`) |
| `dedup` | Scan and merge semantically duplicate memories (vector-assisted when an embedding provider is configured) |
| `checkpoints` | List memory history snapshots (per-memory or global); flags: `--memory-id`, `--limit` |
| `restore` | Revert a memory to the state captured by a checkpoint (`--history-id`) |
| `supersede` | Archive an old fact and point it at its replacement (nothing is deleted; search skips superseded, list keeps them) |
| `status` | Show embedding provider readiness (distinguishes not-configured / explicitly-disabled / configured-but-unavailable) and which features degrade without it |
| `doctor` | Read-only memory hygiene lint: dangling/stale/duplicate/contradicted + machine-readable `--json` |
| `bench` | Task-level memory benchmark: samples your own outcome history, measures lesson retrieval/injection rates; `--judge` adds an LLM-scored "plan without vs. with memory" success delta; each run persists itself for `eval-history` |
| `eval-history` | Trend-over-time view of past `bench`/`doctor` runs — every run persists itself automatically, this just lists what accumulated |
| `decay` | Archive stale memories based on access recency |
| `backup` | Create a consistent point-in-time SQLite backup |
| `export` / `import` | Backup and restore — JSON to a file or a directory (writes `export.json` inside); Markdown to/from a directory or a single `.md` file; import is idempotent (ids already present are skipped, never overwritten) |
| `confirm-read` | Mark memories as read (updates access_count) |

---

## Integrations

### Installing into your agents

MemVault ships native adapters for most agents — one shared store, per-host identity via `MEMVAULT_AGENT_ID`, four tiers (Tier 1/2/3 details below; per-client registration snippets in [integrations/mcp-clients/](integrations/mcp-clients/)).

**Tier 1 — one-command native plugins** (memory injected by hooks; extraction opt-in where the host exposes lifecycle hooks):

| Agent | Install | Recall | Extract |
|---|---|---|---|
| **Claude Code** | `/plugin marketplace add dreamor/memvault`, then `/plugin install memvault@memvault` (two separate prompts) — bundles the MCP server, 4 skills, 3 slash commands | ✅ SessionStart hook | ✅ Stop hook, enable with `MEMVAULT_HOOK_EXTRACT=1` |
| **OpenCode** | merge [`integrations/opencode/opencode.json`](integrations/opencode/opencode.json) into your project | ✅ system transform | ✅ on `session.idle` |
| **DeepSeek Harness (dsh)** | built-in Cordis plugin [`dsh-plugin/`](dsh-plugin/) — see [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh) | ✅ system prompt | ✅ turn-end |
| **Gemini CLI / Antigravity** | `gemini extensions install https://github.com/dreamor/memvault` | ⚠️ rule context + tools | ❌ |
| **Codex CLI** | [`integrations/codex/`](integrations/codex/): config.toml MCP + `memvault sync` + custom prompts | ⚠️ rules + tools | ❌ |

⚠️ = the host has no injection hooks; recall is rule-driven (the agent calls `session_start` once) with the bundled canonical rule text.

**Tier 2 — paste an MCP snippet.** Strict-JSON registrations with per-host identities in [integrations/mcp-clients/](integrations/mcp-clients/) (target paths in its [README](integrations/mcp-clients/README.md)): Cursor · Windsurf · Cline/Roo · Continue · Zed · JetBrains AI/Junie · VS Code (Copilot Chat) · Claude Desktop.

**Tier 3 — native manifests, verify-on-install.** Qoder (`.qoder/rules/` + `.qoder-plugin/` + a `UserPromptSubmit` hook template), Grok Build (`grok plugin install dreamor/memvault --trust`), the Hermes Python plugin ([integrations/hermes/](integrations/hermes/), `pre_llm_call` recall + extraction helper) and the pi extension (`pi-extension/`, `pi install git:github.com/dreamor/memvault`) ship in-repo; OpenClaw and Swival consume the generated root `skills/` (also exported to `.openclaw/skills/`); Devin stays a manual recipe in [integrations/README.md](integrations/README.md).

**Tier 4 — instruction-only rule copies.** Canonical text + `scripts/gen-rule-copies.sh` (parity-checked in CI) produce `AGENTS.md`/`CLAUDE.md` blocks, `.cursor/rules/`, `.clinerules/`, `.kiro/steering/`, Junie guidelines; `memvault sync --watch` keeps them fresh from the store.

Any other MCP-speaking client (domestic or international, IDE plugin or CLI harness) connects with zero MemVault-side changes via the standard stdio config below — not individually verified; PRs adding a verified entry are welcome.

GUI surfaces are independent of agent installs: **Web Dashboard** (9 tabs) · **Obsidian plugin** (α — Vault sync + browse/capture) · **MCP Proxy** (transparent memory injection for any upstream server).

---

## Architecture

```
┌────────────────────────────────────────────────┐
│  Clients (any MCP-compatible agent)             │
│  ┌────────────┐ ┌────────┐ ┌─────┐ ┌────────┐  │
│  │ Claude Code│ │ Cursor │ │ dsh │ │ Others │  │
│  └────────────┘ └────────┘ └─────┘ └────────┘  │
└──────────────────┬───────────────────────────────┘
                   │ MCP (stdio / SSE / HTTP)
┌──────────────────▼───────────────────────────────┐
│  memvault-mcp     (rmcp 3.1.1)                    │
│  ┌──────────────┐ ┌────────────────┐ ┌────────┐  │
│  │  18 tools    │ │  2 Resources   │ │ SSE    │  │
│  │   + REST API │ │  + Auto-Inject │ │ Server │  │
│  └──────┬───────┘ └──────┬─────────┘ └────────┘  │
│         └────────┬───────┘                        │
│              ┌───▼────────┐                       │
│              │ Agent       │ (Agent Registry      │
│              │ Router      │  type/tag filter)     │
│              └───┬────────┘                       │
├──────────────────┼────────────────────────────────┤
│  memvault-core    │                               │
│  ┌──────────┐  ┌─▼───────┐ ┌────────────┐ ┌───┐ │
│  │ storage  │  │retrieval│ │ pipeline   │ │sync│ │
│  │ SQLite   │  │BM25+Vec │ │extractor   │ │   │ │
│  │          │  │RRF+syn  │ │dedup/decay │ │   │ │
│  │embed     │  │onym     │ │export/     │ │   │ │
│  │backfill  │  │scoring  │ │import      │ │   │ │
│  └──────────┘  └─────────┘ └────────────┘ └───┘ │
└────────────────────────────────────────────────┘
```

---

## Project Status

MemVault is in **beta**. The Rust core (storage / search / injection) is CI-gated and stable; plugin adapters and GUI surfaces evolve faster. All configuration is environment-driven (see `.env.example`), and every change is recorded in [CHANGELOG.md](CHANGELOG.md).

## Testing

```bash
cargo test                      # ~1000 tests (full workspace)
cargo clippy --all-targets      # zero warnings
cargo fmt --all -- --check      # format check
cargo llvm-cov --workspace --all-features   # CI gate: line ≥92% / region ≥90% / function ≥85%
```

---

## Documentation

| Doc | Content |
|-----|---------|
| [docs/DESIGN.md](docs/DESIGN.md) | Product & architecture design |
| [docs/INSTALL.md](docs/INSTALL.md) | Installation guide (all platforms) |
| [docs/DOCKER.md](docs/DOCKER.md) | Docker deployment |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | Deployment / health check / rollback runbook |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Symptom → cause → fix troubleshooting guide |
| [docs/experiments/](docs/experiments/README.md) | Hypothesis-validation experiments (H1–H7, 2026-08-11 → 2026-08-27, all CONFIRMED) + runtime plumbing regression (2026-08-28) |
| [docs/RELEASING.md](docs/RELEASING.md) | Release process — what CI automates (Linux/macOS binaries, Docker image, dashboard archive, Obsidian zip) vs. manual steps (Obsidian submission — no macOS signing needed) |
| [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) | Distribution channel map — automated vs. manual channels, required credentials, MCP registries, optional channels |
| [CHANGELOG.md](CHANGELOG.md) | Release history |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Contribution guide |
| [SECURITY.md](SECURITY.md) | Security disclosures |
| [.env.example](.env.example) | Configuration template — single source of truth for every config key |

---

## Contributing

- 🐛 **Bugs:** [Issue Tracker](https://github.com/dreamor/memvault/issues/new)
- 💡 **Ideas:** [Feature Request](https://github.com/dreamor/memvault/issues/new)
- 📖 **Guide:** [CONTRIBUTING.md](CONTRIBUTING.md)
- 🔒 **Security:** [SECURITY.md](SECURITY.md)

---

## License

MemVault is released under the [MIT License](LICENSE).
