# Cross-Agent Personal Memory Tool — Product & Architecture Design

> **Project**: MemVault
> **Version**: v0.7.x design reference for the shipped system
> **Positioning**: a personal memory router for the AI-agent era
> **One-liner**: don't teach agents to query memories — make memories appear in front of the agent automatically.
> **Core promise**: any MCP-compatible agent that connects shares the same user memory automatically.

---

## Table of contents

1. [Vision and the core problem](#1-vision-and-the-core-problem)
2. [Market analysis and competitors](#2-market-analysis-and-competitors)
3. [The core pain: three memory disconnects](#3-the-core-pain-three-memory-disconnects)
4. [Path choice: independent tool + thin clients](#4-path-choice-independent-tool--thin-clients)
5. [Final architecture (with the Memory Router)](#5-final-architecture-with-the-memory-router)
6. [Memory Router core modules](#6-memory-router-core-modules)
7. [Technology stack](#7-technology-stack)
8. [Data model and schema](#8-data-model-and-schema)
9. [Core functional modules](#9-core-functional-modules)
10. [Roadmap status](#10-roadmap-status)
11. [Differentiation strategy](#11-differentiation-strategy)
12. [Risks and mitigations](#12-risks-and-mitigations)
13. [Multi-agent shared memory design (added in v0.3)](#13-multi-agent-shared-memory-design-added-in-v03)
14. [Three-track memory evolution: delivered state](#14-three-track-memory-evolution-delivered-state)
15. [Delivery ledger: shipped, deferred, not doing](#15-delivery-ledger-shipped-deferred-not-doing)

---

## 1. Vision and the core problem

### 1.1 The core pain

Across today's AI-agent ecosystem, memory suffers from **three disconnects**:

User expectation: I told you → you remembered → you recall it next time automatically → and you act on it.

What actually happens:

- ❌ Disconnect 1: stored but not found (low recall)
- ❌ Disconnect 2: found but not injected (the agent never thinks to query)
- ❌ Disconnect 3: injected but not followed (the agent sees it and ignores it)

### 1.2 Vision

> **Build a "memory router" for AI agents — before an agent receives a request, automatically inject the relevant memories into its context in instruction format.**

This gives the user:

- **Data sovereignty**: memories belong to the user, not to any single agent
- **Automatic recall**: agents don't need to "remember to look up" — memories appear automatically
- **Follow-through guarantees**: memories are injected as instructions the agent must honor
- **Human-AI collaboration**: users can review, edit, and delete what the AI "understood" about them
- **Privacy first**: local-first architecture; sensitive data never leaves the machine

### 1.3 Design principles

| Principle | Meaning |
|-----------|---------|
| User-centric | the data model is `User → Memory`, not `Agent → Memory` |
| Auto-injection first | does not rely on agents calling tools — the router distributes automatically |
| Instructional format | memories are not "descriptions", they are "instructions" the agent must honor |
| Memory/presentation decoupled | the core engine is independent of any frontend |
| Open standards | built on MCP, Markdown-compatible, migratable at any time |
| Forgetting | active forgetting, archiving, and decay policies |

---

## 2. Market analysis and competitors

### 2.0 How this section is maintained

> **Refreshed 2026-11** against a 15-product survey of the memory-layer market
> (Mem0, Letta/MemGPT, Cognee, MemOS, OpenViking, Zilliz memSearch, Claude Code,
> Cursor, Codex, Zep/Graphiti, ChatGPT Memory, Supermemory, memU, LangMem,
> AgentScope+ReMe), one benchmark survey, and one in-flight peer
> (openJiuwen **agent-memory**, §2.3).
>
> The previous revision tracked a different generation of competitors
> (MemPalace, TiMEM, Memoria, and the Obsidian ecosystem). Those are retained in
> §2.7 for continuity but are **no longer the reference set** — none of them
> appear in current market surveys.
>
> The load-bearing conclusion changed in this refresh: **auto-injection is no
> longer an empty space.** §2.4 states the corrected gap and §11 is recalibrated
> around it. Any future edit to §2 must re-check §11.1 in the same commit.

### 2.1 Tier 1: framework-agnostic memory layers

| Project | Storage paradigm | Recall approach | Deployment | Gap MemVault exploits |
|---------|------------------|------------------|------------|-----------------------|
| **Mem0** | vector facts (+ graph in Mem0g, often a paid tier) | vector + BM25 + entity boost | self-host + cloud | `add`/`search` only — the agent must call proactively. No injection, no follow-through |
| **Zep / Graphiti** | temporal knowledge graph (bi-temporal, `t_valid`/`t_invalid`) | vector + BM25 + graph + time | SaaS; Graphiti OSS | best time-travel in the market, but heavy (Neo4j) and enterprise-shaped; passive |
| **Letta (MemGPT)** | OS-style paging: core blocks (resident) + archival (retrieved) | resident context + vector | self-host + cloud | elegant self-editing memory, but the agent alone decides what to write (misses/misrecords); no cross-tool sharing |
| **Cognee** | graph + vector, ECL pipeline | vector + graph traversal | self-host + cloud | institutional knowledge, not a personal profile; passive |
| **MemOS** | three-layer OS; heterogeneous memory (plaintext/activation/parameter); composable memory cubes | FTS5 + vector + graph topology | edge **and** cloud plugins | the most systematic governance in the survey, but a large concept surface; no instruction-layer enforcement |
| **OpenViking** (Volcengine) | `viking://` virtual filesystem unifying memory + resource + skill | path browse (`ls`/`find`) + recursive directory descent | self-host REST | its **observable retrieval trajectory** and **L0/L1/L2 progressive loading** are both shipped — disclosure is §6.5, and every dropped candidate carries a reason; single self-hosted form, weak multimodal |
| **Zilliz memSearch** | **Markdown as source of truth**, Milvus as a rebuildable *shadow index* | vector + BM25 + RRF | Lite → Docker → Zilliz Cloud | the Code-Agent-native model — MemVault's markdown backend (§8.3.2) already matches it. Retrieval/storage only: no fact extraction, no conflict resolution |
| **LangMem** | flat KV + vector (CoALA taxonomy) | vector | self-host | bound to LangGraph |
| **AgentScope + ReMe** | short/long two-layer + graph + SQLite audit log | vector + graph + rerank | self-host + Aliyun | bound to AgentScope; long-term memory is an external dependency |
| **Supermemory** | one-shot Context Stack (memory + RAG + profile + connectors) | automatic hybrid | closed cloud; self-host enterprise-only | its **auto-expiry of temporary facts** was copied as `expires_at` (§9.6); otherwise closed and locked-in |
| **memU** | filesystem-layered (category = folder, item = file), Markdown | RAG + LLM dual-mode | local-first | highest published LoCoMo score (92.09%); the intention layer is still unreleased; license is NOASSERTION |
| **ChatGPT Memory** | explicit saved facts + implicit profile summary | RAG injection | closed cloud | a consumer product, not integrable infrastructure — reference only |

### 2.2 Tier 2: platform-native memory (the new competitive front)

> **This tier did not exist when §11 was first written.** All three major Code
> Agents now ship auto-memory with zero infrastructure, which lands directly on
> MemVault's original L2 claim.

| Platform | Mechanism | What it covers | Structural ceiling |
|----------|-----------|----------------|--------------------|
| **Claude Code** | `CLAUDE.md` (hand-written, walks **up** the directory tree) + **Auto Memory** (v2.1.32): agent-written notes in `~/.claude/projects/<p>/memory/`, `MEMORY.md` index (first 200 lines / 25KB) + per-topic files loaded on demand | auto-write **and** auto-load; the layered index saves tokens | Claude Code only — Cursor and Codex cannot read it. No semantic retrieval, no graph, no time axis. A large `CLAUDE.md` is swallowed whole |
| **Codex** | `AGENTS.md` (hierarchical: global `~/.codex/AGENTS.md` + repo-root→cwd **downward** walk, monorepo stacking) + **Memories** (background session summarisation into `~/.codex/memories/`, auto-read next session) + native MCP | agent-written memory without hand-copying facts into Markdown | summary-shaped, no semantic retrieval; deep persistent memory still needs an MCP add-on |
| **Cursor** | native **Memories** (a background model auto-proposes facts, the user approves **item by item**) + `.cursor/rules/*.md` + MCP add-ons | the propose-then-approve review loop | short preferences only; project-scoped; not shared across tools |

**Two things this tier validates, and one thing it does not:**

- ✅ **It validates the file-injection channel.** `AGENTS.md` was donated to the
  Linux Foundation's Agentic AI Foundation in 2025-12 and is now read by
  Codex / Cursor / Aider / Jules. MemVault's `sync` and the Tier-4 rule copies
  target exactly this — and it is the **only injection channel that does not
  require MCP support**, so it reaches agents MemVault cannot otherwise touch.
- ✅ **It validates human-in-the-loop review.** Cursor's per-item approval is the
  same loop as MemVault's Inbox (§9.3), except MemVault's applies across every
  agent rather than one IDE.
- ❌ **It does not cover cross-agent sharing.** Claude Code's memory directory is
  invisible to Cursor; Codex's memories do not leave Codex. Every product in this
  tier is a **single-tool silo** — which is what remains of MemVault's L2.

### 2.3 In-flight peer: openJiuwen `agent-memory`

> Tracked separately because it is the only surveyed project that is both
> (a) shipping real code rather than a design document and (b) aimed at the same
> "neutral memory substrate" position. Apache-2.0, ~198K LOC Python across 851
> modules, 310 test files, three docker-compose profiles (local / online /
> postgres), an encryption module, and **38 public APIs already implemented**.
> Its design docs trace every decision back to a surveyed competitor, so it is
> also the clearest available read on where this market is heading.

| It has, MemVault lacks | Note |
|------------------------|------|
| Bi-temporal model + `as_of` | `t_event` / `t_ingest` / `t_valid` / `t_invalid`; `search(as_of=…)` and `get(as_of=…)` **shipped**. The design detail worth copying is the split: `as_of` is *valid-time* ("which version was in effect at T"), while `time_from`/`time_to` parsed from the query is *event-time* ("when did it happen"). The two axes are **never folded together**. → **MemVault has since copied the split, not just the feature** (§9.6): `as_of` + `expires_at` shipped on valid-time, leaving the pre-existing `occurred_at` untouched as event-time. The event-time filter also shipped: `occurred_from` / `occurred_to` (§9.6) |
| L0/L1/L2 progressive disclosure | per-unit `ContentLayers` (l0 summary / l1 fragment / l2 = the existing full content, not duplicated), disclosed against a `max_tokens` budget. Per-memory, not a global summary — an order of magnitude cheaper than the "dual-track injection" item in §15.2 |
| `edit_md_file` | Letta-style self-editing on a Markdown source of truth: exact unique-`old_text` replacement, atomic write-back, inherit-and-SUPERSEDE. **It writes straight to the source of truth** — MemVault's version must stop at the Inbox first (§8.3.4) |
| Multi-tenant governance | `org + space` hard isolation, configurable `principal_path` (`user_agent` / `agent_user`), grant/revoke, space lifecycle, quotas, `verify_audit` chain-integrity |
| Scenario presets | 11 configurable dimensions + 4 profiles (coding agent / personal assistant / enterprise multi-agent / edge-lite) — "既能全、也能轻" |
| Multimodal ingest | `segments[] + assets[]`, pluggable ASR/OCR/caption normalisers. Note that **retrieval stays text-only** — no multimodal vectors — which is the cheap way to do it |
| Fuser de-duplication across disclosure layers | L0/L1/L2 are *physical entries of the same channel*, not separate channels: it applies MaxP per (channel, unit) **before** cross-channel fusion, so a memory with more layers is not systematically over-weighted. MemVault avoids it precisely because §6.5's layers are derived at injection time and never indexed |

| It lacks, MemVault has | Note |
|------------------------|------|
| **Follow-through tracking** | it has `audit` (who changed a memory) and `verify_audit` (chain integrity) — that is *governance* auditing, not "did the agent obey". No equivalent anywhere in the survey |
| **Instruction-layer enforcement** | `MemoryTier.CORE` means *resident in context* (≈ Letta core memory), not *must be obeyed*. MemVault's **reviewed** MUST is never down-ranked, type-filtered or truncated, and a violation is an error (§6.2) — a semantic promotion nobody else makes |
| **Trust provenance** | its `provenance` is *data lineage* (which units this one was synthesised from, for `trace`). MemVault's `ai_generated` / `human_reviewed` / `identity_verified` + the corroboration gate (§13.7) is *trust provenance* (what entitles a memory to bind an agent). Different axis; the survey has no column for it |
| Single-binary distribution | Rust, no runtime, no compose file, no `python ≥ 3.11` |

### 2.4 Key finding: the market gap (recalibrated 2026-11)

> **Original claim (v0.3):** *"No existing project owns the full chain of Memory
> Router / auto-injection / follow-through guarantee."*
>
> **That is now half wrong, and the correction is load-bearing.**

```
Passive memory (most of Tier 1):
  User message → Agent → [agent decides whether to call search_memory] → LLM

Platform-native memory (Tier 2, shipped 2025-26):
  User message → Agent → [platform auto-loads its own memory files] → LLM

MemVault:
  User message → [Memory Router intercepts] → retrieve + inject → Agent (already carries memories) → LLM
                                                                   ↘ compliance tracked on the way out
```

Auto-**loading** is no longer an empty space: Claude Code, Codex and Cursor all
do it natively, with zero infrastructure and zero configuration. What remains
unowned by anyone in the survey:

1. **Cross-agent auto-injection.** Tier 2 auto-loads *its own silo*. Nobody
   injects one shared corpus into every agent the user runs.
2. **Follow-through.** Nobody measures whether an injected memory was obeyed.
   `inject_session_id` + compliance rate + the SessionEnd finaliser (§13.9) is
   still empty space.
3. **Trust-gated binding instructions.** Nobody asks *what entitles this memory
   to bind the agent*. Identity verification + multi-agent corroboration (§13.7)
   + the `owned`/`quarantine` boundary (§8.3.4) has no counterpart in the entire
   survey — including the one project that has a three-layer authorisation stack,
   because that stack answers *who may access*, not *what may be believed*.

MemVault's moat therefore moves **down** the stack: from "we inject
automatically" (now table stakes) to "we inject **one shared, trusted, enforced**
corpus, and can prove it was followed". §11.1 records this as L2 → 🟡 and a new
L6 → 🔴.

### 2.5 Comparison, on two different dimension sets

The market measures memory systems on six dimensions (agent type / input sources
/ retrieval / modality / storage paradigm / edge-cloud). MemVault loses several
of those rows and should not pretend otherwise. The second table uses dimensions
**no surveyed product is measured on**.

**On the market's dimensions:**

| Dimension | Mem0 | Zep | memSearch | agent-memory | **MemVault** |
|-----------|------|-----|-----------|--------------|--------------|
| Hybrid retrieval | ✅ vec + BM25 + entity | ✅ vec + BM25 + graph + time | ✅ vec + BM25 + RRF | ✅ 4 index types, config-gated | ✅ BM25 (FTS5 + CJK bigram) + vec (int8) + RRF + synonym expansion |
| Temporal / time-travel | ❌ | ✅ **best in market** | ❌ | ✅ bi-temporal + `as_of` | ✅ `as_of` on get/list/search (search recalls from a historical FTS index) + `expires_at` + event-time windows; ⚠️ the historical index is keyword-only → §9.6 |
| Graph relations | ⚠️ paid tier | ✅ | ❌ | ✅ config-gated | ⚠️ single-table one-hop (`memory_relations`) |
| Markdown as source of truth | ❌ | ❌ | ✅ | ✅ | ✅ (§8.3.2 inverted data-flow) |
| Progressive disclosure (L0/L1/L2) | ❌ | ❌ | ❌ | ✅ stored layers | ✅ derived on demand (§6.5) — cheaper and cannot go stale |
| Multimodal input | ❌ | ❌ | ❌ | ✅ | ❌ → §15.2 |
| Edge **and** cloud | ✅ | ⚠️ cloud-first | ✅ Lite → Cloud | ✅ 3 profiles | ⚠️ local-only → §15.3 (deliberate, positioning constraint) |
| Retrieval observability | ❌ | ❌ | ❌ | ✅ trajectory | ✅ **finer** — records why each candidate was *dropped*, not just what was recalled |
| Task-level evaluation | ❌ | ❌ | ❌ | ❌ (plans LoCoMo/BEAM) | ✅ `bench --judge` (§9.7) |

**On dimensions the survey does not measure:**

| Dimension | Mem0 | Zep | Letta | MemOS | memSearch | Claude Code | Codex | Cursor | agent-memory | **MemVault** |
|-----------|------|-----|-------|-------|-----------|-------------|-------|--------|--------------|--------------|
| Cross-agent shared corpus | ⚠️ SDK-level | ❌ | ⚠️ shared blocks | ⚠️ memory cubes | ⚠️ per-tool plugins | ❌ | ❌ | ❌ | ✅ multi-tenant, not personal | ✅ **one store, every agent** |
| Instruction-layer enforcement (MUST) | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ **only** |
| Follow-through / compliance tracking | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ **only** |
| Trust provenance + corroboration gate | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ✅ **only** |
| Injection explainability (drop reasons) | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ | ⚠️ trajectory | ✅ **only** |
| Zero-runtime single binary | ❌ Python | ❌ Python + Neo4j | ❌ Python | ❌ Python | ❌ Python + Milvus | ⚠️ bundled | ⚠️ bundled | ⚠️ bundled | ❌ Python ≥ 3.11 | ✅ **Rust** |

> **Positioning consequence.** Competing on the first table means chasing
> modality (Cognee/memU), time (Zep), graph (Graphiti) and cloud form
> (memSearch) — all losing rows, against better-funded projects. Competing on
> the second table means MemVault is the only player. §11 is written around the
> second table; the first is tracked honestly so that §15.1 records the rows that
> are already closed (`as_of`, disclosure layers) and §15.2/§15.3 say what will
> not be chased.

### 2.6 Benchmark landscape: what to measure, and what not to

> The load-bearing finding here is counter-intuitive and changes what MemVault
> should spend evaluation effort on.

As context windows passed 1M tokens, the **early retrieval-shaped benchmarks
stopped discriminating**: on LoCoMo and LongMemEval the naive "stuff the entire
history into context" baseline now scores competitively. A good LoCoMo number
therefore proves nothing about memory architecture — MemVault could spend a
quarter earning a number that a longer context window earns for free.

| Benchmark | Scale | What it actually tests | Relevance |
|-----------|-------|------------------------|-----------|
| **BEAM** (ICLR 2026) | 100 dialogues / 2,000 probes / 128K·500K·1M·10M | 10 abilities incl. contradiction resolution, cross-session identity consistency, **instruction/preference adherence** | 🎯 **primary target.** The adherence axis is the only public metric where MUST + compliance tracking can win by a wide margin — and win for a reason that is explainable rather than incidental |
| **MemoryAgentBench** | incremental multi-turn, 100K+ | accurate retrieval / test-time learning / long-range understanding / **conflict resolution & selective forgetting** | 🎯 maps directly onto delta-write (§9.1.1) and the three forget paths (§9.5) |
| **LongMemEval** | 500 QA; S ≈ 115K, M ≈ 1.5M tokens | extraction, multi-session reasoning, temporal, **knowledge update**, abstention | ⚠️ becomes useful for the temporal axis once `as_of` ships. **Swings ~±10% on judge prompt** |
| **LoCoMo** | 10 dialogues / ~1,540 QA | single-hop, multi-hop, temporal, adversarial | ⚠️ entry ticket only; community reports annotation noise |
| **PersonaMem v2** | 1,000 personas / 20K+ preferences / 5,000 QA | implicit preference tracking | relevant to profile injection quality |
| **LifeBench** | 10 users / one full year / ~66M tokens | multi-source, non-declarative memory | out of scope for now |

**Where to publish.** **AMB (agentmemorybenchmark.ai)** is currently the only
neutral, unified online leaderboard; it aggregates beam / lifebench / locomo /
longmemeval / personamem / memsim / membench / ama-bench under one harness. Most
academic benchmarks have **no official leaderboard at all** — scores are
scattered across papers and vendor self-reports using incompatible judge models,
retrieval settings and prompts. **Any number MemVault publishes must carry its
full harness specification**, or it is not comparable and should not be cited.

**Engineering metrics must be reported next to accuracy** (the survey's explicit
recommendation; mem0's published frame is ~6.7–7.0K tokens per retrieval,
p50 ≤ 1.1s). This is where two-phase injection (§6.1.2) — MUST resolved
deterministically with **zero embedding calls** and served immediately, the
semantic pipeline landing inside a 250 ms window — should show a structural
advantage from Rust plus in-process native embedding. **That advantage is
currently unmeasured, which for positioning purposes is the same as not having
it.** → §15.1 (harness exists; no published run yet)

### 2.7 Legacy reference set (pre-2026, retained for continuity)

> Kept because §11.3 and older CHANGELOG entries refer to these by name. They are
> **not** the competitive reference set any more: none appear in the 2026 survey,
> and MemPalace — previously treated as the closest peer and a pluggable storage
> backend — is not mentioned in any current market analysis.

**Tier 1 (as assessed in v0.3):**

| Project | Core capability | Recall approach | Gap |
|---------|-----------------|-----------------|-----|
| **MemPalace** | memory-palace hierarchy + ChromaDB | structured hierarchical retrieval, high LongMemEval R@5 | no auto-injection; no human review; purely passive tools |
| **Mem0** | hybrid vector + graph + KV storage | hybrid retrieval + graph enhancement | only add/search APIs; the agent must call proactively |
| **Zep** | temporal knowledge graph | graph traversal + temporal reasoning | passive tool mode; enterprise closed source |
| **TiMEM** | temporal hierarchical memory tree | tops benchmarks | academic prototype, not productized |
| **LangMem** | ecosystem component | LangGraph memory integration | framework-bound, not a standalone product |
| **Letta** (MemGPT) | OS-style memory management | paged context | oriented to agent runtimes |
| **Memoria** | trusted-memory framework | security auditing | recently open-sourced, incomplete |

**Tier 2 — the Obsidian ecosystem:**

| Project | Core capability | Gap |
|---------|-----------------|-----|
| **Khoj** | local RAG + Obsidian plugin | Q&A only; no memory write/review/auto-inject |
| **Smart Connections** | semantic search + backlink recommendations | retrieval only; no write pipeline |
| **mcp-obsidian** | MCP file read/write proxy | file-level operations, no semantic abstraction |
| **Claudian** | Claude + skills system | bound to a single model |

> **One entry in the legacy table deserves a second look.** `Memoria` was
> recorded here as a *trusted-memory framework* that was "recently open-sourced,
> incomplete" — and it does not appear in the 2026 survey at all, which suggests
> it did not survive. The "trusted memory" position it was reaching for is
> therefore still unoccupied, and MemVault now holds most of the parts: the
> corroboration gate (§13.7), honest compliance accounting (§13.9), the
> `owned`/`quarantine` trust boundary (§8.3.4) and the SHA-256 erasure
> tombstone (§9.5). §11.1 promotes this to L6.

---

## 3. The core pain: three memory disconnects

### 3.1 Disconnect analysis

```
┌─────────────────────────────────────────────────────────────┐
│ Disconnect 1: stored but not found (low recall)             │
│ Cause: flat storage, single retrieval mode, no structured   │
│ filtering                                                   │
│ Fix: hybrid retrieval + structured filtering + query        │
│ rewriting                                                   │
├─────────────────────────────────────────────────────────────┤
│ Disconnect 2: found but not injected (agents don't query)   │
│ Cause: memory tools are passive; the agent must "think of"  │
│ calling them                                                │
│ Fix: Memory Router auto-interception + pre-prompt injection │
├─────────────────────────────────────────────────────────────┤
│ Disconnect 3: injected but not followed (agent ignores)     │
│ Cause: memories are descriptive, not instructional; too     │
│ many cause overload                                         │
│ Fix: MUST/REF tiers + instructional format + token budget   │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 Follow-through vs. number of injected memories

| Injected memories | Approx. follow-through |
|-------------------|------------------------|
| 1-3 | ~95% |
| 4-7 | ~80% |
| 8-15 | ~60% |
| >15 | ~30% (overload) |

**Conclusion**: inject at most 5-8 memories per turn; MUST-priority memories always come first.

---

## 4. Path choice: independent tool + thin clients

### 4.1 Decision

> **Build an independent tool with an "Engine + Memory Router + Thin Clients" architecture.**
>
> Storage is MemVault's own local-first backend, switchable between SQLite and
> Markdown (§8.3); the core value is the Router + follow-through layer. The
> Obsidian plugin is the acquisition funnel; the independent tool carries the
> product value.
>
> *(Revised 2026-11: this originally read "storage is pluggable,
> MemPalace/Mem0-compatible". MemPalace has since left the market (§2.7) and no
> Mem0/Zep adapter was ever built — the pluggability that did ship is the
> sqlite↔markdown backend switch of §8.3. Tier 1 memory layers are now peer
> alternatives rather than upstream dependencies (§11.3).)*

### 4.2 Key strategy

> **The storage engine is not where the value is — but it is also not someone
> else's to borrow.** MemVault ships its own local-first store (§8.3) because the
> two properties that matter are only available if you own it: a single
> zero-runtime file the user can move or delete, and a Markdown mode whose
> hand-edits self-heal into the index. The core value remains the Memory Router
> and the follow-through layer on top (§11.1 L3/L6).
>
> *(Revised 2026-11: the original strategy was "do not build a storage engine
> from scratch; treat MemPalace/Mem0 as pluggable backends". That was the right
> instinct about where value sits, but it assumed adapters that were never worth
> building — an adapter over Mem0 inherits Mem0's Python runtime and its
> SDK-shaped API, which costs exactly the zero-runtime single-binary property
> that §2.5's second table shows to be one of MemVault's few uncontested rows.)*

---

## 5. Final architecture (with the Memory Router)

### 5.1 Overall architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│ MemVault system architecture                                        │
│                                                                     │
│ ┌────────────────────────────────────────────────────────────────┐ │
│ │ Layer 5: clients (thin clients)                                │ │
│ │ Obsidian Plugin │ MCP clients  │ Dashboard │ Web │ CLI         │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 4: protocol                                              │ │
│ │ MCP Server (stdio/HTTP) │ REST API │ MCP Resources            │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 3: Memory Router (the core differentiator)               │ │
│ │                                                                │ │
│ │ ┌──────────────┐ ┌──────────────┐ ┌────────────────────────┐  │ │
│ │ │ Auto-Inject  │ │ Format       │ │ Compliance             │  │ │
│ │ │ Engine       │ │ Engine       │ │ Tracker                │  │ │
│ │ │              │ │              │ │                        │  │ │
│ │ │ • Pre-prompt │ │ • MUST/REF   │ │ • compliance stats     │  │ │
│ │ │   injection  │ │   tiers      │ │ • violation detection  │  │ │
│ │ │ • Session    │ │ • instructional│ │ • dynamic priorities │  │ │
│ │ │   bootstrap  │ │   conversion │ │ • feedback loop        │  │ │
│ │ │ • Context    │ │ • Token      │ │                        │  │ │
│ │ │   matching   │ │   budget     │ │                        │  │ │
│ │ └──────────────┘ └──────────────┘ └────────────────────────┘  │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 2: memory engine                                         │ │
│ │                                                                │ │
│ │ ┌────────────┐ ┌────────────┐ ┌────────────┐ ┌────────────┐  │ │
│ │ │ Extractor  │ │ Retriever  │ │ Dedup &    │ │ Decay &    │  │ │
│ │ │            │ │            │ │ Merge      │ │ Forget     │  │ │
│ │ │ • extraction│ │ • hybrid   │ │ • dedup    │ │ • decay    │  │ │
│ │ │ • intent   │ │   retrieval│ │ • merge    │ │ • archive  │  │ │
│ │ │   analysis │ │ • rewriting│ │ • conflict │ │ • review   │  │ │
│ │ │ • classify │ │ • rerank   │ │   detect   │ │            │  │ │
│ │ └────────────┘ └────────────┘ └────────────┘ └────────────┘  │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 1: pluggable storage                                     │ │
│ │                                                                │ │
│ │ ┌──────────────────────────────────────────────────────────┐  │ │
│ │ │ Local-first: SQLite (embedded int8 vector columns) +     │  │ │
│ │ │ Markdown files                                           │  │ │
│ │ ├──────────────────────────────────────────────────────────┤  │ │
│ │ │ Alternative backend: Markdown files as source of truth   │  │ │
│ │ │ (cache derived, hand-edits self-heal via reindex, §8.3.2) │  │ │
│ │ └──────────────────────────────────────────────────────────┘  │ │
│ └────────────────────────────────────────────────────────────────┘ │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

> **Shipped surfaces vs. the diagram**: the MCP server (stdio + streamable
> HTTP), the REST API with its embedded dashboard, the `memory://` resources,
> the CLI, the Obsidian plugin, the dsh plugin and the agent plugins under
> `plugins/` all exist. Two boxes in the drawing do **not**: there is no
> WebSocket transport (the dashboard polls REST), and there is no VS Code
> extension package — VS Code is supported through MCP configuration and the
> `agents.yaml` profile, which is what §10 means by "ships as a config profile".
> The proxy is a separate binary (`memvault-proxy`), not a protocol layer inside
> the server.

### 5.2 Memory Router workflow

#### 5.2.1 Single-agent mode (basic)

```
User sends a message
│
▼
┌─────────────────────────────────────────────┐
│ Memory Router intercepts                    │
│                                             │
│ Step 1: identify the agent                  │
│ agent_id: claude-desktop                    │
│ agent_type: coding-assistant                │
│                                             │
│ Step 2: intent analysis                     │
│ "which domains/projects/preferences does    │
│  this message touch?"                       │
│                                             │
│ Step 3: memory retrieval                    │
│ structured filter (agent + namespace) →     │
│ hybrid retrieval                            │
│                                             │
│ Step 4: priority ranking + token budget     │
│ MUST first → REFERENCE fills up             │
│ capped at 5-8 memories                      │
│                                             │
│ Step 5: format conversion (instructional)   │
│ descriptive memory → imperative instruction │
│                                             │
│ Step 6: inject into agent context           │
│ System Prompt / Context / MCP Resource      │
└─────────────────────┬───────────────────────┘
                      │
                      ▼
What the agent actually receives:
┌─────────────────────────────────────────────┐
│ System: you are a coding assistant...       │
│                                             │
│ [MEMORY CONTEXT - must be followed]:        │
│ [MUST] user prefers Python, not Java        │
│ [MUST] no code comments, ≤10 lines/function │
│ [REF] current project: FastAPI + PostgreSQL │
│ [REF] last discussion: pagination scheme    │
│                                             │
│ User: help me write a...                    │
└─────────────────────────────────────────────┘
```

#### 5.2.2 Multi-agent shared mode (the core difference)

```
                            ┌─────────────────┐
                            │  MCP Server     │
                            │ (shared memory  │
                            │    engine)      │
                            │ SQLite+vectors  │
                            └────────┬─────────┘
                                     │
                    ┌────────────────┼────────────────┐
                    │                │                │
          ┌─────────▼─────────┐  ┌──▼──────────┐  ┌───▼──────────┐
          │  Agent A          │  │  Agent B    │  │  Agent C     │
          │  (Claude Desktop) │  │  (Cline)    │  │  (Cursor)    │
          │  coding assistant │  │  general    │  │  IDE         │
          └─────────┬─────────┘  └──┬──────────┘  └───┬──────────┘
                    │                │                 │
          ┌─────────▼────────────────▼─────────────────▼──────────┐
          │            Memory Router (shared instance)            │
          │                                                       │
          │  ┌─────────────┐  ┌──────────┐  ┌──────────────────┐  │
          │  │ Agent       │  │ Filter   │  │ Inject           │  │
          │  │ Registry    │  │ Engine   │  │ Scheduler        │  │
          │  │             │  │          │  │                  │  │
          │  │ A: coding   │  │ • agent  │  │ A: MUST prefs    │  │
          │  │ B: general  │  │ • ns     │  │ B: REF project   │  │
          │  │ C: code-ide │  │ • intent │  │ C: MUST style    │  │
          │  └─────────────┘  └──────────┘  └──────────────────┘  │
          │                                                       │
          │  each agent gets a different memory subset, yet all   │
          │  read/write the same store                            │
          └───────────────────────────────────────────────────────┘
```

**Core mechanics**:

| Scenario | Behavior |
|----------|----------|
| Agent A saves a memory | every agent can read it (unless visibility is restricted) |
| Agent B queries | sees all matching memories, including Agent A's writes |
| Agent A writes "prefers Python" | does not appear in B's injection (B is a general assistant; irrelevant) |
| User edits via the Dashboard | all agents use the latest version on their next injection |

### 5.3 Memory layering model

`Memory.layer` is a **distillation** axis (`MemoryLayer` in `models.rs`), and it
is orthogonal to memory type. It is not the same L-numbering as §11's moat
layers or §6.5's disclosure levels; those are different scales that happen to
reuse the letter.

| Layer | Meaning | How a row gets there | Status |
|-------|---------|----------------------|--------|
| `L0` | raw evidence / retired source | `trace` ingestion writes evidence rows at L0; `promote` and `supersede` demote consumed sources to L0 so the provenance chain stays readable but out of retrieval (decay archives by `archived:*` namespace instead — it does not change the layer) | shipped |
| `L1` | atom (default) | every ordinary `save` starts here — `MemoryLayer::default()` is `L1` | shipped |
| `L2` | scenario | `run_promote` consolidates ≥3 similar L1 atoms into one L2 summary | shipped |
| `L3` | persona rule | `run_promote` consolidates ≥2 L2 scenarios into an L3 rule | shipped |

The **memory types** (`preference` / `fact` / `episode` / `entity` / `skill`)
carry the three evolution tracks — episodic, procedural, semantic — described in
§14. A type is not a layer: an `episode` row starts at L1 and can become L2
scenario material later.

---

## 6. Memory Router core modules

### 6.1 Auto-Inject Engine

#### 6.1.1 Three injection modes (ordered by feasibility)

> **Important**: the standard MCP protocol keeps the server passive — it only
> responds when an agent calls a tool. A server **cannot** intercept user
> messages before they reach the agent. That constraint is why the priority of
> the three injection modes was adjusted, and it is still true.

| Mode | Trigger | Use case | Mechanism | Status |
|------|---------|----------|-----------|--------|
| **MCP Resource** | agent startup | high-frequency core memories | `memory://user-profile` (MUST, review-gated) + `memory://project-context` (REFERENCE, ungated) | **shipped** |
| **Session bootstrap** ⭐ | new session | every session | `session_start` (MCP), `GET /api/session` (REST), and the host hooks that call `memvault-cli session-start` (`plugins/memvault/hooks/session-start.sh`) | **shipped** |
| **Pre-prompt injection** | each proxied tool-call turn | dynamic context matching | `memvault-proxy` observes tool-call turns, builds a conversation n-gram and refreshes the injection state it feeds to the model (§6.1.2, §6.4.1) | **shipped**, proxy-only |

> **What each host actually gets.** Codex and Claude Code reach bootstrap
> injection through their `SessionStart` hook, not through the model choosing to
> call `session_start`; the hook prints one envelope on stdout and the host adds
> it to context. Pre-prompt refresh only exists when the agent's MCP traffic is
> routed through `memvault-proxy`. Everything else is pull-based: the agent calls
> `search_memory` when it decides to.

#### 6.1.2 Two-phase injection refresh

**Design**: the proxy's `InjectionEngine::refresh_two_phase`:

```
Phase 1 (synchronous): deterministic injection — MUST memories resolved by
                       pure rules (project namespace + global), state written
Phase 2 (background):  full layered semantic pipeline; replaces the state
                       wholesale on completion, keeps the baseline on failure
```

- `wait_full(250ms)`: the caller waits at most a short window for the fuller
  result; on timeout the deterministic baseline proceeds. **Requests are never
  hijacked by embedding latency.**
- A `generation` counter discards a slow prefetch that lost the race to a newer
  refresh.
- Both phases share one `session_id`, so compliance tracking stays continuous
  across the upgrade.
- Project namespaces are canonicalized (`memvault`, `MemVault` and `memvault/`
  are one spelling) on every write path and every read filter; existing rows are
  migrated once on open.

#### 6.1.3 Injection-channel deduplication

Three injection paths exist — MCP `session_start`, proxy interception, `sync`
instruction files — and the same memory could reach one agent repeatedly through
different paths.

**Design**: `InjectChannel` (`mcp` / `proxy` / `sync`) + `AgentProfile.inject_channel`
(`agents.yaml`):

- default `None` = path-unrestricted (backward compatible); setting it enables
  dedup;
- `MemoryRouter::channel_allows(agent_id, channel)` lets each path self-check;
- MCP `session_start` and REST `/api/session` skip injection when the agent is
  configured for another channel and return an explanatory note, so an explicit
  request still gets an auditable answer;
- the proxy injects nothing when it is not the canonical channel.

#### 6.1.4 Pre-prompt injection pseudocode (multi-agent flavor)

> This is the original design sketch, kept for the multi-agent mechanics it
> explains. The shipped pipeline is §9.2; where they differ, §9.2 wins.

```python
class MemoryRouter:
    def __init__(self):
        self.agent_registry = AgentRegistry()
        # register known agents and their context types
        self.agent_registry.register(
            agent_id="claude-desktop",
            agent_type="coding-assistant",
            default_namespace="project:memvault",
            inject_rules={"max_memories": 8, "priority_order": ["MUST", "REF"]}
        )
        self.agent_registry.register(
            agent_id="cline-vscode",
            agent_type="general-assistant",
            default_namespace="global",
            inject_rules={"max_memories": 5, "priority_order": ["MUST"]}
        )

    def intercept(self, user_message: str, agent_context: dict) -> str:
        """auto-inject memories before the message reaches the agent (multi-agent aware)"""

        agent_id = agent_context.get("agent_id", "unknown")
        agent_profile = self.agent_registry.get(agent_id)

        # 1. intent analysis (combined with agent identity)
        intent = self.analyze_intent(user_message, agent_type=agent_profile.type)

        # 2. retrieve relevant memories
        #    - namespace filter: agents can focus on different scopes
        #    - per-agent differentiation happens AFTER retrieval: soft
        #      type/tag penalties plus namespace hinting (no access control)
        memories = self.retrieve(
            query=user_message,
            intent=intent,
            filters={
                "namespace": agent_context.get("project", agent_profile.default_namespace),
            },
            top_k=agent_profile.inject_rules.max_memories * 2
        )

        # 3. priority ranking + token budget trim (per-agent configurable)
        selected = self.rank_and_trim(
            memories,
            max_count=agent_profile.inject_rules["max_memories"],
            token_budget=agent_profile.inject_rules.get("token_budget", 1500),
            priority_order=agent_profile.inject_rules["priority_order"]
        )

        # 4. format conversion (instructional) + injection-session tracking
        inject_session_id = self.generate_session_id()
        formatted = self.format_as_instructions(selected, inject_session_id)

        # 5. inject into the system prompt
        return self.inject_to_context(agent_context, formatted)
```

### 6.2 Format Engine

#### Memory tiers

```yaml
# MUST: the agent must honor; a violation is an error
- priority: MUST
  content: "No code comments, no type hints, ≤10 lines per function"
  enforcement: "HARD_RULE"
  source: "explicit user requirement"

# REFERENCE: the agent may consult when relevant
- priority: REFERENCE
  content: "user mentioned interest in Redis"
  enforcement: "SOFT_HINT"
  source: "inferred from conversation"
```

#### Format conversion rules

```
❌ Descriptive format (easily ignored):
"During the conversation the user mentioned liking concise code style"

✅ Instructional format (must be honored):
"[MUST] Code output rules:
- no comments
- no type hints
- ≤ 10 lines per function
- source: explicit user requirement
- priority: HARD_RULE"
```

### 6.3 Compliance Tracker

> **Status: shipped** — implementation and honesty rules in §13.9.

The MCP server still never sees the agent's replies, so "did the agent follow
the rule" cannot be answered at request time. The delivered design splits the
problem in two:

1. **Record at injection.** `session_start` / `GET /api/session` and the proxy
   stamp an `inject_session_id` and write a `pending` `compliance_events` row per
   injected memory.
2. **Judge at session end.** The host's `SessionEnd` hook runs
   `memvault-cli compliance-enqueue --hook-input`, which only enqueues a durable
   `compliance_finalize_jobs` row and exits — Codex caps `SessionEnd` at three
   seconds. A background worker inside the long-lived
   `memvault-mcp --transport http` process then reads the transcript tail and
   resolves each row.

```
inject memory → agent replies → SessionEnd hook enqueues (no LLM work)
                                        ↓
                background worker reads the transcript tail
                                        ↓
        followed ✅ / violated ❌ / unknown ❔  (+ evidence, + reason)
                                        ↓
   compliance_events → get_compliance_report / get_effectiveness_report
```

With no LLM extractor configured the rows are closed as `unknown` with an
explicit reason — never as `followed`, and never left `pending` forever. Rates
are `Option<f64>`: `N/A`/`null` until a MUST row has a real verdict, so an empty
store cannot report 100% compliance.

**Not implemented:** the automatic escalation in the original sketch
("violations > 3 → raise priority / change injection point / notify the user").
A violation is recorded and surfaced, but nothing rewrites priority or injection
rules automatically — that stays a human decision, consistent with the review
gate in §13.8.

### 6.4 Query rewriting

Two mechanisms exist, both deterministic — no LLM call:

```rust
// query_expand.rs — synonym expansion
pub fn expand_query(query: &str) -> Vec<String>
//  Bidirectional equivalents over a static table (js ↔ javascript, 代码 ↔ code,
//  偏好 ↔ prefer, 简洁 ↔ concise, …). Used when the strict keyword tier returns
//  nothing, reported as KeywordTier::SynonymFallback (§9.2).

// query_expand.rs — recency-weighted multi-turn key
pub fn weight_turns_by_recency(context: &str) -> String
//  Treats multi-line context as a turn sequence: newer turns repeat more
//  (linear decay), total length capped at 600 chars. Used by
//  `session-start --context`; the proxy's `conversation_ngram` applies the same
//  scheme to observed tool-call turns (§6.4.1).
```

#### 6.4.1 Recency-weighted session key

**Design**: the retrieval key is the most recent n turns weighted by recency —
newer turns repeat more (linear decay), total length capped at 600 chars.

- **Proxy transparent injection**: `SessionContext::conversation_ngram(window)`
  assembles the key from observed tool-call turns; window via
  `MEMVAULT_CONTEXT_NGRAM_WINDOW` (default 5).
- **Explicit session entries** (CLI `session-start --context`, MCP
  `session_start`, REST `/api/session`): `query_expand::weight_turns_by_recency`
  applies the same weighting to multi-line context. Single-line input is
  unchanged.

The original sketch's intent-based expansion (`"<query> coding style
preference"`) and "recent topics" branch were **not** built: they injected
vocabulary the user never typed, and the CJK bigram FTS tier plus the synonym
table closed the same recall gap without that risk.

### 6.5 Progressive disclosure under budget (`crate::disclosure`)

> Added 2026-11. Closes the "Progressive disclosure (L0/L1/L2)" row in §2.5,
> which was one of the few rows where agent-memory was ahead (§2.3).

**The problem.** `trim_to_budget` walked candidates in ranked order and **broke**
at the first one that did not fit, cutting it and everything behind it. The cut
was reported (`InjectSkipReason::TokenBudgetExceeded`) rather than silent, but
reported-or-not the agent did not get the memory. With the default 1500-token
budget and a handful of long memories, the tail of the ranked list was routinely
unreachable however relevant it was.

**The change: degrade before dropping.** Three levels, following agent-memory's
`ContentLayers` naming:

| Level | What it is | Derived how |
|-------|-----------|-------------|
| `Full` (L2) | the whole text. **It is `Memory::content`, never a copy** | — |
| `Excerpt` (L1) | leading ~320 chars, cut at a sentence boundary, else a word boundary | extractive, on demand |
| `Summary` (L0) | the first sentence when it is a usable length, else ~96 chars at a word boundary | extractive, on demand |

`trim_to_budget_reporting` searches `Full → Excerpt → Summary` and takes the
first that fits the *remaining* budget, so a memory is shortened only as far as
necessary. Only when even the summary does not fit does the old break-and-report
behaviour apply. Measured on a live server with `token_budget: 120` and four
distinct long memories: the previous logic injected **zero** of them; this
injects one at excerpt level and reports the other three as
`token-budget-exceeded`. Nothing is silently dropped either way.

§3.2 is why this is not just "cram more in": follow-through *falls* as the number
of injected memories rises. The goal is to keep more memories **reachable** at a
lower per-memory cost, not to maximise injected tokens.

**Two invariants.**

- **MUST is never degraded.** "Never filtered, never truncated" is the L3 claim
  (§11.1) and it applies to *relevance, type and budget*: a reviewed MUST is never
  down-ranked away or shortened. The review gate (§13.8) is a separate, earlier
  decision about whether a memory may be injected at all, and it applies to every
  priority. A binding instruction whose qualifying clause was summarised away is
  worse than one that overflowed the budget, so MUST keeps the old behaviour of
  simply exceeding it.
- **A degraded memory says so.** The shortened text carries a ` […]` marker and
  the level is reported in `TrimReport::degraded` → `SessionStartOutput.degraded`
  → REST `/api/session`'s `degraded` array. An abbreviation that looks complete
  is worse than a drop: the agent would act on a rule with its conditions
  silently removed. `degraded` is kept separate from `skipped` because they are
  different outcomes — a degraded memory *was* delivered — and folding them
  together would show a delivered-but-abbreviated rule as a dropped one.

**Why the layers are derived, not stored.** `excerpt_l1`/`summary_l0` are
deterministic and extractive, so they are computed at injection time:

- no migration, no backfill, no frontmatter key, no cache invalidation;
- a derived layer **cannot go stale**. A stored summary sitting next to content
  that was later edited is a contradiction the agent would be shown as fact, and
  keeping the two in sync would need a hook on every write path — `update`,
  `supersede`, markdown reindex, proposal approval (§9.3.1), `restore`;
- it costs microseconds of string scanning, against a budget whose whole purpose
  is to bound cost.

If an LLM-generated summary is added later, the right shape is a stored
`Option<String>` these functions *prefer* when present and fall back to deriving —
not a replacement, since the derived form is what keeps disclosure working with
no provider configured (the `edge-lite` profile, §7.5).

**The fusion trap, and why MemVault does not have it.** §2.3 records that
agent-memory must apply MaxP per (channel, unit) *before* cross-channel fusion,
because it indexes L0/L1/L2 as separate physical entries of the same channel —
otherwise a memory with more layers is recalled more often and RRF accumulates
each as independent evidence, systematically over-weighting long memories while
the metrics look like they improved.

MemVault's layers are derived at *injection* time and are **never indexed**: FTS
covers `content`/`instruction`/`tags`, one row produces one candidate per channel,
and `HybridMerger::merge` already keys its accumulator by `memory.id`. Adding
l0/l1 to the FTS index would break this twice — it would create the
duplicate-evidence problem above, and it would distort BM25 by indexing text that
is a prefix of text already indexed. `disclosure::tests::test_disclosure_layers_do_not_change_hybrid_ranking`
pins the invariant, asserting each fused score equals exactly the RRF sum for its
own rank with one contribution per channel.

---

## 7. Technology stack

### 7.1 Core engine

| Component | Choice | Rationale |
|-----------|--------|-----------|
| Engine language | Rust | performance, low footprint, cross-platform |
| Structured storage | SQLite | zero dependency, local-first |
| Vector retrieval | SQLite embedded columns | int8-quantized, stored alongside rows, no extra dependency (LanceDB evaluated and dropped) |
| Graph database | **not built** — single `memory_relations` table + one-hop expansion (§14 Track C) | FalkorDB/Neo4j stay deferred until relation volume outgrows that cost point (§15.2) |
| Sync protocol | **not built** — CRDTs/Automerge deferred (§15.2) | multi-device merge is explicitly out of scope for v1 (§8.3.8) |
| Memory protocol | MCP | the de-facto standard |
| Pluggable backends | `MemoryStore` trait: sqlite ↔ markdown (§8.3) | same API, two source-of-truth shapes — DB-authoritative or file-authoritative. *(The v0.3 row read "MemPalace / Mem0 adapters"; those were never built and are not worth building — see §4.2)* |

### 7.2 Memory Router

| Component | Choice | Purpose |
|-----------|--------|---------|
| Interception layer | MCP Proxy / HTTP middleware | request interception |
| Intent analysis | local small model / rules | decide which memories a message touches |
| Query rewriting | LLM / templates | expand retrieval terms |
| Format engine | templates + rules | MUST/REF conversion |
| Compliance detection | LLM-as-a-judge | analyze reply compliance |

### 7.3 Clients

| Client | Stack | Role |
|--------|-------|------|
| Web Dashboard | React + TypeScript | primary UI (browser, REST backend) |
| Obsidian plugin | TypeScript | thin client + acquisition funnel |
| VS Code | MCP client configuration + `agents.yaml` profile | **not shipped as an extension package** (see §10) |
| CLI | Rust | developers |

### 7.4 AI / NLP

| Component | Choice | Purpose | Phase |
|-----------|--------|---------|-------|
| Embedding | native embedded (fastembed, default) → optional ollama / OpenAI-compatible | vectorization | local native model is the default since v0.2.0 |
| Memory extraction | rules engine (default) + optional any OpenAI-compatible chat LLM | conversation → structured memories | shipped; enabled via `MEMVAULT_LLM_EXTRACTION_PROVIDER` |
| Compliance detection | optional LLM-as-a-judge (same OpenAI-compatible extractor config) | reply compliance | shipped — runs in the SessionEnd finalizer, §13.9 |
| Rerank | deterministic multi-signal reranker (hybrid score + term overlap + recency + priority + access + authority) | reorder retrieval results | shipped — no cross-encoder model is loaded |

> **Embedding deployment notes**: the default is **native embedded inference**
> (fastembed + ONNX Runtime, in-process, zero external dependencies). The
> default model is the Chinese `bge-small-zh-v1.5` (~95MB); switch to
> multilingual `multilingual-e5-base` with `MEMVAULT_EMBEDDING_MODEL=multilingual`.
> A local Ollama service or any OpenAI-compatible endpoint can be configured via
> `MEMVAULT_EMBEDDING_PROVIDER`; `auto` validates installed models — when the
> default (`nomic-embed-text`) or `MEMVAULT_EMBEDDING_MODEL` model is not
> installed it falls back to native with a WARN rather than constructing a
> provider that is certain to fail. On first use the model downloads from
> HuggingFace; in networks that cannot reach it, set `HF_ENDPOINT=https://hf-mirror.com`.
>
> **Memory-extraction deployment notes**: local-first by default — when
> `MEMVAULT_LLM_EXTRACTION_PROVIDER` is unset, MemVault auto-detects a local
> Ollama (`http://localhost:11434`) and, if found, uses it for context
> extraction with zero config (default model `qwen2.5:7b`, free, stays on the
> machine; it checks `/api/tags` — if the 7b model isn't pulled it prefers
> another installed qwen2.5 chat model, and if no chat model exists it stays on
> pure rule extraction with a WARN). Without a detected Ollama it stays on the
> pure rule/keyword extractor (zero external dependencies). The full
> conversation-aware LLM extraction path lives in
> `crates/memvault-core/src/llm_extractor.rs`; `memvault-proxy`'s
> `notify_response` feeds user_text + response_text together rather than
> scanning line by line. Remote providers (`openai` / `openai-compatible`) must
> be explicitly selected — merely having an `OPENAI_API_KEY` elsewhere never
> triggers paid API calls. This is deliberate: local probing is free and
> zero-config-able; remote calls have real cost and hallucination risk and must
> be explicit opt-in. LLM extraction failures (network/parse errors) fall back
> to rule extraction without disturbing the main flow.

### 7.5 Configuration layers and scenario profiles

> Added 2026-11 and delivered (§15.1). Comes out of
> §2.3: agent-memory exposes eleven configurable axes plus four named presets
> ("既能全、也能轻"), while MemVault had accumulated a 12KB `.env.example` of
> independent switches with no way to select a coherent combination.

Precedence, highest to lowest:

```
  1. CLI flags                        (--db, --markdown-dir, --env-file)
  2. process environment              (mcpServers env injection / shell export / CI)
  3. env file                         ($MEMVAULT_ENV_FILE, else $MEMVAULT_HOME/.env)
  4. MEMVAULT_PROFILE scenario preset ◀── fills only keys layers 1–3 left unset
  5. each reader's built-in default
```

The profile layer is **additive, never overriding**: `profile::apply_to_env`
skips any key already present in the process environment, and because it runs
*after* the env-file loop inside `env_file::load`, a key the file set also
outranks it. That is what makes a profile safe to leave configured permanently —
upgrading MemVault or hand-tuning one key cannot be silently undone by it.

| Profile | Shape | Notable choices and why |
|---------|-------|-------------------------|
| `coding-agent` | Markdown vault, native embeddings, no relations | Files as source of truth so the vault is git-able and readable by tools that speak nothing but Markdown (§8.3.2). Relations off because each costs an LLM call and §15.2 defers graph work until relation volume outgrows single-table one-hop. Auto-forget **off**: irreversible purge should not run on a timer over project decisions |
| `assistant` | SQLite, relations on, auto-forget at 180 days | A personal profile is mostly entities and preferences, which is where one-hop expansion earns its cost. Retention long enough to notice and restore, short enough to actually bound growth. MUST exempt from purge |
| `team` | SQLite + the full trust stack | `IDENTITY_VERIFICATION` + `CORROBORATION_GATE` + `CORROBORATION_MIN_AGENTS=2`, because the gate counts *identity-verified* agents — enabling it without verification would count self-declared ids, which is exactly the spoofing it exists to stop (§13.7). Shared memories exempt from auto-purge. Also sets `MARKDOWN_TRUST_MODE=quarantine` even though the backend is sqlite: if anyone switches backends, `owned` would hand injection authority to everyone who can write the directory (§8.3.4), so it fails safe rather than later |
| `edge-lite` | SQLite, **no embedding provider at all** | The interesting one: MUST memories still inject, because §6.1.2 resolves them deterministically with zero embedding calls. Turning embeddings off is therefore a supported configuration, not a degraded one |

Two deliberate exclusions:

- **No secrets.** A profile names a provider (`native`, `ollama`, `openai`) and
  never writes an `*_API_KEY`. `memvault-cli init` writes a file users routinely
  commit or share, and `SECURITY.md` says keys are never committed.
- **No knobs MemVault does not have.** agent-memory's preset table configures
  graph backend, bi-temporal on/off, multimodal normalisers and edge/cloud
  deployment. MemVault has no graph backend to select, no multimodal input and no
  cloud tier, so exposing those axes would be theatre. The four profiles cover
  only switches that exist.

`memvault-cli init` is the authoring path and `MEMVAULT_PROFILE` the runtime path;
both resolve to the same `profile::ScenarioProfile` definitions, so they cannot
drift. `init` refuses to modify a key that is already set rather than offering a
`--force`, for a concrete reason: inside an env file the **first** occurrence of a
key wins (`load` sets it, then skips the duplicate because the key is now
present), so appending a "replacement" line would be silently inert — the user
would see their new value in the file and the old one at runtime. Changing an
existing key is a hand edit, and the output says so.

Provenance is reported per key (`memvault-cli status`), including a distinct
`profile` source. Without it, profile-applied keys would be indistinguishable
from real exports — `apply_to_env` writes through `set_var`, so all of them would
read as `env` and the user would be left wondering which settings they had
chosen. Hiding the lowest layer makes the highest one look arbitrary.

---

## 8. Data model and schema

### 8.1 Memory schema (multi-agent flavor)

```yaml
---
id: mem_9f2c1a7b
type: preference          # preference | fact | episode | entity | skill
content: "User likes concise code style, dislikes over-commenting"
instruction: "[MUST] no comments, no type hints, ≤10 lines per function"
priority: MUST            # MUST | REFERENCE | BACKGROUND

# --- agent identity ---
source_agent:
  id: claude-desktop       # the agent that wrote it
  type: coding-assistant   # agent type (coding | general | research …)
  session_id: "sess_abc123" # session at write time (traceability)

# --- visibility / layer control ---
namespace: global          # global | project:xxx (canonicalized: lowercase, no trailing slash)
visibility: scoped         # scoped (namespace rules) | shared (team pool, injected everywhere)
layer: L1                  # L0 raw/retired | L1 atom | L2 scenario | L3 persona (§5.3)

confidence: 0.9
tags: [coding, style, python]
created_at: 2026-08-07T15:00:00+08:00
updated_at: 2026-08-07T15:00:00+08:00

# --- three time axes, deliberately never folded into one column ---
# Ingest-time: when MemVault took the memory in. Always set.
# (that is `created_at` above)
occurred_at: 2026-08-01T09:00:00+08:00 # event-time: when the remembered fact
                                      # actually happened in the source
                                      # conversation/document. Null when the
                                      # caller supplied no date. Without it a
                                      # batch import of historical transcripts
                                      # would stamp every row "now".
expires_at: 2026-12-31T00:00:00Z      # valid-time upper bound: when the fact
                                      # stops being true. Null = never expires.
                                      # Past this instant the memory leaves
                                      # retrieval and injection but stays
                                      # inspectable and restorable; the decay
                                      # cycle archives it so the auto-forget
                                      # retention sweep can eventually reach it.
# The third axis — valid-time *as of an arbitrary instant* — is not a stored
# field but a query: `as_of` reconstructs what MemVault believed at T from the
# `memory_history` snapshot chain plus `superseded_by`. Keeping the three apart
# is what makes "what was the deploy policy last month" answerable without
# losing "when did the freeze actually start". See §9.6.

# relations are rows in `memory_relations`, not an inline list (see §14 Track C)
ai_generated: true
human_reviewed: false

# --- pending edit proposals (§9.3.1) ---
# Set on a memory that proposes to REPLACE another one once a human approves it.
# The target is left untouched and keeps injecting until then, so an agent cannot
# suppress a binding MUST merely by proposing an edit to it. Absent on ordinary
# memories. Consumed on approval (the target gains `superseded_by` instead).
proposes_supersede_of: mem_4d8e0c21
decay_score: 1.0
access_count: 0
# Compliance is NOT stored on the memory row: injection writes a
# `compliance_events` row keyed by `inject_session_id` + agent_id, and
# `get_compliance_report` aggregates it (§13.9).
---
```

> **Current implementation note (2026-11)** — the serialized `Memory` struct in
> `crates/memvault-core/src/models.rs` is authoritative; the YAML above is a
> trimmed version of it. Beyond the fields shown, the real row also carries
> `skill_meta` (`trigger`/`steps`/`verification`), `superseded_by`,
> `identity_verified`, `corroborating_agents`, `source_trace_ids`,
> `friction_evidence`, `archived_at` and `last_read_at`. There is **no**
> `agent_access` map: per-agent read isolation was never built and §13.4 keeps
> it out of scope. There is **no** embedded `compliance` block either —
> compliance lives in the separate `compliance_events` table keyed by
> `inject_session_id` + `agent_id` (§13.9). Relations are rows in
> `memory_relations`, not an inline `related` list (§14 Track C). The
> client-facing wire shape is pinned by `contracts/memory-json-keys.json`.

### 8.2 Vault directory layout

```
vault/
├── 00-Inbox/              # auto-captured by AI, awaiting review
├── 10-Daily/              # episodic memory
│   └── 2026-08-07.md
├── 20-Entities/           # semantic memory
│   ├── People/
│   ├── Projects/
│   └── Concepts/
├── 30-Memories/           # long-term facts/preferences
│   ├── MUST-Rules.md      # hard rules
│   ├── Preferences.md
│   └── Facts.md
├── 40-Skills/             # procedural memory
├── 50-Archive/            # decay archive
├── _Router/               # Memory Router configuration
│   ├── inject-rules.yaml  # injection rules
│   ├── format-templates/  # format templates
│   └── compliance-log.json # compliance log
└── Templates/
```

> **Implemented**: the Obsidian plugin (`sync.ts::folderFor`) writes memories
> into these directories by type — episode → `10-Daily`, entity →
> `20-Entities`, fact/preference → `30-Memories`, skill → `40-Skills` — and
> creates subdirectories automatically on sync.

Backend selection and source-of-truth rules are defined in §8.3. The directory
tree above is the Obsidian projection layout; the markdown backend uses the
same type-to-folder mapping but treats the files themselves as canonical.

### 8.3 Storage backends and consistency

> Added in v0.5.0. Two `MemoryStore` implementations share the same
> API: SQLite (the default) and a Markdown vault. The backend is selected
> by `MEMVAULT_STORAGE_BACKEND`; it changes where authority lives, not the
> router's retrieval, review or injection semantics.

#### 8.3.1 Selection and layout

| Setting | Values / default | Notes |
|---|---|---|
| `MEMVAULT_STORAGE_BACKEND` | `sqlite` (default) / `markdown` (alias `md`) | unset or unknown → `sqlite` |
| markdown data dir | `--markdown-dir` (cli/mcp/proxy) or `MEMVAULT_MARKDOWN_DIR` | required in markdown mode; startup fails with a clear error if neither is set |
| markdown trust mode | `MEMVAULT_MARKDOWN_TRUST_MODE` = `owned` (default) / `quarantine` | unset or empty → `owned`; unknown non-empty → `quarantine` (fail closed) |

- **`sqlite`**: one `data.db`; every write is one transaction.
- **`markdown`**: a directory of human/git/Obsidian-editable `{id}.md` files is
  canonical. Each file is YAML frontmatter (every `Memory` field, lossless) plus
  a body and an optional `> Instruction:` line, written atomically (temp file +
  rename). Side files live next to it:
  - `.memvault-cache.db` — disposable, rebuildable index (FTS5, embeddings,
    episodes, skill stats, relations, trace watermarks, plus the compliance and
    eval-history side tables);
  - `.memvault.lock` — advisory single-writer lock (`create_new`, no liveness
    check; a stale lock left by a crash must be removed by hand);
  - `.memvault-forgotten.json` — durable erasure tombstone (§8.3.5).

#### 8.3.2 The inverted data-flow

- **sqlite mode**: `data.db` is authoritative; `memvault-cli sync` *projects* it
  into instruction files and the Obsidian vault (one-way).
- **markdown mode**: the files are authoritative and the cache is derived. Only
  `save` / `update` / `delete` / `save_with_embedding` / `supersede` touch the
  filesystem; every other `MemoryStore` method passes straight through to the
  cache.

#### 8.3.3 Consistency model

- The cache is **eventually consistent**. `memvault-cli reindex` diffs each
  file's content against `Exporter::memory_to_markdown` of its cached row and
  resyncs on any mismatch, so both a crash between the file write and the cache
  write, and a hand-edit made outside MemVault (Obsidian, an editor,
  `git checkout`), self-heal the same way.
- `MarkdownStore::open` runs one `reindex` pass, so a stale or deleted cache
  self-heals on startup. `reindex --watch [--interval <secs>]` polls
  continuously (default 5s; no file-watcher dependency).
- Symlinks are never followed: only regular files directly inside the directory
  are source-of-truth entries. A symlinked `.md` is reported as
  `skipped_symlinks`, so a shared vault cannot pull an arbitrary file into the
  index; forget removes the symlink itself, never its target.

#### 8.3.4 Trust boundary: `owned` vs `quarantine`

A Markdown file carries its own trust metadata in frontmatter — `ai_generated`
is the biggest lever, because `ai_generated: false` makes `is_trusted()` treat
the memory as human-authored and inject it in the instruction block. Priority,
review state, identity verification, corroboration and visibility likewise
affect gating and reach. The backend therefore has to answer: *who is allowed to
say these things about a file?*

- **`owned` (default)** — MemVault is a local-first single-user store. A file the
  user can edit is a user action, so the file wins byte-for-byte, including
  `human_reviewed`, `identity_verified`, `ai_generated`, `priority` and
  `visibility`. Consequences, stated plainly: **write access to the Markdown
  directory is equivalent to memory-write access, which is equivalent to future
  session-injection authority.** That is the intended trust model for a personal
  vault, not a bug. One parser detail: omitting `ai_generated` is **not** a claim
  of human authorship — the safe default `ai_generated=true` applies, so a
  hand-written file that should be injected as a trusted instruction must say
  `ai_generated: false` explicitly (or be approved through review).
- **`quarantine`** — opt-in for a shared/synced vault, a multi-writer setup, or
  any import from a lower-trust source. A file with no cached baseline is
  reduced to an ordinary unreviewed draft: `human_reviewed=false`,
  `identity_verified=false`, corroboration cleared, `ai_generated=true`,
  `priority` capped at `REFERENCE` (so it cannot claim the MUST token/count
  exemptions), `layer` pinned to `L1`, authority tags
  (`decision`/`procedure`/`gotcha`) removed, confidence capped, server-maintained
  decay/access/read bookkeeping reset, and `visibility: shared` downgraded to
  `scoped` (shared bypasses the namespace filter). An existing row may still be
  downgraded but never escalated (`scoped → shared`, `REFERENCE → MUST`,
  `ai_generated → human-authored`). Approval is bound to the reviewed bytes *and
  the reviewed scope*: replacing the body, the instruction or the namespace
  revokes it, and a low-trust rewrite cannot move a reviewed row's namespace.
- **The cache is the baseline.** With no cached row, quarantine cannot tell "a
  file approved yesterday" from "a file dropped in a minute ago". Deleting
  `.memvault-cache.db` therefore demotes every file back to unreviewed —
  fail-closed, but operators must keep the cache (or re-apply approvals).
  `owned` does not have this problem because the file is the baseline.
- **Other untrusted entry points.** Directory/single-file `memvault-cli import`
  and REST `POST /api/import` run the same clamp — an alternate entry point must
  not be a policy bypass. `import-agent` (CLI and REST) parses files produced by
  another tool, so an adapter's `ai_generated: false` is not accepted unless the
  operator passes `--approve`. JSON import and `migrate-to-markdown` are
  lossless trusted paths and are not sanitized.
- **Review gate interaction.** `MEMVAULT_INJECT_REQUIRE_REVIEW` defaults on, so a
  quarantined new file is not injected at all before human approval, even if its
  content says otherwise. The clamps are defence in depth for deployments that
  disable that gate.

#### 8.3.5 Permanent-erasure tombstone

Permanent forget must survive the very thing a Markdown vault encourages: a
stale `git checkout`, sync client, editor undo, or backup copy reappearing after
the erase. `<markdown-dir>/.memvault-forgotten.json` is the durable tombstone —
an append-only JSON list of `{memory_id, content_hash, namespace, priority,
visibility, scope, reason, requester, erased_at}` with **no content body**,
written atomically (temp file + rename) and permission-restricted before the
cache mirror.

On every `reindex`, an id present in that file is removed from disk, its stale
cache row is dropped, and it is never re-imported (`skipped_forgotten`).
`save`/`update` reject the id as well: a forgotten id is never reusable. A final
check under the write gate prevents a concurrent `reindex` from racing a forget
and restoring the stale row.

Operational consequences:

- Keep the tombstone with the vault and in backups. If it is deleted, stale
  copies can resurrect the erased memory. In `owned` mode any process that can
  delete it can defeat the protection — the same trust boundary as every other
  vault file.
- Pre-upgrade installations may only have the `forgotten_log` audit table in
  `.memvault-cache.db`; `reindex` merges that legacy mirror with the durable
  file, so the first reindex after upgrading still blocks known ids.
- A forgotten memory's id/content may still appear inside free-text blobs
  (`eval_runs.summary_json`, exports, logs) that are not keyed by memory id;
  those are not rewritten by `purge()`.

#### 8.3.6 Migration

`memvault-cli migrate-to-markdown --dir <dir>` copies every memory from the
active store into a new markdown store, carrying embeddings over
(`save_with_embedding`). It is a trusted store-to-store copy, not a file-origin
import: it deliberately bypasses quarantine sanitization so an already-approved
SQLite memory stays approved. The same applies to a JSON export/import round
trip. Use Markdown import (or hand-drop a file) when the bytes actually come from
an untrusted source.

#### 8.3.7 Feature boundaries

`backup` / `checkpoints` / `restore` are SQLite-only (their semantics are
SQLite-specific) and fail with a clear error on the markdown backend rather than
silently no-op'ing. `GET /api/system/backend` reports the active backend
(`sqlite` / `markdown`) so clients and the dashboard can surface it up front.

#### 8.3.8 Explicitly out of scope (v1)

Git auto-commit, running both backends at once, multi-machine merge of
concurrent edits, per-namespace subdirectories, and NFS-safe locking. Each is
deferred, not designed away (§15.3).

### 8.4 MCP tool definitions (multi-agent flavor)

The five original tools, with the parameters the server actually accepts today
(see the parameter structs in `crates/memvault-mcp/src/server.rs`):

```json
{
  "tools": [
    {
      "name": "save_memory",
      "description": "save one memory (records the source agent)",
      "parameters": {
        "content": "memory content",
        "priority": "MUST | REFERENCE | BACKGROUND (default REFERENCE)",
        "type": "preference | fact | episode | entity | skill (default fact)",
        "namespace": "global | project:xxx",
        "instruction": "optional directive rendered at injection time",
        "layer": "L0 | L1 | L2 | L3 (auto-assigned if omitted)",
        "visibility": "scoped | shared",
        "confidence": 0.0,
        "tags": [],
        "expires_at": "RFC3339, optional — valid-time bound; see §9.6",
        "occurred_at": "RFC3339, optional — event-time; see §9.6",
        "skill_trigger": "string (type=skill only)",
        "skill_steps": ["..."],
        "skill_verification": "string",
        "force_insert": false,
        "agent_id": "claude-desktop",
        "agent_type": "coding-assistant",
        "api_key": "optional; required once the agent has a registered key"
      }
    },
    {
      "name": "search_memory",
      "description": "search user memories (shared across agents)",
      "parameters": {
        "query": "what to search",
        "mode": "hybrid (default) | keyword | semantic",
        "top_k": 10,
        "namespace": "",
        "type_filter": "",
        "priority_filter": "",
        "expand_relations": false,
        "min_score": null,
        "as_of": "RFC3339, optional — point-in-time (valid-time) query; see §9.6",
        "occurred_from": "RFC3339, optional — event-time lower bound",
        "occurred_to": "RFC3339, optional — event-time upper bound",
        "agent_id": "current agent id — authentication/audit only, never a filter (§13.3)",
        "api_key": "optional"
      }
    },
    {
      "name": "session_start",
      "description": "called at session start; preloads memories filtered by the agent profile",
      "parameters": {
        "agent_id": "claude-desktop",
        "agent_type": "coding-assistant",
        "context_hint": "hint about the current conversation topic",
        "project": "current project name",
        "api_key": "optional"
      }
    },
    {
      "name": "review_memory",
      "description": "review a memory awaiting confirmation (admin credentials required when any agent key is configured)",
      "parameters": {
        "action": "approve | reject | edit",
        "memory_id": "",
        "edited_content": "",
        "edited_instruction": "",
        "agent_id": "",
        "api_key": ""
      }
    },
    {
      "name": "get_compliance_report",
      "description": "fetch a compliance report — one injection session, or an aggregate summary",
      "parameters": {
        "inject_session_id": "optional — report for one session",
        "agent_id": "optional — filter the aggregate summary",
        "limit": 10,
        "api_key": "optional"
      }
    }
  ],
  "resources": [
    {
      "uri": "memory://user-profile",
      "name": "User Profile & MUST Rules",
      "description": "core user preferences and hard rules, review-gated and loaded every session (shared by all agents)"
    },
    {
      "uri": "memory://project-context",
      "name": "Current Project Context",
      "description": "tech stack and conventions of the current project (REFERENCE only; unreviewed drafts are wrapped as reference data)"
    }
  ]
}
```

> **Current implementation**: the MCP surface has grown to **21 tools** — on
> top of those five, `record_outcome` / `infer_outcome` (task outcomes and
> lessons), `import_skills` (Markdown SOP import), `add_evidence` /
> `get_memory_evidence` (supports / contradicts / sourced_from), `run_promote`
> (L1→L2→L3 promotion), `forget_memory` (GDPR-style erasure), `propose_memory_edit`
> and `delete_memory`, plus the maintenance tools `run_dedup` / `run_decay` /
> `confirm_read` / `list_inbox`, and `report_compliance` /
> `get_compliance_report` / `get_effectiveness_report`. Full list in the root
> README ("21 MCP Tools").

---

## 9. Core functional modules

### 9.1 Memory write pipeline

There is no single "inbox-first" pipeline any more; the write path branches on
who is writing (`crate::writer::MemoryWriter`):

```
manual/agent save (CLI save, MCP save_memory, REST POST /api/memories)
        ↓
authenticate the agent (§13.7) → delta-write lookup in the same namespace (§9.1.1)
        ├─ > 0.95 similarity → Skip: no new row, return the existing memory
        ├─ > threshold       → Merge: append the residual, union tags, re-embed
        └─ otherwise         → Insert
        ↓
human_reviewed = false   for MCP/REST/agent paths
human_reviewed = true    for `memvault-cli save` and the dashboard's direct save
        ↓
`MEMVAULT_INJECT_REQUIRE_REVIEW` (default on) keeps unreviewed rows out of
injection until a human approves them in the inbox
```

`merge_memory` deliberately revokes trust when unreviewed material is absorbed:
a merged row carrying a never-reviewed directive stays pending rather than
inheriting the old row's approval (§13.7). Auto-extraction (`extract_memories`,
the Stop-hook extractor, `record_outcome` lessons) always lands unreviewed and
`ai_generated = true`; approving in the inbox flips the flags in place.
Rejection is a **soft** delete — the row is snapshotted into `memory_history`
and restorable with `restore --history-id`; permanent erasure is §9.5's
`forget`. `00-Inbox/` is an Obsidian-side projection on the markdown backend,
not a staging area every write passes through.

#### 9.1.1 Delta-write policy

Dedup and merge live on the write path rather than in a later cleanup pass.

**Motivation**: the Qwen3.8-Flash-Next report's GDN rule estimates the value
already associated with a key and writes only the residual, so repeated keys
update an association instead of accumulating without bound. An append-only memory store is exactly that "unbounded extrinsic
additive memory", leaving `dedup` to clean up afterwards.

**Design**: `memvault-core::writer` (`MemoryWriter` + `merge_memory`) moves
dedup/merge into the write path. All three user save channels — CLI `save`, MCP
`save_memory`, REST `POST /api/memories` — route through it:

```
save request → same-namespace lookup
  ├─ token-overlap similarity > 0.95, or vector cosine > 0.9
  │        → Skip: no new row; return the existing memory
  ├─ token-overlap similarity > 0.7
  │        → Merge: append the residual, refresh timestamps/decay,
  │          monotonically upgrade priority, union tags, re-embed
  └─ otherwise → normal insert
```

**Key trade-offs**:

- **Two thresholds, deliberately.** Token overlap (0.7) is the primary signal
  because cosine rates "api /v1 vs /v2" as 0.87-similar when those are mutually
  exclusive facts. Vectors act only as a 0.9 near-identity backstop, with an
  extra character-bag guard for content a tokenizer cannot split. Corrective new
  facts go through `supersede`, never merge.
- **Skills are never merged.** Procedural knowledge has no merge semantics with
  facts; a skill save is always an insert.
- **`expires_at` merges asymmetrically.** An explicit bound on the incoming save
  wins in both directions (a re-save can bring an expiry forward); `None` leaves
  the existing bound alone. `occurred_at` is provenance and stays
  first-non-empty-wins. `metadata_would_change` carries the same asymmetry,
  because it decides whether the merge runs at all.
- **Bypasses**: CLI `--force`, MCP/REST `force_insert`, global
  `MEMVAULT_DELTA_WRITE=off`.

### 9.2 Router retrieval pipeline (multi-agent flavor)

```
session_start / `session-start --context` / proxy refresh
     ↓
agent identification → AgentProfile.inject_rules + inject_channel (agents.yaml)
     ↓
candidate retrieval (top_k = max_memories × 2)
  ├─ hybrid: FTS5 keyword (CJK unigram + bigram) + int8 vector cosine, fused by RRF
  ├─ empty strict keyword tier → relaxed unigram → synonym expansion (§6.4)
  └─ optional one-hop expansion over `memory_relations` (expand_relations)
     ↓
structural filters — one shared predicate, storage::memory_matches_filters
  namespace × type × priority × superseded/pending-proposal × expires_at × occurred_at
     ↓
review gate (`MEMVAULT_INJECT_REQUIRE_REVIEW`): drop untrusted rows and record why
     ↓
rerank (multi-signal) → MUST completeness → token-budget trim with disclosure (§6.5)
     ↓
format as instructions (MUST directive / REFERENCE body) → record_injection
     ↓
hand to the host: hook JSON envelope / MCP tool result / REST payload
```

Two things the original sketch got wrong, both load-bearing today:

- **Agent identity never filters retrieval.** Every agent searches the same
  corpus (§13.3); per-agent differentiation is `inject_rules` and
  `inject_channel` (§6.1.3), applied after retrieval. `exclude_types` is a
  **soft down-rank** (`score × 0.3` for non-MUST rows whose type or tags match)
  plus an intent-based penalty — not a hard filter, and **MUST is always
  exempt**. `namespace_filter` is a fallback namespace hint, not an access
  boundary.
- **There is no graph or daily-note query branch.** Relations are a single
  table with one-hop expansion; time is `occurred_from`/`occurred_to` and
  `as_of` (§9.6), not a lookup over dated files.

### 9.3 Human-in-the-loop review flow

1. An agent or extractor writes a memory with `human_reviewed: false` (and
   `ai_generated: true` for model output). It is stored immediately — there is
   no staging directory in the write path; `00-Inbox/` is an Obsidian-side
   projection, not the authority on the sqlite backend.
2. Dashboard → Review Inbox and the Obsidian sidebar list pending rows
   (`list_pending` / `list_inbox`).
3. The reviewer approves, edits or rejects. Approval flips
   `human_reviewed = true`. It does **not** synthesise an `instruction`: that
   field is written by the extractor or the caller, and injection renders it as
   the directive when present (§6.2).
4. On the markdown backend the frontmatter is rewritten; Obsidian backlinks and
   reindexing happen on sync.
5. Every injection channel — `session_start`, the MCP resources and `sync` —
   applies the same review gate, so an unapproved row is not injected anywhere
   (§13.8).

#### 9.3.1 Agent-proposed edits (`crate::proposal`)

> Added 2026-11. The review flow above covers memories an agent *creates*. This
> covers the harder case: an agent that wants to **correct a memory it did not
> author**, possibly a binding MUST.

An agent gets a file-shaped edit tool — exact unique-`old_text` replacement,
`field=content|instruction` — via `propose_memory_edit` (MCP),
`POST /api/memories/{id}/propose-edit` (REST) and `memvault-cli propose-edit`.
It is a **proposal**, never an edit:

```
agent proposes ─▶ new memory saved with  human_reviewed = false
                                         ai_generated   = true
                                         identity_verified = false
                                         proposes_supersede_of = <target>
                  target memory: UNTOUCHED, still reviewed, still injecting

human approves ─▶ proposal becomes reviewed AND supersedes the target
                  (one action — `proposal::approve`; doing either half alone is wrong)
human rejects  ─▶ proposal deleted; the target was never changed
```

**Approval is one storage-layer operation.** `proposal::approve_memory`
delegates the whole transition to `MemoryStore::approve_supersede`, which runs
"check the target is still unclaimed → retire the target (`superseded_by`,
L0) → approve the proposal (`human_reviewed`, link consumed)" as one
indivisible step: a single `BEGIN IMMEDIATE` transaction on SQLite — the
read-check happens under the write lock, which is what makes it a
compare-and-swap rather than a check-then-act — and an in-process serialized
critical section on Markdown, under the vault-wide cross-process lock. This
closes the two failure classes the earlier ordered-but-separable writes could
not: a crash between the writes left the old rule retired and the replacement
unapproved (nothing live to inject), and two concurrent approvals of two
proposals for one target both read "unclaimed" and both landed (two live
replacements of the same fact). Refusals (`TargetGone`, `AlreadyClaimed`)
write nothing and leave the proposal reusable. The retry arm — target already
points at this proposal — completes idempotently, recovering half-landed
legacy state from before this change and Markdown's (serialized, but not
crash-atomic) two-file window.

**Why a separate row rather than an in-place edit.** The obvious alternative —
apply the edit and revoke its trust — looks equivalent, because §13.8's gate
excludes untrusted memories from injection anyway. It is not equivalent: the
target stops injecting too, so **any agent could suppress any MUST simply by
proposing an edit to it**. That is a denial-of-injection vector aimed squarely at
the threat model §13.7 exists for. Keeping the target live until a human decides
costs one extra row and closes it.

**Why the link is a column, not a tag.** Approving consumes it and supersedes a
*different* row. An action like that should be keyed on a validated field rather
than a string prefix inside a user-editable list. The link is cleared on approval
so a second approve cannot re-supersede; the trail survives on the other side
(`target.superseded_by`) and in `memory_history`.

**Trust is never inherited.** A proposal resets `human_reviewed`,
`ai_generated`, `identity_verified` and `corroborating_agents` rather than copying
them from the target, so a reviewed, identity-verified, corroborated MUST cannot
lend its authority to an agent's rewrite of itself. It *does* inherit everything
that makes it a replacement rather than a downgrade — priority, namespace, type,
layer, visibility, `occurred_at` and `expires_at` (rewording a temporary fact
must not quietly make it permanent).

**While pending, a proposal is excluded from retrieval** (`proposes_supersede_of
IS NULL`, alongside the existing `superseded_by` filter): a pending change is not
current truth, and returning a memory next to its near-identical pending edit
would leave the caller unable to tell which is real. It stays visible through
`list_pending` / the Inbox, which is where it belongs.

**Ambiguity is refused, never resolved.** `old_text` must occur exactly once;
zero matches and 2+ matches are both errors naming the count. Non-overlapping
counting, so `"aa"` in `"aaa"` is one match, not two.

**One failure mode is detected and reported: `directive_still_says_old`.** Found
by running the real flow end to end rather than by the unit tests. Injection
renders `instruction` as the directive and `content` as the body (§6.2), so an
agent that corrects only the body leaves the thing every future session actually
obeys saying the old thing — the proposal approves cleanly, supersedes the
target, and changes nothing, while leaving body and directive contradicting each
other. It is reported (CLI warning with the exact follow-up command, MCP/REST
`directive_still_says_old`) rather than silently patched, because rewriting the
instruction too would be a guess about text the caller never asked to change.

**Authorization is split by consequence.** Proposing authenticates as an *agent*
— requiring admin there would make the endpoint useless to the only caller that
needs it. Approving requires admin (`approve_memory`), which is where the
destructive action lives.

**Pending proposals are also excluded from promotion.** `run_promote` builds new
higher-layer memories (L2 scenario summaries, L3 persona rules) by consolidating
existing ones. A proposal inherits its target's priority and therefore its
**layer**, so one aimed at a REFERENCE memory arrives at L2 with type
`preference` — which satisfies the L3 `stable` filter
(`access_count >= 2 || type == Preference`) with no access history at all. Left
alone, an unreviewed agent rewrite would be consolidated into a persona rule that
a human then approves on the strength of a summary that looks authoritative, and
only *then* starts injecting. `promote::hold_back` excludes proposals (and expired
memories) at all four phases and reports the counts in
`PromoteResult::excluded`. Superseded memories need no such check: `supersede`
demotes the old row to L0, so the layer filters already skip it — and it is
deliberately *not* counted as an exclusion, since inflating the report with
memories no phase ever considered is how a number gets ignored.

**Every review surface must label proposals.** A pending proposal is
*indistinguishable* from a near-duplicate draft — same namespace, near-identical
content, both unreviewed — so a reviewer who cannot see the link will reject it as
a duplicate (silently discarding a correction) or approve it without knowing a
different memory is about to be superseded. All four surfaces therefore carry it:
CLI prints `[proposes edit to <target>]`, `list_inbox` returns a `proposal` block
with the target's current text and what each action would do, REST returns the
field on `Memory`, and the review skill plus its four command variants instruct
the agent to check it first. The target's text ships *with* the listing so the
diff is one call rather than a lookup per proposal.

The three `SKILL.md` copies are kept byte-identical by
`scripts/check-rule-parity.sh` (canonical: `plugins/memvault/skills`), so editing
one alone fails CI. The four *command* variants are not copies but independently
worded summaries, so byte parity cannot apply to them; they are covered instead by
a content check that each still carries the `proposes_supersede_of` instruction —
without it, a rewrite that dropped the safety note would pass CI silently.

The same reasoning applies to approval: `review_memory` exists in **two**
implementations (`memvault-mcp` and `memvault-proxy`), so both had to be routed
through `proposal::approve`. Seven call sites across four surfaces now reach one
core function — which is the point of having one, and the reason the bug existed
is that the copies were being found by reading call sites rather than by grepping
for a helper.

### 9.4 Compliance feedback loop

The loop that shipped is deliberately short: **inject → record → judge at
session end → report**. See §6.3 for the mechanism and §13.9 for the honesty
rules.

```
injected memory + inject_session_id
        ↓
SessionEnd hook enqueues a finalize job (returns immediately)
        ↓
background worker reads the transcript tail and judges each row
  followed ✅ | violated ❌ | unknown ❔ (+ evidence, + reason)
        ↓
compliance_events → get_compliance_report / get_effectiveness_report / dashboard
```

**What is not implemented**: the original sketch's closed-loop remediation
("violations > 3 → automatically raise priority / change the injection point /
notify the user"). Verdicts are recorded and reported; priority and injection
rules are never rewritten from them. Automatic escalation would let a
mis-judged session change what binds every future session, so it stays a human
decision.

Effectiveness judgments (`useful` / `neutral` / `harmful` /
`insufficient_context`) are recorded separately from compliance verdicts by
`record_outcome`; the two are never folded together, because "the rule was
followed" and "the rule helped" are different questions.

### 9.5 Forgetting and decay

- `decay_score` decreases over time
- long-unaccessed → auto-archived to `50-Archive/`
- accessed again → weight restored
- past its `expires_at` valid-time bound → archived on the strength of the
  declaration alone, regardless of score and regardless of `must_exempt` (§9.6)
- policy is user-configurable

Archiving above was, until the forget feature below, a one-way ratchet with
no actual erasure: `delete`/`Review --reject` are soft deletes that snapshot
the full row into `memory_history` forever (restorable via `restore
--history-id`), and decay's own archiving never purged anything. Three
forget paths close that gap (`crates/memvault-core/src/forget.rs`):

- **Auto-forget**: memories that have sat in `archived:*` longer than
  `MEMVAULT_FORGET_RETENTION_DAYS` (default 90) are PERMANENTLY purged —
  live row, `memory_history` snapshots, and orphaned `compliance_events`
  rows are all deleted; only a content-hash audit entry survives in
  `forgotten_log`. Off by default (`MEMVAULT_FORGET_AUTO_ENABLED=1` to
  enable) — unlike decay's archiving, this is irreversible, so flipping
  every existing install's "archived forever" into "eventually purged" on
  upgrade must be an explicit opt-in. Must-priority memories are exempt by
  default (`MEMVAULT_FORGET_MUST_EXEMPT`); Shared-visibility memories are
  exempt from this automatic path specifically (`MEMVAULT_FORGET_SHARED_AUTO_EXEMPT`)
  since another agent may depend on them. The sweep is attached to every
  "run decay" entry point — `memvault-cli decay`, the `run_decay` MCP tool,
  and `POST /api/decay` — via `DecayManager::with_forget`, so it fires at
  the end of a normal decay cycle whenever the env var is set; it is not a
  separately scheduled job. Regardless of that flag, an admin can
  preview/run the identical sweep on demand (`namespace_prefix: "archived:"`
  + `archived_before_days`) through `POST /api/forget`, `memvault-cli
  forget --auto`, or the Dashboard's Settings → "Auto-Forget Sweep" panel —
  which also surfaces current enabled/retention status via `GET
  /api/forget/config`.
- **Manual/bulk forget**: a human can explicitly erase one memory or a
  criteria-matched set (namespace/namespace-prefix/source agent/tag/type/age)
  via `memvault-cli forget`, the `forget_memory` MCP tool, or `POST
  /api/forget` — always dry-run-previewable (the default) and requiring an
  explicit confirmation to execute, plus an explicit `--allow-must`/`allow_must`
  override to touch Must-priority memories. The Shared-visibility exemption
  does not apply here — a human explicitly choosing to forget a shared
  memory is sufficient confirmation. This is the GDPR right-to-erasure
  mechanism; `delete`/`Review --reject` remain soft deletes with
  forever-undo via `restore`.
- **Audit trail**: every erasure (auto, manual, or bulk) writes one
  `forgotten_log` row — memory id, a SHA-256 content hash (never the
  content itself), namespace, priority/visibility, scope, reason,
  requester, and timestamp — queryable via `memvault-cli forget-log` /
  `GET /api/forgotten-log`, proving an erasure happened without retaining
  what was erased. On the Markdown backend the same metadata is also written
  to the durable `<markdown-dir>/.memvault-forgotten.json` tombstone (§8.3.5)
  so a stale file copy cannot be reindexed back to life; keep that file with
  the vault and in backups, and treat a forgotten id as permanently retired.
- Known residual-text limitation: a forgotten memory's id/content may still
  appear inside free-text blobs elsewhere (e.g. `eval_runs.summary_json`,
  exports, logs) that aren't structurally keyed by memory id and are not
  rewritten by `purge()`.

### 9.6 Temporal model: three axes and point-in-time reads

> Added 2026-11. Closes the "Temporal / time-travel" row in §2.5's first table,
> which was the cheapest of MemVault's losing rows to close: the version chain
> already existed (`memory_history` snapshots from §9.3's review flow, plus
> `superseded_by` from §14 Track C), it just was not queryable by time.

MemVault keeps three time axes separate. Folding any two of them is what makes
"what did we believe in August" unanswerable without also losing "when did this
actually happen", so the separation is a constraint, not an accident:

| Axis | Field(s) | Question it answers | Set by |
|------|----------|---------------------|--------|
| **Ingest-time** | `created_at`, `updated_at` | when did MemVault take this in / last change it | the store, always |
| **Event-time** | `occurred_at` | when did the remembered fact actually happen | the ingesting caller, optional |
| **Valid-time** | `expires_at` + the `as_of` query | from when until when is this true, and what was true at T | `expires_at` by the caller; `as_of` by the reader |

**`expires_at` — the valid-time upper bound.** For facts that are inherently
temporary ("the deploy freeze runs until Friday"). Semantics:

- Past the bound, the memory leaves retrieval and injection. It behaves exactly
  like a superseded memory: still visible via `list` / `checkpoints` / `restore`,
  never physically removed by expiry alone — erasure stays `forget`'s job (§9.5).
- Filtered in every path that can reach an agent's context: keyword search,
  vector search, and `list_shared` (the team pool, which injects into *every*
  session and is therefore the most damaging place a stale fact could surface).
  Deliberately **not** filtered from `list`, which is the inspection path.
- The bound is **exclusive** (`expires_at > now`), and `Memory::is_expired_at`
  uses `<=` so the Rust and SQL forms cannot disagree.
- On disk the column holds a fixed-width stamp
  (`2026-12-31T00:00:00.000000Z`, via `expiry_stamp`) rather than
  `to_rfc3339()`'s variable-precision form. Fixed width is what makes plain
  lexicographic SQL comparison exactly chronological, which is what lets
  `idx_memories_expires_at` be used instead of wrapping the column in
  `datetime()` and forcing a scan.
- The decay cycle archives expired memories **regardless of `decay_score`**, and
  **regardless of `must_exempt`**. The asymmetry is deliberate: `must_exempt`
  protects a MUST from an *inferred* signal (disuse), while `expires_at` is an
  *explicit* declaration. A MUST whose window has closed is a binding
  instruction that is no longer true, and injecting it is worse than retiring
  it. `DecayReport::expired_archived` keeps the count visible rather than folded
  into `archived`. Archiving on expiry is also what connects expiry to §9.5:
  `ForgetManager::auto_forget` measures retention against `archived_at`, so an
  expired-but-never-archived memory would linger in the live table forever.
- **Derived memories inherit the earliest bound of their sources.** The
  promotion pipeline (L1→L2 consolidation, L2→L3 persona rules, fact
  consolidation) builds summaries that assert their sources' content, and a
  summary must not outlive the shortest-lived of them: the new memory's
  `expires_at` is the minimum of the sources' non-`None` bounds, `None` only
  when every source is permanent. A `None` default laundered temporary facts
  into permanent summaries that kept injecting — human approval and all —
  long after every source had retired; the pipeline's already-expired input
  exclusion cannot help there, because the summary itself never expires.
  Turning temporary knowledge into permanent knowledge is a deliberate human
  act (review the summary, clear or extend the bound), never an omission of
  the pipeline. The rule can shorten or preserve a lifetime, never lengthen.
- **Expiry is a state change `sync --watch` can see.** Expiry writes nothing,
  so a `COUNT`/`MAX(updated_at)` change-detection hash never moved on it and
  the generated rules files kept serving retired rules until some unrelated
  write happened. `sync_state_hash` now folds in the live count of expired
  memories: it flips at the expiry instant — triggering the regeneration,
  which already excludes them — and is stable between crossings, so watchers
  do not rewrite files on every poll. The markdown backend delegates the hash
  to its SQLite cache, so the one change covers both.

**`as_of` — point-in-time reads.** `write_history` snapshots the *pre-operation*
row on every update and delete, so the chain reads: snapshot 1 was current during
`[created_at, t1)`, snapshot 2 during `[t1, t2)`, … and the live row during
`[tn, now)`. The state at `as_of` is therefore the **first snapshot whose
`changed_at` postdates `as_of`**, falling back to the live row. No new schema was
needed.

Three read paths, with different guarantees:

| Path | Guarantee |
|------|-----------|
| `get_as_of(id, as_of)` / `memvault-cli as-of <id> --at <T>` | **Exact.** Returns `None` when there was no state then (not created yet, already deleted, or past its `expires_at`) rather than falling back to the current row. Reports `AsOfSource::Live` vs `AsOfSource::History { history_id, replaced_at }` so a far-back answer is never mistaken for a current one |
| `list_as_of(as_of, …)` / `memvault-cli list --as-of <T>` | **Exact.** Enumerates live ids ∪ `memory_history` ids, so a memory deleted *after* `as_of` still appears — at `as_of` it was current truth. Namespace filtering happens *after* reconstruction, because a deleted memory's namespace is only recoverable from its snapshot. Not a hot path: cost scales with vault history, not with `limit` |
| `search(as_of=…)` / `memvault-cli search --as-of <T>` | **Keyword-exact, vector-partial.** Two recall passes are unioned: the current FTS index projected onto `as_of`, and `memory_history_fts` (migration 28) which matches the text a memory had *then*. So a memory reworded since `as_of` is found by its old words — the limitation this row originally recorded. A historical match is kept only when that snapshot is the one actually in effect at `as_of`, because a memory edited three times has three historical versions and only one was current then. Such hits carry `hist-kw#N` provenance (`HitSource::HistoryKeyword`) so "matched today" and "matched then" stay distinguishable, and `as_of_source` says what was reconstructed. **Residual:** the vector path still embeds current text, so a reworded memory is reachable by its old words but not by its old meaning — covering it would mean embedding every snapshot and keeping those vectors in step with the history table |

Supersession interacts correctly by construction: because `supersede` goes through
`update`, the pre-supersede snapshot has `superseded_by: None`, so querying a
superseded memory at an `as_of` before the supersede returns it as the current
truth it was then.

**Event-time is a separate parameter pair, not a shared range.**
`occurred_from` / `occurred_to` filter on `occurred_at` and are never folded into
`as_of`, because a memory can have *happened* in March and still have been
*believed* in August — one `since`/`until` pair would make one of those two
questions unanswerable, and which one it loses would depend on the caller's
guess. `test_event_time_and_valid_time_axes_stay_independent` pins exactly that
case: the same memory, the same month, opposite answers from the two axes.

Undated memories never match an event-time window. That is deliberate — an undated
memory cannot be said to have happened inside a range, and admitting it would
return the same rows for every window, which is noise dressed as a result. The
consequence is that the filter only means anything over memories saved with a
date, which is why `occurred_at` became settable on the direct-save path (§9.6) and
why the CLI states the rule whenever the flag is used rather than only when the
result comes back empty. An inverted window is an error, not an empty result: an
empty result would be *correct* and completely unhelpful, sending the caller to
debug their data instead of their typo.

Unlike `expires_at`, the window is compared through SQL `datetime()` rather than
as raw text. `expires_at` is written in a fixed-width form precisely so plain
lexicographic comparison stays chronological and `idx_memories_expires_at` remains
usable; `occurred_at` is written by `to_rfc3339()` with variable fractional
precision and has no index, so there is nothing to lose by normalising both sides,
and second precision is ample for an event window.

**Where these predicates are evaluated.** Three retrieval paths need them and
only one can express them in SQL. The live keyword path can, via
`append_filter_clauses`. The historical `as_of` path cannot, because a state
reconstructed from `memory_history` may have no live row at all. And the
semantic/hybrid composition paths cannot, because `vector_search` takes only an
embedding, a `top_k` and a namespace — it merges keyword hits that *were*
filtered in SQL with vector hits that were not. The two Rust paths therefore
share one predicate, `storage::memory_matches_filters`, and the SQL one stays
SQL because it has to.

The invariant to hold onto: **a filter that only one path applies is not a
filter.** It fails as a search bug rather than a filter bug, so it reads like
bad ranking instead of a missing predicate, and it has been violated three
times — `as_of` alone, then the event-time window, then
`type_filter`/`priority_filter`. Both Rust paths also agree on *order*: `as_of`
projects first and filters the reconstructed state, so a memory superseded
since `as_of` still counts as the current truth it was then.

**`as_of` recall also searches the historical index.** `memory_history_fts`
(migration 28) indexes each snapshot's searchable text through the *same*
`fts_columns` helper as the live index, so the two can never tokenize one memory
differently. An `as_of` search unions two passes — the current index projected
onto `as_of`, and the historical index — and keeps a historical match only when
that snapshot is the one actually in effect at `as_of`, because a memory edited
three times has three historical versions and only one of them was current then.
Such hits carry `hist-kw#N` provenance, and the single-path provenance stamping
at the end of `search` fills only *empty* `hit_sources` — it used to overwrite
unconditionally, which would have relabelled every historical hit as a
current-text match and erased the one signal distinguishing the two. The index is
backfilled on open by count comparison, the way `rebuild_fts_index` self-heals, so
an upgraded vault's existing snapshots become searchable without a repair command.

The residual limitation is that this is the **keyword** path only. The vector path
still embeds current text, so a reworded memory is reachable by its old *words*
but not by its old *meaning*. Covering it would mean embedding every snapshot and
keeping those vectors in step with the history table — a much larger change for a
narrower gain, since the failure being fixed was "the wording changed", which is
lexical.

**Markdown backend caveat.** The cache is the derived side of §8.3.2's inverted
data-flow, and a history snapshot is not a file — so a rebuilt cache holds no
snapshots and every reconstruction degrades to `AsOfSource::Live`. Point-in-time
reads on that backend only see changes made since the last reindex. `expires_at`
*does* round-trip fully, via the `expires:` frontmatter key. Making the version
chain durable there would mean writing history into the vault itself, which is a
separate decision (§8.3.8).

---

### 9.7 Task-level evaluation

**Motivation**: the Qwen3.8-Flash-Next report's most counter-intuitive result —
training loss falls monotonically as memory vocabulary grows, while downstream
task performance saturates. Retrieval recall is not task success, and competitors report
retrieval metrics.

**Design**: `memvault-core::bench` + `memvault-cli bench` samples the user's own
failure history (episodes that distilled a lesson) in three layers:

| Layer | Metric | Dependency |
|---|---|---|
| retrieval | does the lesson reach top-k when the historical task text is the query? | none |
| injection | does it survive the full `session_start` pipeline and the token budget? | none |
| judge (`--judge`) | an LLM writes "plan without memory" vs "plan with the injected lesson" and scores both against the known failure cause | an LLM provider |

The first two layers always run; a judge failure degrades to "not evaluated"
rather than erroring out. Every run is persisted, and
`memvault-cli eval-history --kind bench` shows the trend.

---

## 10. Roadmap status

All planned phases (0–6) have been delivered; the section records the roadmap
that produced the current architecture plus which items remain open.

| Phase | Scope | Status |
|-------|-------|--------|
| 0 — technical validation | rmcp selection, embedding latency baseline, MCP Resource injection, `session_start` auto-call | ✅ delivered (LanceDB PoC was evaluated and replaced by SQLite-embedded int8 vector columns in v0.2) |
| 1 — core engine + router + multi-agent basics | local service skeleton, SQLite storage, MCP Server (save/search/session_start), MCP Resources (`memory://user-profile`, `memory://project-context`), Agent Registry, per-type filtering, MUST/REF format engine, token budget, Markdown + frontmatter, vector retrieval, CLI, multi-agent E2E, Claude Desktop integration | ✅ delivered. Injection in Phase 1 uses MCP Resource + `session_start`; pre-prompt injection (needs the proxy) and the compliance tracker were deferred at the time and have since shipped (§6.1, §6.3, §13.9) |
| 1.5 — multi-agent sharing hardening | config-driven Agent Registry (YAML), structured `source_agent` (id/type/session_id), `inject_session_id` tracking, per-agent compliance logging, two concurrent MCP clients, shared-memory correctness | ✅ delivered |
| 2 — retrieval + proxy + Obsidian | hybrid retrieval (BM25 + vector + structured filters), query rewriting, rerank, MCP proxy prototype, local embedding models, Obsidian plugin connection, inbox review panel, bidirectional sync, MCP Resource auto-load | ✅ delivered |
| 3 — dashboard + compliance | web dashboard (`memvault-mcp --transport http --serve-web`), review-queue UI, agent activity panel, compliance dashboard, memory-type graph view, full proxy integration, import/export | ✅ delivered |
| 4 — smart pipeline + ecosystem | background extractor worker, dedup/merge/conflict detection, forgetting curve/decay, VS Code extension, pluggable storage backends | ✅ delivered (background extraction runs via proxy hooks; VS Code ships as a config profile, not an extension package); forgetting curve/decay extended with permanent auto/manual/bulk forget (GDPR erasure) — see §9.5 |
| 5 — extensions | web app, CRDTs multi-device sync, graph database, team-shared memory pool, plugin marketplace | partially delivered — team shared pool shipped (see §14); CRDTs and graph database remain open (§15.2); plugin marketplace not started |
| 6 — three-track memory loop | episodic (Phase A), procedural (Phase B), semantic (Phase C), optional items (Phase D) | ✅ delivered; acceptance evidence in `docs/experiments/REPORT.md` (see §14) |

---

## 11. Differentiation strategy

### 11.1 Core differentiation (layered moat)

> **Recalibrated 2026-11** against the refreshed market analysis (§2). The
> original version of this table rated L2 as a 🔴 core moat on the grounds that
> "almost nobody" did auto-injection. That is no longer true — Claude Code,
> Codex and Cursor all auto-load memory natively (§2.2). L2 is therefore
> **demoted to 🟡**, and its surviving differentiation is narrowed to the part
> the platforms structurally cannot do: one corpus shared across every agent.
> L6 is **new**, promoted from parts that were already shipped but never named as
> a single position (§2.7).

| Layer | State of the art (2026-11) | Our approach | Moat strength |
|-------|----------------------------|--------------|---------------|
| L1: storage & retrieval | done well by Mem0 / Zep / memSearch / agent-memory | integrate, don't rebuild | 🟢 leverage |
| L2: auto-injection (router) | **no longer empty space** — Claude Code Auto Memory, Codex Memories and Cursor Memories all auto-load natively, with zero infrastructure | pre-prompt + MCP Resource + session bootstrap, over **one corpus shared by every agent the user runs** | 🟡 differentiator — the *injection* is now table stakes; the *cross-agent sharing* is not, because each platform only loads its own silo |
| L3: follow-through guarantee | **still empty space** — no surveyed product measures whether an injected memory was actually obeyed | MUST/REF + instructional format + `inject_session_id` compliance tracking + honest accounting via the SessionEnd finaliser (§13.9) | 🔴 core moat |
| L4: human collaboration | partially exists — Cursor's per-item approval is the same loop, for one IDE | Inbox + Dashboard + markdown-backend two-way consistency (§8.3.3) | 🟡 differentiator |
| L5: native Obsidian | Khoj/SC do RAG | memory-management-specific UI | 🟡 differentiator |
| L6: trusted memory | **empty space** — `Memoria` reached for this position and did not survive (§2.7); agent-memory's `provenance` is *data lineage*, not *trust* | provenance-gated MUST (§6.2) · identity verification + multi-agent corroboration gate (§13.7) · unreviewed memories excluded from injection (§13.8) · agent-proposed edits gated through review rather than applied (§9.3.1) · `owned`/`quarantine` trust boundary (§8.3.4) · SHA-256 erasure tombstone (§9.5) | 🔴 core moat |

> **L6 is the answer to a question nobody else asks.** Every other layer asks
> *what should the agent know*. L6 asks *what entitles this memory to bind the
> agent* — and then, via L3, *whether it did*. The two are what remains once
> auto-loading is free. Note the asymmetry with agent-memory's three-layer
> authorisation stack (§2.3): that stack answers *who may access a memory*, which
> is a multi-tenancy concern; L6 answers *whether a memory may be believed*,
> which is a content-trust concern. They are not substitutes, and only L6 is
> load-bearing for a local-first single-user vault.

### 11.2 Positioning statement

> "Mem0 and Zep solved 'how to store and find'.
> Claude Code, Codex and Cursor solved 'how to make memories appear' — each for
> itself alone.
> We solve 'how to make **one shared** memory appear in **every** agent you run,
> **what entitles it to bind them**, and **whether they obeyed it**'.
> Don't teach agents to query memories — make memories appear automatically."

The last line is the original v0.3 tagline and is still the one-liner in the
document header and README. It is kept because it remains the correct
*onboarding* promise — it is simply no longer the *differentiating* one, since
§2.2 shows three platforms now deliver it for their own ecosystem. The three
bolded clauses above are what MemVault is positioned on instead, and they map
one-to-one onto §11.1's L2 (shared) / L6 (entitles) / L3 (obeyed).

### 11.3 Relationship to the ecosystem

```
  Tier 1 — memory layers (Mem0 · Zep · memSearch · agent-memory)
           solve storage & retrieval … for one SDK, one graph, one tenant model
                                    ↓
  Tier 2 — platform-native memory (Claude Code · Codex · Cursor)
           solve auto-loading … for their own silo only (§2.2)
                                    ↓
  MemVault Memory Router
     one shared corpus → every agent   ·   MUST enforcement at the instruction layer
     trust-gated binding (L6)          ·   compliance measured on the way out (L3)
                                    ↓
  delivered through whichever channel the target agent actually supports
     MCP stdio/SSE · MCP Proxy · AGENTS.md / CLAUDE.md sync · Obsidian · Dashboard
     (per-agent `inject_channel` keeps one memory from reaching one agent twice)
```

Two notes on how this differs from the v0.3 version of this diagram.

**Storage is no longer drawn as an upstream dependency.** The original diagram
showed `MemPalace / Mem0 (storage backends) → pluggable integration → MemVault`.
MemPalace has since disappeared from the market (§2.7), and MemVault's storage is
its own SQLite-or-markdown backend (§8.3) rather than an adapter over someone
else's. Drawing Tier 1 as *upstream* implied MemVault was a front-end for it;
Tier 1 is now drawn as a **peer alternative** that users choose between.

**The distribution layer is channel-plural on purpose.** §2.2 establishes that
`AGENTS.md` is now a Linux Foundation standard read by Codex, Cursor, Aider and
Jules — which makes `sync` the only injection path that works on agents with no
MCP support at all. That is a wider reach than the MCP path, and it is why
§11.1's L2 survives as 🟡 rather than collapsing to 🟢: MemVault can inject into
agents the platforms' own memory cannot reach, and into all of them from the same
store.

---

## 12. Risks and mitigations

| Risk | Severity | Mitigation |
|------|----------|------------|
| **Platform-native memory erodes the auto-injection moat** | 🔴 high | Claude Code Auto Memory, Codex Memories and Cursor Memories now auto-load with zero infrastructure (§2.2), so "we inject automatically" is no longer a reason to install MemVault. Mitigation is repositioning, not features: the platforms are structurally single-tool silos, so MemVault competes on L3/L6 (follow-through, trusted memory) and on reaching every agent from one store (§11.1). Concretely: an agent that only reads `AGENTS.md` gets MemVault's corpus and none of its platform's |
| **Evaluation effort spent on non-discriminating benchmarks** | 🟡 medium | LoCoMo/LongMemEval no longer separate a memory system from a 1M-token context window (§2.6). Target BEAM's instruction-adherence axis and MemoryAgentBench's conflict/forgetting axis instead, publish on AMB with a full harness specification, and always report p50/p95 next to accuracy — the unmeasured latency advantage of two-phase injection (§6.1.2) is worth nothing as positioning |
| Cold start is hard | 🔴 high | Obsidian plugin as the funnel; the markdown backend (§8.3) makes an existing vault importable, and `sync` means an agent benefits before it supports MCP at all |
| Router mis-injects (irrelevant memories) | 🟡 medium | conservative policy: fewer over more; users can disable auto-injection; every dropped candidate is recorded with a reason, so mis-injection is auditable rather than mysterious |
| Agents ignore the MCP contract | 🟡 medium | fallback modes: tool + resource + system prompt; `sync` to `AGENTS.md`/`CLAUDE.md`/`.cursor/rules/` as the MCP-free path |
| Editing UX worse than Obsidian | 🟡 medium | don't clone an editor; focus on memory management |
| Sync complexity | 🟡 medium | local-first with file export as the escape hatch |
| Token explosion | 🟡 medium | hard token budget caps; summaries by default; and since 2026-11 **progressive disclosure** (§6.5) — a memory that does not fit is degraded to an excerpt or summary before it is dropped, with the abbreviation marked in the text and the level reported. MUST is exempt from degradation: a binding instruction with its qualifying clause summarised away is worse than one that overflowed |
| **Local embedding latency** | 🟡 medium | native in-process inference; models are small and cached locally; two-phase injection (§6.1.2) serves the deterministic MUST baseline immediately and never blocks on the semantic pipeline |
| Privacy compliance | 🔴 high | local-first; data dir `0700` / DB `0600` and no network calls unless a provider is configured — **no at-rest encryption**, so rely on OS disk encryption (FileVault/LUKS) for a stolen-disk threat; the forget feature (§9.5) delivers the GDPR right-to-erasure mechanism — auto/manual/bulk permanent purge with audit trail; residual-text in free-form logs/exports remains a known gap |
| Big-vendor copying | 🟡 medium | **Raised from 🟢 after the 2026-11 refresh** — three vendors shipped native memory within a year (§2.2), so this is an observed behaviour, not a hypothesis. Open core + community moat + the vertical (L3/L6) that a per-tool vendor has no incentive to build, since it requires memory to be shared *across* vendors' own products |
| ~~MCP protocol does not support message interception~~ | ✅ retired | Resolved: the MCP Proxy shipped in Phase 2/3 (§10). Kept as a row so the historical mitigation chain in older CHANGELOG entries still resolves |
| ~~Compliance tracker can't see agent replies~~ | ✅ retired | Resolved: the proxy architecture plus the SessionEnd finaliser deliver honest compliance accounting (§13.9) |

---

## 13. Multi-agent shared memory design (added in v0.3)

### 13.1 Design goal

```
multiple agents → one MemVault instance → shared memories, filtered per consumer
                               ↓
                 every agent gets ALL approved MUST rules (never filtered)
                 non-MUST candidates are ranked differently per agent
                 (exclude_types soft penalty, namespace hint, token budget)
                 ──────────────────────────────
                 underlying store is the same SQLite (with embedded vectors)
```

### 13.2 Agent Registry

Each connecting agent registers an identity; the router tailors injection
accordingly:

```yaml
# _Router/agent-registry.yaml
agents:
  - id: claude-desktop
    type: coding-assistant
    description: "daily coding assistant"
    inject_rules:
      max_memories: 8
      token_budget: 1500
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
      exclude_types: ["writing", "design"]  # don't inject writing/design memories

  - id: cline-vscode
    type: general-assistant
    description: "general assistant inside VS Code"
    inject_rules:
      max_memories: 5
      token_budget: 1000
      priority_order: ["MUST"]
      namespace_filter: ["global"]
      exclude_types: []

  - id: cursor-ide
    type: code-ide
    description: "assistant embedded in Cursor IDE"
    inject_rules:
      max_memories: 6
      token_budget: 1200
      priority_order: ["MUST", "REFERENCE"]
      namespace_filter: ["global", "project:*"]
      exclude_types: []
```

> **Field semantics, as implemented**: `max_memories`, `token_budget` and
> `priority_order` are hard bounds used by the trimmer. `exclude_types` is a
> **soft penalty** on non-MUST rows (×0.3, plus an intent penalty when the tag
> domain conflicts); it never removes a row and never touches MUST.
> `namespace_filter` is a fallback namespace hint (`global` plus one project
> scope), not a restriction — the store query is never narrowed by agent
> identity.

### 13.3 Sharing policy matrix

| Operation | Policy | Notes |
|-----------|--------|-------|
| **Write** | all agents write to the same store | data is naturally shared; `source_agent` records the origin |
| **Read** | every agent reads every memory | no isolation in Phase 1 |
| **Inject** | non-MUST ranked per agent; MUST shared by all | `exclude_types` is a soft score penalty (×0.3) and MUST is exempt; `namespace_filter` is a hint, not an access boundary |
| **Review** | one review benefits all agents | approved `human_reviewed: true` applies globally |
| **Compliance** | aggregate, filterable per agent | `compliance_events` rows keyed by `inject_session_id` + `agent_id`; `get_compliance_report` takes an optional `agent_id` (§13.9) |
| **Forgetting** | applies globally | once archived, no agent receives it |

> **`agent_id` never filters retrieval.** It is provenance metadata only —
> recorded in `source_agent` at write time, never consulted by store queries
> (search / list / vector search filter on namespace × type × priority).
> Every agent searches the same shared corpus; per-agent differentiation
> happens only at injection time via `inject_rules` (type/tag soft scoring,
> §13.2), never in the store.
>
> `SearchQuery` accordingly has **no** `agent_id` field. It carried one until
> 0.7.x: the request-level `agent_id` (which is the caller's identity, used for
> authentication and audit) was copied into it at eight call sites, and no store
> query ever read it back. The field sat between `query` and the three real
> filters, so it read as a fourth one that had come loose rather than as
> something this section forbids — which is why it survived. Do not re-add it;
> if per-agent retrieval isolation is ever wanted it is the Phase 3 work listed
> below, and it needs this section and the Read row of the matrix rewritten
> first.

### 13.4 Phase 1 multi-agent scope

```
✅ Supported scenarios:
  - Claude Desktop + Cline + Cursor connected to one MemVault instance
  - all agents share the same memory data
  - source_agent records the write origin
  - the router soft-ranks non-MUST candidates by agent type (MUST always shared)

❌ Not yet supported (Phase 3+):
  - per-agent permission isolation (allowlist/denylist)
  - multi-device sync (CRDTs)
  - semantic conflict merge for concurrent writes
  - agent-private namespaces
```

### 13.5 Typical deployment topology

```
┌─────────────────────────────────────────────────────────┐
│ same machine                                            │
│                                                         │
│  ┌──────────────┐    ┌──────────────┐                   │
│  │ Claude       │    │ VS Code      │                   │
│  │ Desktop      │    │ (MCP client) │                   │
│  │ (MCP client) │    │              │                   │
│  └──────┬───────┘    └──────┬───────┘                   │
│         │                   │                           │
│         └─────────┬─────────┘                           │
│                   │ MCP protocol                        │
│         ┌─────────▼─────────┐                           │
│         │  MemVault Service │                           │
│         │  (single process) │                           │
│         │                   │                           │
│         │  ┌──────────────┐ │                           │
│         │  │ SQLite +     │ │                           │
│         │  │ embedded     │ │                           │
│         │  │ vectors      │ │                           │
│         │  │ (shared)     │ │                           │
│         │  └──────────────┘ │                           │
│         └───────────────────┘                           │
└─────────────────────────────────────────────────────────┘
```

### 13.6 Multi-agent-specific risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Agent A writes a wrong memory, polluting Agent B | 🟡 medium | default inbox review in Phase 1; allowlists later; since v0.3.1 MUST memories additionally face identity verification + multi-agent corroboration gates (§13.7) |
| Two agents write contradictory memories | 🟡 medium | dedup & merge; timestamp LWW strategy |
| Agent identity forgery | 🟡 medium | MCP connection verification; token auth later; since v0.3.1 the write path records whether the call actually passed registry key verification (`identity_verified`) instead of trusting a self-reported `agent_id` (§13.7) |
| Injection volume doubles (each agent injecting separately) | 🟢 low | each agent has an independent token budget |

### 13.7 Delivered: identity verification + multi-agent corroboration trust gate for MUST memories (v0.3.1)

**Motivation**: the core contradiction from §13.6 is that `is_trusted`
(`router/format.rs`) — which decides whether a MUST memory may be injected as
an instruction — previously relied only on `human_reviewed`/`ai_generated`,
both self-reported by the writer. Under the "local memory hub managing all
local agents" positioning, any prompt-injected agent could simply declare
`ai_generated: false` at write time and bypass the whole trust gate, spreading
a malicious MUST instruction to **every** agent reading the hub — the blast
radius is all agents, not the writer itself. Namespace hard-isolation is the
wrong countermeasure (it contradicts the shared-hub product intent); what was
missing is "who really wrote this memory, is it credible" and "did other
independent agents corroborate the same content".

**Design**: two new `Memory` fields, both additive with defaults preserving old
data/behavior:

- `identity_verified: bool` — whether this write's `agent_id` actually
  registered an `api_key` in `agents.yaml` and passed `AgentAuth::authenticate`,
  rather than merely claiming the id. Produced by
  `MemoryRouter::authenticate_agent_verified` at both entry points (the MCP
  `save_memory` tool and REST `POST /api/memories`). Recordable only; does not
  change trust decisions. Disable with `MEMVAULT_IDENTITY_VERIFICATION=off`
  (records on, default).
- `corroborating_agents: Vec<String>` — on the delta-write merge path
  (`writer::merge_memory`, §9.1.1), the set of distinct `agent_id`s, each
  `identity_verified`, that independently corroborated the content; unverified
  writes are not counted.

`is_trusted` gained a third decision path (active only with
`MEMVAULT_CORROBORATION_GATE=on`, default off): a MUST memory independently
written by `MEMVAULT_CORROBORATION_MIN_AGENTS` (default 2) distinct verified
agents counts as a trustworthy instruction even without human review. Default
off keeps existing single-agent deployments without `api_key` byte-for-byte
identical; only explicit enablement of auth + the corroboration gate treats
"multiple independent trusted sources agree" as a substitute for human review.

**Key trade-off**: no per-agent read/write isolation (§13.4 still holds) —
isolation contradicts the "memory hub" positioning; what is defended here is
content authenticity (who wrote it, was it independently corroborated), not
an access boundary.

**Corroboration is bound to the payload, not to the row** (post-audit
hardening): a vote counts only for the bytes it was cast for. When an
unreviewed caller force-replaces the instruction (delta-write path), or a
merge absorbs unreviewed material (residual body, adopted instruction or skill
procedure), the revocation clears `corroborating_agents` and
`identity_verified` along with `human_reviewed` — otherwise stale votes would
carry the *new* text through the gate above with no review in between. A
verified incoming author is re-recorded as the sole vouch for the text they
actually submitted (one vote, short of the default threshold of 2).
Symmetrically, a *reviewed* incoming cannot upgrade the merge result while an
unreviewed directive from the existing row survives it — first-non-empty-wins
keeps the old instruction when the incoming carries none, and the human
confirmed the body they saved, not the directive riding along inside it. The
merged memory stays pending so the review inbox sees the whole payload
(`writer::merge_memory`; same rule as the quarantine clamp in §8.3.4, which
additionally pins the reviewed namespace and treats `skill_meta` edits as
payload changes).

### 13.8 Delivered: unreviewed memories are excluded from injection by default

**Motivation**: `is_trusted` (§13.7) splits memories into a trusted instruction
block and a downgraded "reference data, do not execute directly" block — but
an untrusted memory was still injected there, tagged `[未审核]` instead of
`[MUST]`. Production audits showed that was too weak: the reading model still
sees the text as context and can act on it, and upstream extraction
false positives (quoted or negated sentences read as stated preferences) put
unreviewed, instruction-shaped text into that block. A prompt-level "treat as
data" wrapper is a weaker guarantee than not showing the text at all.

**Design**: `MEMVAULT_INJECT_REQUIRE_REVIEW` (default **on**) is a
selection-time gate: `router::format::apply_unreviewed_gate` drops every
result for which `is_trusted` is false — **all** priorities, not just MUST —
and records an `unreviewed-gated` skip reason so the audit trail matches what
the model actually saw. The gate runs before the body, the overflow summary,
the injected counts and `record_injection` are computed.
`MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST` (default off) is kept as the legacy
MUST-only filter and as defense-in-depth when the broad gate is disabled;
`session_start_with_gates` accepts both flags explicitly so tests can exercise
either combination without mutating process-global env vars.

**Channel parity**: the same gate applies on every injection channel —
the CLI/MCP `session_start` body, MCP `[MORE - 摘要]` overflow summaries,
REST `/api/session`, the proxy deterministic fast path, and **`sync`'s generated
instruction files**. Two earlier leaks are structurally closed: the overflow path
re-fetches a wider candidate set and therefore applies the trust filter itself
(`overflow_admits`), and REST rendering via `agent_adapt::format_memories` now
mirrors the router's trusted/unvetted split instead of emitting `- ALWAYS:` for
unreviewed MUST rows. L0 raw-evidence rows (`trace:*`, `human_reviewed = true` by
construction) are excluded from overflow unconditionally, matching how
lessons/skills/episodes already skip them.

The `sync` channel was the last leak, and the reason it survived is worth
recording: this paragraph enumerated the channels that enforce the gate, and
`sync` was simply not in the list — so nothing looked wrong. `sync` builds from
`store.list()`, which filters *nothing* on purpose because it is the inspection
path, and it wrote straight into `AGENTS.md` / `CLAUDE.md` / `.cursor/rules/`,
which Codex, Cursor and Aider read at session start. That is injection with no
prompt-level wrapper at all — strictly weaker than the path this section
hardened. `SyncEngine::select_memories_reporting` now applies the gate by
**calling `router::format::apply_unreviewed_gate` rather than reimplementing the
predicate**, so the file channel and the session channel cannot drift, and
reports the count it held back (`SyncReport::memories_excluded`, printed by
`memvault-cli sync`). The same call site applies the three structural exclusions
that `list()` also does not perform: superseded (§14 Track C), pending proposals
(§9.3.1) and expired (§9.6). Setting `MEMVAULT_INJECT_REQUIRE_REVIEW=false`
restores the previous unfiltered behaviour for the review gate specifically; the
three structural exclusions are unconditional, since writing a superseded or
lapsed rule into an instruction file is not a trust question.

The MCP `memory://` resources were the next channel missing from that list, and
they are only *half* in it now — deliberately, which is worth stating plainly
because "one channel gated, the adjacent one not" is exactly the shape that
looks like an oversight.

`memory://user-profile` **is** gated. It filters to MUST, and MUST is by
definition binding: it is the priority whose whole purpose is "act on this".
Serving an unapproved one under §13.7's "reference data, do not execute" wrapper
is self-contradictory — either it is a MUST rule and it should have been
reviewed, or it is reference data and it should not be MUST. The resource is also
advertised to clients through `list_resources` as "Loaded at conversation
start", which makes it an injection channel rather than a browse-only dump, so it
gets the injection rule. It calls `apply_unreviewed_gate` rather than
reimplementing the predicate, for the same reason `sync` does, and gates before
`trim_to_budget` so the token budget is spent on memories that will be served
rather than on ones about to be dropped.

`memory://project-context` **is not** gated, and that asymmetry is the design
rather than a gap in it. It filters to REFERENCE, where an unreviewed draft
genuinely is reference data and §13.7's wrapper says something true. Gating both
would hand every fresh vault an empty project context: `Memory::new` defaults to
`ai_generated = true`, so agent-captured and auto-extracted memories — the
high-volume paths in an agent-first product — are all unreviewed until somebody
reads them. The "Provenance vs. auto-drafts" rule below is what keeps that from
being a hole: deliberate human writes are trusted at the source, so what stays
behind the gate is only drafts nobody has looked at.

Two consequences to keep in view. `list_resources`' description for
`project-context` now says out loud that it is ungated reference data which may
contain unapproved drafts — previously it described itself as neutral project
context while `user-profile` claimed conversation-start loading, so what the
server advertised and what it filtered disagreed. That has to hold in **both**
servers: the proxy answers `memory://` reads locally rather than forwarding
them, and `merge_by_key` prefers local over upstream, so a vaguer description
in `memvault-proxy` silently overrides the honest one `memvault-mcp` advertises
for the same URI. The two are kept in step for that reason.

And the residual difference
from `session_start` is real, not an oversight: `session_start` holds back
unreviewed REFERENCE too, because it produces the instruction block injected
into a session, whereas `project-context` is a named data resource whose body
carries the wrapper. `e2e_mcp_resources` pins both halves — the gate on one
resource and the pass-through on the other — because a test asserting only the
gate would still pass if both resources were gated, and one asserting only the
pass-through would still pass if neither were.

**Provenance vs. auto-drafts**: `memvault-cli save` is an explicit
human-initiated write and stamps `human_reviewed = true`,
`ai_generated = false`. Stop-hook extraction, the `extract_memories` tool and
`record_outcome` lessons keep landing unreviewed — which is exactly what the
gate is for.

**Compliance honesty** (§13.9): a store full of `pending` rows used to report
100% compliance because the rate was hard-coded to `1.0` whenever no verdict
existed. Rates are now `Option<f64>` — `None`/`N/A` until a MUST row has a
real followed/violated verdict — and the `SessionEnd` hook
(`MEMVAULT_HOOK_COMPLIANCE=1`) closes each session's pending rows as judged
(with an LLM extractor configured) or explicitly `unknown`, never fabricated.

**Key trade-off**: a dropped memory is not deleted — it still exists in the
store, stays in the review inbox, is returned by `search_memory`, and surfaces
in `session_start` once approved (`human_reviewed = true`); this only narrows
what auto-injection shows.

### 13.9 Delivered: honest compliance accounting and the SessionEnd finalizer

**Motivation**: `record_injection` recorded that a memory was injected, but
nothing in the hook path ever recorded whether the agent followed it. Every
`compliance_events` row stayed `pending`, and the report hard-coded `1.0`
when no MUST verdict existed — a session the agent ignored looked perfectly
compliant, which made the metric worse than useless.

**Design**: `compliance_rate`, `overall_rate` and `must_rate` are
`Option<f64>`; `None` renders as `N/A` in the dashboard and as `null` in
MCP/REST payloads. A rate only exists when a MUST row carries a real
followed/violated verdict.

`SessionStart` stamps `compliance_events.host_session_id`, and the plugin's
`SessionEnd` hook (`MEMVAULT_HOOK_COMPLIANCE=1`, default on) runs
`memvault-cli compliance-enqueue --hook-input`. The hook performs no LLM
work: Codex hard-caps `SessionEnd` at three seconds, so it only upserts a
durable row in `compliance_finalize_jobs` and exits. The long-lived
`memvault-mcp --transport http` process owns the single background worker;
it reads the transcript tail and judges each injected memory (followed /
violated / unknown, with evidence) outside the hook deadline.

Repeated SessionEnd deliveries for the same batch are idempotent per
`host_session_id`; a completed job is reopened when a resumed host produces
a new pending injection batch. Jobs survive process restarts. A `running`
job left by a crashed worker is requeued after ten minutes, and that sweep
now runs periodically in the worker loop (not only at startup), so a server
restarted before the threshold still recovers it later. An active job
heartbeats its lease every 30 seconds, preventing a slow-but-alive judge
from being reclaimed and run twice. A per-job deadline
(`MEMVAULT_COMPLIANCE_WORKER_TIMEOUT_SECS`, default 90 s) bounds a wedged
provider. Missing or empty transcripts, no configured LLM judge, invalid
judge output, provider errors, and deadline expiry all close the remaining
rows as explicitly `unknown` with a reason. The judge never guesses, and a
missing memory is reported as `unknown` rather than dropped.

Each claim issues an opaque `lease_token`; heartbeat, completion, and failure
only mutate the job while that token still owns it, so a paused worker that
resumes after a takeover cannot overwrite the replacement worker's state.
Verdict and unknown-fallback writes carry the same token check in their
`UPDATE`, and a failed heartbeat cancels the in-flight judge future, so a
stale worker cannot write compliance rows for a batch it no longer owns. The
claim snapshots the highest pending event `batch_seq`. That sequence is
persisted in `compliance_event_seq` and only moves forward, unlike SQLite's
implicit `rowid`, which can be reused after deleting the newest event. The
worker judges only rows up to that watermark, and completion atomically
returns the job to `queued` when any newer pending row exists. A batch that
arrives mid-judgment therefore waits for its own claim and transcript instead
of being judged against the previous one.

The watermark migration runs in a single immediate transaction and records a
completion marker in `compliance_meta` after copying the legacy value, so an
upgrade interrupted between `ALTER TABLE` and the copy recovers on the next
start. The backfill never runs again after that marker exists: a claimed batch
with no pending rows legitimately has `batch_max_seq = 0`, and treating that
zero as an unfinished migration used to re-inflate it from the dead
`batch_max_rowid` column on every restart. The heartbeat is a separate future polled in
parallel with the judge work; it never awaits the store mutex inside a
selected branch, where it could block the work future that owns the next lock
grant.

**Backlog control**: `compliance-finalize --stale-before <rfc3339>` remains
the synchronous manual/legacy path and closes a backlog directly. Every
`SessionStart` also sweeps rows older than 24 h that never received a report
— covering MCP/REST callers, hosts where the session-end event never fired,
and periods when the HTTP worker was not running. All fallback paths write
`unknown`, never a fabricated `followed`.

**Key trade-off**: with no LLM provider configured the metric degrades to
"how many sessions were closed" rather than "how well did the agent comply".
That is deliberate: `N/A` is honest and visible, whereas `100%` hid the gap.
Configuring the extractor is the step that turns compliance into a real
signal.

---

## 14. Three-track memory evolution: delivered state

> The standalone evolution plan was implemented in full across its four
> phases (mirroring roadmap Phase 6); acceptance evidence lives in
> `docs/experiments/REPORT.md` and `docs/experiments/`.

| Track | Deliverables | Acceptance |
|-------|--------------|------------|
| **A: episodic memory** | `episodes` table + `memories.superseded_by` (migrations 6-9); `record_outcome` (MCP/CLI/REST) + `GET /api/episodes`; lesson reflection `reflection.rs` (source-role guard prevents self-reinforcing drift); `task_type` lesson injection (quota ≤3, MUST exempt); Dashboard "Episodic" page | ✅ lesson transfer rate validated on a live `memvault-mcp` server |
| **B: procedural memory** | skill `trigger` × intent matching → structured `[SKILL]` injection (shown once ≥3 success samples); `skill_stats` (migration 10); failure → `needs-revision`, revision `version+1`; repeated successes auto-draft a skill into the review queue | ✅ transfer and false-trigger metrics validated end-to-end |
| **C: semantic memory** | `memory_relations` triple table (migrations 11-13) + LLM relation extraction (`MEMVAULT_RELATIONS=on` to enable); promote gains fact consolidation / entity normalization (`consolidated_from`/`superseded_by` provenance); `supersede` flow (searches exclude superseded facts by default, no physical deletion, reversible); retrieval supports one-hop `expand_relations` | ✅ all acceptance metrics confirmed |
| **D: optional items (delivered)** | team shared pool (`visibility=shared`, cross-namespace injection cap of 20); SOP/Markdown bulk skill import (`sop.rs` + CLI `import-skills` + MCP `import_skills`); Obsidian per-type directory sync (`folderFor`) | ✅ full workspace tests green |

> **Graph database integration** remains intentionally deferred: start only
> when relation volume outgrows the "single table + one-hop expansion" cost
> point (see §15.2).

## 15. Delivery ledger: shipped, deferred, not doing

> This section replaces the earlier "longer-term plans (not started)" table,
> which listed eight already-shipped capabilities as open work and eight open
> items with no trigger for ever starting them. The split below is the honest
> one: **what exists**, **what waits for a named trigger**, and **what was
> deliberately ruled out**.

### 15.1 Shipped after the v0.3 design baseline

| Capability | What actually shipped | Where |
|---|---|---|
| Valid-time reads and expiry | `expires_at` removes a memory from every path that can reach an agent once the bound passes (even MUST — an explicit declaration is not the inferred signal `must_exempt` protects), while `as_of` reconstructs the state at an arbitrary instant from the `memory_history` chain | §9.6 |
| Historical keyword index | `memory_history_fts` + `search_history_at`; an `as_of` query unions the historical pass with the projected current pass and tags those hits `hist-kw#N`. **Keyword only** — the vector path still embeds current text | §9.6 |
| Event-time filters | `occurred_from` / `occurred_to` on `SearchQuery`, MCP `search_memory`, `POST /api/search` and `memvault-cli search`. Undated rows never match a window; an inverted window is a 400, not an empty result | §9.6 |
| `occurred_at` on direct saves | `POST /api/memories`, `PUT /api/memories/{id}` (tri-state clear), MCP `save_memory`, `memvault-cli save --occurred-at`; one parser (`models::parse_optional_stamp`) so the three surfaces cannot drift | §9.6 |
| Disclosure layers | Full → Excerpt (~320 chars) → Summary (~96 chars), derived at injection time, never indexed; MUST is never degraded | §6.5 |
| Agent-proposed edits | `propose_memory_edit` (MCP/REST/CLI) + `proposal::approve`, executed as **one** compare-and-swap (`approve_supersede`) rather than two ordered writes | §9.3.1 |
| Scenario profiles | `coding-agent` / `assistant` / `team` / `edge-lite`; additive, never overriding an env var or env-file key | §7.5 |
| Identity + corroboration gate | `identity_verified` / `corroborating_agents` recorded on write; `MEMVAULT_CORROBORATION_GATE` opt-in | §13.7 |
| Review gate on every channel | `session_start`, `sync` and the proxy call one shared predicate; `memory://user-profile` is gated, `memory://project-context` is deliberately ungated REFERENCE data | §13.8 |
| Honest compliance | `SessionEnd` enqueues only; a background finalizer judges or records `unknown` with a reason; rates are `Option<f64>` | §6.3, §13.9 |
| Markdown backend | File-authoritative store with a rebuildable cache, `owned` / `quarantine` trust modes, durable forget tombstone | §8.3 |
| BEAM adherence harness | `docs/experiments/beam_adherence.py` — third-party probes with their own grading contract. **Harness only: no published run yet**, so there is still no external number to cite | `docs/experiments/README.md` |

### 15.2 Deferred, each with a trigger

| Item | Why it is not built | Start when |
|---|---|---|
| **Graph database** | `memory_relations` is one table plus one-hop `expand_relations`; a graph store buys nothing until traversal depth or relation volume outgrows that | relation volume outgrows single-table one-hop — the same conclusion agent-memory reached for its optional graph channel |
| **CRDT multi-device sync** | no conflict-merge policy survives concurrent edits of the same memory; §8.3.8 already excludes multi-machine merge | a real multi-device user **and** a chosen merge policy |
| **Multimodal input** | only a governed text projection is worth indexing; multimodal vectors would fork the retrieval chain | a source that cannot be reduced to text without losing the fact |
| **Global compressed summary (dual-track)** | per-memory disclosure (§6.5) was the cheap 80%; a maintained aggregate needs a summary-generation and invalidation pipeline, not a better trimmer | injection pressure that per-memory degradation cannot absorb |
| **Two-level retrieval (coarse cluster rank → in-budget fine rank)** | current memory scale has not hit the retrieval-latency ceiling; it is scalability insurance, not today's bottleneck | retrieval latency or context pressure outgrows the single-stage reranker |
| **Distilled / trained ranker** | there is not yet enough accumulated `bench` and usage signal to train against | the benchmark history is large enough to evaluate a learned ranker out of sample |
| **Plugin marketplace** | distribution is per-channel today: Homebrew, npm, the MCP registry, Obsidian community plugins | more than ~3 first-party clients to keep in sync |
| **Transactional write protocol (plan → SHA-256 → apply)** | the markdown backend is single-writer with `reindex` reconciliation; a plan/approve/apply triple protects against a second writer that does not exist yet | concurrent file-side writers (§13) or a review flow that edits files directly |
| **`agent_adapt::format_memories` treat-as-data wrapping** | REST `/api/search` returns data to a caller that renders it itself; the instruction-shaped risk is at injection, which is already wrapped | a REST consumer that feeds search results straight into a prompt |
| **Obsidian health check (lint / stale-index patrol)** | `detectOrphans` covers the failure mode users actually hit; `reindex` self-heals the rest | reports of vault drift that reindex does not repair |

### 15.3 Not doing

| Item | Why not |
|---|---|
| General world-knowledge base | the model already has commons; a personal vault that also stores Wikipedia is a worse product and a worse privacy story |
| Edge/cloud hybrid sync | the local-first single binary *is* the product; a hosted tier drags in the multi-tenant governance stack §8.3.4 deliberately avoids |
| Markdown-backend v1 exclusions | git auto-commit, running both backends at once, multi-machine merge, per-namespace subdirectories, NFS-safe locking — each is a separate design, not a missing flag (§8.3.8) |
| Predictive half-life decay | already covered by `contradiction_multiplier`, type-stability coefficients and `access_boost`; a second predictive model would add opacity without a measured gain |
| N-gram-style content compression | the source report found no stable gain from token normalization, non-uniform allocation or frequency partitioning |

---

## Appendix

### A. Reference projects

- [Mem0](https://github.com/mem0ai/mem0) — hybrid memory layer; the closest
  Tier-1 peer on storage/retrieval.
- [Zep](https://github.com/getzep/zep) — temporal knowledge graph; the benchmark
  for the temporal axis (§2.5).
- [MCP specification](https://modelcontextprotocol.io/) — the protocol standard
  every client integration targets.
- [LanceDB](https://lancedb.github.io/lancedb/) — embedded vector DB, evaluated
  and dropped in v0.2 in favour of int8 columns in SQLite.
- [Khoj](https://github.com/khoj-ai/khoj) — Obsidian/RAG peer; the L5 reference.
- [Automerge](https://automerge.org/) — CRDT library, the named option if
  multi-device sync is ever built (§15.2).
- openJiuwen `agent-memory` — the closest Tier-1 design peer (Apache-2.0, ~198K
  LOC Python); no stable public URL is recorded here, and §2.3 carries the
  comparison table.

### B. Decision log

Only decisions that still bind the code are listed; superseded ones live in the
CHANGELOG.

| Decision | Rationale |
|---|---|
| Independent tool + thin clients | breaks past the Obsidian ceiling; every host keeps its own UI |
| The Memory Router is the core differentiator | auto-injection is table stakes (§2.4), cross-agent sharing and follow-through are not |
| One shared corpus; `agent_id` never filters retrieval | §13.3, and `SearchQuery` has no `agent_id` field |
| MUST/REFERENCE/BACKGROUND tiers; a reviewed MUST is never down-ranked or truncated | a binding rule that is silently dropped is worse than one that overflows the budget — but the review gate still decides whether it may be injected at all (§13.8) |
| MCP as the primary protocol, `sync` as the MCP-free fallback | writes `AGENTS.md` / `CLAUDE.md` / `.cursor/rules/` for hosts that cannot speak MCP |
| Local-first, single binary, no hosted tier | privacy is the product; §15.3 records the alternatives that were ruled out |
| Vectors in SQLite int8 columns, not LanceDB | zero extra dependency, quantized on-row storage |
| Two-phase injection with a deterministic fast path | a request must never block on embedding latency |
| Delta writes on save (0.7 overlap / 0.9 vector / 0.95 skip) | keeps the store bounded without a batch dedup pass (§9.1.1) |
| A pending edit is a separate row, approval is one transaction | an in-place edit would let any agent suppress a MUST just by proposing one (§9.3.1) |
| Unreviewed memories are not injected by default | a prompt-level "treat as data" wrapper is weaker than not showing the text (§13.8) |
| Compliance is judged after the fact, never fabricated | SessionEnd enqueues; a worker judges or records `unknown` (§13.9) |

### C. Hypotheses and outcomes

| # | Hypothesis | Outcome |
|---|---|---|
| H1–H4 | The four core design hypotheses (injection value, format effect, review willingness, token budget) | confirmed 2026-08-11 — `docs/experiments/REPORT.md` |
| H5 | Lesson injection reduces repeat-failure rate on similar tasks (episodic track) | confirmed: knowledge conveyance 0% → 90% |
| H6 | Semantic track: supersede propagation, cross-session consistency, entity consolidation | confirmed ×3: 0% → 100%, 100%, 100% |
| H7 | Skill injection improves first-attempt success with < 5% trigger mis-fire (procedural track) | confirmed: 0% → 78%, 0 mis-injections in 40 unrelated contexts |
| Runtime regression | Injection / outcome loop / audit trail against a real `memvault-mcp --transport http` | 6/6 pass (2026-08-28) |
| BEAM adherence | Instruction/preference adherence on a public benchmark, reported with harness spec + latency | **not yet run**; only the harness exists (§15.1) |
