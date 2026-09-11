# Cross-Agent Personal Memory Tool — Product & Architecture Design

> **Project**: MemVault
> **Version**: v0.3
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
13. [Business model](#13-business-model)
14. [Multi-agent shared memory design (added in v0.3)](#14-multi-agent-shared-memory-design-added-in-v03)
15. [Three-track memory evolution: delivered state](#15-three-track-memory-evolution-delivered-state)
16. [Longer-term plans (not started)](#16-longer-term-plans-not-started)
17. [Memory design informed by the Qwen3.8-Flash-Next architecture report](#17-memory-design-informed-by-the-qwen38-flash-next-architecture-report)

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

### 2.1 Tier 1: memory storage and retrieval

| Project | Core capability | Recall approach | Gap |
|---------|-----------------|-----------------|-----|
| **MemPalace** | memory-palace hierarchy + ChromaDB | structured hierarchical retrieval, high LongMemEval R@5 | no auto-injection; no human review; purely passive tools |
| **Mem0** | hybrid vector + graph + KV storage | hybrid retrieval + graph enhancement | only add/search APIs; the agent must call proactively |
| **Zep** | temporal knowledge graph | graph traversal + temporal reasoning | passive tool mode; enterprise closed source |
| **TiMEM** | temporal hierarchical memory tree | tops benchmarks | academic prototype, not productized |
| **LangMem** | ecosystem component | LangGraph memory integration | framework-bound, not a standalone product |
| **Letta** (MemGPT) | OS-style memory management | paged context | oriented to agent runtimes |
| **Memoria** | trusted-memory framework | security auditing | recently open-sourced, incomplete |

### 2.2 Tier 2: the Obsidian ecosystem

| Project | Core capability | Gap |
|---------|-----------------|-----|
| **Khoj** | local RAG + Obsidian plugin | Q&A only; no memory write/review/auto-inject |
| **Smart Connections** | semantic search + backlink recommendations | retrieval only; no write pipeline |
| **mcp-obsidian** | MCP file read/write proxy | file-level operations, no semantic abstraction |
| **Claudian** | Claude + skills system | bound to a single model |

### 2.3 Key finding: the market gap

> **The shared architectural blind spot of all existing projects:**
>
> ```
> Existing projects:  User message → Agent → [agent decides whether to call search_memory] → LLM
>
> Our approach:       User message → [Memory Router intercepts automatically] → retrieve+inject → Agent (already carries memories) → LLM
> ```
>
> **No existing project owns the full chain of "Memory Router / auto-injection / follow-through guarantee".**

### 2.4 Comparison with the closest competitors

| Dimension | MemPalace | Mem0 | Khoj | **MemVault** |
|-----------|-----------|------|------|--------------|
| Memory write | ✅ | ✅ | ❌ | ✅ |
| Structured retrieval | ✅ | ✅ | ⚠️ | ✅ (reusable backends) |
| **Auto-injection (router)** | ❌ | ❌ | ❌ | ✅ **core difference** |
| **Follow-through (MUST/REF)** | ❌ | ❌ | ❌ | ✅ **core difference** |
| Human review flow | ❌ | ⚠️ limited | ❌ | ✅ Inbox + Dashboard |
| Cross-agent (MCP) | ⚠️ | ✅ | ❌ | ✅ |
| Native Obsidian experience | ❌ | ❌ | ✅ | ✅ |
| Compliance tracking | ❌ | ❌ | ❌ | ✅ |
| Forgetting / decay | ❌ | ❌ | ❌ | ✅ |

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
> Storage is pluggable (MemPalace/Mem0-compatible); the core value is the
> Router + follow-through layer. The Obsidian plugin is the acquisition
> funnel; the independent tool carries the product value.

### 4.2 Key strategy

> **Do not build a storage engine from scratch. Treat MemPalace/Mem0 as
> pluggable backends; the core value is the Memory Router and the
> follow-through layer on top.**

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
│ │ │ Pluggable backends: MemPalace │ Mem0 │ Zep │ custom      │  │ │
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
| Pluggable backends | MemPalace / Mem0 adapters | leverage the existing ecosystem |

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
related: ["[[Python]]", "[[Code Style]]"]
ai_generated: true
human_reviewed: false
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

> **Truth-source principle**: **SQLite is the single source of truth**; the
> Obsidian vault directories and future file-based export/client surfaces are
> **projections/caches**. File-side content may be rewritten by external tools
> and must never be treated as write authority. That is why file-side writes
> need protective mechanisms such as the "transactional write protocol" (§16);
> the timing of that investment is driven by multi-agent shared writes /
> human-review flow needs.

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
        "confidence": 0.0
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
        "token_budget": 2000
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

> **Current implementation**: the MCP surface has grown to **16 tools** — on
> top of the originals, `record_outcome` (task outcomes/lessons),
> `import_skills` (Markdown SOP import), and `add_evidence` (evidence links:
> supports / contradicts / sourced_from) were added; `search_memory` and
> `POST /api/search` gained the `expand_relations` parameter. Full list in the
> root README ("16 MCP Tools") and §15.

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
- policy is user-configurable

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
| 4 — smart pipeline + ecosystem | background extractor worker, dedup/merge/conflict detection, forgetting curve/decay, VS Code extension, pluggable storage backends | ✅ delivered (background extraction runs via proxy hooks; VS Code ships as a config profile, not an extension package) |
| 5 — extensions | web app, CRDTs multi-device sync, graph database, team-shared memory pool, plugin marketplace | partially delivered — team shared pool shipped (see §15); CRDTs and graph database remain open (§16); plugin marketplace not started |
| 6 — three-track memory loop | episodic (Phase A), procedural (Phase B), semantic (Phase C), optional items (Phase D) | ✅ delivered; acceptance evidence in `docs/experiments/REPORT.md` (see §15) |

---

## 11. Differentiation strategy

### 11.1 Core differentiation (layered moat)

| Layer | State of the art | Our approach | Moat strength |
|-------|------------------|--------------|---------------|
| L1: storage & retrieval | done well by MemPalace/Mem0 | integrate, don't rebuild | 🟢 leverage |
| L2: auto-injection (router) | almost nobody | pre-prompt + MCP Resource + session bootstrap | 🔴 core moat |
| L3: follow-through guarantee | empty space | MUST/REF + instructional format + compliance tracking | 🔴 core moat |
| L4: human collaboration | partially exists | Inbox + Dashboard + bidirectional sync | 🟡 differentiator |
| L5: native Obsidian | Khoj/SC do RAG | memory-management-specific UI | 🟡 differentiator |

### 11.2 Positioning statement

> "Mem0 and MemPalace solved 'how to store and find'.
> We solve 'how to get it to the agent automatically, and make it act'.
> Don't teach agents to query memories — make memories appear automatically."

### 11.3 Relationship to the ecosystem

```
MemPalace / Mem0 (storage backends)
        ↓ pluggable integration
MemVault Memory Router (auto-injection + follow-through)
        ↓ distributed to
Obsidian / VS Code / Dashboard / any MCP client
```

---

## 12. Risks and mitigations

| Risk | Severity | Mitigation |
|------|----------|------------|
| **MCP protocol does not support message interception** | 🔴 high | Phase 1 uses MCP Resource + session_start; Phase 2 implements the MCP Proxy |
| Cold start is hard | 🔴 high | Obsidian plugin as the funnel; MemPalace/Mem0 compatibility lowers migration cost |
| Router mis-injects (irrelevant memories) | 🟡 medium | conservative policy: fewer over more; users can disable auto-injection |
| Agents ignore the MCP contract | 🟡 medium | fallback modes: tool + resource + system prompt |
| Editing UX worse than Obsidian | 🟡 medium | don't clone an editor; focus on memory management |
| Sync complexity | 🟡 medium | local-first with file export as the escape hatch |
| Token explosion | 🟡 medium | hard token budget caps; summaries by default |
| **Local embedding latency** | 🟡 medium | native in-process inference; models are small and cached locally |
| **Compliance tracker can't see agent replies** | 🟡 medium | deferred to Phase 3 behind the MCP Proxy architecture |
| Privacy compliance | 🔴 high | local-first; encryption; GDPR red lines |
| Big-vendor copying | 🟢 low | open core; community moat; vertical experience |

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
| **Graph database integration** | introduce a graph DB (replacing/upgrading the `memory_relations` query path) only when relation scale outgrows single-table one-hop expansion | deferred, awaiting scale signal |
| **General world-knowledge base** | general commons stays with the model; MemVault only accumulates personal/project/org domain knowledge | not doing (design constraint) |
| **CRDTs multi-device sync** | offline collaboration across devices | not started |
| **Dual-track injection (compressed summary + verbatim retrieval)** | split the injection budget into a continuously maintained fixed-size "user/project state summary" plus on-demand full-text retrieval. Rationale: the Qwen3.8-Flash-Next report (§2.1.1) demonstrates that compressed state and verbatim retrieval are complementary. Requires new memory shapes (aggregate summary generation/maintenance pipeline), beyond current scope | not started (long-term) |
| **Plugin marketplace** | publishing/operations for VS Code / Obsidian / dsh plugins | not started |

### Unimplemented candidates from the claude-obsidian review

> An item-by-item code review of
> [AgriciDaniel/claude-obsidian](https://github.com/AgriciDaniel/claude-obsidian)
> was completed and archived; the implemented items (P0 injection-safe
> wrapping / P1 evidence links and evidence-driven decay / P1.5
> `memvault doctor`) are in the CHANGELOG. The remaining candidates below are
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

**Design**: the `memvault-core::bench` module + `memvault bench` command.
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

> This document is maintained as the product/architecture reference for
> v0.3. Subsequent evolution beyond it lives in §16 (longer-term plans).
