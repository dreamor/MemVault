<div align="center">

# MemVault

### The Shared Memory Layer for Every AI Agent You Run

> Not "agent learns to search memory" — memory finds the agent.

**MCP Native &nbsp;·&nbsp; Hybrid Retrieval &nbsp;·&nbsp; Auto-Injection &nbsp;·&nbsp; Zero-Config Sync**

**Open Source &nbsp;·&nbsp; Self-Hosted &nbsp;·&nbsp; Private &nbsp;·&nbsp; MIT Licensed**

[![CI](https://img.shields.io/github/actions/workflow/status/dreamor/memvault/ci.yml?style=flat-square&branch=main)](https://github.com/dreamor/memvault/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE) [![Rust](https://img.shields.io/badge/Rust-stable-orange.svg?style=flat-square)](https://www.rust-lang.org) [![MCP](https://img.shields.io/badge/MCP-compatible-blue.svg?style=flat-square)](https://modelcontextprotocol.io) [![Status](https://img.shields.io/badge/status-beta-yellow.svg?style=flat-square)](#project-status)

**English** &nbsp;·&nbsp; [简体中文](README.zh-CN.md)

```bash
cargo install memvault-cli memvault-mcp
```

</div>

---

Every AI agent session starts from scratch. Claude Desktop doesn't know what Cursor just learned. Your coding assistant forgets your preferences every time you start a new conversation.

You've been manually repeating context — project conventions, personal preferences, past decisions — across agents that should already know. This isn't a limitation of the models. It's a missing infrastructure layer.

MemVault is that layer. A lightweight, self-hosted memory router that sits between your agents and their context. Any MCP-compatible agent connected to MemVault automatically shares the same persistent memory — no SDK, no API integration, no code changes required.

**Who it's for:**

- **Claude Code / Claude Desktop users** who want preferences, project context, and past decisions to persist across sessions without repeating yourself
- **Multi-agent power users** running Claude, Cursor, VS Code extensions, and Obsidian side by side — all sharing the same memory without configuration
- **Platform teams** deploying AI-assisted workflows where consistency matters: code review conventions, architecture decisions, project-specific preferences
- **Anyone tired of telling their AI the same thing twice** — MemVault works the way your brain should: you say it once, it's there when you need it

**[Quick Start](#quick-start)** &nbsp;·&nbsp; **[How It Works](#how-it-works)** &nbsp;·&nbsp; **[What MemVault Gives You](#what-memvault-gives-you)** &nbsp;·&nbsp; **[Why MemVault](#why-memvault)** &nbsp;·&nbsp; **[MCP Server](#mcp-server)** &nbsp;·&nbsp; **[CLI Reference](#cli-reference)** &nbsp;·&nbsp; **[Integrations](#integrations)** &nbsp;·&nbsp; **[Architecture](#architecture)** &nbsp;·&nbsp; **[Project Status](#project-status)** &nbsp;·&nbsp; **[Testing](#testing)** &nbsp;·&nbsp; **[Documentation](#documentation)** &nbsp;·&nbsp; **[Contributing](#contributing)** &nbsp;·&nbsp; **[License](#license)**

---

## Quick Start

```bash
# Install
cargo install memvault-cli memvault-mcp

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

<div align="center">

If MemVault solves a real problem for you, a star helps others find it.

**[⭐ Star on GitHub](https://github.com/dreamor/memvault)** &nbsp;·&nbsp; **[Report Bug](https://github.com/dreamor/memvault/issues/new)**

</div>

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

- **Storage:** SQLite with bundled FTS5 (full-text search) + vector extension
- **Retrieval:** BM25 keyword search, local-first embedding (Ollama by default, or any OpenAI-compatible model), RRF fusion, synonym expansion, relevance scoring, soft intent filtering
- **Pipeline:** Automatic entity extraction, semantic deduplication, time-based decay, archive of stale memories
- **Sync:** Zero-invasion file generation — `memvault sync` produces CLAUDE.md, AGENTS.md, etc. directly from database contents

---

## What MemVault Gives You

- **Auto-Injected Context:** Session start automatically pulls relevant memory by agent identity — MUST-level rules land as instructions, not just chat history
- **Hybrid Retrieval:** BM25 + vector + RRF fusion with synonym expansion and relevance scoring — available via CLI, MCP tool, and REST API
- **MUST Enforcement:** MUST-priority memories are never filtered or truncated. Always in context, always obeyed
- **Multi-Agent Awareness:** Agent Registry with type/tag-based soft filtering (score demotion, not hard exclusion)
- **MCP Proxy:** Transparent proxy that injects memory into ANY upstream MCP server's responses — zero client changes
- **Compliance Tracking:** `inject_session_id` traces what was injected and measures follow-through rate
- **Cross-Platform:** CLI + MCP Server (stdio & SSE) + Tauri Dashboard + VS Code Extension + Obsidian Plugin
- **Zero-Invasion Sync:** Generate AGENTS.md / CLAUDE.md from memory — no per-agent config files to edit
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
| **MCP native** | No | No | stdio + SSE + Proxy |
| **Agent differentiation** | Global file | Query filter | Type/tag registry |
| **Compliance tracking** | None | None | inject_session_id + rate |
| **Self-hosted** | Yes | Varies | Single binary, no cloud |

MemVault complements your existing agent setup rather than replacing it. Keep your LLM, your IDE, and your workflow exactly as they are. MemVault adds the memory layer underneath.

---

## MCP Server

### stdio (Claude Desktop / Claude Code)

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

### Claude Code

```bash
claude mcp add memvault /path/to/memvault-mcp -- --db ~/.memvault/data.db
```

### SSE (multi-client, network-accessible)

```bash
memvault-mcp --transport sse --host 127.0.0.1 --port 3777
# Clients connect at http://127.0.0.1:3777/mcp
```

SSE features: multi-client simultaneous connections, auto-triggered embedding backfill on initialization, HTTP remote access.

> **Note:** `--transport sse` only mounts the MCP-over-HTTP endpoint (`/mcp`) — it does **not** expose the REST API (`/api/*`). The Dashboard talks to SQLite directly, but the VS Code extension and Obsidian plugin use the REST API and require `--transport http` instead. See [docs/INSTALL.md §2.6](docs/INSTALL.md#26-rest-apivs-code--obsidian-客户端专用).

### 13 MCP Tools

| Tool | Description |
|------|-------------|
| `save_memory` | Save with auto-embedding |
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

### 2 MCP Resources

| URI | Content |
|-----|---------|
| `memory://user-profile` | MUST-level rules, auto-loaded on connect |
| `memory://project-context` | REFERENCE-level project context |

### Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `MEMVAULT_EMBEDDING_PROVIDER` | Provider: `native` (in-process, default), `ollama`/`local`, `openai`, or `openai-compatible` (any OpenAI-compatible endpoint) | `native` |
| `OPENAI_API_KEY` / `MEMVAULT_EMBEDDING_API_KEY` | API key for remote providers (not needed for local Ollama) | (none, keyword-only) |
| `OPENAI_API_BASE` / `MEMVAULT_EMBEDDING_API_BASE` | Any OpenAI-compatible base URL (OpenAI / Azure / vLLM / gateway...) | `https://api.openai.com/v1` |
| `MEMVAULT_EMBEDDING_MODEL` | Embedding model: default `bge-small-zh` (zh, ~95MB), `multilingual`/`e5-base` for multilingual, or any model name for API providers | `bge-small-zh` (native) / `text-embedding-3-small` (API) |
| `MEMVAULT_EMBEDDING_DIM` | Vector dimensions | `768` (local) / `1536` (API) |
| `MEMVAULT_DB` | SQLite database path | `~/.memvault/data.db` |
| `RUST_LOG` | Log verbosity | `info` |

---

## CLI Reference

`save` · `search` · `list` · `delete` · `session-start` · `resource` · `extract` · `dedup` · `decay` · `promote` · `backup` · `export` · `import` · `confirm-read` · `sync`

```bash
memvault <command> --help   # detailed usage per command
```

### Key Commands

| Command | What It Does |
|---------|--------------|
| `save` | Save a memory with priority, type, optional instruction |
| `search` | Hybrid retrieval with relevance scoring; flags: `--query`, `--top-k`, `--namespace` |
| `session-start` | Simulate what context an agent receives on connect |
| `extract` | Parse free text, extract structured memories |
| `sync` | Generate AGENTS.md / CLAUDE.md from memory (with `--watch`) |
| `dedup` | Scan and merge semantically duplicate memories |
| `decay` | Archive stale memories based on access recency |
| `backup` | Create a consistent point-in-time SQLite backup |
| `export` / `import` | Backup and restore (JSON / Markdown) |
| `confirm-read` | Mark memories as read (updates access_count) |

---

## Integrations

| Surface | Status | Description |
|---------|--------|-------------|
| **Claude Desktop** | ✅ | Simple MCP stdio config, auto-injection on session start |
| **Claude Code** | ✅ | `claude mcp add` one-liner |
| **Cursor** | ✅ | MCP stdio config, shares memory with Claude |
| **Any MCP client** | ✅ | SSE transport, multi-client simultaneous connections |
| **Tauri Dashboard** | ✅ Alpha | GUI memory management (4 pages) |
| **VS Code Extension** | ✅ Alpha | Sidebar + search + right-click save |
| **Obsidian Plugin** | ✅ Alpha | Sidebar + search + create/edit/delete + one-way vault sync (DB→notes) |
| **MCP Proxy** | ✅ | Transparent proxy injecting memory into any upstream server |
| **DeepSeek Harness (dsh)** | ✅ | Standard MCP stdio config — see [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh) |

---

## Architecture

```
┌────────────────────────────────────────────────┐
│  Clients                                       │
│  ┌──────────────┐ ┌──────────┐ ┌────────────┐  │
│  │ Claude Code  │ │ Cursor   │ │ Other MCP  │  │
│  └──────────────┘ └──────────┘ └────────────┘  │
└──────────────────┬───────────────────────────────┘
                   │ MCP (stdio / SSE / HTTP)
┌──────────────────▼───────────────────────────────┐
│  memvault-mcp     (rmcp 3.1.1)                    │
│  ┌──────────────┐ ┌────────────────┐ ┌────────┐  │
│  │  13 tools    │ │  2 Resources   │ │ SSE    │  │
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

> v0.2.0 — Core + retrieval + dashboard + pipeline + recall optimization + MCP Proxy + compliance + layered injection + promote + extraction.

| Module | Status | Notes |
|--------|--------|-------|
| `memvault-core` | ✅ v0.2.0 | 18 modules: storage, routing, retrieval, embedding, dedup, decay, sync, query expansion, auth, rerank, promote, compliance |
| `memvault-cli` | ✅ v0.2.0 | 15 subcommands (incl. promote, backup) |
| `memvault-mcp` | ✅ v0.2.0 | MCP Server (rmcp 3.1.1) with 13 tools + 2 resources + SSE + REST API |
| `memvault-proxy` | ✅ v0.2.0 | Transparent proxy + injection + extraction loop + compliance |
| Dashboard (Tauri 2) | ✅ Alpha | 4 pages |
| VS Code Extension | ✅ Alpha | Sidebar + search + right-click save |
| Obsidian Plugin | ✅ Alpha | Sidebar + search + create/edit/delete + one-way vault sync (DB→notes) |
| Recall optimization (7 items) | ✅ Done | Word-level tokenization, synonym expansion, scoring, soft filtering, cross-namespace, embedding backfill |
| Sync (`--watch`) | ✅ Done | Zero-invasion agent file generation |
| Rerank / Inbox / Auth | ✅ Done | Multi-signal rerank, REST inbox endpoints, SHA-256 API key auth |
| Compliance tracker | ✅ Done | `inject_session_id` tracking + follow-through rate |
| Layered injection (L0-L3) | ✅ Done | MemoryLayer enum, overflow summaries, promote pipeline (L1→L2→L3) |
| Structured Skill | ✅ Done | SkillMeta: trigger / steps / verification / version |
| Extraction loop | ✅ Done | Proxy `notify_response` tool, whitelist extraction into Inbox |
| Core test coverage | ✅ 90%+ | 375 tests (core 267 + MCP 39 + proxy 50 + CLI 19) |

### Roadmap

- [x] Phase 1 — Core Engine + MCP Server + CLI
- [x] Phase 2 — Hybrid retrieval (keyword + vector + RRF)
- [x] Phase 3 — Tauri Dashboard
- [x] Phase 4 — Auto-embedding + pipeline
- [x] Phase 5 — VS Code / Obsidian ecosystem
- [x] Phase 6 — Recall optimization
- [x] Phase 7 — Multi-agent sync (`memvault sync --watch`)
- [x] Phase 8 — MCP Proxy (transparent proxy + pre-prompt injection + dynamic resource)
- [x] Phase 9 — Auth / Rerank / Inbox / Compliance / Benchmarks
- [x] Phase 9.5 — Layered injection / MemoryLayer / SkillMeta / Promote / Extraction

---

## Testing

```bash
cargo test                      # 375 tests
cargo clippy --all-targets      # zero warnings
cargo fmt --all -- --check      # format check
cargo llvm-cov --lib            # coverage (core 90%+)
```

---

## Documentation

| Doc | Content |
|-----|---------|
| [docs/DESIGN.md](docs/DESIGN.md) | Product & architecture design |
| [docs/INSTALL.md](docs/INSTALL.md) | Installation guide (all platforms) |
| [docs/DOCKER.md](docs/DOCKER.md) | Docker deployment |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | Deployment / health check / rollback runbook |
| [docs/RELEASING.md](docs/RELEASING.md) | Release process — what CI automates vs. manual steps (Marketplace publish, Obsidian submission, macOS signing) |
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