# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Release-channel verification (`scripts/verify-release-channels.py`)** — `release.yml` gained a `verify-release` job that reads every release-locked channel back over the public network once the publish jobs finish and fails the run on any mismatch: the GitHub Release is published (not a draft) and carries the four platform archives plus `SHA256SUMS`, all four crates are on crates.io, the Homebrew tap pins the tag, `ghcr.io/dreamor/memvault:<tag>` and its Docker Hub mirror resolve, and the `io.github.dreamor/memvault` listing is `active` **and** the ghcr tag it advertises still resolves. "Released" now means "verified released" instead of "the push step exited 0".
- **Daily release-drift check (`.github/workflows/verify-release-drift.yml`)** — runs the same assertions against the latest GitHub Release at 02:47 UTC and opens (or comments on) a `Release channel drift detected` issue when a channel stops serving it, so a formula rollback, a deleted image tag or a retired listing does not sit unnoticed the way a `0.3.0` listing advertising a deleted ghcr tag did.
- **`retire_version` mode for the MCP Registry workflow** — `mcp-publisher status --status deleted` is the only supported way to take down a listing whose image tag no longer exists (the registry rejects a repoint to an unlabelled image and has no DELETE endpoint).

### Changed

- **The MCP Registry listing is published by the release run itself** — `release.yml` now calls `publish-mcp-registry.yml` as a `publish-mcp` job, so a tag release no longer depends on someone remembering to re-publish the listing by hand. The manual workflow is kept for repairs (repoint / retire).
- **Silent channel skips are now explicit failures** — `release.yml`'s Docker Hub mirror and Homebrew steps used to warn and stay green when `DOCKERHUB_*` / `HOMEBREW_TAP_TOKEN` were unset, so a release could quietly ship GHCR-only or a stale formula while the run said "success". Both now fail with an actionable message, and the opt-out is the explicit `publish-dockerhub = false` / `publish-homebrew = false` dispatch input.
- **`verify-release-channels.py` is scoped to the release-locked channels** — GitHub Releases, crates.io, Homebrew, ghcr, Docker Hub and the MCP Registry. The npm dsh plugin and the Obsidian plugin release on their own schedule, so they are deliberately excluded from both the per-release and the daily check.

## [0.5.0] - 2026-10-16

### Added

- **Markdown storage backend (`MEMVAULT_STORAGE_BACKEND=markdown`)** — an alternative to the default SQLite store where a directory of human/git/Obsidian-editable `{id}.md` files is canonical, with an internal SQLite cache (FTS/embeddings/episodes/skills/relations) kept eventually consistent and self-healing via `memvault-cli reindex` (or `reindex --watch` for continuous polling). `memvault-cli migrate-to-markdown <dir>` does a one-time copy from the active store, carrying embeddings over. Checkpoints/restore/backup stay SQLite-only for now and fail with a clear error on the markdown backend. Select the directory with `--markdown-dir` / `MEMVAULT_MARKDOWN_DIR`.
- **`memvault-cli export-skill`** — export one skill (`--id`) or every skill in scope (`--namespace`/`--all`) as a shareable SOP Markdown file/pack, the inverse of `import-skills` (round-trips back through it). The underlying Markdown codec (`io::Exporter`/`Importer`) now losslessly round-trips every `Memory` field, not just the human-facing ones, as a prerequisite for both this and the markdown storage backend.
- **`memvault-cli bench` reports a success-rate weighted token-savings estimate** — alongside the existing retrieval/injection/judge layers, `--judge` runs now compare the expected token cost to reach one passing plan with vs. without memory (cost-per-attempt ÷ pass-rate), surfaced as `avg_tokens_without`/`avg_tokens_with`/`token_savings_pct` in the report and CLI output.

## [0.4.5] - 2026-10-16

### Fixed

