<div align="center">

<img src="https://cdn.jsdelivr.net/gh/dreamor/memvault@master/assets/memvault-logo.png" alt="MemVault — a round hamster mascot hugging its memory nut, honey gold and brown on cream" width="360" />

# MemVault

### The Shared Memory Layer for Every AI Agent You Run

> Not "agent learns to search memory" — memory finds the agent.

**MCP Native · Hybrid Retrieval · Auto-Injection · Zero-Config Sync**

**Open Source · Self-Hosted · Private · MIT Licensed**

[![Crates.io](https://img.shields.io/crates/v/memvault-cli.svg?style=flat-square)](https://crates.io/crates/memvault-cli) [![GitHub Release](https://img.shields.io/github/v/release/dreamor/memvault?style=flat-square)](https://github.com/dreamor/memvault/releases) [![CI](https://img.shields.io/github/actions/workflow/status/dreamor/memvault/ci.yml?style=flat-square&branch=master)](https://github.com/dreamor/memvault/actions) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg?style=flat-square)](LICENSE) [![Rust](https://img.shields.io/badge/Rust-stable-orange.svg?style=flat-square)](https://www.rust-lang.org) [![MCP](https://img.shields.io/badge/MCP-compatible-blue.svg?style=flat-square)](https://modelcontextprotocol.io) [![Glama](https://glama.ai/mcp/servers/dreamor/MemVault/badges/score.svg)](https://glama.ai/mcp/servers/dreamor/MemVault) [![Status](https://img.shields.io/badge/status-beta-yellow.svg?style=flat-square)](#project-status)

**English** · [简体中文](README.zh-CN.md)

<div align="center">

If MemVault solves a real problem for you, a star helps others find it.

**[⭐ Star on GitHub](https://github.com/dreamor/memvault)** · **[Report Bug](https://github.com/dreamor/memvault/issues/new)**

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

**[Quick Start](#quick-start)** · **[How It Works](#how-it-works)** · **[What MemVault Gives You](#what-memvault-gives-you)** · **[Why MemVault](#why-memvault)** · **[MCP Server](#mcp-server)** · **[CLI Reference](#cli-reference)** · **[Integrations](#integrations)** · **[Architecture](#architecture)** · **[Project Status](#project-status)** · **[Testing](#testing)** · **[Documentation](#documentation)** · **[Contributing](#contributing)** · **[License](#license)**

---

## Quick Start

Don't install MemVault by hand — paste this one prompt into Claude Code, Codex, Cursor, dsh, or any other MCP-capable agent. It points the agent at a machine-readable install guide, so the agent does the whole setup itself:

```text
Install MemVault as your long-term memory and wire it into this agent. Fetch and follow https://raw.githubusercontent.com/dreamor/memvault/master/docs/AGENT-INSTALL.md — install the binaries, register MemVault as an MCP server for this host, and verify recall works. Then keep saving my preferences, project decisions, and lessons from each session so my next agent starts with them instead of from scratch.
```

Or install manually:

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

# The installers auto-start the local REST backend (host of the Web
# Dashboard). Open it, or start it on demand:
memvault-cli dashboard

# Save a MUST-level preference (injected as instruction, agent must follow)
memvault-cli save --content "User prefers Python" --priority MUST --type preference \
  --instruction "Use Python for code, not Java" --tags "coding,python"

# Search across all memory (hybrid: keyword + semantic when embedding is enabled)
memvault-cli search --query "Python"

# See what context gets injected when a specific agent connects
memvault-cli session-start --agent-id claude-desktop --context "Help me write code"

# Extract structured memories from free text
memvault-cli extract --text "I prefer dark mode. Our project uses Rust." --save

# Auto-generate agent instruction files from memory
memvault-cli sync

# All in one: dedup, decay, archive stale memories
memvault-cli dedup && memvault-cli decay
```

> **System requirements:**
> - **Prebuilt Linux binaries** (install script / GitHub release): glibc ≥ 2.39 + GLIBCXX_3.4.31 (GCC 13-era runtime — Ubuntu 24.04+ / Fedora 40+ / Arch). Release builds are pinned to Ubuntu 24.04 runners, and the release workflow gates the highest glibc/GLIBCXX symbol version every binary may require plus a startup smoke test, so this floor cannot silently drift upward (0.6.9 shipped x86_64 binaries requiring glibc 2.43 that way). TLS is rustls, so no OpenSSL dependency. Ubuntu 22.04 / RHEL or EL8/EL9 users: use the Docker image (`dreamor/memvault`) or build from source.
> - **Building from source** (`cargo install` / `cargo build`): links fastembed's prebuilt ONNX Runtime static library and needs a GCC 13-class toolchain. Older toolchains (e.g. GCC 8, CentOS 7/8-era libstdc++) fail at link time with missing C++20/23 stdlib symbols (`std::format`, `std::to_chars`). macOS / Windows (MSVC) / Homebrew / Docker are unaffected.

**Verify your install in 5 seconds:**

```bash
memvault-cli --version
# memvault 0.6.12

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
memvault-cli status     # Embedding provider: configured and reachable
memvault-cli save --content "Build server IP is 10.20.30.40"   # output (embedded int8)
memvault-cli outcome --task "Deploy trading service" --status failure --cause "Disk space insufficient" --task-type deploy
#   → Lesson (Llm): ... means failure reflection ran through the local LLM (not the rule-based fallback)
```

With no configuration at all (neither env vars nor `~/.memvault/.env`), LLM extraction auto-detects a local Ollama and enables itself (default model `qwen2.5:7b`; pull it in advance with `ollama pull qwen2.5:7b`, or point `MEMVAULT_LLM_EXTRACTION_MODEL` at an installed model). Embeddings still default to the in-process native embedder; set `MEMVAULT_EMBEDDING_PROVIDER=auto` in `~/.memvault/.env` to prefer Ollama and fall back to native when it isn't running. The native model's first launch downloads the weights from HuggingFace **silently in the background** — the server/dashboard come up immediately and hot-swap to semantic search once the download finishes, so a fresh install never blocks on it (set `MEMVAULT_EMBEDDING_PROVIDER=none` to run pure keyword and skip the download).

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
- **Sync:** Zero-invasion file generation — `memvault-cli sync` produces CLAUDE.md, AGENTS.md, etc. directly from database contents

---

## What MemVault Gives You

- **Auto-Injected Context:** Session start automatically pulls relevant memory by agent identity — MUST-level rules land as instructions, not just chat history
- **Hybrid Retrieval:** BM25 + vector + RRF fusion with synonym expansion, relevance scoring, and per-result provenance (which path recalled each memory, at what rank) — available via CLI, MCP tool, and REST API
- **Explainable Injection:** every candidate dropped on the way into an agent's context is recorded with a reason (budget, caps, intent/type penalties) — "why didn't the agent get this memory?" always has an answer
- **Degrade Before Dropping:** when the token budget runs out, a memory is shortened — excerpt, then summary — rather than cut, so more of the ranked list stays reachable at a lower per-memory cost. Shortened text always carries a ` […]` marker and the level is reported separately from the dropped list, because an abbreviation that *looks* complete is worse than a drop. MUST is never shortened: a binding rule with its qualifying clause summarised away is worse than one that overflowed
- **MUST Enforcement:** MUST-priority memories are never filtered or truncated. Always in context, always obeyed — trust comes from provenance (human-authored/reviewed), with an opt-in fallback for memories independently corroborated by multiple identity-verified agents (`MEMVAULT_CORROBORATION_GATE`), so a single spoofed/compromised agent can't unilaterally inject a binding MUST
- **Multi-Agent Awareness:** Agent Registry with type/tag-based soft filtering (score demotion, not hard exclusion)
- **`agent_id` Is Provenance, Not a Filter:** retrieval never filters by `agent_id` — the field only records which agent wrote a memory (`source_agent`). Every agent searches the same shared corpus; per-agent differences apply only at injection time (type/tag soft scoring via `agents.yaml` `inject_rules`)
- **MCP Proxy:** Transparent proxy that injects memory into ANY upstream MCP server's responses — zero client changes
- **Compliance Tracking:** `inject_session_id` traces what was injected and measures follow-through rate
- **Cross-Platform:** CLI + MCP Server (stdio & SSE) + Web Dashboard (browser) + Obsidian Plugin
- **Zero-Invasion Sync:** Generate AGENTS.md / CLAUDE.md from memory — no per-agent config files to edit. Applies the same exclusions as session injection (unreviewed drafts behind the review gate, superseded facts, pending edit proposals, expired memories) and reports how many it held back and why, so "why isn't this rule in my AGENTS.md" has an answer
- **Contextual Extraction, Local-First:** Rule-based keyword extraction by default; optionally understands a full user+assistant exchange via an LLM, auto-detecting a local Ollama for free before ever touching a remote API
- **History & Rollback:** Every update/delete is snapshotted into `memory_history` — `memvault-cli checkpoints` + `memvault-cli restore` roll one memory back without touching the rest
- **Time Travel (`as_of`):** ask what MemVault believed at any past instant — `memvault-cli as-of <id> --at 2026-08-01T00:00:00Z`, `list --as-of`, `search --as-of`, `search_memory`, `POST /api/search`. Reconstructed from the same snapshot chain that powers rollback, so it needed no new version schema. Memories deleted or superseded *after* that instant still come back, because they were the current truth *then*. Search also recalls from a **historical full-text index**, so a memory reworded since `as_of` is still found by the words it had — those hits are tagged `hist-kw#N` to keep "matched today" distinct from "matched then". Each answer says where it came from (live row vs. a named history snapshot), so a far-back result is never mistaken for a current one
- **Two Time Axes, Never Folded:** `as_of` asks *what did MemVault believe in March*; `--occurred-from`/`--occurred-to` ask *what happened in March*. A memory can be true for both answers at once, so they are separate parameters rather than one `since`/`until` range — folding them is what makes one of the two questions permanently unanswerable. Undated memories never match an event-time window, and the CLI says so whenever the flag is used rather than only when the result comes back empty
- **Self-Expiring Facts (`expires_at`):** a temporary fact ("the deploy freeze runs until Friday") retires itself instead of decaying slowly and being re-injected as stale truth. `save --expires-at <RFC3339>` or `--ttl 7d`. Past the bound it leaves retrieval and injection but stays inspectable and restorable — expiry retires a fact, only `forget` erases one. Honored even for MUST: an explicit bound is not the inferred disuse signal that `must_exempt` protects against
- **Self-Diagnostics:** `memvault-cli status` reports exactly which features are degraded when no embedding provider is configured, plus a schema fingerprint (migration version + checksum) for cross-database comparison
- **Episodic Memory:** `record_outcome` records task results; failures are distilled into lessons and auto-injected next time (REFERENCE → MUST only with human approval), so the same trap isn't hit twice
- **Procedural Skill Activation:** skills whose `trigger` matches intent are injected as structured `[SKILL]` blocks with success-rate stats (shown after ≥3 runs); failures flag the skill for revision (`version++` on human edit), repeated successes auto-draft new skills into the review inbox
- **Semantic Knowledge Links:** lightweight relation triples, repeated facts consolidated into a linked semantic fact with provenance, and superseded facts archived (never re-injected, still listable & restorable)
- **Team Shared Pool & SOP Import:** memories marked `shared` are injected into every session (capped at 20); Markdown SOPs can be batch-imported as verifiable skills
- **Delta-Write on Save:** every save is checked against its namespace first — near-duplicates are skipped, similar memories absorb only the *residual* (what's genuinely new) and get their strength refreshed, so the library converges instead of accumulating near-copies; `--force` / `force_insert` bypasses
- **Task-Level Evaluation:** `memvault-cli bench` samples your own failure history (episodes that distilled a lesson) and measures lesson retrieval/injection rates — and with `--judge`, an LLM scores "plan without vs. with memory" against the known failure cause, so you see *task-success* lift, not just retrieval recall
- **Two-Phase Injection (never blocks):** MUST rules resolve deterministically with zero embedding calls and are served immediately; the semantic pipeline prefetches in the background and lands within a short window (250ms) — if it doesn't, the deterministic baseline is served and the request moves on (two-phase design)
- **Conversation-N-Gram Retrieval:** retrieval keys are conditioned on the recent turn window, weighted by recency so the current focus dominates — not a single flat query
- **Single Canonical Injection Channel:** per-agent `inject_channel` (`mcp` / `proxy` / `sync` in `agents.yaml`) restricts automatic injection to one delivery path, so the same memory is never sent to the same agent twice
- **Data You Own:** Single SQLite file. Full export/import. No cloud dependency. Your data, your machine.
- **The Index Is Disposable, the Memory Is Not:** every derived structure in MemVault can be thrown away and rebuilt, so a corrupt or stale index is never a data-loss event.
  - *markdown backend* — the `.md` files **are** the memory; `.memvault-cache.db` is a shadow index derived from them. Delete the cache and the next open rebuilds it from the vault. Edit a file by hand in Obsidian, in an editor, or via `git checkout`, and `memvault-cli reindex` (or `--watch`) reconciles it — content is diffed against the cached row and resynced on any mismatch. Your memories are plain text you can `grep`, `git blame`, and take to any other tool.
  - *sqlite backend* — `data.db` is the source of truth, and the derived parts inside it still self-heal: the FTS index is rebuilt automatically on open whenever its row count disagrees with `memories`, and the embedding column can be dropped and backfilled (switch embedding models, or run with no provider configured and add vectors later).
  - What is never derived: your content, your review state, your provenance, and the `memory_history` snapshot chain that powers rollback and `as_of` time travel. Note the one asymmetry — history snapshots live in the cache, so on the markdown backend a rebuilt cache loses the version chain and point-in-time reads fall back to the live row (`docs/DESIGN.md` §9.6). Expiry and everything else survive a rebuild intact.

---

## Why MemVault

Storing and retrieving memories is solved — a dozen products do it well, and
since 2025 the Code Agents do it natively. What is *not* solved is the part after
retrieval: getting one shared corpus into **every** agent you run, deciding
**what entitles a memory to bind** that agent, and knowing **whether it obeyed**.

The claims below come from a 15-product survey refreshed 2026-11
([`docs/DESIGN.md` §2](docs/DESIGN.md#2-market-analysis-and-competitors)). They
are falsifiable — the table there names every product and every source.

### What only MemVault does

| Capability | What it actually means | Who else has it |
|---|---|---|
| **Follow-through tracking** | `inject_session_id` traces what was injected into which session and measures whether the agent obeyed. Not "was it retrieved" — *was it followed* | **Nobody.** The closest is agent-memory's `audit`, which records who changed a memory, not whether an agent listened to one |
| **Instruction-layer enforcement** | MUST memories are never filtered, never truncated, and are rendered as directives rather than context. A memory becomes a *constraint*, not a suggestion | **Nobody.** `MemoryTier.CORE` in agent-memory and core blocks in Letta mean "resident in context", which is not the same claim |
| **Trust-gated binding** | What entitles a memory to bind an agent: provenance, identity verification, and an opt-in corroboration gate so one spoofed or compromised agent cannot unilaterally inject a MUST everyone obeys | **Nobody.** Multi-tenant ACLs answer *who may access*; this answers *what may be believed*. Different axis, and the survey has no column for it |
| **Injection explainability** | Every candidate dropped on the way into an agent's context is recorded with a reason — budget, cap, intent penalty, review gate. "Why didn't the agent get this memory?" always has an answer | agent-memory has retrieval *trajectory* (what was recalled). Nobody reports what was **dropped and why** |
| **Review-gated agent edits** | An agent can propose a correction to any memory, including a MUST — as a pending proposal that leaves the target live until a human decides | **Nobody.** Letta's agent self-edits memory directly, which is its own listed weakness |
| **Task-level evaluation** | `bench --judge` samples your own failure history and scores "plan without memory vs. plan with memory" against the known failure cause — task success, not retrieval recall | **Nobody.** Every published number in this space is a recall metric |

### What MemVault does differently

| | Claude Code Auto Memory | Codex Memories | Mem0 | Zilliz memSearch | **MemVault** |
|---|---|---|---|---|---|
| Which agents see the memory | Claude Code only | Codex only | whatever embeds its SDK | per-tool plugins | **every agent you run, from one store** |
| How it reaches the agent | platform loads its own files | platform loads its own files | you call `search` | you call `search` | **auto-injected** over MCP, proxy, *or* generated `AGENTS.md`/`CLAUDE.md` |
| Works without MCP | ✅ (its own files) | ✅ (its own files) | ❌ | ❌ | ✅ — `sync` writes the files Codex/Cursor/Aider/Jules already read |
| Source of truth | agent-written notes | session summaries | vector store | **Markdown files** | **your choice**: SQLite, or Markdown files with a rebuildable shadow index |
| Runtime | bundled | bundled | Python + a vector DB | Python + Milvus | **one Rust binary**, no service, no Python |
| Injection latency | — | — | p50 ≤ 1.1 s (published) | — | **p50 ≈ 1.2 ms** for the deterministic MUST path, measured with no embedding provider at all ([§2.6](docs/DESIGN.md#26-benchmark-landscape-what-to-measure-and-what-not-to)) |

> **On that latency comparison, honestly.** The two numbers are not measured the
> same way and should not be read as a like-for-like win. Mem0's published p50
> covers its whole retrieval pipeline; ours is the `/api/session` injection call
> alone, from a 20-probe plumbing run against a local debug build with no
> embedding provider configured. The defensible claim is narrower and still worth
> making: **MUST resolution does not touch an embedding model at all**, so its
> latency does not depend on one being installed, warm, or reachable — that is
> §17.3's two-phase design, and it is why `edge-lite` can turn embeddings off
> without losing MUST injection. A comparable end-to-end number needs the BEAM
> scored run, which has not happened.

Platform-native memory is the real competition now, and it is good — zero
infrastructure, zero configuration. Its structural ceiling is that **each platform
only loads its own silo**: Claude Code's memory directory is invisible to Cursor,
and Codex's memories do not leave Codex. If you run more than one agent, that is
the gap.

### Where MemVault is behind

Stated plainly, because a comparison that only lists wins is not a comparison:

| | Leader | MemVault today |
|---|---|---|
| **Multimodal input** | agent-memory (image/audio/video, text projections) | text only |
| **Graph relations** | Zep/Graphiti (temporal KG, multi-hop) | single-table one-hop triples; a real graph is deferred until relation volume demands it |
| **Cloud / team-hosted tier** | memSearch (Lite → Docker → Cloud), Mem0 | local-first only, by choice — see the positioning constraint in [§16](docs/DESIGN.md#16-longer-term-plans-not-started) |
| **Time travel over the vector path** | Zep (bi-temporal graph) | `as_of` recall covers the **keyword** index; a reworded memory is findable by its old words but not yet by its old meaning |
| **Published benchmark scores** | Mem0, memU, Zep (LoCoMo / DMR / LongMemEval) | harness built and plumbing-validated on BEAM, **no scored run yet** — deliberately not chasing LoCoMo, which a 1M-token context window now scores competitively without any memory system |

Everything in that table is tracked in [§16](docs/DESIGN.md#16-longer-term-plans-not-started)
with either a plan or a reason it is not being done.

MemVault complements your existing agent setup rather than replacing it. Keep your
LLM, your IDE, and your workflow exactly as they are. MemVault adds the memory
layer underneath — and the part that checks whether it was used.

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

> **Note:** `--transport sse` only mounts the MCP-over-HTTP endpoint (`/mcp`) — it does **not** expose the REST API (`/api/*`). The Web Dashboard is served by the REST backend (`memvault-mcp --transport http --serve-web <dist>`), and the Obsidian plugin also uses the REST API and requires `--transport http` instead. See [docs/INSTALL.md §2.6](docs/INSTALL.md#26-rest-api-required-by-the-obsidian-plugin).

### 21 MCP Tools

| Tool | Description |
|------|-------------|
| `save_memory` | Save with auto-embedding; delta-write by default (near-dupes skip, similar merge) — `force_insert` to bypass |
| `record_outcome` | Record a task outcome (episodic memory); failures reflect into lessons |
| `infer_outcome` | Classify + record a task outcome from host-provided evidence (friction counts, terminal state) instead of a raw transcript — same rule-floor-plus-optional-LLM-judge logic as the Stop hook's `outcome --infer`; built for hosts without a Claude-Code-shaped JSONL transcript (e.g. dsh/Cordis). Returns `skipped` (not an error) when nothing is inferred |
| `import_skills` | Import skills from a Markdown SOP (headings → skills, list items → steps) |
| `search_memory` | Hybrid (default) / keyword / semantic; semantic modes fall back to keyword when embeddings are unavailable |
| `session_start` | Agent-aware context injection; honors the agent's `inject_channel` (skips with an explanation when another channel is canonical); records compliance injections when enabled |
| `propose_memory_edit` | Propose a text edit to a memory **for human review** — creates a pending memory carrying the edit and leaves the target untouched and still injecting. Approving supersedes the target; rejecting discards the proposal. `old_text` must occur exactly once (an ambiguous anchor is refused, never guessed); `field` selects `content` or `instruction`. Use instead of delete+save when correcting a memory you did not author |
| `review_memory` | Approve / reject / edit; edit also works on already-approved memories, reject only applies to pending ones. Approving a pending **proposal** also supersedes its target |
| `delete_memory` | Soft delete — removes the live row and cascaded evidence relations, restorable via CLI `restore`; use `forget_memory` for irreversible purge |
| `forget_memory` | Permanently erase one memory or a criteria-matched bulk set (GDPR-style, irreversible); `dry_run` defaults to `true` |
| `extract_memories` | Structured extraction from text |
| `run_dedup` | Read-only deduplication scan (reports near-duplicates; does not merge/delete) |
| `run_decay` | Decay + auto-archive; opt-in permanent auto-forget when `MEMVAULT_FORGET_AUTO_ENABLED=1` |
| `confirm_read` | Mark read (increments `access_count`; repeated calls increment again) |
| `list_inbox` | List memories pending human review; `limit` only, no pagination |
| `run_promote` | Promote pipeline (L1→L2→L3), archive sources to L0 |
| `report_compliance` | Report follow/violate status for an injected session; bad items are skipped, so check the returned updated/total counts |
| `get_compliance_report` | Compliance rates per session or aggregate |
| `add_evidence` | Record evidence relations: supports / contradicts / sourced_from; identical relations are rejected rather than duplicated |
| `get_memory_evidence` | Get the raw-evidence chain a memory was distilled from (its L0 trace rows) plus its evidence profile — read-only grounding, so an agent can quote the original session text and name its sources |
| `get_effectiveness_report` | Automatic effectiveness judgments for injected memories (useful/neutral/harmful/insufficient-context rates, judged from `record_outcome`) — independent of the manual `report_compliance` flow |

### 2 MCP Resources

| URI | Content |
|-----|---------|
| `memory://user-profile` | MUST-level rules, auto-loaded on connect |
| `memory://project-context` | REFERENCE-level project context |

### Configuration (.env file & environment variables)

Copy [.env.example](.env.example) to `~/.memvault/.env` and uncomment what you need — it is also the single source of truth documenting every key.

Precedence (high → low): **CLI flags > process environment > `~/.memvault/.env` > `MEMVAULT_PROFILE` scenario profile > built-in defaults**. Every binary loads the env file first thing at startup; `--env-file <path>` or `MEMVAULT_ENV_FILE` points elsewhere, and a missing file is silently skipped. `memvault-cli status` prints where each setting came from (env / file / profile / default).

**Don't want to read fourteen switches?** `memvault-cli init --profile <name>` writes a coherent bundle instead — `coding-agent`, `assistant`, `team`, `edge-lite` — with each line commented by its rationale (`init --list` shows them all, `--dry-run` previews). A profile is the *lowest*-precedence layer: it fills only keys your shell and env file leave unset, so leaving one configured permanently can never silently undo your own tuning. It writes no secrets, only provider names. See [.env.example](.env.example) for what each profile sets and why.

Two groups are intentionally not in the table below: the host installation contract (`MEMVAULT_AGENT_ID`, `MEMVAULT_HOOK_EXTRACT`, … — per-agent values authored in each host's plugin/mcpServers config), and the proxy's `upstreams` topology (structured data, lives in `~/.memvault/proxy.yaml`).

| Variable | Purpose | Default |
|----------|---------|---------|
| `MEMVAULT_PROFILE` | Scenario profile applied at startup: `coding-agent` \| `assistant` \| `team` \| `edge-lite`. Fills only keys no higher layer set. An unknown name applies **nothing** and warns — it never falls back to a default profile, because a typo must not silently apply a different profile's settings. `memvault-cli init --profile <name>` writes the same settings into the env file instead, which is usually what you want | (none) |
| `MEMVAULT_EMBEDDING_PROVIDER` | Provider: `native` (in-process, default), `auto` (Ollama-first, native fallback), `ollama`/`local`, `openai`, `openai-compatible` (any OpenAI-compatible endpoint), or `none` (pure keyword, skips the download) | `native` |
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
| `MEMVAULT_HOOK_OUTCOME_MIN_FRICTION` | Minimum friction score for a Stop-hook `outcome --infer` to record a `Resolved` session; `0` disables the floor (mirrors `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION`) | `1` |
| `MEMVAULT_HOOK_OUTCOME_MIN_CONFIDENCE` | Minimum LLM-judge confidence to accept its `outcome --infer` verdict over the rule floor | `0.6` |
| `MEMVAULT_HOOK_COMPLIANCE` | `1` makes `SessionEnd` enqueue a durable compliance job and return immediately; the REST server worker judges it later. Missing transcripts, no judge, errors, and timeouts close rows as `unknown`, never fabricated `followed` | `1` |
| `MEMVAULT_COMPLIANCE_WORKER_TIMEOUT_SECS` | Per-job deadline for the REST server's compliance worker; on expiry the remaining rows become `unknown` | `90` |
| `MEMVAULT_COMPLIANCE_WORKER_POLL_MS` | Idle poll interval for the compliance worker | `1000` |
| `MEMVAULT_HOOK_DEBUG` | Set to `1` to write host-hook diagnostics to `${TMPDIR:-/tmp}/memvault-hook.log` | (off) |
| `MEMVAULT_IDENTITY_VERIFICATION` | Record whether a `save_memory` call's `agent_id` actually had a registered `agents.yaml` API key checked (`Memory.identity_verified`), vs. running unauthenticated. `false` stops recording it; recording alone never changes trust decisions | true |
| `MEMVAULT_CORROBORATION_GATE` | Opt-in MUST trust path: a MUST memory independently corroborated by enough distinct identity-verified agents (see `MEMVAULT_CORROBORATION_MIN_AGENTS`) is treated as trusted even without human review. `true` turns it on — off by default, so `is_trusted` output is unchanged unless you opt in | false |
| `MEMVAULT_CORROBORATION_MIN_AGENTS` | Minimum distinct identity-verified agents required for the corroboration gate above | `2` |
| `MEMVAULT_INJECT_REQUIRE_REVIEW` | Injection gate, **on by default**: any AI-extracted memory a human has not reviewed is removed from injection entirely — every priority, not just MUST — across the CLI/MCP body, the MCP `[MORE - 摘要]` overflow summaries, REST `/api/session`, and the proxy deterministic fast path, with an `unreviewed-gated` skip reason kept for audit. Unreviewed drafts stay in the review inbox and remain reachable via `search_memory`; they only enter new sessions after approval. Set `0` only for a deliberately unreviewed/experimental store, where unreviewed entries fall back to the `[未审核]` reference-data block | true |
| `MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST` | Legacy MUST-only render gate, kept for compatibility and as defense-in-depth when the broad gate above is disabled: with `MEMVAULT_INJECT_REQUIRE_REVIEW=0`, setting this to `true` still drops unreviewed MUST memories instead of rendering them as `[未审核]` reference data (`unreviewed-must-gated` skip reason) | false |
| `MEMVAULT_DB_POOL_SIZE` | SQLite connection pool size | `5` |
| `MEMVAULT_CORS_ORIGIN` | Comma-separated allowed CORS origins for REST (unset = localhost only) | (localhost only) |
| `MEMVAULT_SERVER_START_TIMEOUT` | Seconds `server start` / the SessionStart hook waits for a freshly spawned REST server to become healthy | `15` |
| `MEMVAULT_SERVE_WEB` | Web Dashboard static-asset directory served on `--transport http`, overriding the `dist/` embedded in the binary (`--serve-web` wins; ignored on stdio/sse) | embedded `dist/` |
| `MEMVAULT_IMPORT_ROOTS` | Path-list (`:`-separated on unix) of roots a caller-supplied `path` on `/api/agents/import/*` must live under (canonicalized before the check, so `..`/symlink escapes are rejected). Unset = no confinement (the endpoint is admin-gated) | — |
| `MEMVAULT_FORGET_AUTO_ENABLED` | Background auto-forget: permanently purge memories archived longer than the retention window (irreversible, unlike decay's reversible archiving) | `false` |
| `MEMVAULT_FORGET_RETENTION_DAYS` | Archived-retention window (days) for the auto-forget sweep | `90` |
| `MEMVAULT_FORGET_MUST_EXEMPT` | Protect MUST memories from forget unless the caller explicitly overrides (`allow_must`) | `true` |
| `MEMVAULT_FORGET_SHARED_AUTO_EXEMPT` | Exclude shared-visibility memories from the auto sweep (manual/bulk forget can still purge them) | `true` |
| `MEMVAULT_DB` | SQLite database path | `$MEMVAULT_HOME/data.db` when `MEMVAULT_HOME` is set, else `~/.memvault/data.db` |
| `MEMVAULT_STORAGE_BACKEND` | Storage backend: `sqlite` (default) or `markdown` (alias `md`) — markdown makes a directory of `{id}.md` files authoritative and an internal `.memvault-cache.db` a rebuildable index (see [`docs/DESIGN.md` §18](docs/DESIGN.md#18-storage-backends-sqlite-vs-markdown)) | `sqlite` |
| `MEMVAULT_MARKDOWN_DIR` | markdown-backend data directory (also `--markdown-dir` on cli/mcp/proxy); required when the backend is `markdown`. `backup`/`checkpoints`/`restore` stay sqlite-only | — (markdown mode only) |
| `MEMVAULT_MARKDOWN_TRUST_MODE` | Who is trusted to set a Markdown file's own trust metadata. `owned` (default) = local single user; hand-edited `human_reviewed`/`ai_generated`/`identity_verified`/priority/visibility are authoritative, so write access to the vault is session-injection authority. `quarantine` = shared/synced vault or low-trust writer; new files start unreviewed + `ai_generated`, priority capped at REFERENCE, layer pinned to L1, authority tags and forged ranking/freshness metadata stripped, `visibility: shared` downgraded to `scoped`, existing rows cannot be escalated, and symlinked `.md` entries are skipped. Unknown non-empty values fail closed to `quarantine` | `owned` |
| `RUST_LOG` | Log verbosity | `info` |
| `MEMVAULT_HOME` | Base directory: where `.env` lives, plus the model cache (`~/.memvault/models`) and `agents.yaml`. Environment-only — it can't be set inside the `.env` file itself (a file can't define its own location) | `~/.memvault` |
| `MEMVAULT_ENV_FILE` | Path to the env file (`--env-file <path>` also works); process-env only — the file can't relocate itself | `$MEMVAULT_HOME/.env` / `~/.memvault/.env` |
| `HF_ENDPOINT` | HuggingFace endpoint override for native model downloads (e.g. `https://hf-mirror.com` on CN networks) | (HuggingFace default) |
| `MEMVAULT_EXTRACT_ASSISTANT` | Proxy-path contextual extraction of agent-produced text: on by default but saved downgraded (`review:required`, lowered confidence — nothing agent-produced is trusted before human review). `off`/`disabled`/`false`/`0` disables extracting from agent responses entirely | (on — downgraded) |

---

## CLI Reference

`save` · `outcome` · `search` · `list` · `review` · `delete` · `session-start` · `compliance-enqueue` · `compliance-finalize` · `resource` · `extract` · `dedup` · `decay` · `forget` · `forget-log` · `doctor` · `promote` · `backup` · `export` · `export-skill` · `import` · `import-skills` · `import-agent` · `ingest` · `confirm-read` · `backfill-embeddings` · `sync` · `reindex` · `migrate-to-markdown` · `checkpoints` · `restore` · `supersede` · `status` · `bench` · `eval-history` · `server start|status|stop` · `dashboard`

```bash
memvault-cli <command> --help   # detailed usage per command
```

### Key Commands

| Command | What It Does |
|---------|--------------|
| `init` | Apply a scenario profile — `--profile coding-agent\|assistant\|team\|edge-lite` writes a coherent bundle of `MEMVAULT_*` defaults into your env file, each line commented with its rationale. `--list` shows every profile and setting; `--dry-run` previews. Profiles are the **lowest**-precedence layer, so they only fill keys your shell and env file leave unset and can never undo your tuning. Writes no secrets |
| `save` | Save a memory with priority, type, optional instruction. Delta-write by default: near-duplicates are skipped, similar memories absorb the residual; `--force` to bypass. `--expires-at <RFC3339>` (or `--ttl 30m\|12h\|7d`) sets a valid-time bound so a temporary fact retires itself |
| `outcome` | Record a task result (success/failure/partial); failures are distilled into lessons that auto-inject into similar future tasks |
| `search` | Hybrid retrieval with relevance scoring; flags: `--query`, `--top-k`, `--namespace`, `--as-of <RFC3339>` (point-in-time; recalls from the historical index too, so reworded memories are found by their old words — keyword path only), `--occurred-from` / `--occurred-to` (event-time window, a different axis from `--as-of`) |
| `as-of` | Show one memory as it stood at a past instant: `memvault-cli as-of <id> --at 2026-08-01T00:00:00Z`. Reconstructed from the `memory_history` snapshot chain; reports whether the answer came from the live row or a named snapshot, and says so plainly when there was no state then |
| `propose-edit` | Propose a text edit to a memory, for human review: `--id`, `--old`, `--new`, `--field content\|instruction`. Creates a **pending** memory and leaves the target untouched and still injecting; `review --approve` then supersedes it. `--old` must match exactly once — an ambiguous anchor errors rather than guessing. Warns when the edit would change no behaviour (body edited while the injected directive still says the old thing) |
| `init` | Apply a scenario profile — `--profile coding-agent\|assistant\|team\|edge-lite` writes a coherent bundle of `MEMVAULT_*` defaults into your env file, each line commented with its rationale. `--list` shows every profile and setting; `--dry-run` previews. Profiles are the **lowest**-precedence layer, so they only fill keys your shell and env file leave unset and can never undo your tuning. Writes no secrets |
| `list` | List memories; `--limit`, `--namespace`, and `--as-of <RFC3339>` to list what was in effect at a past instant — including memories superseded, expired or deleted *since* then, which were the current truth at that point |
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
| `dashboard` | Ensure the local REST backend is running (spawns `memvault-mcp --transport http` detached if needed) and open the Web Dashboard in your browser |
| `server start` / `server status` / `server stop` | Manage that same backend: idempotent start (health-probes first, auto-moves to the next free port when 3777 is taken by a foreign process), status, and stop. Prints the active URL (with `--quiet`, the URL alone) |
| `doctor` | Read-only memory hygiene lint: dangling/stale/duplicate/contradicted + machine-readable `--json` |
| `bench` | Task-level memory benchmark: samples your own outcome history, measures lesson retrieval/injection rates; `--judge` adds an LLM-scored "plan without vs. with memory" success delta; each run persists itself for `eval-history` |
| `eval-history` | Trend-over-time view of past `bench`/`doctor` runs — every run persists itself automatically, this just lists what accumulated |
| `decay` | Archive stale memories based on access recency; also archives memories past their `expires_at` bound regardless of score (and regardless of MUST-exemption), reporting `expired_archived` separately from score-based `archived` |
| `backup` | Create a consistent point-in-time SQLite backup |
| `export` / `import` | Backup and restore — JSON to a file or a directory (writes `export.json` inside); Markdown to/from a directory or a single `.md` file; import is idempotent (ids already present are skipped, never overwritten) |
| `confirm-read` | Mark memories as read (updates access_count) |

---

## Integrations

### Installing into your agents

MemVault ships native adapters for most agents — one shared store, per-host identity via `MEMVAULT_AGENT_ID`, four tiers (Tier 1/2/3 details below; per-client registration snippets in [integrations/mcp-clients/](integrations/mcp-clients/)).

**Core packages & versions:**

| Package | Version | Install / Source |
|---|---|---|
| `memvault-core` (crates.io) | 0.6.12 | library dependency |
| `memvault-cli` · `memvault-mcp` · `memvault-proxy` (crates.io) | 0.6.12 | `cargo install memvault-cli memvault-mcp memvault-proxy` |
| `@dreamor/dsh-memvault` (npm) | 0.6.12 | npm registry (dsh Cordis plugin, below) |

**Shipped plugins & versions** — each bumps on its own schedule:

| Plugin | Version | Distributed via |
|---|---|---|
| Claude Code / Codex plugin bundle ([`plugins/memvault/`](plugins/memvault/)) | 0.6.12 | `dreamor/memvault` marketplace |
| Gemini CLI extension (`gemini-extension.json`) | 0.6.12 | `gemini extensions install` |
| Qoder plugin (`.qoder-plugin/`) | 0.6.12 | in-repo manifest |
| Obsidian plugin ([`obsidian-plugin/`](obsidian-plugin/)) | 0.3.7 | [`dreamor/memvault-obsidian`](https://github.com/dreamor/memvault-obsidian) (BRAT / community dir) |
| dsh Cordis plugin ([`dsh-plugin/`](dsh-plugin/)) | 0.6.12 | npm [`@dreamor/dsh-memvault`](https://www.npmjs.com/package/@dreamor/dsh-memvault) |

**Tier 1 — one-command native plugins** (memory injected by hooks; extraction opt-in where the host exposes lifecycle hooks):

| Agent | Install | Recall | Extract |
|---|---|---|---|
| **Claude Code** | `/plugin marketplace add dreamor/memvault`, then `/plugin install memvault@memvault` (two separate prompts) — bundles the MCP server, 4 skills, 4 slash commands | ✅ SessionStart hook | ✅ Stop hook, enable with `MEMVAULT_HOOK_EXTRACT=1` |
| **OpenCode** | merge [`integrations/opencode/opencode.json`](integrations/opencode/opencode.json) into your project | ✅ system transform | ✅ on `session.idle` |
| **DeepSeek Harness (dsh)** | built-in Cordis plugin [`dsh-plugin/`](dsh-plugin/) — see [docs/INSTALL.md §2.5](docs/INSTALL.md#25-deepseek-harness-dsh) | ✅ system prompt | ✅ turn-end |
| **Gemini CLI / Antigravity** | `gemini extensions install https://github.com/dreamor/memvault` | ⚠️ rule context + tools | ❌ |
| **Codex CLI** | `codex plugin marketplace add dreamor/memvault`, then install `memvault@memvault` from the plugin browser — the same [`plugins/memvault/`](plugins/memvault/) bundle; manual fallback in [integrations/codex/](integrations/codex/) | ✅ SessionStart hook — approve the bundled hook once in `/hooks` | ✅ Stop hook — approve in `/hooks`; opt-in via `MEMVAULT_HOOK_EXTRACT=1` / `MEMVAULT_HOOK_OUTCOME=1` |

⚠️ = the host has no injection hooks; recall is rule-driven (the agent calls `session_start` once) with the bundled canonical rule text.

**Tier 2 — paste an MCP snippet.** Strict-JSON registrations with per-host identities in [integrations/mcp-clients/](integrations/mcp-clients/) (target paths in its [README](integrations/mcp-clients/README.md)): Cursor · Windsurf · Cline/Roo · Continue · Zed · JetBrains AI/Junie · VS Code (Copilot Chat) · Claude Desktop.

**Tier 3 — native manifests.** Qoder (`.qoder/rules/` + `.qoder-plugin/` + a `UserPromptSubmit` hook template), Grok Build (`grok plugin install dreamor/memvault --trust`), the Hermes Python plugin ([integrations/hermes/](integrations/hermes/), `pre_llm_call` recall + extraction helper) and the pi extension (`pi-extension/`, declared by the root `package.json` `pi` manifest; `pi install git:github.com/dreamor/memvault`) ship in-repo; OpenClaw and Swival consume the generated root `skills/` (also exported to `.openclaw/skills/`); Devin stays a manual recipe in [integrations/README.md](integrations/README.md).

**Tier 4 — instruction-only rule copies.** Canonical text + `scripts/gen-rule-copies.sh` (parity-checked in CI) produce `AGENTS.md`/`CLAUDE.md` blocks, `.cursor/rules/`, `.clinerules/`, `.kiro/steering/`, Junie guidelines; `memvault-cli sync --watch` keeps them fresh from the store.

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
│  │  21 tools    │ │  2 Resources   │ │ SSE    │  │
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
cargo test                      # ~1590 tests (full workspace)
cargo clippy --all-targets      # zero warnings
cargo fmt --all -- --check      # format check
cargo llvm-cov --workspace --all-features   # CI gate: line ≥92% / region ≥90% / function ≥85%
cd dashboard && npm test        # Vitest (UI + API layer)
bash scripts/e2e-release-check.sh           # end-to-end, real binaries + real REST server
python3 docs/experiments/beam_adherence.py --dry-run --with-server   # benchmark harness plumbing
```

The unit and integration suites are the primary gate, but `e2e-release-check.sh`
exists because several defects were visible **only** by running the built binaries
end to end — an edit that approved cleanly and changed no behaviour, a second
injection channel that bypassed the review gate, a provenance field dropped by the
reranker. All of them lived in the wiring *between* layers, where every layer's own
tests passed. It needs no LLM key and no embedding provider, so it is safe to gate
a release on, and CI runs it on every push.

---

## Documentation

| Doc | Content |
|-----|---------|
| [docs/DESIGN.md](docs/DESIGN.md) | Product & architecture design |
| [docs/INSTALL.md](docs/INSTALL.md) | Installation guide (all platforms) |
| [docs/AGENT-INSTALL.md](docs/AGENT-INSTALL.md) | Agent-facing install guide — the target of the one-prompt Quick Start (install → register as MCP → verify recall) |
| [docs/DOCKER.md](docs/DOCKER.md) | Docker deployment |
| [docs/RUNBOOK.md](docs/RUNBOOK.md) | Deployment / health check / rollback runbook |
| [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) | Symptom → cause → fix troubleshooting guide |
| [docs/experiments/](docs/experiments/README.md) | Two kinds of evidence. **Hypothesis validation** (H1–H7, 2026-08-11 → 2026-08-27, all CONFIRMED) + runtime plumbing regression (2026-08-28) — MemVault's own design assumptions, on hand-written scenarios. **External benchmark harness** ([`beam_adherence.py`](docs/experiments/beam_adherence.py), 2026-11) — BEAM instruction/preference adherence, the public axis that measures whether an injected rule was *obeyed*; plumbing-validated, no scored run yet |
| [docs/RELEASING.md](docs/RELEASING.md) | Release process — what CI automates (Linux/macOS binaries, Docker image, dashboard archive, Obsidian zip) vs. manual steps (Obsidian submission — no macOS signing needed) |
| [docs/DISTRIBUTION.md](docs/DISTRIBUTION.md) | Distribution channel map — automated vs. manual channels, required credentials, MCP registries, optional channels |
| [CHANGELOG.md](CHANGELOG.md) | Release history |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Contribution guide |
| [SECURITY.md](SECURITY.md) | Security disclosures |
| [ACCESSIBILITY.md](ACCESSIBILITY.md) | Accessibility statement — commitment, supported environments, known limitations, how to report a barrier |
| [.env.example](.env.example) | Configuration template — single source of truth for every config key |

---

## Contributing

- 🐛 **Bugs:** [Issue Tracker](https://github.com/dreamor/memvault/issues/new)
- 💡 **Ideas:** [Feature Request](https://github.com/dreamor/memvault/issues/new)
- 📖 **Guide:** [CONTRIBUTING.md](CONTRIBUTING.md)
- 🔒 **Security:** [SECURITY.md](SECURITY.md)
- ♿ **Accessibility:** [ACCESSIBILITY.md](ACCESSIBILITY.md)

---

## License

MemVault is released under the [MIT License](LICENSE).
