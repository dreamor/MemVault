# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Web Dashboard i18n and light theme** — language switcher (English/中文, defaults to the system language, persisted, applied before first paint to avoid flash) and dark/light theme switcher. All static UI copy is backed by a bilingual dictionary; memory data itself stays untranslated.
- **`.env` configuration layer** — all binaries load `~/.memvault/.env` (or `$MEMVAULT_HOME/.env`) at startup. Precedence: CLI flag > process env > `.env` file > built-in defaults; `--env-file` / `MEMVAULT_ENV_FILE` select an alternative file; a missing file is skipped silently, so zero-config still works. `memvault status` prints per-key provenance (`env` / `file` / `default`, API keys masked). `.env.example` is the canonical template covering every key.
- **Agent-native plugin registration** — first-class integrations beyond raw MCP config:
  - Claude Code plugin (`/plugin marketplace add dreamor/memvault`): SessionStart auto-injection, optional Stop-hook extraction (`MEMVAULT_HOOK_EXTRACT=1`), bundled stdio MCP server, skills and slash commands.
  - OpenCode plugin, Codex integration (MCP config + `memvault sync` AGENTS.md + custom prompts), Gemini CLI extension.
  - Rule snippets for 8 MCP clients plus adapters for Hermes, pi, Qoder, Grok Build and OpenClaw/Swival, guarded by CI parity checks and release-time manifest validation.
- **MUST memory poisoning defenses** (off by default, backward compatible) — writes are tagged `identity_verified` when the agent authenticated with a registered API key, and merges accumulate distinct verified `corroborating_agents`. With `MEMVAULT_CORROBORATION_GATE=on`, a MUST memory counts as trusted only when independently written by at least `MEMVAULT_CORROBORATION_MIN_AGENTS` (default 2) verified agents — a single prompt-injection-hijacked agent can no longer pass off instructions as trusted by self-reporting.

### Changed

- **TLS switched to pure rustls** — workspace `reqwest` uses reqwest 0.13's `rustls` feature (rustls-platform-verifier keeps reading the system certificate store, so admin-installed enterprise CAs still work) and fastembed uses `ort-download-binaries-rustls-tls` / `hf-hub-rustls-tls`. Prebuilt Linux binaries no longer link `libssl.so.3` / `libcrypto.so.3`, removing the OpenSSL 3 requirement from the prebuilt install path on older distros.
- **Obsidian plugin releases moved to the dedicated `dreamor/memvault-obsidian` repo** — `obsidian-plugin/` here stays the source of truth for code; pushes auto-sync it and bumping `manifest.json`'s version releases the plugin there automatically. MemVault releases no longer attach the plugin zip / individual plugin assets.
- **Configuration naming cleaned up** (one-time, no compatibility shims): the embedding endpoint now only reads `MEMVAULT_EMBEDDING_API_BASE` — `OPENAI_API_BASE` is gone, so leaked env vars from unrelated tools can no longer hijack endpoint inference. `OPENAI_API_KEY` remains solely as a fallback API-key alias. All boolean config keys go through a single parser. Per-host agent identity variables intentionally stay out of `.env`.
- **Web Dashboard redesigned** — dark slate technical design system: Fira Sans / Fira Code, glass sticky header, unified radii and elevation, focus-visible rings, `prefers-reduced-motion` fallbacks, responsive layout.

### Removed

- **VS Code extension deleted; Obsidian plugin slimmed** to vault sync + browse/search/selection capture. Editor-side management (review inbox, dedup/decay/promote, export/import, checkpoints) lives in the Web Dashboard and CLI.

### Fixed

- `agents.yaml` API keys were hashed twice, so keyed-agent authentication always failed.
- Re-importing your own export no longer fails on duplicate ids; imports report skipped entries instead.
- CLI `session-start` now honors `agent.inject_channel`, matching REST/MCP injection deduplication.
- CLI `export`/`import` accept directories and single files as their help text promises.
- `POST /api/promote` tolerates an empty request body.
- `memvault status` no longer misreports native embedding failures as "none configured".

## [0.3.0] — 2026-09-07

### Added