- **Codex actually loads the plugin hooks again** — the bundle shipped `plugins/memvault/plugin.json` (an agent-plugins.org manifest) *and* `hooks/hooks.json`. When a plugin root carries a `plugin.json`, Codex treats that manifest as authoritative and never enumerates the plugin's hooks: MCP servers and skills still loaded, so the plugin looked healthy, but the `SessionStart` recall and `Stop` extraction hooks were never registered — and a hook that is never registered can never be trusted or run, which is why sessions started with no recalled memory and no stderr warning. The bundle now ships the single `.claude-plugin/plugin.json` manifest that both Claude Code and Codex read, verified end to end against Codex 0.162 (`hooks/list` reports the two plugin hooks; `codex exec` executes them). `plugins/memvault/tests/run-tests.sh` and CI's `agent-plugins` job now fail if a root `plugin.json` is re-added, and the Codex docs gained the hook-trust step ("hooks need review before they can run" until the `SessionStart`/`Stop` entries are approved in `/hooks`).

## [0.4.4] - 2026-10-16

### Added

- **Release-time version gate (`scripts/check-release-versions.py`)** — one script now holds the list of every version-bearing file (the Cargo workspace plus the three `memvault-core` path dependencies, the Claude/Codex, Gemini, Qoder and pi manifests, `dsh-plugin` with its lockfile, `dashboard/package.json`, `integrations/hermes/plugin.yaml`, the MCP-registry listing *including* the ghcr image tag it advertises, and the `--version` output printed in the READMEs). `release.yml` runs it as a `verify-version` job that a tag push must pass before anything is built, published or pushed, and CI runs the same check on every build — so a mixed-version release can no longer be cut, which is how `dashboard/package.json` stayed at 0.4.0 through the 0.4.2/0.4.3 releases and how 0.4.1 escaped to npm and a stray image without ever being tagged.

### Changed

- **Docker image tags are always `v<version>`** — `rebuild-docker.yml` used to default to a free-form tag (`0.3.0`), which is where the registries' only bare tag came from, and it moved `:latest` on every run. It now requires an explicit released version, normalizes the `v` prefix, and only moves `:latest` when asked; `release.yml` refuses to tag or push an image unless it runs on a `v<major>.<minor>.<patch>` tag.

### Fixed

- **Bare `memvault` command in docs and generated files** — no distribution channel ever shipped a `memvault` binary (crates.io, GitHub release archives, the Homebrew formula, `install.sh` and `install.ps1` all provide `memvault-cli` / `memvault-mcp` / `memvault-proxy`), yet the README quick start, `docs/`, plugin skills/commands/rules, the generated instruction-file copies (`integrations/rules/`, `.qoder/rules/`, `skills/`, `.openclaw/skills/`), `CONTRIBUTING.md`, `.env.example`, the installer closing hints, and the CLI's own help/error text all told users to run `memvault <subcommand>`. All corrected to `memvault-cli`, and the stale installer hint claiming a "`memvault` wrapper name works when `~/.memvault/bin` is in PATH" was dropped (no wrapper is installed).
- **`dashboard/package.json` was stuck at 0.4.0** while its archive shipped as `memvault-dashboard-v0.4.3.tar.gz`; it now tracks the release version and is covered by the version gate above.

## [0.4.3] - 2026-10-15

### Fixed

- **Hook failure warnings report the CLI's real exit code** — `session-start.sh` read `rc=$?` *after* the `if` compound, whose status is 0 whenever the branch simply did not match, so every `session-start failed (rc=…)` line printed `rc=0` even for a CLI that exited 2 (a stale binary rejecting `--format hook-json`) or 3. The capture moved into the `else` branch, where `$?` is the failed command's status, and both hook tests now assert the reported code instead of only "something went wrong". The 0.4.2 warning line named the failing binary but its exit code was useless for triage.

- **Tag pushes actually publish to crates.io, npm and Homebrew** — each of those jobs was gated on `inputs.publish-crates != false` (and the npm/Homebrew twins). On a tag push `inputs` is empty, and `null != false` coerces to `false`, so the jobs were silently **skipped**: the `v0.4.3` tag run published the GitHub Release and the Docker image, then stopped before any package channel. The gate is now `github.event_name != 'workflow_dispatch' || inputs.publish-<channel> != false`, so tag pushes always publish and a dispatched `publish-only` run can still opt out per channel.

