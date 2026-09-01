<div align="center">

<img src="assets/memvault-logo.png" alt="MemVault — a round hamster mascot hugging its memory nut, honey gold and brown on cream" width="360" />

# MemVault

### The Shared Memory Layer for Every AI Agent You Run

> Not "agent learns to search memory" — memory finds the agent.

**MCP Native &nbsp;·&nbsp; Hybrid Retrieval &nbsp;·&nbsp; Auto-Injection &nbsp;·&nbsp; Zero-Config Sync**

**Open Source &nbsp;·&nbsp; Self-Hosted &nbsp;·&nbsp; Private &nbsp;·&nbsp; MIT Licensed**

[![CI](https://img.shields.io/github/actions/workflow/status/dreamor/memvault/ci.yml?style=flat-square&branch=master)](https://github.com/dreamor/memvault/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE) [![Rust](https://img.shields.io/badge/Rust-stable-orange.svg?style=flat-square)](https://www.rust-lang.org) [![MCP](https://img.shields.io/badge/MCP-compatible-blue.svg?style=flat-square)](https://modelcontextprotocol.io) [![Status](https://img.shields.io/badge/status-beta-yellow.svg?style=flat-square)](#project-status)

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

# Or, once published to crates.io:
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
# memvault 0.2.0

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

# 3. (Optional) Explicitly enable local Ollama
export MEMVAULT_EMBEDDING_PROVIDER=ollama
export MEMVAULT_LLM_EXTRACTION_PROVIDER=ollama
export MEMVAULT_LLM_EXTRACTION_MODEL=qwen2.5:3b-instruct

# 4. Verify
memvault status     # Embedding provider: configured and reachable
memvault save --content "Build server IP is 10.20.30.40"   # output (embedded int8)
memvault outcome --task "Deploy trading service" --status failure --cause "Disk space insufficient" --task-type deploy
#   → Lesson (Llm): ... means failure reflection ran through the local LLM (not the rule-based fallback)
```

With no environment variables set, LLM extraction auto-detects a local Ollama and enables itself (default model `qwen2.5:7b`; pull it in advance with `ollama pull qwen2.5:7b`, or point `MEMVAULT_LLM_EXTRACTION_MODEL` at an installed model). Embeddings still default to the in-process native embedder; set `MEMVAULT_EMBEDDING_PROVIDER=auto` to prefer Ollama and fall back to native when it isn't running.

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
- **Pipeline:** Automatic entity extraction, semantic deduplication, time-based decay, archive of stale memories
- **Sync:** Zero-invasion file generation — `memvault sync` produces CLAUDE.md, AGENTS.md, etc. directly from database contents

---

## What MemVault Gives You

- **Auto-Injected Context:** Session start automatically pulls relevant memory by agent identity — MUST-level rules land as instructions, not just chat history
- **Hybrid Retrieval:** BM25 + vector + RRF fusion with synonym expansion, relevance scoring, and per-result provenance (which path recalled each memory, at what rank) — available via CLI, MCP tool, and REST API
- **Explainable Injection:** every candidate dropped on the way into an agent's context is recorded with a reason (budget, caps, intent/type penalties) — "why didn't the agent get this memory?" always has an answer
- **MUST Enforcement:** MUST-priority memories are never filtered or truncated. Always in context, always obeyed
- **Multi-Agent Awareness:** Agent Registry with type/tag-based soft filtering (score demotion, not hard exclusion)
- **MCP Proxy:** Transparent proxy that injects memory into ANY upstream MCP server's responses — zero client changes
- **Compliance Tracking:** `inject_session_id` traces what was injected and measures follow-through rate
- **Cross-Platform:** CLI + MCP Server (stdio & SSE) + Web Dashboard (browser) + VS Code Extension + Obsidian Plugin
- **Zero-Invasion Sync:** Generate AGENTS.md / CLAUDE.md from memory — no per-agent config files to edit
- **Contextual Extraction, Local-First:** Rule-based keyword extraction by default; optionally understands a full user+assistant exchange via an LLM, auto-detecting a local Ollama for free before ever touching a remote API
- **History & Rollback:** Every update/delete is snapshotted into `memory_history` — `memvault checkpoints` + `memvault restore` roll one memory back without touching the rest
- **Self-Diagnostics:** `memvault status` reports exactly which features are degraded when no embedding provider is configured, plus a schema fingerprint (migration version + checksum) for cross-database comparison
- **Episodic Memory:** `record_outcome` records task results; failures are distilled into lessons and auto-injected next time (REFERENCE → MUST only with human approval), so the same trap isn't hit twice
- **Procedural Skill Activation:** skills whose `trigger` matches intent are injected as structured `[SKILL]` blocks with success-rate stats (shown after ≥3 runs); failures flag the skill for revision (`version++` on human edit), repeated successes auto-draft new skills into the review inbox
- **Semantic Knowledge Links:** lightweight relation triples, repeated facts consolidated into a linked semantic fact with provenance, and superseded facts archived (never re-injected, still listable & restorable)
- **Team Shared Pool & SOP Import:** memories marked `shared` are injected into every session (capped at 20); Markdown SOPs can be batch-imported as verifiable skills
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
| **Decay / archival** | No | No | Time-based + auto archive |
| **Memory extraction** | Manual | N/A | Rule-based by default; optional local-first LLM extraction |
| **MCP native** | No | No | stdio + SSE + Proxy |
| **Agent differentiation** | Global file | Query filter | Type/tag registry |
| **Compliance tracking** | None | None | inject_session_id + rate |
| **Self-hosted** | Yes | Varies | Single binary, no cloud |

MemVault complements your existing agent setup rather than replacing it. Keep your LLM, your IDE, and your workflow exactly as they are. MemVault adds the memory layer underneath.

---

## MCP Server

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

> **Note:** `--transport sse` only mounts the MCP-over-HTTP endpoint (`/mcp`) — it does **not** expose the REST API (`/api/*`). The Web Dashboard is served by the REST backend (`memvault-mcp --transport http --serve-web <dist>`), and the VS Code extension and Obsidian plugin also use the REST API and require `--transport http` instead. See [docs/INSTALL.md §2.6](docs/INSTALL.md#26-rest-apivs-code--obsidian-客户端专用).

### 16 MCP Tools

| Tool | Description |
|------|-------------|
| `save_memory` | Save with auto-embedding |
| `record_outcome` | Record a task outcome (episodic memory); failures reflect into lessons |
| `import_skills` | Import skills from a Markdown SOP (headings → skills, list items → steps) |
| `search_memory` | Keyword / semantic / hybrid |
| `session_start` | Agent-aware context injection |
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

### 2 MCP Resources

| URI | Content |
|-----|---------|
| `memory://user-profile` | MUST-level rules, auto-loaded on connect |
| `memory://project-context` | REFERENCE-level project context |

### Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `MEMVAULT_EMBEDDING_PROVIDER` | Provider: `native` (in-process, default), `auto` (Ollama-first, native fallback), `ollama`/`local`, `openai`, or `openai-compatible` (any OpenAI-compatible endpoint) | `native` |
| `OPENAI_API_KEY` / `MEMVAULT_EMBEDDING_API_KEY` | API key for remote providers (not needed for local Ollama) | (none, keyword-only) |
| `OPENAI_API_BASE` / `MEMVAULT_EMBEDDING_API_BASE` | Any OpenAI-compatible base URL (OpenAI / Azure / vLLM / gateway...). For `ollama`/`local` the embedder uses Ollama's native endpoint `http://localhost:11434/api` | `https://api.openai.com/v1` / `http://localhost:11434/api` (Ollama) |
| `MEMVAULT_EMBEDDING_MODEL` | Embedding model: `bge-small-zh` (zh, ~95MB) / `multilingual`/`e5-base` for native; `nomic-embed-text` (768-dim) for Ollama; or any model name for API providers | `bge-small-zh` (native) / `nomic-embed-text` (Ollama) / `text-embedding-3-small` (API) |
| `MEMVAULT_EMBEDDING_DIM` | Vector dimensions | `768` (local/Ollama) / `1536` (API) |
| `MEMVAULT_LLM_EXTRACTION_PROVIDER` | Optional: enables LLM-based *contextual* memory extraction (understands a full user+assistant exchange, not just keyword lines). Unset/`auto` → **local-first**: auto-detects a running local Ollama and uses it for free, no config needed; falls back to rule-based if none is running. `openai`/`openai-compatible`/custom → explicit remote provider (never auto-enabled just because an API key exists elsewhere — remote calls cost money and carry hallucination risk). `off`/`disabled`/`none` → force pure rule-based, even if local Ollama is running | (unset — local-first, rule-based if no local Ollama) |
| `MEMVAULT_LLM_EXTRACTION_API_KEY` (falls back to `OPENAI_API_KEY`) / `MEMVAULT_LLM_EXTRACTION_API_BASE` / `MEMVAULT_LLM_EXTRACTION_MODEL` | Chat-completions endpoint config for LLM extraction | local: `http://localhost:11434/v1` / `qwen2.5:7b` (no key) — remote: `https://api.openai.com/v1` / `gpt-4o-mini` |
| `MEMVAULT_RELATIONS` | Opt-in LLM relation extraction: `on` makes `extract_memories` (mode=llm) also persist `supports`/`contradicts`/`sourced_from` triples | (unset / off) |
| `MEMVAULT_DB_POOL_SIZE` | SQLite connection pool size | `5` |
| `MEMVAULT_CORS_ORIGIN` | Comma-separated allowed CORS origins for REST (unset = localhost only) | (localhost only) |
| `MEMVAULT_DB` | SQLite database path | `~/.memvault/data.db` |
| `RUST_LOG` | Log verbosity | `info` |

---

## CLI Reference

`save` · `outcome` · `search` · `list` · `review` · `delete` · `session-start` · `resource` · `extract` · `dedup` · `decay` · `doctor` · `promote` · `backup` · `export` · `import` · `import-skills` · `confirm-read` · `sync` · `checkpoints` · `restore` · `supersede` · `status`

```bash
memvault <command> --help   # detailed usage per command
```

### Key Commands

| Command | What It Does |
|---------|--------------|
| `save` | Save a memory with priority, type, optional instruction |
| `outcome` | Record a task result (success/failure/partial); failures are distilled into lessons that auto-inject into similar future tasks |
| `search` | Hybrid retrieval with relevance scoring; flags: `--query`, `--top-k`, `--namespace` |
| `session-start` | Simulate what context an agent receives on connect |
| `extract` | Parse free text, extract structured memories |
| `import-skills` | Import skills from a Markdown SOP (`# / ##` headings → skills, list items → steps); enters the review inbox unless `--approve` |
| `sync` | Generate agent instruction files (AGENTS.md / CLAUDE.md / MEMORY-INDEX.md, …) from memory (with `--watch`) |
| `dedup` | Scan and merge semantically duplicate memories (vector-assisted when an embedding provider is configured) |
| `checkpoints` | List memory history snapshots (per-memory or global); flags: `--memory-id`, `--limit` |
| `restore` | Revert a memory to the state captured by a checkpoint (`--history-id`) |
| `supersede` | Archive an old fact and point it at its replacement (nothing is deleted; search skips superseded, list keeps them) |
| `status` | Show embedding provider readiness and which features degrade without it |
| `doctor` | Read-only memory hygiene lint: dangling/stale/duplicate/contradicted + machine-readable `--json` |
| `decay` | Archive stale memories based on access recency |
| `backup` | Create a consistent point-in-time SQLite backup |
| `export` / `import` | Backup and restore (JSON / Markdown) |
| `confirm-read` | Mark memories as read (updates access_count) |

---

## Integrations

MemVault is MCP-native, so it isn't tied to any one vendor or region — the table below is what's been explicitly verified, not the ceiling of what works.

| Surface | Status | Description |
|---------|--------|-------------|
| **Claude Code / Claude Desktop** | ✅ | Standard MCP stdio config, or `claude mcp add` one-liner for Claude Code |
| **Cursor / Cline / Continue** | ✅ | Same standard `mcpServers` JSON config, shares memory with everything else connected |
| **DeepSeek Harness (dsh)** | ✅ | Two options: zero-code MCP client plugin, or the deep-integration native Cordis plugin (`dsh-plugin/`) with automatic injection + extraction — see [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh) |
| **Any other MCP client** | Should work | Domestic or international, IDE plugin or CLI harness — anything speaking standard MCP stdio/SSE connects with zero MemVault-side changes. Not individually verified; PRs adding a verified entry are welcome |
| **Web Dashboard** | ✅ Alpha | GUI memory management (6 tabs, in-browser) |
| **VS Code Extension** | ✅ Alpha | Sidebar + search + right-click save |
| **Obsidian Plugin** | ✅ Alpha | Sidebar + search + create/edit/delete + one-way vault sync (DB→notes) |
| **MCP Proxy** | ✅ | Transparent proxy injecting memory into any upstream server's responses, regardless of which client is on the other end |

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
│  │  16 tools    │ │  2 Resources   │ │ SSE    │  │
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

> v0.2.0 — Core + retrieval + dashboard + pipeline + recall optimization + MCP Proxy + compliance + layered injection + promote + extraction + episodic/procedural/semantic memory + evidence-driven decay + doctor + injection safety.

| Module | Status | Notes |
|--------|--------|-------|
| `memvault-core` | ✅ v0.2.0 | 29 modules: storage, routing, retrieval (fts/hybrid/rerank/query_expand), embedding, dedup, decay, sync, doctor, auth, promote, compliance, capabilities, intent, config, LLM-based contextual extraction, episodic (episode/reflection), semantic (evidence/relations), procedural (sop) |
| `memvault-cli` | ✅ v0.2.0 | 23 subcommands (incl. doctor, outcome, supersede, import-skills, review) |
| `memvault-mcp` | ✅ v0.2.0 | MCP Server (rmcp 3.1.1) with 16 tools + 2 resources + SSE + REST API |
| `memvault-proxy` | ✅ v0.2.0 | Transparent proxy + injection + extraction loop + compliance |
| Web Dashboard | ✅ Alpha | 6 tabs (browser, REST backend) |
| VS Code Extension | ✅ Alpha | Sidebar + search + right-click save |
| Obsidian Plugin | ✅ Alpha | Sidebar + search + create/edit/delete + one-way vault sync (DB→notes) |
| Recall optimization (7 items) | ✅ Done | Word-level tokenization, synonym expansion, scoring, soft filtering, cross-namespace, embedding backfill |
| Sync (`--watch`) | ✅ Done | Zero-invasion agent file generation |
| Rerank / Inbox / Auth | ✅ Done | Multi-signal rerank, REST inbox endpoints, SHA-256 API key auth |
| Compliance tracker | ✅ Done | `inject_session_id` tracking + follow-through rate |
| Layered injection (L0-L3) | ✅ Done | MemoryLayer enum, overflow summaries, promote pipeline (L1→L2→L3) |
| Structured Skill | ✅ Done | SkillMeta: trigger / steps / verification / version |
| Extraction loop | ✅ Done | Proxy `notify_response` tool, whitelist extraction into Inbox |
| History & Rollback | ✅ Done | `memory_history` snapshots on update/delete + `checkpoints` / `restore` CLI |
| Capability report | ✅ Done | `memvault status` — degraded-feature self-diagnostics without an embedding provider |
| Authority-tier rerank | ✅ Done | L2/L3 layer + `decision`/`procedure`/`gotcha` tags boost; soft nudge, not a filter; MUST untouched |
| Evidence-driven decay | ✅ Done | supports / contradicts / sourced_from relations; memories with active contradiction decay 3× faster |
| Memory hygiene (`doctor`) | ✅ Done | Read-only lint: dangling/stale/duplicate/contradicted + machine-readable `--json` |
| Injection safety (P0) | ✅ Done | Trust-tiered wrapping + treat-as-data for unreviewed AI-extracted memories |
| Core test coverage | ✅ 92%+ | 716 tests (core 493 + e2e 19, MCP 96 + 4, proxy 65 + 7, CLI 30 + smoke 2) — CI gate: line ≥92% / region ≥90% / function ≥85% |

### Roadmap

- [x] Phase 1 — Core Engine + MCP Server + CLI
- [x] Phase 2 — Hybrid retrieval (keyword + vector + RRF)
- [x] Phase 3 — Web Dashboard
- [x] Phase 4 — Auto-embedding + pipeline
- [x] Phase 5 — VS Code / Obsidian ecosystem
- [x] Phase 6 — Recall optimization
- [x] Phase 7 — Multi-agent sync (`memvault sync --watch`)
- [x] Phase 8 — MCP Proxy (transparent proxy + pre-prompt injection + dynamic resource)
- [x] Phase 9 — Auth / Rerank / Inbox / Compliance / Benchmarks
- [x] Phase 9.5 — Layered injection / MemoryLayer / SkillMeta / Promote / Extraction
- [x] Phase 9.6 — Memory history (`memory_history`) + `checkpoints`/`restore` + `status` self-diagnostics
- [x] Phase 10 — Three-memory evolution loop (episodic / procedural / semantic + shared pool / SOP import / typed Obsidian sync) — H5/H6/H7 all CONFIRMED

---

## Testing

```bash
cargo test                      # 716 tests (full workspace)
cargo clippy --all-targets      # zero warnings
cargo fmt --all -- --check      # format check
cargo llvm-cov --workspace --all-features   # CI gate: line ≥92% / region ≥90% / function ≥85%
```

---

## Documentation

| Doc | Content |
|-----|---------|
| [docs/DESIGN.md](docs/DESIGN.md) | Product & architecture design |
| [docs/DSH-BRIDGE-DESIGN.md](docs/DSH-BRIDGE-DESIGN.md) | DeepSeek Harness (dsh) native bridge plugin design |
| [docs/INSTALL.md](docs/INSTALL.md) | Installation guide (all platforms) |
| [docs/DOCKER.md](docs/DOCKER.md) | Docker deployment |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | Deployment / health check / rollback runbook |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Symptom → cause → fix troubleshooting guide |
| [docs/experiments/](docs/experiments/README.md) | Hypothesis-validation experiments (H1–H7, 2026-08-11 → 2026-08-27, all CONFIRMED) + runtime plumbing regression (2026-08-28) |
| [docs/PERSONAL-MEMORY-INSPIRATION.md](docs/PERSONAL-MEMORY-INSPIRATION.md) | Personal-memory-system article analysis → 4 adopted changes (type-stability decay / injected conflict hints / intent-type boost / MEMORY-INDEX) |
| [docs/RELEASING.md](docs/RELEASING.md) | Release process — what CI automates (Linux/macOS binaries, Docker image, dashboard archive, `.vsix`, Obsidian zip) vs. manual steps (VS Code Marketplace publish, Obsidian submission — no macOS signing needed) |
| [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) | Distribution channel map — automated vs. manual channels, required credentials, MCP registries, optional channels |
| [docs/DISTRIBUTION-TODO.md](docs/DISTRIBUTION-TODO.md) | Distribution todo checklist — what is shipped vs. pending, phases, required secrets (repo currently private) |
| [CHANGELOG.md](CHANGELOG.md) | Release history |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Contribution guide |
| [SECURITY.md](SECURITY.md) | Security disclosures |
| [.env.example](.env.example) | Environment variable reference |

---

## Contributing

- 🐛 **Bugs:** [Issue Tracker](https://github.com/dreamor/memvault/issues/new)
- 💡 **Ideas:** [Feature Request](https://github.com/dreamor/memvault/issues/new)
- 📖 **Guide:** [CONTRIBUTING.md](CONTRIBUTING.md)
- 🔒 **Security:** [SECURITY.md](SECURITY.md)

---

## License

MemVault is released under the [MIT License](LICENSE).