- **Delta writes on save** — every save runs same-namespace dedup/merge first: near-duplicates (similarity > 0.95) are skipped outright, otherwise the new content's residual is appended to the existing memory and re-embedded, with priority and tags monotonically upgraded. Skills always insert directly. Switch off with `MEMVAULT_DELTA_WRITE=off`. Keeps the store from growing without bound.
- **Two-phase proxy injection** — a synchronous, zero-embedding deterministic pass (MUST rules) is served immediately; the full semantic pipeline runs in the background and replaces the state when it lands. Callers may wait up to 250ms for the fuller result. Injection latency is never hostage to embedding.
- **Conversation n-gram retrieval** — retrieval keys weight the most recent conversation turns by recency instead of relying on a single-line flat context (both the proxy injection engine and explicit session entry points).
- **Injection channel deduplication** — `AgentProfile.inject_channel` (`mcp` / `proxy` / `sync`) pins each agent to one canonical injection path, so the same memory is no longer delivered three times through MCP, proxy and sync files.
- **Task-level benchmark `memvault bench`** — evaluates memory value against your own outcome history, in three layers: were lessons retrieved, were they actually injected (with a cost estimate), and optionally an LLM judge comparing with/without-memory answers to the same task.
- **Cross-agent cold-start import** — `memvault import-agent` with adapters for Claude Code, Codex, Hermes, Qoder and OpenClaw, plus a `--paste` fallback for any other agent. Imported candidates are forced to `REFERENCE` priority and enter the review inbox unless `--approve`.
- **Episodic memory** — `record_outcome` (MCP / CLI / REST), automatic lesson reflection on failures (LLM-first, conservative rule fallback), lessons recalled by task type into injections, and MUST-upgrade hints after repeated failures.
- **Procedural (skill) memory** — trigger-based skill injection with per-session quotas, success-rate tracking, version bumping with `needs-revision` flags, and auto-drafted skills after repeated successes of the same task type.
- **Semantic memory** — relation triples (`supports` / `contradicts` / `sourced_from`), LLM relation extraction, consolidation during `promote`, fact supersede with rollback, and one-hop relation expansion in search and injection.
- **Shared team memory pool** — a `visibility` field (`scoped`/`shared`) so `shared` memories can be injected into any namespace session; SOP skill import (`memvault import-skills` / MCP `import_skills`); per-type Obsidian folder sync.
- **Evidence-driven forgetting** — active contradictions accelerate decay 3×; `add_evidence` records supports/contradicts/source relations between memories.
- **Memory hygiene audit `memvault doctor`** — seven read-only, deterministic, offline checks: dangling superseded/lesson pointers, stale memories, active contradictions, near-duplicates, pending review, flagged skills.
- **Web Dashboard phases 1–4** — extract-from-text pane, search mode switching with relation expansion, System tab (capabilities/metrics/doctor), Data tab (export/import/backup/checkpoints/SOP import/cross-agent import), read-only Agents tab.
- **Personal-memory-inspired refinements** — type-aware decay stability (skills/preferences persist longer, episodes fade faster), explicit conflict surfacing in injection (`[MEMORY CONFLICT]` block left to the agent to resolve), intent-aware retrieval boosts (Coding/Writing/Design/Research/Project), and a lightweight `MEMORY-INDEX.md` produced by `memvault sync`.

### Changed

- **Web Dashboard replaces the Tauri desktop app** — `memvault-mcp --serve-web <dist>` serves the REST API and frontend on one port; Tauri was removed entirely.
- **Search upgraded to real FTS5** — CJK bigram tokenization (short Chinese terms actually match), a three-tier match fallback that never degrades silently, and FTS-syntax-safe user queries. Vector storage now uses per-row int8 quantization (~1/4 the size, cosine parity > 0.99).
- **LLM contextual extraction** — user + response context is extracted jointly via any OpenAI-compatible endpoint, with prompt-injection-guarded prompts; a local Ollama is auto-detected first and rule extraction remains the unchanged fallback.
- **Observability throughout** — hit-source tracing (`kw#2`/`vec#5`), injection skip-reason accounting, extraction coverage reporting, and schema checksum verification that fails closed on drift.
- **dsh plugin renamed** `@memvault/dsh-plugin` → `@dreamor/dsh-memvault`; existing profiles need a reinstall.

### Security

- **Injection safety wrapping** — trusted memories (human-created or human-reviewed) are injected as instructions; AI-generated, unreviewed memories (including MUST) are injected as reference data with explicit treat-as-data wrappers.
- Upgraded `h2` to 0.4.19 (RUSTSEC-2026-0258) and patched vulnerable transitive dependencies (`fast-uri`, `qs`).

### Fixed

- Proxy upstream forwarding was completely broken (the connection was dropped immediately).
- VS Code/Obsidian REST clients never unwrapped the API envelope, breaking essentially every call; `/api/search` and `/api/memories` response shapes fixed to match.
- `save_with_embedding` silently dropped `layer` and skill metadata.
- A configured `port` was always overridden by the CLI default.

## 0.2.0 — 2026-08-11

### Added

- **Layered injection** — MUST memories injected in full, references summarized to fit the token budget, with a "N more available via search_memory" hint; the proxy engine uses the same format.
- **Memory layers L0–L3** — raw/atom/scenario/persona derived automatically from priority, plus a `promote` pipeline that consolidates L1 memories into scenario summaries and promotes hot ones to MUST-level persona rules (sources archived to L0).
- **Structured skills** — `SkillMeta` (trigger / steps / verification / version) across CLI and MCP.
- **Extraction loop** — proxy responses auto-extracted into the review inbox via `notify_response`, whitelist-gated and rate-limited.
- **Agent authentication** — optional SHA-256 API keys per agent (`agents.yaml`), enforced across MCP/REST/proxy; unkeyed agents keep working.
- **Multi-signal reranking** — hybrid score, query overlap, recency, priority and access frequency; can be disabled for plain backward-compatible behavior.
- **Review inbox** — pending list with approve/reject/edit endpoints and an MCP `list_inbox` tool.
- **Compliance tracking endpoints** — per-session and per-agent aggregate follow-through reports tied to injection sessions.
- **MCP over SSE** — `--transport sse` for network clients.
- **Embedding auto-backfill** — memories missing embeddings get them generated asynchronously on session start; `memvault sync --watch` regenerates instruction files on DB change.

## 0.1.0 — 2026-08-09

### Added

- Initial release:
  - `memvault-core` (12 modules: storage, router, intent, embedding, hybrid search, extractor, dedup, decay, io, models, config, error).
  - `memvault-mcp` MCP server (stdio, 8 tools + 2 resources), `memvault-cli` (11 subcommands), web dashboard, VS Code extension and Obsidian plugin.
  - Hybrid retrieval (keyword + vector via RRF), per-agent registry with namespace/tag filtering, MUST/REF injection formats, dedup/decay/archive pipeline, and multi-agent shared storage (SQLite + LanceDB).

[Unreleased]: https://github.com/dreamor/memvault/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/dreamor/memvault/releases/tag/v0.3.0