### Changed

- **MCP Registry listing publishes from CI** — new `Publish MCP Registry listing` workflow (`mcp-publisher login github-oidc`, no stored secret). A laptop-side `mcp-publisher login github` JWT expires and fails the listing with `401 ... token has invalid claims: token is expired`, which is exactly what happened when re-publishing `v0.4.2`. The listing's `packages[0].identifier` now points at the release tag's image (`ghcr.io/dreamor/memvault:v0.4.3`) — the bare `0.4.x` tags the listing used before are only created by hand via `rebuild-docker.yml`, so the registry answered `400 ... OCI image does not exist` and the published listing had still been advertising a `0.3.0` image.

## [0.4.2] - 2026-10-15

### Added

- **Codex runs under its own agent identity** — the SessionStart/Stop hooks detect the host from the environment and default to `codex` under Codex (`claude-code` elsewhere), and `agents.example.yaml` ships a matching `codex` profile with `namespace_filter: ["global", "project:*"]`. Previously Codex hooks ran as `claude-code`, sharing that injection profile; naively pinning the id without a profile would have fallen back to `default` and silently dropped project-scoped memories. Override with `MEMVAULT_AGENT_ID`.
- **Hook failures post one diagnostic line to stderr** — a missing CLI, an unreachable REST backend or a non-zero CLI exit now prints one actionable line (`session-start failed (rc=…, bin=…): <CLI's last stderr line>`) instead of writing only to the debug log, so a stale binary or a dead backend is visible without `MEMVAULT_HOOK_DEBUG=1`. stdout stays reserved for the hook envelope and every path still exits 0.
- **`memvault-mcp --version`** — the MCP/REST server now reports its own version like `memvault-cli` does, so an operator can tell which build a host actually resolves.

### Changed

- **Plugin bundle version aligned with the product release** — `plugin.json`, `plugins/memvault/plugin.json` and `plugins/memvault/.claude-plugin/plugin.json` moved from `0.3.0` to `0.4.2`, and the Gemini, Qoder, Hermes and MCP-registry manifests plus the README/INSTALL version references follow, so the marketplace bundle no longer lags the shipped binaries.
- **Release workflow uploads binary assets directly to GitHub Release** — `build-binaries` and `dashboard-web` now publish archives straight to the (draft) release instead of Action artifacts, so build artifacts no longer consume Actions storage. The cumulative `SHA256SUMS` installer checksum file is still assembled from the per-archive checksums, and the draft is published only after every platform has uploaded.

## [0.4.1] - 2026-10-14 — withdrawn (partial release)

> **Never shipped as a release.** There is no `v0.4.1` git tag, GitHub Release,
> crates.io publish or Homebrew formula. Two things did escape: the dsh plugin
> (`@dreamor/dsh-memvault@0.4.1` — npm is append-only) and an image tag
> `ghcr.io/dreamor/memvault:v0.4.1` plus its Docker Hub mirror, pushed with no
> release behind it. Its content shipped in 0.4.2, so nothing here is worth
> installing — use 0.4.2 or newer and do not pin `:v0.4.1`. The version gate
> added in 0.4.4 is what makes this class of half-release impossible to repeat.

### Changed

- **DSH plugin verifies against DSH 0.2** — dsh-plugin development types now target `@deepseek-ai/dsh-*@0.2.0-rc.2` (Cordis 4.0.4); wildcard peer ranges preserve compatibility with DSH 0.1.
## [0.4.0] - 2026-09-11

### Added

