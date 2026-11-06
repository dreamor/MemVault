# Cross-Agent Personal Memory Tool — Product & Architecture Design

> **Project**: MemVault
> **Version**: v0.3 document baseline, describing the shipped v0.6.x system
> **Market analysis**: refreshed 2026-11 (§2) — this revision changed the
> differentiation conclusions in §11 and the risk register in §12
> **Positioning**: a personal memory router for the AI-agent era
> **One-liner**: don't teach agents to query memories — make memories appear in front of the agent automatically.
> **Core promise**: any MCP-compatible agent that connects shares the same user memory automatically.
>
> ⚠️ **The one-liner above is the onboarding promise, not the differentiator.**
> Since it was written, Claude Code, Codex and Cursor have all shipped native
> auto-memory (§2.2), so "make memories appear automatically" is now table stakes
> within each of those tools. What remains uncontested is that MemVault makes
> **one shared** corpus appear in **every** agent, gates **what may bind** an
> agent (L6), and measures **whether it was obeyed** (L3). See §11.2.

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
13. [Business model](#13-business-model)
14. [Multi-agent shared memory design (added in v0.3)](#14-multi-agent-shared-memory-design-added-in-v03)
15. [Three-track memory evolution: delivered state](#15-three-track-memory-evolution-delivered-state)
16. [Longer-term plans (not started)](#16-longer-term-plans-not-started)
17. [Memory design informed by the Qwen3.8-Flash-Next architecture report](#17-memory-design-informed-by-the-qwen38-flash-next-architecture-report)
18. [Storage backends: sqlite vs markdown](#18-storage-backends-sqlite-vs-markdown)

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
| **OpenViking** (Volcengine) | `viking://` virtual filesystem unifying memory + resource + skill | path browse (`ls`/`find`) + recursive directory descent | self-host REST | its **observable retrieval trajectory** and **L0/L1/L2 progressive loading** are both worth adopting (§16); single self-hosted form, weak multimodal |
| **Zilliz memSearch** | **Markdown as source of truth**, Milvus as a rebuildable *shadow index* | vector + BM25 + RRF | Lite → Docker → Zilliz Cloud | the Code-Agent-native model — MemVault's markdown backend (§18.2) already matches it. Retrieval/storage only: no fact extraction, no conflict resolution |
| **LangMem** | flat KV + vector (CoALA taxonomy) | vector | self-host | bound to LangGraph |
| **AgentScope + ReMe** | short/long two-layer + graph + SQLite audit log | vector + graph + rerank | self-host + Aliyun | bound to AgentScope; long-term memory is an external dependency |
| **Supermemory** | one-shot Context Stack (memory + RAG + profile + connectors) | automatic hybrid | closed cloud; self-host enterprise-only | its **auto-expiry of temporary facts** is worth copying (§16); otherwise closed and locked-in |
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
| Bi-temporal model + `as_of` | `t_event` / `t_ingest` / `t_valid` / `t_invalid`; `search(as_of=…)` and `get(as_of=…)` **shipped**. The design detail worth copying is the split: `as_of` is *valid-time* ("which version was in effect at T"), while `time_from`/`time_to` parsed from the query is *event-time* ("when did it happen"). The two axes are **never folded together**. → **MemVault has since copied the split, not just the feature** (§9.6): `as_of` + `expires_at` shipped on valid-time, leaving the pre-existing `occurred_at` untouched as event-time. Still open: an event-time *filter* (§16) |
| L0/L1/L2 progressive disclosure | per-unit `ContentLayers` (l0 summary / l1 fragment / l2 = the existing full content, not duplicated), disclosed against a `max_tokens` budget. Per-memory, not a global summary — an order of magnitude cheaper than the "dual-track injection" item in §16 |
| `edit_md_file` | Letta-style self-editing on a Markdown source of truth: exact unique-`old_text` replacement, atomic write-back, inherit-and-SUPERSEDE. **It writes straight to the source of truth** — MemVault's version must stop at the Inbox first (§18.3.1) |
| Multi-tenant governance | `org + space` hard isolation, configurable `principal_path` (`user_agent` / `agent_user`), grant/revoke, space lifecycle, quotas, `verify_audit` chain-integrity |
| Scenario presets | 11 configurable dimensions + 4 profiles (coding agent / personal assistant / enterprise multi-agent / edge-lite) — "既能全、也能轻" |
| Multimodal ingest | `segments[] + assets[]`, pluggable ASR/OCR/caption normalisers. Note that **retrieval stays text-only** — no multimodal vectors — which is the cheap way to do it |
| Fuser de-duplication across disclosure layers | L0/L1/L2 are *physical entries of the same channel*, not separate channels: it applies MaxP per (channel, unit) **before** cross-channel fusion, so a memory with more layers is not systematically over-weighted. MemVault's RRF fusion will hit this bug the moment §16's disclosure layers land |

| It lacks, MemVault has | Note |
|------------------------|------|
| **Follow-through tracking** | it has `audit` (who changed a memory) and `verify_audit` (chain integrity) — that is *governance* auditing, not "did the agent obey". No equivalent anywhere in the survey |
| **Instruction-layer enforcement** | `MemoryTier.CORE` means *resident in context* (≈ Letta core memory), not *must be obeyed*. MemVault's MUST is never filtered, never truncated, and a violation is an error (§6.2) — a semantic promotion nobody else makes |
| **Trust provenance** | its `provenance` is *data lineage* (which units this one was synthesised from, for `trace`). MemVault's `ai_generated` / `human_reviewed` / `identity_verified` + the corroboration gate (§14.7) is *trust provenance* (what entitles a memory to bind an agent). Different axis; the survey has no column for it |
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
   `inject_session_id` + compliance rate + the SessionEnd finaliser (§14.9) is
   still empty space.
3. **Trust-gated binding instructions.** Nobody asks *what entitles this memory
   to bind the agent*. Identity verification + multi-agent corroboration (§14.7)
   + the `owned`/`quarantine` boundary (§18.3.1) has no counterpart in the entire
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
| Temporal / time-travel | ❌ | ✅ **best in market** | ❌ | ✅ bi-temporal + `as_of` | ✅ `as_of` on get/list (exact) + `expires_at`; ⚠️ search matches current index text, and no event-time filter yet → §9.6 |
| Graph relations | ⚠️ paid tier | ✅ | ❌ | ✅ config-gated | ⚠️ single-table one-hop (`memory_relations`) |
| Markdown as source of truth | ❌ | ❌ | ✅ | ✅ | ✅ (§18.2 inverted data-flow) |
| Progressive disclosure (L0/L1/L2) | ❌ | ❌ | ❌ | ✅ stored layers | ✅ derived on demand (§6.5) — cheaper and cannot go stale |
| Multimodal input | ❌ | ❌ | ❌ | ✅ | ❌ → §16 |
| Edge **and** cloud | ✅ | ⚠️ cloud-first | ✅ Lite → Cloud | ✅ 3 profiles | ⚠️ local-only → §16 (deliberate, positioning constraint) |
| Retrieval observability | ❌ | ❌ | ❌ | ✅ trajectory | ✅ **finer** — records why each candidate was *dropped*, not just what was recalled |
| Task-level evaluation | ❌ | ❌ | ❌ | ❌ (plans LoCoMo/BEAM) | ✅ `bench --judge` (§17.2) |

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
> second table; the first is tracked honestly so that §16 closes the rows that
> are cheap to close (`as_of`, disclosure layers) and deliberately leaves the
> rest.

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
| **MemoryAgentBench** | incremental multi-turn, 100K+ | accurate retrieval / test-time learning / long-range understanding / **conflict resolution & selective forgetting** | 🎯 maps directly onto delta-write (§17.1) and the three forget paths (§9.5) |
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
p50 ≤ 1.1s). This is where two-phase injection (§17.3) — MUST resolved
deterministically with **zero embedding calls** and served immediately, the
semantic pipeline landing inside a 250 ms window — should show a structural
advantage from Rust plus in-process native embedding. **That advantage is
currently unmeasured, which for positioning purposes is the same as not having
it.** → §16

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
> corroboration gate (§14.7), honest compliance accounting (§14.9), the
> `owned`/`quarantine` trust boundary (§18.3.1) and the SHA-256 erasure
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
> Markdown (§18); the core value is the Router + follow-through layer. The
> Obsidian plugin is the acquisition funnel; the independent tool carries the
> product value.
>
> *(Revised 2026-11: this originally read "storage is pluggable,
> MemPalace/Mem0-compatible". MemPalace has since left the market (§2.7) and no
> Mem0/Zep adapter was ever built — the pluggability that did ship is the
> sqlite↔markdown backend switch of §18. Tier 1 memory layers are now peer
> alternatives rather than upstream dependencies (§11.3).)*

### 4.2 Key strategy

> **The storage engine is not where the value is — but it is also not someone
> else's to borrow.** MemVault ships its own local-first store (§18) because the
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
│ │ Obsidian Plugin │ VS Code Ext │ Dashboard │ Web │ CLI          │ │
│ └────────────────────────────┬───────────────────────────────────┘ │
│                              │                                      │
│ ┌────────────────────────────▼───────────────────────────────────┐ │
│ │ Layer 4: protocol                                              │ │
│ │ MCP Server │ REST API │ WebSocket │ MCP Resource               │ │
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
│ │ │ (cache derived, hand-edits self-heal via reindex, §18.2) │  │ │
│ │ └──────────────────────────────────────────────────────────┘  │ │
│ └────────────────────────────────────────────────────────────────┘ │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

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

| Layer | Type | Lifetime | Storage | Example |
|-------|------|----------|---------|---------|
| L1 | working | session | memory | current conversation context |
| L2 | episodic | days/weeks | daily notes + vectors | "discussed architecture today" |
| L3 | semantic | long-term | entity notes + graph | "user prefers Python" |
| L4 | procedural | permanent | skill notes | "deployment process" |

> Delivered: the L2 episodic / L3 semantic / L4 procedural cognitive-memory
> tracks are implemented (the standalone evolution plan was merged into this
> document); architecture and interfaces in §15.

---

## 6. Memory Router core modules

### 6.1 Auto-Inject Engine

#### Three injection modes (ordered by feasibility)

> **Important**: the standard MCP protocol keeps the server passive — it only
> responds when an agent calls a tool. A server **cannot** intercept user
> messages before they reach the agent. The priority of the three injection
> modes therefore needed adjustment.

| Mode | Trigger | Use case | Mechanism | Phase |
|------|---------|----------|-----------|-------|
| **MCP Resource** ⭐ | agent startup | high-frequency core memories (default) | MCP Resource URI, loaded automatically by the client | **Phase 1** |
| **Session bootstrap** | new session | long sessions, project switches | `session_start` tool; requires client auto-call support | **Phase 1** |
| **Pre-prompt injection** | before each user message | dynamic context matching | needs the MCP Proxy architecture | **Phase 2** |

> Phase 1 strategy: MCP Resources carry MUST-level core memories (loaded at
> startup); the `session_start` tool provides context-filtered REFERENCE
> memories. Pre-prompt injection arrives in Phase 2 through the MCP Proxy.

#### Pre-prompt injection pseudocode (multi-agent flavor)

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
        #    - agent_access filter: memories may be visible/hidden per agent
        memories = self.retrieve(
            query=user_message,
            intent=intent,
            filters={
                "namespace": agent_context.get("project", agent_profile.default_namespace),
                "agent_access_mode": "all"   # Phase 1: everything visible
            },
            top_k=20
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

> **Note**: in standard MCP, the server never sees the agent's replies — it
> handles tool-call requests only and does not receive the LLM's final output.
> This design therefore needs an extra feedback mechanism and was deferred to
> Phase 3.
>
> Candidate implementation paths:
> - **Option A**: the MCP proxy intercepts the full conversation flow (usable once the Phase 2 proxy exists)
> - **Option B**: offline analysis of conversation logs (feasible, not real-time)
> - **Option C**: the agent calls a `report_compliance` tool (depends on agent cooperation, unreliable)

```
inject memory → agent replies → analyze reply compliance
                                      │
                      ┌───────────────┼───────────────┐
                      ▼               ▼               ▼
                 followed ✅     partial ⚠️       violated ❌
                 (record)        (record+remind)  (record+escalate)
                                                  │
                                                  ▼
                                  raise priority / change injection point
                                  / notify user review
```

### 6.4 Query rewriting

```python
def rewrite_query(original_query: str, intent: str) -> list[str]:
    """expand a short agent query into multiple retrieval branches"""

    rewrites = []

    # synonym expansion
    rewrites.append(expand_synonyms(original_query))

    # intent-based expansion
    if intent == "coding":
        rewrites.append(f"{original_query} coding style preference")

    # history-based expansion
    recent_topics = get_recent_topics(limit=3)
    for topic in recent_topics:
        rewrites.append(f"{original_query} {topic}")

    return rewrites
```

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
  (§11.1). A binding instruction whose qualifying clause was summarised away is
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
| Graph database | FalkorDB (optional) | introduce at P2 if scale demands |
| Sync protocol | CRDTs (Automerge) | conflict-free multi-device |
| Memory protocol | MCP | the de-facto standard |
| Pluggable backends | `MemoryStore` trait: sqlite ↔ markdown (§18) | same API, two source-of-truth shapes — DB-authoritative or file-authoritative. *(The v0.3 row read "MemPalace / Mem0 adapters"; those were never built and are not worth building — see §4.2)* |

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
| VS Code extension | TypeScript | coding context |
| CLI | Rust | developers |

### 7.4 AI / NLP

| Component | Choice | Purpose | Phase |
|-----------|--------|---------|-------|
| Embedding | native embedded (fastembed, default) → optional ollama / OpenAI-compatible | vectorization | local native model is the default since v0.2.0 |
| Memory extraction | rules engine (default) + optional any OpenAI-compatible chat LLM | conversation → structured memories | shipped; enabled via `MEMVAULT_LLM_EXTRACTION_PROVIDER` |
| Compliance detection | lightweight LLM | reply compliance | Phase 3 |
| Rerank | bge-reranker | reorder retrieval results | Phase 2 |

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

> Added 2026-11. Closes the "Scenario profiles" item in §16, which came out of
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
| `coding-agent` | Markdown vault, native embeddings, no relations | Files as source of truth so the vault is git-able and readable by tools that speak nothing but Markdown (§18.2). Relations off because each costs an LLM call and §16 defers graph work until relation volume outgrows single-table one-hop. Auto-forget **off**: irreversible purge should not run on a timer over project decisions |
| `assistant` | SQLite, relations on, auto-forget at 180 days | A personal profile is mostly entities and preferences, which is where one-hop expansion earns its cost. Retention long enough to notice and restore, short enough to actually bound growth. MUST exempt from purge |
| `team` | SQLite + the full trust stack | `IDENTITY_VERIFICATION` + `CORROBORATION_GATE` + `CORROBORATION_MIN_AGENTS=2`, because the gate counts *identity-verified* agents — enabling it without verification would count self-declared ids, which is exactly the spoofing it exists to stop (§14.7). Shared memories exempt from auto-purge. Also sets `MARKDOWN_TRUST_MODE=quarantine` even though the backend is sqlite: if anyone switches backends, `owned` would hand injection authority to everyone who can write the directory (§18.3.1), so it fails safe rather than later |
| `edge-lite` | SQLite, **no embedding provider at all** | The interesting one: MUST memories still inject, because §17.3 resolves them deterministically with zero embedding calls. Turning embeddings off is therefore a supported configuration, not a degraded one |

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
id: mem_20260807_001
type: preference          # preference | fact | episode | entity | skill
content: "User likes concise code style, dislikes over-commenting"
instruction: "[MUST] no comments, no type hints, ≤10 lines per function"
priority: MUST            # MUST | REFERENCE | BACKGROUND

# --- agent identity ---
source_agent:
  id: claude-desktop       # the agent that wrote it
  type: coding-assistant   # agent type (coding | general | research …)
  session_id: "sess_abc123" # session at write time (traceability)

# --- visibility control ---
namespace: global          # global | project:xxx
agent_access:
  mode: all                # all | allowlist | denylist
  allowlist: []            # ["agent:claude-desktop", "agent:cline-vscode"]
  # Phase 1 only implements mode: all; allowlist/denylist are Phase 3

confidence: 0.9
tags: [coding, style, python]
created: 2026-08-07T15:00:00+08:00
updated: 2026-08-07T15:00:00+08:00

# --- three time axes, deliberately never folded into one column ---
# Ingest-time: when MemVault took the memory in. Always set.
# (that is `created` above)
occurred: 2026-08-01T09:00:00+08:00   # event-time: when the remembered fact
                                      # actually happened in the source
                                      # conversation/document. Null when the
                                      # caller supplied no date. Without it a
                                      # batch import of historical transcripts
                                      # would stamp every row "now".
expires: 2026-12-31T00:00:00Z         # valid-time upper bound: when the fact
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

related: ["[[Python]]", "[[Code Style]]"]
ai_generated: true
human_reviewed: false

# --- pending edit proposals (§9.3.1) ---
# Set on a memory that proposes to REPLACE another one once a human approves it.
# The target is left untouched and keeps injecting until then, so an agent cannot
# suppress a binding MUST merely by proposing an edit to it. Absent on ordinary
# memories. Consumed on approval (the target gains `superseded_by` instead).
proposes_supersede_of: mem_20260807_000
decay_score: 1.0
access_count: 0
compliance:
  rate: 0.95               # combined compliance across all agents
  by_agent:                # compliance split per agent
    claude-desktop: 0.97
    cline-vscode: 0.90
  violation_count: 1       # violations across all agents
  violations:              # concrete records (linked to inject_session_id)
    - agent_id: cline-vscode
      inject_session_id: "inj_def456"
      timestamp: 2026-08-07T16:00:00+08:00
      violation_type: ignored_must
---
```

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

> **Truth-source principle — depends on the active backend** (`MEMVAULT_STORAGE_BACKEND`):
>
> - **`sqlite` (default)**: SQLite (`data.db`) is the single source of truth; the
>   Obsidian vault directories and file-based export/client surfaces are
>   **projections/caches**. File-side content may be rewritten by external tools
>   and must never be treated as write authority — which is why file-side writes
>   need protective mechanisms such as the "transactional write protocol" (§16);
>   the timing of that investment is driven by multi-agent shared writes /
>   human-review flow needs.
> - **`markdown`**: the directory of `{id}.md` files **is** the source of truth,
>   and the internal `.memvault-cache.db` is a rebuildable index. File-side
>   edits (Obsidian, an editor, `git checkout`) are authoritative and are
>   reconciled into the cache by `memvault-cli reindex` — see §18. The default
>   `owned` trust mode treats every trust field in a file as the user's own
>   action. In the opt-in `quarantine` mode the cache also carries the
>   "previously seen" trust baseline, so deleting it demotes files that have no
>   durable approval record — fail-closed, but not free.

### 8.3 MCP tool definitions (multi-agent flavor)

```json
{
  "tools": [
    {
      "name": "save_memory",
      "description": "save one memory (records the source agent)",
      "parameters": {
        "agent_id": "claude-desktop",
        "agent_type": "coding-assistant",
        "type": "preference | fact | episode | entity | skill",
        "content": "memory content",
        "priority": "MUST | REFERENCE | BACKGROUND",
        "tags": [],
        "namespace": "global | project:xxx",
        "confidence": 0.0,
        "expires_at": "RFC3339, optional — valid-time bound; see §9.6"
      }
    },
    {
      "name": "search_memory",
      "description": "search user memories (shared across agents)",
      "parameters": {
        "query": "what to search",
        "agent_id": "current agent id",
        "type_filter": "",
        "priority_filter": "",
        "time_range": "",
        "namespace": "",
        "top_k": 10,
        "token_budget": 2000,
        "as_of": "RFC3339, optional — point-in-time (valid-time) query; see §9.6"
      }
    },
    {
      "name": "session_start",
      "description": "called automatically at session start; preloads memories filtered by agent identity",
      "parameters": {
        "agent_id": "claude-desktop",
        "agent_type": "coding-assistant",
        "context_hint": "hint about the current conversation topic",
        "project": "current project name",
        "max_memories": 8
      }
    },
    {
      "name": "review_memory",
      "description": "review a memory awaiting confirmation",
      "parameters": {
        "action": "approve | reject | edit",
        "memory_id": "",
        "edited_content": ""
      }
    },
    {
      "name": "get_compliance_report",
      "description": "fetch a compliance report (supports cross-agent aggregation)",
      "parameters": {
        "time_range": "7d | 30d | all",
        "agent_filter": "",
        "aggregate": true
      }
    }
  ],
  "resources": [
    {
      "uri": "memory://user-profile",
      "name": "User Profile & MUST Rules",
      "description": "core user preferences and hard rules, loaded every session (shared by all agents)"
    },
    {
      "uri": "memory://project-context",
      "name": "Current Project Context",
      "description": "tech stack and conventions of the current project (filtered per agent type)"
    }
  ]
}
```

> **Current implementation**: the MCP surface has grown to **21 tools** — on
> top of the originals, `record_outcome` / `infer_outcome` (task
> outcomes/lessons), `import_skills` (Markdown SOP import), `add_evidence` /
> `get_memory_evidence` (evidence links: supports / contradicts /
> sourced_from), `run_promote` (L1→L2→L3 promotion), `forget_memory`
> (permanent GDPR-style erasure), and `report_compliance` /
> `get_compliance_report` / `get_effectiveness_report` (compliance &
> effectiveness) were added; `search_memory` and `POST /api/search` gained the
> `expand_relations` parameter. Full list in the root README ("21 MCP Tools")
> and §15.

---

## 9. Core functional modules

### 9.1 Memory write pipeline

```
Agent conversation → Extractor → dedup → write to Inbox → notify review
                                              ↓
                              user approves → final directory + instruction generated
                              user rejects   → mark as deleted
```

### 9.2 Router retrieval pipeline (multi-agent flavor)

```
user message arrives (with agent_id)
     ↓
agent identification → look up Agent Registry for injection rules
     ↓
intent analysis → which domains/projects/preferences are touched
                  (combined with agent type)
     ↓
structured filter (type × namespace × priority × agent_access)
     ↓
hybrid retrieval:
  ├─ exact query → SQLite WHERE
  ├─ semantic query → SQLite embedded vectors (cosine)
  ├─ relational query → graph / backlinks
  └─ temporal query → daily notes
     ↓
filter memories irrelevant to this agent type
(e.g. writing-style MUSTs are not injected into coding agents)
     ↓
rerank + token-budget trim (≤8)
     ↓
format conversion (instructional + injection session id)
     ↓
inject into the agent system prompt
```

### 9.3 Human-in-the-loop review flow

1. AI writes → `00-Inbox/`, marked `human_reviewed: false`
2. Dashboard / Obsidian sidebar show a pending-review panel
3. User reviews → approving auto-generates the `instruction` field
4. After approval the frontmatter is updated and backlinks created
5. The router prefers `human_reviewed: true` memories

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

**Why a separate row rather than an in-place edit.** The obvious alternative —
apply the edit and revoke its trust — looks equivalent, because §14.8's gate
excludes untrusted memories from injection anyway. It is not equivalent: the
target stops injecting too, so **any agent could suppress any MUST simply by
proposing an edit to it**. That is a denial-of-injection vector aimed squarely at
the threat model §14.7 exists for. Keeping the target live until a human decides
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

The same reasoning applies to approval: `review_memory` exists in **two**
implementations (`memvault-mcp` and `memvault-proxy`), so both had to be routed
through `proposal::approve`. Seven call sites across four surfaces now reach one
core function — which is the point of having one, and the reason the bug existed
is that the copies were being found by reading call sites rather than by grepping
for a helper.

### 9.4 Compliance feedback loop

```
injected memory → agent reply → compliance check → record
                                                  ↓
                                  violations > 3
                                                  ↓
                                  auto-raise priority
                                  or change injection point
                                  or notify the user
```

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
  to the durable `<markdown-dir>/.memvault-forgotten.json` tombstone (§18.3.2)
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
> `superseded_by` from §15 Track C), it just was not queryable by time.

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
| `search(as_of=…)` / `memvault-cli search --as-of <T>` | **Partial — documented, not hidden.** Matching still runs against the *current* FTS/vector index; only the in-effect filter and the returned state are point-in-time. A memory whose wording has since changed may not be recalled. Every result carries `as_of_source`, the CLI prints the caveat up front, and `score` is left as-computed rather than re-scored against reconstructed text (which would misrepresent which path recalled the hit). Exact historical-text search needs an FTS index over `memory_history` snapshots → §16 |

Supersession interacts correctly by construction: because `supersede` goes through
`update`, the pre-supersede snapshot has `superseded_by: None`, so querying a
superseded memory at an `as_of` before the supersede returns it as the current
truth it was then.

**Markdown backend caveat.** The cache is the derived side of §18.2's inverted
data-flow, and a history snapshot is not a file — so a rebuilt cache holds no
snapshots and every reconstruction degrades to `AsOfSource::Live`. Point-in-time
reads on that backend only see changes made since the last reindex. `expires_at`
*does* round-trip fully, via the `expires:` frontmatter key. Making the version
chain durable there would mean writing history into the vault itself, which is a
separate decision (§18.6).

---

## 10. Roadmap status

All planned phases (0–6) have been delivered; the section records the roadmap
that produced the current architecture plus which items remain open.

| Phase | Scope | Status |
|-------|-------|--------|
| 0 — technical validation | rmcp selection, embedding latency baseline, MCP Resource injection, `session_start` auto-call | ✅ delivered (LanceDB PoC was evaluated and replaced by SQLite-embedded int8 vector columns in v0.2) |
| 1 — core engine + router + multi-agent basics | local service skeleton, SQLite storage, MCP Server (save/search/session_start), MCP Resources (`memory://user-profile`, `memory://project-context`), Agent Registry, per-type filtering, MUST/REF format engine, token budget, Markdown + frontmatter, vector retrieval, CLI, multi-agent E2E, Claude Desktop integration | ✅ delivered. Injection in Phase 1 uses MCP Resource + `session_start`; pre-prompt injection (needs the proxy) deferred to Phase 2; compliance tracker (needs agent replies) deferred to Phase 3 |
| 1.5 — multi-agent sharing hardening | config-driven Agent Registry (YAML), structured `source_agent` (id/type/session_id), `inject_session_id` tracking, per-agent compliance logging, two concurrent MCP clients, shared-memory correctness | ✅ delivered |
| 2 — retrieval + proxy + Obsidian | hybrid retrieval (BM25 + vector + structured filters), query rewriting, rerank, MCP proxy prototype, local embedding models, Obsidian plugin connection, inbox review panel, bidirectional sync, MCP Resource auto-load | ✅ delivered |
| 3 — dashboard + compliance | web dashboard (`memvault-mcp --transport http --serve-web`), review-queue UI, agent activity panel, compliance dashboard, memory-type graph view, full proxy integration, import/export | ✅ delivered |
| 4 — smart pipeline + ecosystem | background extractor worker, dedup/merge/conflict detection, forgetting curve/decay, VS Code extension, pluggable storage backends | ✅ delivered (background extraction runs via proxy hooks; VS Code ships as a config profile, not an extension package); forgetting curve/decay extended with permanent auto/manual/bulk forget (GDPR erasure) — see §9.5 |
| 5 — extensions | web app, CRDTs multi-device sync, graph database, team-shared memory pool, plugin marketplace | partially delivered — team shared pool shipped (see §15); CRDTs and graph database remain open (§16); plugin marketplace not started |
| 6 — three-track memory loop | episodic (Phase A), procedural (Phase B), semantic (Phase C), optional items (Phase D) | ✅ delivered; acceptance evidence in `docs/experiments/REPORT.md` (see §15) |

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
| L3: follow-through guarantee | **still empty space** — no surveyed product measures whether an injected memory was actually obeyed | MUST/REF + instructional format + `inject_session_id` compliance tracking + honest accounting via the SessionEnd finaliser (§14.9) | 🔴 core moat |
| L4: human collaboration | partially exists — Cursor's per-item approval is the same loop, for one IDE | Inbox + Dashboard + markdown-backend two-way consistency (§18.3) | 🟡 differentiator |
| L5: native Obsidian | Khoj/SC do RAG | memory-management-specific UI | 🟡 differentiator |
| L6: trusted memory | **empty space** — `Memoria` reached for this position and did not survive (§2.7); agent-memory's `provenance` is *data lineage*, not *trust* | provenance-gated MUST (§6.2) · identity verification + multi-agent corroboration gate (§14.7) · unreviewed memories excluded from injection (§14.8) · agent-proposed edits gated through review rather than applied (§9.3.1) · `owned`/`quarantine` trust boundary (§18.3.1) · SHA-256 erasure tombstone (§9.5) | 🔴 core moat |

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
its own SQLite-or-markdown backend (§18) rather than an adapter over someone
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
| **Evaluation effort spent on non-discriminating benchmarks** | 🟡 medium | LoCoMo/LongMemEval no longer separate a memory system from a 1M-token context window (§2.6). Target BEAM's instruction-adherence axis and MemoryAgentBench's conflict/forgetting axis instead, publish on AMB with a full harness specification, and always report p50/p95 next to accuracy — the unmeasured latency advantage of two-phase injection (§17.3) is worth nothing as positioning |
| Cold start is hard | 🔴 high | Obsidian plugin as the funnel; the markdown backend (§18) makes an existing vault importable, and `sync` means an agent benefits before it supports MCP at all |
| Router mis-injects (irrelevant memories) | 🟡 medium | conservative policy: fewer over more; users can disable auto-injection; every dropped candidate is recorded with a reason, so mis-injection is auditable rather than mysterious |
| Agents ignore the MCP contract | 🟡 medium | fallback modes: tool + resource + system prompt; `sync` to `AGENTS.md`/`CLAUDE.md`/`.cursor/rules/` as the MCP-free path |
| Editing UX worse than Obsidian | 🟡 medium | don't clone an editor; focus on memory management |
| Sync complexity | 🟡 medium | local-first with file export as the escape hatch |
| Token explosion | 🟡 medium | hard token budget caps; summaries by default; and since 2026-11 **progressive disclosure** (§6.5) — a memory that does not fit is degraded to an excerpt or summary before it is dropped, with the abbreviation marked in the text and the level reported. MUST is exempt from degradation: a binding instruction with its qualifying clause summarised away is worse than one that overflowed |
| **Local embedding latency** | 🟡 medium | native in-process inference; models are small and cached locally; two-phase injection (§17.3) serves the deterministic MUST baseline immediately and never blocks on the semantic pipeline |
| Privacy compliance | 🔴 high | local-first; encryption; the forget feature (§9.5) delivers the GDPR right-to-erasure mechanism — auto/manual/bulk permanent purge with audit trail; residual-text in free-form logs/exports remains a known gap |
| Big-vendor copying | 🟡 medium | **Raised from 🟢 after the 2026-11 refresh** — three vendors shipped native memory within a year (§2.2), so this is an observed behaviour, not a hypothesis. Open core + community moat + the vertical (L3/L6) that a per-tool vendor has no incentive to build, since it requires memory to be shared *across* vendors' own products |
| ~~MCP protocol does not support message interception~~ | ✅ retired | Resolved: the MCP Proxy shipped in Phase 2/3 (§10). Kept as a row so the historical mitigation chain in older CHANGELOG entries still resolves |
| ~~Compliance tracker can't see agent replies~~ | ✅ retired | Resolved: the proxy architecture plus the SessionEnd finaliser deliver honest compliance accounting (§14.9) |

---

## 13. Business model

### 13.1 Product tiers

| Tier | Price | Features |
|------|-------|----------|
| Free / OSS | free | core engine + router + CLI + Obsidian plugin |
| Pro | $8/mo | dashboard + compliance tracking + cloud sync + advanced visualization |
| Team | $20/user/mo | shared memory pool + permissions + audit |
| Enterprise | custom | private deployment + SSO + SLA |

### 13.2 Growth flywheel

```
open core + router → developer trust
       ↓
Obsidian plugin → free users
       ↓
"agent ignores memories" pain → upgrade to Pro (compliance tracking)
       ↓
revenue → R&D → better router → more users
```

---

## 14. Multi-agent shared memory design (added in v0.3)

### 14.1 Design goal

```
multiple agents → one MemVault instance → shared memories, filtered per consumer
                               ↓
                 agent A sees coding-related MUSTs
                 agent B sees writing-style MUSTs
                 agent C sees all global REFERENCEs
                 ──────────────────────────────
                 underlying store is the same SQLite (with embedded vectors)
```

### 14.2 Agent Registry

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

### 14.3 Sharing policy matrix

| Operation | Policy | Notes |
|-----------|--------|-------|
| **Write** | all agents write to the same store | data is naturally shared; `source_agent` records the origin |
| **Read** | every agent reads every memory | no isolation in Phase 1 |
| **Inject** | filtered per agent type | coding agents don't get writing memories (noise reduction) |
| **Review** | one review benefits all agents | approved `human_reviewed: true` applies globally |
| **Compliance** | stats split per agent | the `compliance.by_agent` field |
| **Forgetting** | applies globally | once archived, no agent receives it |

> **`agent_id` never filters retrieval.** It is provenance metadata only —
> recorded in `source_agent` at write time, never consulted by store queries
> (search / list / vector search filter on namespace × type × priority).
> Every agent searches the same shared corpus; per-agent differentiation
> happens only at injection time via `inject_rules` (type/tag soft scoring,
> §14.2), never in the store.

### 14.4 Phase 1 multi-agent scope

```
✅ Supported scenarios:
  - Claude Desktop + Cline + Cursor connected to one MemVault instance
  - all agents share the same memory data
  - source_agent records the write origin
  - the router filters by agent type (hard-coded rules)

❌ Not yet supported (Phase 3+):
  - per-agent permission isolation (allowlist/denylist)
  - multi-device sync (CRDTs)
  - semantic conflict merge for concurrent writes
  - agent-private namespaces
```

### 14.5 Typical deployment topology

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

### 14.6 Multi-agent-specific risks

| Risk | Severity | Mitigation |
|------|----------|------------|
| Agent A writes a wrong memory, polluting Agent B | 🟡 medium | default inbox review in Phase 1; allowlists later; since v0.3.1 MUST memories additionally face identity verification + multi-agent corroboration gates (§14.7) |
| Two agents write contradictory memories | 🟡 medium | dedup & merge; timestamp LWW strategy |
| Agent identity forgery | 🟡 medium | MCP connection verification; token auth later; since v0.3.1 the write path records whether the call actually passed registry key verification (`identity_verified`) instead of trusting a self-reported `agent_id` (§14.7) |
| Injection volume doubles (each agent injecting separately) | 🟢 low | each agent has an independent token budget |

### 14.7 Delivered: identity verification + multi-agent corroboration trust gate for MUST memories (v0.3.1)

**Motivation**: the core contradiction from §14.6 is that `is_trusted`
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
  (`writer::merge_memory`, §17.1), the set of distinct `agent_id`s, each
  `identity_verified`, that independently corroborated the content; unverified
  writes are not counted.

`is_trusted` gained a third decision path (active only with
`MEMVAULT_CORROBORATION_GATE=on`, default off): a MUST memory independently
written by `MEMVAULT_CORROBORATION_MIN_AGENTS` (default 2) distinct verified
agents counts as a trustworthy instruction even without human review. Default
off keeps existing single-agent deployments without `api_key` byte-for-byte
identical; only explicit enablement of auth + the corroboration gate treats
"multiple independent trusted sources agree" as a substitute for human review.

**Key trade-off**: no per-agent read/write isolation (§14.4 still holds) —
isolation contradicts the "memory hub" positioning; what is defended here is
content authenticity (who wrote it, was it independently corroborated), not
an access boundary.

### 14.8 Delivered: unreviewed memories are excluded from injection by default

**Motivation**: `is_trusted` (§14.7) splits memories into a trusted instruction
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
REST `/api/session`, and the proxy deterministic fast path. Two earlier leaks
are structurally closed: the overflow path re-fetches a wider candidate set
and therefore applies the trust filter itself (`overflow_admits`), and REST
rendering via `agent_adapt::format_memories` now mirrors the router's
trusted/unvetted split instead of emitting `- ALWAYS:` for unreviewed MUST
rows. L0 raw-evidence rows (`trace:*`, `human_reviewed = true` by
construction) are excluded from overflow unconditionally, matching how
lessons/skills/episodes already skip them.

**Provenance vs. auto-drafts**: `memvault-cli save` is an explicit
human-initiated write and stamps `human_reviewed = true`,
`ai_generated = false`. Stop-hook extraction, the `extract_memories` tool and
`record_outcome` lessons keep landing unreviewed — which is exactly what the
gate is for.

**Compliance honesty** (§14.9): a store full of `pending` rows used to report
100% compliance because the rate was hard-coded to `1.0` whenever no verdict
existed. Rates are now `Option<f64>` — `None`/`N/A` until a MUST row has a
real followed/violated verdict — and the `SessionEnd` hook
(`MEMVAULT_HOOK_COMPLIANCE=1`) closes each session's pending rows as judged
(with an LLM extractor configured) or explicitly `unknown`, never fabricated.

**Key trade-off**: a dropped memory is not deleted — it still exists in the
store, stays in the review inbox, is returned by `search_memory`, and surfaces
in `session_start` once approved (`human_reviewed = true`); this only narrows
what auto-injection shows.

### 14.9 Delivered: honest compliance accounting and the SessionEnd finalizer

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

## 15. Three-track memory evolution: delivered state

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
> point (see §16).

## 16. Longer-term plans (not started)

| Item | Description | Status |
|------|-------------|--------|
| **Valid-time query (`as_of`) + `expires_at`** | Both closed rows in §2.5's first table. Delivered as §9.6: `expires_at` (valid-time upper bound, filtered out of every path that can reach an agent's context, archived by the decay cycle so the auto-forget sweep can reach it, and honored even for MUST because an explicit declaration is not the inferred signal `must_exempt` protects against) and `as_of` on `get_as_of` / `list_as_of` (exact, reconstructed from the existing `memory_history` snapshot chain — no new version schema needed). The design constraint taken from agent-memory (§2.3) held: **valid-time and event-time stay separate axes** (`as_of` vs the pre-existing `occurred_at`), never folded into one column | ✅ delivered (§9.6) |
| **Historical-text search** | The residual piece of the item above. `search(as_of=…)` filters and reconstructs correctly but still *matches* against the current FTS/vector index, so a memory whose wording has since changed may not be recalled. Closing it needs an FTS index over `memory_history` snapshots. Until then `get_as_of`/`list_as_of` are the exact paths and every search result carries `as_of_source` saying what was reconstructed | not started |
| **Event-time filtering (`occurred_from`/`occurred_to`)** | `occurred_at` is stored but not filterable, so "what happened in March" is not directly queryable even though "what did we believe in March" now is. Kept separate from `as_of` on purpose — agent-memory's rule (§2.3) is that the two axes are never folded, and a single `since`/`until` pair would fold them | not started |
| **Event-time is unsettable on the direct-save path** | `occurred_at` can only be supplied through the *extraction* endpoints (`POST /api/extract`, MCP `extract_memories`), not through `POST /api/memories` / `save_memory` / `memvault-cli save`. The split is coherent — event-time describes the source conversation an extraction came from — but it means a memory ingested from a historical transcript and then written directly loses its event date, and a backfill of dated material has no way to carry the dates. Found while building the BEAM harness (`docs/experiments/beam_adherence.py`), which resolves a session date for every probe and then has nowhere to put it | not started |
| **Per-memory L0/L1 disclosure layers** | Delivered as §6.5, derived on demand rather than stored. The Fuser caveat that came with it (§2.3) turned out **not to apply**: agent-memory needs MaxP because it indexes each layer as a separate physical entry, whereas MemVault's layers are injection-time derivations that are never indexed — one row still produces one candidate per channel and `HybridMerger` already keys by `memory.id`. The constraint that keeps it that way is recorded in §6.5 and pinned by `test_disclosure_layers_do_not_change_hybrid_ranking`; adding l0/l1 to the FTS index would reintroduce the bug and also distort BM25 by indexing a prefix of text already indexed | ✅ delivered (§6.5) |
| **Agent self-editing of Markdown memories (`edit_md_file`)** | Give an agent a file-shaped edit tool: exact unique-`old_text` replacement, atomic write-back, and an inherit-and-SUPERSEDE version step, instead of only `update(id, patch)`. agent-memory ships exactly this (§2.3) — **but it writes straight to the source of truth**, which §18.3.1 says is session-injection authority. Delivered with the gate that makes it safe: the edit lands as a *pending proposal* and the target keeps injecting until a human approves (§9.3.1). Not tracked anywhere before 2026-11 — it had appeared only in the §2.3 comparison table, which describes other products and is not a work list | ✅ delivered (§9.3.1) |
| **Scenario profiles** | Bundle the scattered `MEMVAULT_*` switches into named presets (`coding-agent` / `assistant` / `team` / `edge-lite`) so onboarding does not require reading a 12KB `.env.example`. `agents.yaml` already does this per-agent for `inject_rules`; this extends it to the global storage/evolution/forget/relations switches. Pattern per agent-memory §13.3 — see §7.5 for what was adopted and what was deliberately left out | ✅ delivered (§7.5) |
| **Public benchmark run (BEAM + AMB)** | Run BEAM's instruction/preference-adherence axis and MemoryAgentBench's conflict-resolution/selective-forgetting axis; publish on AMB with a complete harness specification; report retrieval token count and p50/p95 latency next to accuracy so two-phase injection (§17.3) becomes a number instead of a claim. Explicitly **not** LoCoMo-first (§2.6). **Scaffolding delivered**: `docs/experiments/beam_adherence.py` pulls the real `Mohammadta/BEAM` rows over the HF datasets-server HTTP API (no pyarrow needed), extracts the `instruction_following` / `preference_following` probes with their `source_chat_ids` provenance, seeds each rule as a human-authored MUST, runs the control/experiment A/B, and emits the full harness spec alongside the numbers. Validated end to end on 20 probes across 5 conversations with `--dry-run --with-server`: 20/20 seeded, 20/20 rule present in the injection block, 20/20 delivered via the `MustRef` instruction layer, injection p50 ≈1.2 ms / p95 ≈1.6 ms with embeddings off (repeats within ±0.1 ms) — which is itself the §17.3 deterministic-MUST claim measured, against mem0's published p50 ≤ 1.1 s frame. **Remaining**: an actual scored run needs an LLM key and a judge-stability measurement, and the numbers must not be cited without them | scaffolding delivered; scored run pending |
| **Graph database integration** | introduce a graph DB (replacing/upgrading the `memory_relations` query path) only when relation scale outgrows single-table one-hop expansion. agent-memory reached the same conclusion independently — it makes graph index an *optional, config-gated* channel, off by default | deferred, awaiting scale signal |
| **General world-knowledge base** | general commons stays with the model; MemVault only accumulates personal/project/org domain knowledge | not doing (design constraint) |
| **CRDTs multi-device sync** | offline collaboration across devices. agent-memory also parks this as an open question (consistency level, conflict-merge policy, conflict visualisation all unresolved) — no peer has a design worth copying yet | not started |
| **Multimodal input** | if pursued, copy the cheap shape rather than the ambitious one: keep the original asset (or a reference) as source of truth, derive a governed **text projection** (ASR/OCR/caption), and **retrieve only over the projection** — no multimodal vectors, so the retrieval chain stays modality-agnostic (§2.3) | not started |
| **Edge/cloud hybrid sync** | a hosted tier plus selective two-way sync. Deliberately **not** pursued on the strength of §2: it is the one capability where a local-first single binary is the *product*, and every peer that has it also has a multi-tenant governance stack MemVault has explicitly chosen not to build (§18.3.1) | not doing (positioning constraint) |
| **Dual-track injection (compressed summary + verbatim retrieval)** | split the injection budget into a continuously maintained fixed-size "user/project state summary" plus on-demand full-text retrieval. Rationale: the Qwen3.8-Flash-Next report (§2.1.1) demonstrates that compressed state and verbatim retrieval are complementary. Requires new memory shapes (aggregate summary generation/maintenance pipeline), beyond current scope. **The per-memory L0/L1 item above was the cheap 80% and has now shipped** (§6.5); what remains here is the *global* maintained aggregate, which is a different and much larger thing | not started (long-term) |
| **Plugin marketplace** | publishing/operations for VS Code / Obsidian / dsh plugins | not started |
| **Markdown-backend v1 exclusions** | git auto-commit, running both backends at once, multi-machine merge of concurrent edits, per-namespace subdirectories, NFS-safe locking | not doing in v1 (see §18.6) |

### Unimplemented candidates from the claude-obsidian review

> An item-by-item code review of
> [AgriciDaniel/claude-obsidian](https://github.com/AgriciDaniel/claude-obsidian)
> was completed and archived; the implemented items (P0 injection-safe
> wrapping / P1 evidence links and evidence-driven decay / P1.5
> `memvault-cli doctor`) are in the CHANGELOG. The remaining candidates below are
> started only when their trigger conditions are met.

| Item | Description | Trigger / status |
|------|-------------|------------------|
| **Transactional write protocol (plan → sha256 → apply)** | file-side writes (Obsidian sync etc.) become "emit plan → verify SHA-256 → apply", with a matching MCP plan-approve-apply tool triple. Currently `obsidian-plugin/src/sync.ts` still writes create/update/skip directly | deferred — invest when multi-agent shared writes (§14) or the human review flow (§9.3) demand it |
| **REST `/api/memories/{id}/evidence` endpoint** | REST surface for the Dashboard's evidence graph on a per-memory basis; MCP read/write already exists (`add_evidence` write, `get_memory_evidence` read — internally `trace_evidence_chain` + `evidence_summary`, test-covered); REST is a thin wrapper | ✅ implemented (shipped with the dashboard detail-panel evidence chain) |
| **`agent_adapt.rs::format_memories` treat-as-data wrapping** | give the REST `/api/search` formatting path the same injection-safe wrapping as `router/format.rs` in P0 | later candidate |
| **Obsidian plugin memory health check** | full lint / stale-index patrol in the plugin (modeled on claude-obsidian `lint_engine.py`); today `detectOrphans` only handles orphan notes | later candidate |

> **Open questions from the original plan**: Q1 (lessons ascend to MUST only
> with human confirmation), Q2 (episode ↔ memories 1:1), Q3 (minimum success
> samples = 3), Q4 (relation extraction off by default; `MEMVAULT_RELATIONS=on`
> to enable), Q5 (lessons default to their namespace; global only with a manual
> mark) — all decided and shipped with the implementations; nothing pending.

## 17. Memory design informed by the Qwen3.8-Flash-Next architecture report

> The analysis drew on three memory-relevant components of the
> Qwen3.8-Flash-Next technical report (§2.1.1 GDN compressed state +
> periodic full attention, §2.1.2 QSA two-level sparse retrieval, §2.3 N-gram
> conditional memory), which map onto MemVault's design. Five items were
> implemented, one deferred. The paper's methodology carries over: every change
> is evaluated along **quality / cost / stability** axes, with negative results
> recorded.

### 17.1 Delta writes on save (paper §2.1.1 GDN delta rule)

**Motivation**: GDN's write rule estimates the value already associated with a
key and **writes only the residual** — repeated/similar keys update existing
associations instead of accumulating without bound. A purely append-only
memory store is the "unbounded extrinsic additive memory" the paper argues
against, leaving `dedup` to clean up afterwards.

**Design**: the `memvault-core::writer` module (`MemoryWriter` + `merge_memory`)
moves dedup/merge from batch commands into the write path. All three user save
channels (CLI `save`, MCP `save_memory`, REST `POST /api/memories`) route
through it:

```
save request → same-namespace lookup (token-overlap jaccard; stricter threshold on the vector path)
  ├─ similarity > 0.95 → Skip: no new row; return the existing memory
  ├─ similarity > threshold → Merge: residual appended to the old memory,
  │            refresh timestamps/decay, monotonically upgrade priority,
  │            union tags, re-embed
  └─ otherwise → normal insert
```

**Key trade-offs**:
- **Token-overlap and vector-score thresholds** (0.7 / 0.9): cosine similarity
  will rate "api /v1 vs /v2" at 0.87 "similar", but those are **mutually
  exclusive new facts** — merging would corrupt data. Token overlap is the
  better proxy for "same assertion"; vectors only act as a high-threshold
  backstop. Corrective new facts go through `supersede` (old fact archived,
  new fact points at it), never through merge.
- **Skills (SOPs) are never merged**: procedural knowledge has no merge
  semantics with facts; they are always inserted directly.
- **Bypasses**: CLI `--force`, MCP/REST `force_insert`; global switch
  `MEMVAULT_DELTA_WRITE=off`.

### 17.2 Task-level evaluation benchmark `bench` (paper §2.3.2 "loss ≠ downstream")

**Motivation**: the paper's most counter-intuitive finding — training loss
drops monotonically as memory vocabulary grows, but **downstream task
performance saturates or even wobbles**. In product terms: **retrieval recall
≠ agent task success**. Competitors report retrieval metrics like LongMemEval
R@5; MemVault's `outcome` mechanism provides real task history for task-level
evaluation — a differentiation opportunity.

**Design**: the `memvault-core::bench` module + `memvault-cli bench` command.
Samples come from the user's own failure history (episodes that distilled a
lesson), in three layers:

| Layer | Metric | Dependency |
|-------|--------|------------|
| retrieval | does the lesson reach top-k when the historical task text is used as the query? | none |
| injection | does the lesson actually get injected after the full `session_start` pipeline + injection-cost estimate? | none |
| judge (`--judge`) | an LLM generates "plan without memory" vs "plan with injected memory" and scores them against the known failure cause; the delta is memory's task-level value | LLM provider (best-effort; skipped when unavailable) |

The first two layers always run with zero external dependencies; any failure in
the judge layer degrades to "not evaluated" rather than erroring out.

### 17.3 Two-phase injection: deterministic fast path + async prefetch (paper §2.3 deterministic addressing)

**Motivation**: n-gram memory tables can sit in host memory with async
prefetch because **addressing is deterministic** — no need to finish reading
the context to know what to fetch. MemVault's analogue: MUST rules +
namespace rules are decidable by pure rules with zero embedding calls, so they
can return synchronously; semantic retrieval is prefetched in the background.

**Design**: the `InjectionEngine`'s `refresh_two_phase`:

```
Phase 1 (synchronous): deterministic injection — MUST memories resolved by pure rules
               (covering project namespaces and global, mirroring the
               full path's cross-namespace fallback), state written immediately
Phase 2 (background): full layered semantic pipeline; on completion the state is replaced wholesale;
               on failure → keep the deterministic baseline
```

- `wait_full(250ms)`: the caller waits at most a short window for the fuller
  semantic result; on timeout the deterministic baseline proceeds — **requests
  are never hijacked by embedding latency**.
- `generation` counter: when a slow prefetch returns and a newer refresh
  already happened, stale results are discarded instead of overwriting fresh
  state.
- Both phases share the same `session_id`: compliance tracking stays
  continuous across state upgrades (Deterministic → Full).
- Fixed a latent bug along the way: `SessionContext::get_project()` returned a
  namespace already prefixed with `project:`, while `session_start` added
  another layer, producing malformed `project:project:*` namespaces; now
  normalized by `router::project_namespace()` (accepts either input form and
  adds the prefix exactly once).
- **Project namespaces are canonicalized** (`MemVault`, `memvault` and
  `memvault/` are one spelling): `canonical_project_namespace()` lowercases
  the project id and strips trailing slashes/whitespace on every write path
  (`save`/`update`, CLI/MCP/REST alike) and every read filter (`list`,
  `search`, `vector_search`, `list_pending`, session injection). Rows/files
  written before canonicalization are migrated once on open — sqlite via an
  idempotent backfill, markdown by rewriting the file frontmatter during
  `reindex` — so existing data stays reachable instead of being silently
  orphaned by the stricter spelling.

### 17.4 Session n-gram retrieval key (paper §2.3 "conditional memory")

**Motivation**: n-gram embedding upgrades the retrieval key from "the identity
of a single symbol" to "the local context ending at the present" — that single
change improved every benchmark. MemVault's analogue: the retrieval key
becomes the **most recent n turns, weighted by recency** — the turn being
processed dominates, earlier turns still condition the query.

**Design**: both channels implement the same scheme (recency weighting = newer
turns repeat more, linear decay, total length capped at ≤600 chars):
- **proxy transparent injection**: `SessionContext::conversation_ngram(window)`
  assembles the retrieval key from the most recent observed tool-call turns;
  both refresh modes use it; window via `MEMVAULT_CONTEXT_NGRAM_WINDOW`
  (default 5).
- **explicit session entries** (CLI `session-start --context`, MCP
  `session_start`, REST `/api/session`): `query_expand::weight_turns_by_recency`
  treats multi-line context as a turn sequence with the same weighting;
  single-line input behavior unchanged.

### 17.5 Injection-path dedup: single canonical path (paper Table 7)

**Motivation**: the paper's layer ablation shows that spreading the same
parameter budget across multiple layers yields **no stable gain**. MemVault
has three injection paths (MCP `session_start`, proxy interception, `sync`
instruction files); the same memory can reach one agent repeatedly through
different paths.

**Design**: an `InjectChannel` enum (`mcp` / `proxy` / `sync`) +
`AgentProfile.inject_channel` (configurable in `agents.yaml`):

- **default `None` = path-unrestricted** (fully backwards compatible); setting
  it enables dedup.
- `MemoryRouter::channel_allows(agent_id, channel)` lets each path self-check.
- MCP `session_start` and REST `/api/session` (the `mcp` channel): skip
  injection when non-canonical and return an explanatory note ("this agent is
  injected via channel X") — explicit requests still get an auditable answer.
- proxy transparent injection: no injection state when non-canonical.

### 17.6 Deferred and not doing

| Item | Decision | Rationale |
|------|----------|-----------|
| Feature E two-level retrieval (clustered coarse-rank + in-budget fine-rank) | deferred | corresponds to paper §2.1.2; current memory scale hasn't hit the trigger condition — a scalability insurance, not today's bottleneck |
| Dual-track injection (persistent compressed summary + on-demand verbatim) | long-term (§16) | paper §2.1.1 shows compressed state and verbatim retrieval are complementary; needs new memory shapes (aggregate summary pipeline), beyond current scope |
| Predictive half-life decay | not doing | the data-driven decay idea is already covered by `contradiction_multiplier` + type-stability coefficients + `access_boost` |
| Distill/train a dedicated ranker | watchlist | needs `bench`/usage-signal data to accumulate first |
| N-gram-style memory compression (squeezing content for budget) | not doing | the paper explicitly reports no stable gains from such tricks (token normalization, non-uniform allocation, frequency-based partitioning) |

## 18. Storage backends: sqlite vs markdown

> Added in v0.5.0 — the EverOS-inspired second storage backend (Phase 4; the
> cross-backend consistency suite is Phase 5). Selected by
> `MEMVAULT_STORAGE_BACKEND`; the default is unchanged, so existing installs
> never see a difference.

### 18.1 Selection and layout

| Setting | Values / default | Notes |
|---------|------------------|-------|
| `MEMVAULT_STORAGE_BACKEND` | `sqlite` (default) / `markdown` (alias `md`) | unset or unknown → `sqlite`, backward compatible |
| markdown data dir | `--markdown-dir` (cli/mcp/proxy) or `MEMVAULT_MARKDOWN_DIR` | required in markdown mode; startup fails with a clear error if neither is set |
| markdown trust mode | `MEMVAULT_MARKDOWN_TRUST_MODE` = `owned` (default) / `quarantine` | unset or empty → `owned`; unknown non-empty → `quarantine` (fail closed) |

- **`sqlite`**: one `data.db`; every write is one transaction (behavior unchanged).
- **`markdown`**: a directory of human/git/Obsidian-editable `{id}.md` files is
  canonical. Each file is YAML frontmatter (every `Memory` field, lossless) plus
  body (plus an optional `> Instruction:` line), written atomically (temp file +
  rename). An internal `.memvault-cache.db` is a disposable, rebuildable index
  holding FTS5, embeddings, episodes, skill stats, relations and trace
  watermarks — plus the side tables (`ComplianceStore`, `EvalHistoryStore`) that
  in sqlite mode share `data.db`.

### 18.2 The inverted data-flow

- **sqlite mode**: `data.db` is authoritative; `memvault-cli sync` *projects* it
  into instruction files and the Obsidian vault (one-way).
- **markdown mode**: the files are authoritative; the cache is derived. Only
  `save` / `update` / `delete` / `save_with_embedding` / `supersede` touch the
  filesystem — every other `MemoryStore` method passes straight through to the
  cache.

### 18.3 Consistency model

- The cache is **eventually consistent**. `memvault-cli reindex` diffs each
  file's content against `Exporter::memory_to_markdown` of its cached row and
  resyncs on any mismatch, so a crash between a file write and its cache write,
  and a hand-edit made outside MemVault (Obsidian, an editor, `git checkout`),
  self-heal the same way.
- `MarkdownStore::open` runs one `reindex` pass, so a stale or deleted cache
  self-heals on startup. `reindex --watch [--interval <secs>]` polls
  continuously (default 5s; no file-watcher dependency).
- An advisory single-writer lock `{dir}/.memvault.lock` (`create_new` semantics,
  no liveness check) guards the directory. A lock left behind by a crash must be
  removed manually — deliberate, to avoid unsafe cross-platform stale-lock
  detection.

#### 18.3.1 Trust boundary: `owned` vs `quarantine`

A Markdown file carries its own trust metadata in frontmatter — `ai_generated`
is the biggest lever, because `ai_generated == false` makes `is_trusted()` treat
the memory as human-authored and inject it in the instruction block. Priority,
review state, identity verification, corroboration and visibility likewise
affect ranking, gating and reach. The storage backend therefore has to answer:
*who is allowed to say these things about a file?*

- **`owned` (default)** — MemVault is a local-first, single-user store. A file
  the user can edit is a user action, so the file wins byte-for-byte. This
  includes `human_reviewed`, `identity_verified`, `ai_generated`, `priority`,
  `visibility`, and all other persisted metadata. Consequences, stated plainly:
  **write access to the Markdown directory is equivalent to memory-write access,
  which is equivalent to future session-injection authority.** That is the
  intended trust model for a personal vault, not a bug. Tools that share the
  vault with an untrusted process should not use `owned`. One parser detail
  matters here: omitting `ai_generated` is **not** a claim of human authorship.
  A file without the field keeps the safe default `ai_generated=true` and is
  treated as an unreviewed draft; a hand-written file that should be injected
  as a trusted instruction must say `ai_generated: false` explicitly (or be
  approved through the review flow).
- **`quarantine`** — opt-in for a shared/synced vault, a multi-writer setup, or
  any import from a lower-trust source. A file with no cached baseline is
  reduced to an ordinary unreviewed draft: `human_reviewed=false`,
  `identity_verified=false`, corroboration cleared, `ai_generated=true`,
  `priority` capped at `REFERENCE` (so it cannot claim the MUST token/count
  exemptions), `layer` pinned to `L1`, authority tags (`decision`/`procedure`/
  `gotcha`) removed, confidence capped, server-maintained decay/access/read
  bookkeeping reset, and `visibility: shared` downgraded to `scoped` (shared
  bypasses the namespace filter). An existing row may still be downgraded
  (`shared → scoped`, lower priority/layer, review revoked), but never
  escalated
  (`scoped → shared`, `REFERENCE → MUST`, `ai_generated → human-authored`).
  Approval is bound to the reviewed bytes: replacing the body or instruction
  in quarantine revokes `human_reviewed`, `identity_verified`, and prior
  corroboration, and resets `ai_generated` to `true`, instead of letting the
  replacement inherit the old frontmatter.
  Ranking metadata is pinned too: a file cannot raise `layer`, add an authority
  tag, raise `confidence`, or forge `decay_score`/`access_count`/`last_read`/
  `created`/`updated` to buy retrieval authority or freshness.
  When a value is clamped, the file is rewritten so the next pass converges.
  The clamp is applied by `reindex`, directory/single-file `memvault-cli
  import`, and REST `POST /api/import` alike — a different entry point must not
  be a policy bypass. JSON import and `migrate-to-markdown` remain lossless
  trusted paths and are not sanitized.
- **Symlinks are never reindexed.** Only regular files directly inside the
  Markdown directory are source-of-truth entries. A symlinked `.md` is skipped
  (`skipped_symlinks`) rather than followed, so a shared vault cannot pull an
  arbitrary file from outside the vault into the memory index. Forget removes a
  symlink itself, never its target.
- **Cold-start agent imports are untrusted by default.** `import-agent` (CLI
  and REST) parses files produced by another tool, so adapters' source-level
  `ai_generated: false` is not accepted as a trust decision unless the local
  operator passes `--approve`. Unapproved imports stay unreviewed and
  `ai_generated=true`; REST has no approve parameter. JSON import,
  `import-skills` review behavior, and `migrate-to-markdown` remain explicit
  operator-trusted paths.
- **Quarantine's cache caveat.** With no cached row, quarantine has no way to
  tell "a file that was approved yesterday" from "a hostile file dropped in a
  minute ago". The cache is that baseline. Deleting `.memvault-cache.db` in
  `quarantine` mode therefore demotes every file back to unreviewed; this is
  fail-closed, but operators must preserve the cache (or re-apply approvals)
  when using this mode. `owned` does not have this problem because the file is
  the baseline.
- **Review gate interaction.** `MEMVAULT_INJECT_REQUIRE_REVIEW` defaults on, so
  a quarantined new file is not injected at all before human approval, even if
  its content says otherwise. The priority/visibility clamps are defense in
  depth for deployments that explicitly disable that gate.

### 18.3.2 Permanent-erasure tombstone

Permanent forget must survive the very thing a Markdown vault encourages: a
stale `git checkout`, sync client, editor undo, or backup copy reappearing after
the erase. `<markdown-dir>/.memvault-forgotten.json` is the durable tombstone —
an append-only JSON list of `{memory_id, content_hash, namespace, priority,
visibility, scope, reason, requester, erased_at}` with **no content body**. It
is written atomically (temp file + rename) and permission-restricted before the
cache mirror, so a reported erasure always has a surviving tombstone.

On every `reindex`, an id present in that file is removed from disk, its stale
cache row is dropped, and it is never re-imported (`skipped_forgotten` in the
report and CLI output). `save`/`update` also reject the id; a forgotten id is
never reusable, even if the user wants to create a "new" memory with the same
name. Operational consequences:

- If a stale file is the only surviving copy (the disposable cache was rebuilt
  and has neither a live row nor history), `forget` reads that file directly,
  records the same hash-only audit entry and durable tombstone, then removes it.
  `save`/`update` checks use an in-process tombstone set refreshed by
  `reindex`, and a final check under a write gate prevents a concurrent
  `reindex` from racing a forget and restoring the stale row.

- The tombstone is **not** a cache. Keep it with the vault and in backups. If it
  is deleted, stale copies can resurrect the erased memory again.
- In `owned` mode, any process that can delete the tombstone can defeat this
  protection — the same trust boundary as every other vault file. Use
  `quarantine` plus OS permissions for untrusted co-writers.
- Pre-upgrade installations may only have the `forgotten_log` audit table in
  `.memvault-cache.db`; `reindex` merges that legacy mirror with the durable
  file, so the first reindex after upgrading still blocks known ids.

### 18.4 Migration

`memvault-cli migrate-to-markdown --dir <dir>` does a one-time copy from the
currently active store into a new markdown store, carrying embeddings over
(`save_with_embedding`). This is a trusted store-to-store copy, not a
file-origin import: it deliberately bypasses `quarantine` sanitization so an
already-approved SQLite memory stays approved. The same applies to a JSON
export/import round trip. Use Markdown import (or hand-drop a file) when the
bytes actually come from an untrusted source.

### 18.5 Feature boundaries

`backup` / `checkpoints` / `restore` stay SQLite-only (their semantics are
SQLite-specific) and fail with a clear error on the markdown backend rather than
silently no-op'ing. `GET /api/system/backend` reports the active backend
(`sqlite` / `markdown`) so clients and the dashboard can surface it up front.

### 18.6 Explicitly out of scope (v1)

Git auto-commit, running both backends at once, multi-machine merge of concurrent
edits, per-namespace subdirectories, and NFS-safe locking. Each is deferred, not
designed away (§16).

## Appendix

### A. Reference projects

- [MemPalace](https://github.com/) — memory-palace storage
- [Mem0](https://github.com/) — hybrid memory layer
- [Zep](https://github.com/) — temporal knowledge graph
- [MCP specification](https://spec.modelcontextprotocol.io/) — the protocol standard
- [LanceDB](https://lancedb.github.io/) — embedded vector DB (evaluated; not used since v0.2 — vectors live in SQLite embedded columns)
- [Khoj](https://khoj.dev/) — Obsidian RAG
- [Automerge](https://automerge.org/) — CRDTs

### B. Key decision log

| Decision | Rationale |
|----------|-----------|
| Independent tool + thin clients | break past the Obsidian ceiling |
| The Memory Router is the core differentiator | market gap, no competitor |
| Pluggable storage, MemPalace/Mem0-compatible | don't rebuild the wheel |
| MUST/REF memory tiers | solves the "not followed" problem |
| MCP as the primary protocol | the healthiest ecosystem |
| Local-first | privacy first |
| Multi-agent shared memory: one MCP server instance + Agent Registry | every agent reads/writes through the same service; shared SQLite data |
| Phase 1 ships no per-agent permission isolation | MVP simplicity; every memory visible to every agent |
| The router filters injection content by agent type | coding agents don't receive writing memories; noise reduction |
| **Phase 1 injection: MCP Resource + session_start** | **MCP cannot intercept messages; pre-prompt injection deferred to Phase 2 behind the MCP Proxy** |
| **Compliance Tracker deferred to Phase 3** | **a standard MCP server never sees agent replies; depends on the MCP Proxy** |
| Vectors stored in SQLite embedded int8 columns (not LanceDB) | zero extra dependency, quantized on-row storage |
| Two-phase injection with a deterministic fast path | requests must never be blocked on embedding latency |

### C. Hypotheses to validate

1. Does pre-prompt injection actually improve agent follow-through? (A/B test)
2. Follow-through difference between instructional vs descriptive formats? (target: +20%)
3. Will users invest time in reviewing memories? (review rate > 30%)
4. What token budget is optimal? (1000 / 1500 / 2000 tokens)
5. Can router mis-injection stay under 5%?
6. Obsidian plugin → Dashboard conversion rate? (target > 5%)

> All core hypotheses — including the three-track acceptance ones — have been
> tested on a live server; evidence and methodology are recorded in
> `docs/experiments/REPORT.md` and §15.

---

> This document is maintained as the product/architecture reference for the
> v0.3 design baseline as shipped in v0.6.x. Subsequent evolution beyond it
> lives in §16 (longer-term plans).
>
> §2 (market analysis), §11 (differentiation) and §12 (risks) were refreshed
> together in 2026-11 and must be re-checked together: §2 supplies the evidence,
> §11.1 the moat ratings, §12 the residual risks. Editing one without the others
> is how the previous revision came to rate auto-injection as an empty space for
> two years after three platforms shipped it.