- **Web Dashboard embedded in the `memvault-mcp` binary** — rust-embed bakes `crates/memvault-mcp/assets/web` (synced from `dashboard/dist` via `scripts/sync-dashboard-assets.sh`, CI-checked for drift) into the binary, so every channel — `cargo install`, Homebrew, Release binaries, install scripts, Docker — serves the UI with `--transport http` and zero extra setup. `--serve-web <dir>` / `MEMVAULT_SERVE_WEB=<dir>` override the embedded assets (flag wins). The Docker image rebuilds `dist/` fresh in a `web` stage before baking; the `memvault-dashboard-<tag>.tar.gz` Release archive remains for installs that don't upgrade the binary.
- **Web Dashboard i18n and light theme** — language switcher (English/中文, defaults to the system language, persisted, applied before first paint to avoid flash) and dark/light theme switcher. All static UI copy is backed by a bilingual dictionary; memory data itself stays untranslated.
- **`.env` configuration layer** — all binaries load `~/.memvault/.env` (or `$MEMVAULT_HOME/.env`) at startup. Precedence: CLI flag > process env > `.env` file > built-in defaults; `--env-file` / `MEMVAULT_ENV_FILE` select an alternative file; a missing file is skipped silently, so zero-config still works. `memvault-cli status` prints per-key provenance (`env` / `file` / `default`, API keys masked). `.env.example` is the canonical template covering every key.
- **Agent-native plugin registration** — first-class integrations beyond raw MCP config:
  - Claude Code plugin (`/plugin marketplace add dreamor/memvault`): SessionStart auto-injection, optional Stop-hook extraction (`MEMVAULT_HOOK_EXTRACT=1`), bundled stdio MCP server, skills and slash commands.
  - OpenCode plugin, Gemini CLI extension, and a Codex plugin served by a repo marketplace (`codex plugin marketplace add dreamor/memvault`): portable `plugin.json` + `mcp.json` live next to the Claude layout, hooks need a one-time `/hooks` trust review in Codex, and the config.toml three-step stays as a fallback.
  - Rule snippets for 8 MCP clients plus adapters for Hermes, pi, Qoder, Grok Build and OpenClaw/Swival, guarded by CI parity checks and release-time manifest validation.
- **Codex rollout transcripts are parsed natively** — hook ingestion recognizes Codex's `response_item` message rows alongside Claude Code's shape: developer app-context rows, tool calls, and UI event duplicates stay out of the extraction payload, and watermark-based incremental ingestion works on Codex too. Previously unknown formats fell back to noisy raw-tail pass-through.
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
- `memvault-cli status` no longer misreports native embedding failures as "none configured".
- **Claude Code plugin loads on current Claude Code** — the plugin manifest declared `"hooks": "./hooks/hooks.json"`, but Claude Code also loads that file by convention, and its duplicate-hook-file detection failed the whole plugin (observed on 2.1.199). The manifest now relies on the auto-load convention, and its version is aligned with the workspace (`0.3.0`).
- **`MEMVAULT_DB` / `MEMVAULT_HOME` are actually honored** — `memvault-cli` and `memvault-mcp` had the database path baked into the `--db` clap default, so the documented env and `.env` layers never applied. All three binaries now resolve the db the way `.env.example` documents: CLI flag > `MEMVAULT_DB` > `$MEMVAULT_HOME/data.db` > `~/.memvault/data.db`. The marketplace description also says 18 tools now (the MCP server's actual count).
- **Rule-based extraction reads sweeping Chinese phrasing** — cues match by contiguous substring, so "以后所有脚本都用 X" (所有 breaks 以后都) classified as no-signal and Chinese sessions yielded near-zero rule-based extractions. The shorter sweeping "都用" cue plus explicit save requests ("请记住:…", prefix stripped from the generated instruction) now extract as review-inbox drafts.

## [0.3.0] — 2026-09-07

### Added

- **Delta writes on save** — every save runs same-namespace dedup/merge first: near-duplicates (similarity > 0.95) are skipped outright, otherwise the new content's residual is appended to the existing memory and re-embedded, with priority and tags monotonically upgraded. Skills always insert directly. Switch off with `MEMVAULT_DELTA_WRITE=off`. Keeps the store from growing without bound.
- **Two-phase proxy injection** — a synchronous, zero-embedding deterministic pass (MUST rules) is served immediately; the full semantic pipeline runs in the background and replaces the state when it lands. Callers may wait up to 250ms for the fuller result. Injection latency is never hostage to embedding.
- **Conversation n-gram retrieval** — retrieval keys weight the most recent conversation turns by recency instead of relying on a single-line flat context (both the proxy injection engine and explicit session entry points).
- **Injection channel deduplication** — `AgentProfile.inject_channel` (`mcp` / `proxy` / `sync`) pins each agent to one canonical injection path, so the same memory is no longer delivered three times through MCP, proxy and sync files.
- **Task-level benchmark `memvault-cli bench`** — evaluates memory value against your own outcome history, in three layers: were lessons retrieved, were they actually injected (with a cost estimate), and optionally an LLM judge comparing with/without-memory answers to the same task.
- **Cross-agent cold-start import** — `memvault-cli import-agent` with adapters for Claude Code, Codex, Hermes, Qoder and OpenClaw, plus a `--paste` fallback for any other agent. Imported candidates are forced to `REFERENCE` priority and enter the review inbox unless `--approve`.
- **Episodic memory** — `record_outcome` (MCP / CLI / REST), automatic lesson reflection on failures (LLM-first, conservative rule fallback), lessons recalled by task type into injections, and MUST-upgrade hints after repeated failures.
- **Procedural (skill) memory** — trigger-based skill injection with per-session quotas, success-rate tracking, version bumping with `needs-revision` flags, and auto-drafted skills after repeated successes of the same task type.
- **Semantic memory** — relation triples (`supports` / `contradicts` / `sourced_from`), LLM relation extraction, consolidation during `promote`, fact supersede with rollback, and one-hop relation expansion in search and injection.
- **Shared team memory pool** — a `visibility` field (`scoped`/`shared`) so `shared` memories can be injected into any namespace session; SOP skill import (`memvault-cli import-skills` / MCP `import_skills`); per-type Obsidian folder sync.
- **Evidence-driven forgetting** — active contradictions accelerate decay 3×; `add_evidence` records supports/contradicts/source relations between memories.
- **Memory hygiene audit `memvault-cli doctor`** — seven read-only, deterministic, offline checks: dangling superseded/lesson pointers, stale memories, active contradictions, near-duplicates, pending review, flagged skills.
- **Web Dashboard phases 1–4** — extract-from-text pane, search mode switching with relation expansion, System tab (capabilities/metrics/doctor), Data tab (export/import/backup/checkpoints/SOP import/cross-agent import), read-only Agents tab.
- **Personal-memory-inspired refinements** — type-aware decay stability (skills/preferences persist longer, episodes fade faster), explicit conflict surfacing in injection (`[MEMORY CONFLICT]` block left to the agent to resolve), intent-aware retrieval boosts (Coding/Writing/Design/Research/Project), and a lightweight `MEMORY-INDEX.md` produced by `memvault-cli sync`.

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
- **Embedding auto-backfill** — memories missing embeddings get them generated asynchronously on session start; `memvault-cli sync --watch` regenerates instruction files on DB change.

## 0.1.0 — 2026-08-09

### Added

- Initial release:
  - `memvault-core` (12 modules: storage, router, intent, embedding, hybrid search, extractor, dedup, decay, io, models, config, error).
  - `memvault-mcp` MCP server (stdio, 8 tools + 2 resources), `memvault-cli` (11 subcommands), web dashboard, VS Code extension and Obsidian plugin.
  - Hybrid retrieval (keyword + vector via RRF), per-agent registry with namespace/tag filtering, MUST/REF injection formats, dedup/decay/archive pipeline, and multi-agent shared storage (SQLite + LanceDB).

[Unreleased]: https://github.com/dreamor/memvault/compare/v0.4.4...HEAD
[0.4.4]: https://github.com/dreamor/memvault/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/dreamor/memvault/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/dreamor/memvault/compare/v0.4.0...v0.4.2
[0.4.1]: https://www.npmjs.com/package/@dreamor/dsh-memvault/v/0.4.1
[0.4.0]: https://github.com/dreamor/memvault/releases/tag/v0.4.0
[0.3.0]: https://github.com/dreamor/memvault/releases/tag/v0.3.0
