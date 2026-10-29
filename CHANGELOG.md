# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Changed

- **MCP tool definitions now advertise annotations and disclose their real
  side effects.** All 20 tools carry title/read-only/destructive/idempotent/
  closed-world hints, so clients can distinguish read-only lookups from
  mutations without parsing prose. The riskiest descriptions were rewritten:
  `delete_memory` now explains soft-delete vs `forget_memory`, cascade and CLI
  restore; `review_memory` now says edit also works on approved memories;
  `run_decay` discloses opt-in permanent auto-forget; and `search_memory` no
  longer documents `keyword` as the default when the implementation defaults
  to `hybrid`.

- **The README nav/tagline rows no longer leak literal `&nbsp;` outside
  GitHub.** Glama's Markdown renderer escapes HTML entities, so the entity
  text appeared verbatim in the directory listing; the English and Chinese
  READMEs now use plain `·` separators that render correctly everywhere.

- **The README badge rows now link to MemVault's Glama directory listing.**
  Both the English and Chinese READMEs show the live Glama score badge, so
  readers can verify the directory entry and jump straight to the listing.

- **The repository now declares its Glama maintainer metadata.** `glama.json`
  records `dreamor` as the listing maintainer, so the Glama claim and future
  build-spec edits remain tied to the project rather than an ad-hoc account
  association.

## [0.6.9] - 2026-10-29

### Changed

- **REST memory records now use the canonical serde wire casing — a small
  breaking change for exact-comparison consumers.** Every endpoint built on
  `memory_to_json` (list/search/update/inbox/history-restore), plus
  `/api/extract` and the agent-import candidates, serialized `priority`,
  `type` and `layer` with Rust `Debug` formatting (`"Must"`, `"Fact"`) — a
  REST-only dialect diverging from MCP tool output, persisted payloads and
  the models' serde format (`"MUST"`, `"fact"`), and the root cause of the
  v0.6.8 dashboard MUST-gate miss. Responses now carry the serde casing
  everywhere (`"MUST"` / `"REFERENCE"` / `"BACKGROUND"`, `"preference"` /
  `"fact"` / `"episode"` / `"entity"` / `"skill"`, `"L0"`–`"L3"` unchanged).
  Request parsing was and remains case-insensitive, so only clients that
  compare RESPONSE strings case-sensitively against the old Debug casing
  need updating.

- **The MCP Registry listing now carries discovery metadata.** `integrations/mcp-registry/server.json`
  gained `repository`, `websiteUrl`, and `icons`, and its description now leads with
  searchable capabilities ("local-first memory for AI agents: save, recall, dedupe,
  and share across MCP clients") instead of the internal "shared memory router"
  phrasing. Directory cards and registry search results no longer render as a
  bare name with no source link or icon.

- **The Claude Code / Codex plugin listing now leads with the product, not the
  mechanism.** The marketplace and manifest cards say what MemVault is
  (local-first memory shared across agents) and what it replaces (per-session
  amnesia), while the plugin README opens with a recorded command demo, a
  two-minute first-run path, and an explicit read/write/opt-out data section.
  `/memvault-doctor` now checks CLI/plugin version alignment, the MCP stdio
  handshake, `/api/health` identity, database status, hooks, and embeddings,
  and returns one executable fix per failure.

- **The Obsidian plugin's settings tab is now declarative (v0.3.5).**
  `getSettingDefinitions()` describes every setting, so they become findable in
  Obsidian's settings search on 1.13.0+; `display()` stays as the fallback for
  older apps, which is the pattern the Obsidian docs prescribe for a
  `minAppVersion` below 1.13.0 — the framework renders declarations *instead of*
  `display()` whenever the array is non-empty, so the two paths never overlap.
  Declarative `control` bindings write straight to `this.plugin.settings`, which
  drops the per-field `onChange` + `saveSettings` plumbing.

- **The Obsidian plugin now defaults to the backend the installers actually
  run (v0.3.4).** The community plugin previously shipped with
  `http://127.0.0.1:8080`, while `memvault-cli dashboard` and the installers use
  port `3777`, so a fresh install could not connect without manual settings.
  The default is now `3777`; **Detect server** probes `3777` first and the
  legacy `8080` port second, accepting only a `/api/health` response that
  identifies `memvault-mcp`. Loading settings never rewrites an existing URL.
  The sidebar now shows the exact `memvault-cli dashboard` recovery command on
  connection failure, and the manifest is honestly marked desktop-only because
  a phone cannot reach the user's loopback backend.

### Fixed

- **The Docker smoke job no longer fails on a closed pipe.** It piped
  `memvault-cli list` straight into `grep -q`, which exits at the first match
  and closes the pipe; under `set -o pipefail` the CLI's remaining stdout
  writes failed with `write /dev/stdout: broken pipe` and a passing smoke test
  went red (seen on PR #113's first run). The list output is now written to a
  file and grepped afterwards.

- **REST `/api/session` now carries the shared memory-conflict warning.**
  The third entry point with the CLI/MCP conflict-parity gap: when the
  injected set contains an active `contradicts` pair, the response's
  `formatted` text (both the JSON field and the `?output=plain` hook path)
  now ends with the same byte-identical `[MEMORY CONFLICT - 需要你决定]`
  block the MCP tool and CLI `session-start` emit, and the JSON response
  additionally exposes the structured pairs as `conflicts`
  (`[{memory_id, conflicting_with}]`, empty in the common case). Detection
  and rendering reuse the single shared implementation
  (`MemoryRouter::collect_conflicts` + the shared block renderer), so the
  three entry points cannot drift apart again.

- **Dashboard: the Agent-injection preview no longer renders "Nothing to
  inject" for non-empty previews, the MUST forget gate works again, and the
  Settings tab is clickable at 1280–1440 px.** Three fixes from the v0.6.8
  dashboard test pass. (1) REST `/api/session` never sent the structured
  `results` list the preview reads — only `count`/`formatted` — so a
  `count=8` preview showed as empty; the endpoint now returns the same
  per-result shape as `/api/search`, and the UI additionally falls back to
  rendering `formatted` verbatim (it is exactly what the agent would
  receive) whenever `results` is unavailable. (2) The forget modal's
  extra-confirmation gate compared `priority === "MUST"` while REST
  serializes priority in Debug case (`"Must"`), so for a real MUST memory
  the override checkbox never appeared and typing the ID alone enabled the
  delete button; the comparison is now case-insensitive (server-side MUST
  protection was never affected). (3) The header tab row had no overflow
  handling above the 900 px breakpoint, so with wider fallback fonts the
  overflowing tabs slid under the later header controls and the language
  toggle covered the rightmost tab (Settings) at 1280/1440 px; `.tabs` now
  scrolls horizontally at every width.

- **CLI `session-start` now emits the same memory-conflict warning as MCP
  `session_start`.** The `[MEMORY CONFLICT - 需要你决定]` block (with the
  "不要自动二选一" instruction) was rendered only by the MCP layered
  formatter; the CLI's plain path (`session_start` + `format_as_instructions`)
  skipped conflict detection entirely, so the plugin SessionStart hook —
  which prefers the CLI binary and returns as soon as it succeeds — injected
  contradicting reviewed MUST memories with no hint that the system already
  knows they conflict. Both entry points now share ONE detection + rendering
  implementation: `MemoryRouter::conflict_warnings` (over
  `evidence::contradictions_among`) plus a single `format_conflict_block`
  used by the layered renderer too, so the warning is byte-identical in MCP
  output, CLI plain output, and the `--format hook-json` envelope's
  `additionalContext` (stdout stays exactly one valid JSON envelope).
  Detection remains scoped to active `contradicts` relations whose BOTH
  ends are actually injected: supports-only never warns, a bidirectional
  relation renders one bullet, and memories excluded by review gating, L0
  archive status, or namespace scope cannot leak through the warning.

- **Obsidian plugin releases now carry a bare version tag, so the community
  store actually serves them.** `scripts/sync-obsidian-plugin.sh` pushed
  `v<version>` into `dreamor/memvault-obsidian`, while every release there so
  far (0.3.0–0.3.3) was tagged with the bare version. Obsidian resolves an
  installable release by a tag *identical* to `manifest.json`'s version, so
  0.3.4 was built, attested, and green in CI — and yet invisible to the store,
  which kept offering 0.3.3. The sync now pushes the bare version, the target
  repo's release workflow no longer strips a `v` prefix (that normalization is
  what let the wrong tag pass), and 0.3.4 was re-released under the correct tag.

- **Obsidian plugin 0.3.5 clears two review-scorecard findings and honours
  "0 to disable" for auto-refresh.** The `Detect MemVault server` command is now
  `Detect server`, since the palette already prefixes the plugin name; the
  delete-confirmation button prefers `setDestructive()` (1.13.0+) and keeps
  `setWarning()` only for apps that predate it; and the auto-refresh interval is
  a number field — the old text field ran the input through `parseInt(v) || 10`,
  so typing the documented `0` silently became 10 seconds instead of disabling
  the timer.

- **Obsidian plugin 0.3.6 stops calling an API newer than its `minAppVersion`,
  and CI now lints with the community-directory rules instead of finding out
  after release.** 0.3.5 preferred `ButtonComponent.setDestructive()` behind a
  runtime `typeof` guard, but the review's `obsidianmd/no-unsupported-api` rule
  is *static*: it flagged `src/main.ts` as requiring Obsidian 1.13.0 while
  `minAppVersion` is 1.7.2, turning an informational deprecation note into a
  release error. The API available at 1.7.2 (`setWarning()`) is back — its
  deprecation note is the accepted cost of supporting Obsidian below 1.13.0.
  `obsidian-plugin/` now carries the same `eslint-plugin-obsidianmd` config the
  review uses, the `Obsidian plugin` CI job runs it on every PR, and the config
  is synced to the release repo so `npm run lint` reproduces the review locally.
  A runtime guard is not an acceptable substitute for `minAppVersion`.

### Security

- **The last open Dependabot alert (moment, GHSA-4p3w-j4w9-5jqw) is closed
  with an npm override.** `eslint-plugin-obsidianmd@0.4.2` pins
  `obsidian@1.12.3`, which pins `moment@2.29.4` — inside the
  `>= 2.29.2, < 2.31.0` range for the `moment.locale()` path-traversal
  bypass (CVE-2026-17495). That copy is lint-time only: `obsidian` is
  externalized from the esbuild bundle, nothing in `src/` imports `moment`,
  and the advisory only affects server-side callers that pass a non-string
  locale. `obsidian-plugin/package.json` now overrides `moment` to `2.31.0`,
  so the lockfile holds exactly one deduped copy under both `obsidian`
  trees; `typecheck`, `lint`, `build` and the 39 plugin tests are unchanged.

## [0.6.8] - 2026-10-28

### Fixed

- **`forget_memory` now rejects an unknown `memory_type` instead of
  silently widening the purge.** An unrecognized value (a typo, or a
  model-hallucinated enum) parsed to "no type filter", so a bulk
  permanent-erase scoped to one memory type matched EVERY type — on an
  irreversible hard-delete path, with `dry_run` and execute sharing the
  same parsing. MUST protection still applied (it is an independent
  predicate), so the blast radius was "every non-MUST type in scope"
  rather than the whole store. Absent `memory_type` still legitimately
  means "all types"; a present-but-unknown value now returns
  `invalid_params` (-32602) before any delete plan is built, and
  case/whitespace noise around a valid value (`" FACT "`) is normalized
  instead of falling into the same trap.

- **Archived L0 rows no longer re-enter live context through the
  injection body, the deterministic fast path, or MCP resources.** The
  L0 exclusion added in 0.6.7 covered the overflow channel and the
  lessons/skills matchers only. A MUST-priority memory demoted to L0 was
  still pulled into `session_start` by the MUST-completeness union (MUST
  is exempt from the score floor and count caps, and the strict gate
  only checks review state — so it leaked in all four gate × review
  combinations, landing in the `[MUST]` instruction block once
  reviewed), into `deterministic_injection`, and into
  `memory://user-profile` / `memory://project-context`. L0 is archive
  material: priority, review state, and gating are orthogonal to it. A
  single `is_archived` predicate now guards every exposure path — body
  selection, fast path, both resources, and overflow — so the rule
  cannot drift apart again.

- **The proxy now actually exposes and forwards upstream tools.**
  `tools/list` and `tools/call` were the macro-generated local-only
  implementations: `UpstreamManager`'s tool aggregation and forwarding
  existed but were never wired into the handler, so an upstream's tools
  were invisible and calling one returned "tool not found" — even though
  the proxy's own instructions promise "all upstream tools are available
  alongside MemVault memory tools". Both methods are now overridden:
  local tools are listed first and win name conflicts, upstream tools
  merge behind them, and calls route local-then-upstream with results
  and errors passed through. Regression covers the full client → proxy →
  upstream chain over the wire.

- **Markdown-backend CLI errors no longer leak the directory lock.**
  `extract` with no input called `process::exit(2)` while the store was
  open — a hard exit skips destructors, so `LockGuard`'s Drop never ran
  and a dead-PID `.memvault.lock` stayed behind, making every subsequent
  command on that store fail with "lock file already exists" until the
  file was removed by hand. Usage errors are now returned as a dedicated
  `UsageError` and mapped to exit code 2 in `main` after `run` has
  unwound and released the store; every other error keeps the previous
  Debug-print + exit-1 shape.

- **`import`/`export` with an unknown `--format` now fail with exit code
  2 instead of exiting 0.** The branch printed "Unknown format: …" to
  stdout and fell through to `Ok(())` (import even ran its backfill), so
  scripts keying on the exit code believed the operation happened. The
  notice now goes to stderr and the command fails with no side effects.

- **Multi-upstream `resources/list` and `prompts/list` no longer show
  duplicates.** The merge helpers only checked upstream items against
  the LOCAL key set and never registered accepted ones, so two upstreams
  serving the same URI/name listed it twice while `resources/read` /
  `prompts/get` stayed first-wins — the list advertised entries the
  router could never address separately. All three merges (tools,
  resources, prompts) now share one first-wins `merge_by_key` whose
  dedup matches the forwarding index exactly.

## [0.6.7] - 2026-10-27

### Fixed

- **The strict MUST gate now holds across every injection channel — and raw
  L0 evidence no longer rides the MCP overflow summaries.**
  `MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST=1` only filtered the injection
  BODY at render time, so MCP's layered `session_start` — which re-fetches a
  wider candidate set for the `[MORE - 摘要]` overflow channel — echoed the
  same sentence back into the model context. An isolated-store reproduction
  (ingest one user turn → distilled unreviewed MUST candidate + L0
  raw-evidence twin) showed the leaked row was the L0 evidence, which trace
  ingestion marks `human_reviewed = true` ("raw logs need no human review")
  — no trust-keyed gate can ever catch it. Overflow admission
  (`MemoryRouter::overflow_admits`) now excludes L0 rows on **every**
  deployment, matching the lessons/skills/episodes paths that always skipped
  them, and excludes untrusted MUST candidates when the gate is on. The gate
  itself moved from render time to SELECTION time: a gated candidate leaves
  `session_start` / `deterministic_injection` with an explicit
  `unreviewed-must-gated` skip reason (auditable like every other drop), and
  compliance `record_injection` no longer claims the model saw text the
  renderer hid. The render-time filter stays as defense in depth;
  `search_memory` still surfaces gated candidates on explicit recall.

- **REST `/api/session` no longer bypasses the unvetted-content downgrade.**
  The endpoint rendered through `agent_adapt::format_memories`, which had no
  notion of trust: an AI-extracted, unreviewed MUST reached hermes /
  opencode / pi-extension / hook REST-fallback contexts with full
  instruction force (`- ALWAYS: …`, `<memory priority="must">`, a bullet
  under `### Rules (MUST follow)`), bypassing both the `[未审核]`
  reference-data fix and `MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST`. All four
  inject formats now mirror the router body's trusted/unvetted split:
  unvetted content renders into an explicit reference-data section
  (`[未审核]` block / `<unvetted_reference>` / `- [unreviewed]` /
  `### Unreviewed`), never carries an instruction tag, and unreviewed MUST
  candidates are dropped entirely when the gate is on. Trusted (human-authored
  or human-approved) memories render exactly as before.

- **The layered overflow sweep no longer crosses namespace boundaries.**
  `session_start_layered`'s extended query — the wider re-fetch behind the
  `[MORE - 摘要]` summaries — resolved its namespace from the `project`
  argument alone, ignoring the agent profile's `namespace_filter`: with no
  project active it swept EVERY namespace in the store, so `trace:*` raw
  evidence and other projects' memories could surface as overflow summaries
  even for profiles that never allowed them (a global-only profile like
  copilot's saw project-scoped content). The sweep now resolves through the
  same `resolved_injection_namespace` as the body search, plus a `global`
  sweep when the session is project-scoped — mirroring the body's
  cross-namespace fallback, so budget-trimmed global candidates stay
  reachable as summaries — and merged candidates are score-ordered before
  the top-5 cut instead of concatenation-ordered.

- **A bumpy-but-clean session no longer distills a contentless lesson.**
  The outcome-inference rule floor records a `Partial` outcome for a session
  that had friction but ended cleanly, with `cause` set to its generic
  `session had friction before ending cleanly (retry×N, rejection×N,
  correction×N)` summary — counts, not any specific error or correction
  text. Lesson reflection's rule fallback treated that cause like any other
  and echoed it back verbatim as `Verify before retrying: session had
  friction before ending cleanly (...)`, so every such session (friction
  present, no LLM configured or the LLM declined) added one of these to the
  review inbox regardless of whether anything actually went wrong.
  `reflection::reflect_lesson` now recognizes this generic cause and skips
  reflection entirely — same treatment as "no cause at all" — without even
  calling an LLM, since neither path has case-specific evidence to turn into
  advice. The episode itself (and its `Partial` status, for `bench` and
  analytics) is still recorded; only the lesson is skipped.

## [0.6.6] - 2026-10-27

### Changed

- **`rmcp` bumped to 3.7** (`3.1.4` → `3.7.0`). The protocol types it
  renames land with it: the deprecated `rmcp::model::ServerInfo` alias is
  now `ServerConfig` (the old name collided with the protocol's
  `serverInfo` identity field), and an empty `#[tool_router]` must say so —
  the proxy's test-only fake upstream now declares `allow_empty`.

### Fixed

- **Codex attachment scaffolding no longer rides into the episode `task`.**
  The transcript layer already dropped the AGENTS.md / `<environment_context>`
  rows and unwrapped the `# Files mentioned by the user:` preamble, but the
  app appends its own bits to the human's *own* row: an
  `<image name=… path=…></image>` placeholder per attachment and a
  `Codex could not read the local image at …` notice when the file never
  reached disk. Those survived the unwrap, so a session whose first message
  carried a screenshot recorded a `task` like
  `看看这款产品怎么样？\nCodex could not read the local image at …` — and the
  same text went to extraction and the outcome judge. Both fragments are now cut
  out of user turns (the open/close tags are stripped per line, since the real
  rollout splits them across two), and a row left holding nothing but
  scaffolding contributes no turn at all. Verified against the actual
  `rollout-*.jsonl` files on this machine.

## [0.6.5] - 2026-10-27

### Added

- **`ACCESSIBILITY.md`** — an accessibility statement covering the Web
  Dashboard, CLI, documentation, and client plugins: our commitment and
  its limits (no formal WCAG audit), supported environments, the
  accessibility features already in place (`aria-label`s, landmarks,
  reduced-motion support, themed/i18n UI), the known gaps (no focus trap
  in dialogs, no ARIA `tablist`, unaudited contrast, partial
  localization), and how to report a barrier. Linked from the docs index
  in both `README.md` and `README.zh-CN.md`.

### Changed

- **The Docker build now uses Rust 1.99** (`rust:1.99-slim-trixie`, was
  `1.88`), matching the `stable` toolchain the CI Rust jobs already use. The
  `1.88` pin no longer compiled the dependency graph: `uuid` 1.29 needs
  rustc >= 1.89, and `fastembed` 7.x pulls `ort` 2.0.0-rc, which needs
  >= 1.92.

- **Config and command references resynced with the code** — the hand-written
  configuration tables in `README.md` / `README.zh-CN.md`, the generated env
  reference in `CONTRIBUTING.md`, and `.env.example` now document the hook
  outcome knobs (`MEMVAULT_HOOK_OUTCOME_MIN_FRICTION` /
  `MEMVAULT_HOOK_OUTCOME_MIN_CONFIDENCE`), `MEMVAULT_HOOK_DEBUG`,
  `MEMVAULT_ENV_FILE`, `MEMVAULT_SERVER_START_TIMEOUT`,
  `MEMVAULT_IMPORT_ROOTS`, the four `MEMVAULT_FORGET_*` auto-forget knobs, and
  `HF_ENDPOINT` — the values `crates/` actually reads but the reference tables
  had drifted past. `docs/RUNBOOK.md` regains the four REST routes its endpoint
  table was missing (`GET /api/health`, `POST /api/forget`,
  `GET /api/forgotten-log`, `GET /api/forget/config`), its subcommand count is
  corrected to 35, and the `README` CLI lists pick up `forget` / `forget-log` /
  `export-skill` / `backfill-embeddings` / `reindex` / `migrate-to-markdown`.
  `CONTRIBUTING.md` also states the branch-cleanup convention (delete the local
  **and** remote branch after a merge — this repository does not auto-delete
  head branches).

- **A master push now runs the whole CI matrix** — every job except
  `rust-core` was gated on `github.event_name == 'pull_request' ||
  github.event_name == 'workflow_dispatch'`, so a push run showed one green
  `rust-core` next to eight skipped siblings (`Detect changed paths`,
  `Coverage gate`, `Docker image (build + smoke)`, `Dashboard`, `Obsidian
  plugin`, `agent-plugins` ×2, `REST API smoke test`, `cargo-deny`) — visually
  indistinguishable from a partial or broken CI, and it meant the merge commit
  on master was never validated past fmt/clippy/`cargo test`. Jobs are no
  longer event-gated: the `paths:` filter still decides *whether* a push
  starts a run (a docs-only push still starts none), but once a run exists it
  runs everything, so a green master run means the whole matrix is green. The
  `Detect changed paths` job goes with it — `dorny/paths-filter`, its
  `pull-requests: read` permission, and the `needs: changes` edges — because
  its single `core` output was consumed by no job condition, so it only ever
  gated job *ordering* while making the run look filtered.

### Fixed

- **macOS Apple Silicon binaries link again.** The `fastembed` 7.5.0 bump
  pulled `ort` 2.0.0-rc.15, whose build script no longer emits the
  `clang_rt.osx` link on Apple targets; the ONNX Runtime prebuilt objects
  (`onnxruntime::coreml::util::CoreMLVersion`) still reference
  `___isPlatformVersionAtLeast`, so `aarch64-apple-darwin` failed at link
  time with `Undefined symbols for architecture arm64` (`release.yml`'s
  macOS job and `release-npm.yml`'s binary job both died there; the Linux CI
  matrix never exercises it). `fastembed` is now pinned to `=7.1.0`, the last
  7.x on `ort` 2.0.0-rc.13 — the exact `ort` that built the v0.6.4 macOS
  release — with `.github/dependabot.yml` holding the bump until upstream
  restores the Apple link.

- **One bumpy session no longer records an episode per `Stop`.** Episodes are
  deduplicated per session on `(task, status, cause)`, but the rule floor
  embeds the session's *cumulative* friction counters in the cause
  (`session had friction before ending cleanly (retry×1, rejection×0,
  correction×6)`). Those counters grow with the transcript, so every `Stop`
  produced a "new" cause and appended another episode plus another distilled
  lesson: one Codex session recorded 16 episodes for a single task, and
  `doctor` reported 34 duplicate pairs. The comparison now normalizes that
  cause onto its shared prefix (`episode::normalize_outcome_cause`), so a
  counter bump folds into the existing episode. Every other cause —
  LLM-authored, terminal detail, human-supplied — is still compared verbatim,
  so genuinely different outcomes keep recording.
- **Hook-based hosts now open an inject session, so their compliance and
  effectiveness channels are no longer permanently empty.** `memvault-cli
  session-start` — the command behind the Claude Code and Codex
  `SessionStart` hooks — formatted and returned the injected memories but
  never called `ComplianceStore::record_injection`, while the MCP
  `session_start` tool and `POST /api/session` both did. Every Codex and
  Claude Code session therefore injected memories without a trace: no
  inject session to `report_compliance` against, and no pending injection
  for `outcome` to judge effectiveness against. The CLI now records the
  same best-effort inject session as the other two entry points — skipped
  candidates are still never recorded, and a compliance write failure never
  affects the injected context.

- **The MemVault icon and website link are back in Codex's plugin detail
  page.** Codex reads a plugin's listing (app icon, brand color, website)
  from the `interface` block of the plugin's own manifest — never from the
  marketplace index — and the block had been lost with the plugin-root
  `plugin.json` that 0.6.2 removed to restore hook discovery. Since then the
  plugin browser showed a generic gray placeholder and the detail page
  reported the website as unavailable. The listing is restored on the single
  `.claude-plugin/plugin.json` manifest that Claude Code and Codex both read
  (`displayName`, `shortDescription`, `longDescription`, `developerName`,
  `category`, `capabilities`, `websiteURL`, `brandColor`, `composerIcon`,
  `logo`, `logoDark`); Codex resolves the icon paths plugin-relative and
  Claude Code's non-strict schema ignores the extra key. `tests/run-tests.sh`
  now fails if the block ever goes missing again.

- **Codex plugin hooks no longer file their sessions under `claude-code`** —
  `plugins/memvault/hooks/lib.sh` picked the host agent id from `CODEX_*` in
  the hook process environment alone, and Codex Desktop exports none of them
  into hook subprocesses (the briefing LaunchAgent sets only
  `MEMVAULT_HOOK_*`). Every Codex session was therefore attributed to
  `claude-code`: episodes and extracted memories carried
  `source_agent_id=claude-code` next to a Codex `source_session_id`, the
  `extract-cli:claude-code` watermark was shared with real Claude Code
  sessions, and Codex-scoped injection rules could never match Codex's own
  sessions. The hooks now pass their captured payload to the detector, which
  identifies Codex by evidence Claude Code does not produce — `turn_id` (a
  field Codex's hook schema documents as a "Codex extension", required on
  every turn-scoped event), a
  `sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl` transcript path, or the
  `model` + `permission_mode` pair every Codex hook payload carries (this is
  what still identifies `SessionStart`, whose payload has no `turn_id`). A
  Claude Code `…/.claude/projects/…` transcript path outranks all of it, so a
  Codex-shaped token quoted inside a prompt or an assistant message cannot
  flip the verdict; `MEMVAULT_AGENT_ID` still overrides everything.

- **`server start` replaces a backend left over from an older build** — the
  probe-and-adopt path treated *any* healthy `/api/health` answer as "already
  running" and returned it, so after `brew upgrade` / `install.sh` replaced
  the binary on disk the detached server kept answering from the version the
  user had just upgraded away from, and nothing short of an explicit
  `memvault-cli server stop` ever picked up the new one (observed live: an
  HTTP server started from 0.6.1 was still serving days after 0.6.4 was
  installed, silently skipping the fix the upgrade was for). `/api/health`
  reports its build version, so `memvault-cli server start` now stops and
  replaces a *strictly older* server (one notice line on stderr, stdout still
  carries only the URL under `--quiet`), adopts a same-or-newer one, and
  never kills a server whose version it cannot rank. A concurrent
  `SessionStart` hook that stopped the same server first is not an error.

- **`memvault-cli ingest` no longer distills an assistant's own words into
  memory** — the trace-ingestion path called the *unguarded*
  `Extractor::extract_with_coverage` and ignored each turn's speaker role, so
  a session's assistant turns were extracted exactly like the user's
  (observed: an assistant turn `I prefer dark mode and I always use tabs`
  landed as a `[MUST]` candidate). Each turn now goes through
  `Extractor::extract_guarded` with its role (`Assistant →
  SourceRole::Agent`, refused), matching the `extract`/Stop-hook path.

- **`memvault-cli ingest` candidates now use the delta-write dedup path** —
  `process_turn` saved each candidate with a direct `store.save`, bypassing
  the `MemoryWriter` dedup every other save path uses, so two sessions
  containing the same statement produced two memories. The old comment
  claimed "the review inbox and later dedup/promote cycles own that", but
  `dedup` only *reports* duplicates and `promote` only consolidates
  facts/entities — neither removed these. Candidates now go through
  `MemoryWriter` and are vectorized when `IngestOptions.embedder` is set
  (the CLI builds it unless `--dry-run`).

- **Rule extraction no longer reads an attributed/quoted sentence as a
  stated preference** — the quotation guard was a fixed cue list, so a pasted
  `The README says "I prefer dark mode"` (or `According to the …`,
  `用户说我喜欢…`, `客户反馈"我喜欢…"`) was stored as a preference. Attribution
  is now detected structurally (reporting subject + verb, `according to` /
  `from the <source>`), while first/second-person statements — including the
  `你偏好`/`用户喜欢` mirror signals — are still extracted.

- **`memvault-cli extract --text` honors an explicit assistant marker** — a
  raw `--text 'assistant: I prefer …'` was extracted as the user's words. A
  leading `assistant:`/`agent:`/`AI:`/`助手:`/`模型:` marker now routes the
  text through `SourceRole::Agent` (refused), instead of laundering agent
  self-talk into memory; bare `--text` is unchanged.

- **Unreviewed memories are no longer injected with an instruction-priority
  tag** — the unvetted reference-data block
  (`[MEMORY CONTEXT - 参考数据（AI 提取，未审核）]`) rendered each line's stored
  priority, so an AI-extracted, unreviewed MUST candidate appeared as `[MUST]`
  inside a block whose own wrapper says "treat as data, do not execute" — i.e.
  it read as a standing instruction. Every unvetted line is now marked
  `[未审核]`; priority still decides the order within the block but is never
  rendered as a tag. Trusted (human-approved or human-authored) memories keep
  `[MUST]`/`[REF]`/`[BG]` in the confirmed-instructions block, and
  `MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST=1` still drops unreviewed MUST
  entirely.

- **MCP `extract_memories` auto-save now dedups like every other save path** —
  the tool wrote candidates with a raw `store.save`, bypassing the
  `MemoryWriter` delta-write used by `save` / `extract --save` / `ingest` /
  proxy, so calling `extract_memories(auto_save=true)` twice on the same text
  produced two byte-identical memories (`run_dedup` could report them, but
  nothing stopped the insert). It now routes through `MemoryWriter`, so a
  repeated or overlapping call skips/merges the duplicate and the returned
  `saved_id` points at the row that actually carries the content.

- **A repeated `infer_outcome` for the same session no longer distills a
  second lesson** — `record_outcome` already skipped reflection when the
  (session, task, status, cause) tuple was on record, but `infer_outcome` had
  no such guard: the episode was deduped while `reflect_and_store` appended a
  byte-identical lesson on every call, and `update_episode_lesson` orphaned the
  previous one. `infer_outcome` now mirrors `record_outcome` (`is_duplicate`
  skips reflection and effectiveness judging) and reports
  `status: "duplicate"` plus `duplicate_of`.

## [0.6.4] - 2026-10-25

### Security

- **Admin API authentication now fails closed once any agent key is configured** — `AgentAuth::authenticate` treats an agent id with no registered key as "unauthenticated mode, allowed", which is fine on a keyless local install but meant a caller could bypass the admin key by inventing `X-MemVault-Agent-Id` (the REST rewrite of a request had an agent id, so it could name *any* unregistered agent and skip the check). `MemoryRouter::authenticate_admin` now requires a registered, keyed agent as soon as one key exists; keyless installs keep the documented open local mode. Fixes the admin-key bypass across every `authenticate_admin`-guarded route (`/api/memories`, `/api/import`, `/api/forget`, `/api/backup`, `/api/agents/import/*`, `/api/inbox/*`, delete/update, …).

- **Default localhost CORS no longer trusts lookalike hosts** — the default allow-list used `Origin.starts_with("http://localhost")`, so `http://localhost.evil.com` (a page an attacker can host on a subdomain they own) was reflected as an allowed origin and could read the local API from any browser. The check now parses the origin and compares the host exactly (`localhost` / `127.0.0.1` / `[::1]`, http or https, any port).

- **Markdown backend rejects path-traversal memory ids** — ids become `{id}.md` filenames, but `Importer::parse_markdown` / `Importer::import_json` set `id` straight from file/JSON content. An id like `../../x` let `POST /api/import` (or `import-agent`/`import-skills`, or a directory import) read, overwrite or delete files outside `MEMVAULT_MARKDOWN_DIR`. `MarkdownStore::save`/`update`/`delete`/`purge` now validate the id is a single safe path component (`[A-Za-z0-9._-]`, no `..`, no leading `.`).

- **`~/.memvault` is owner-only by default** — a fresh install created the data directory and `data.db` with the process umask (typically `0755`/`0644`), so every memory was readable by other local users. New data directories are now `0700` and `data.db` (plus its `-wal`/`-shm` siblings and the markdown cache) is `0600`. Best-effort, unix-only; an existing directory's mode is left untouched.

- **`MEMVAULT_IMPORT_ROOTS` confines agent imports** — opt-in allow-list of roots (`:`-separated path list) a caller-supplied `path` on `/api/agents/import/*` must live under; canonicalized before the check so `..`/symlink escapes are rejected. Unset keeps the admin-gated permissive default.

- **12 RUSTSEC advisories cleared, and CI now gates on them** — `rustls` `0.23.45 → 0.23.46`, `rustls-webpki` `0.103.15 → 0.103.16`, `webpki-roots`/`webpki-root-certs` `1.0.9 → 1.0.10`, `quinn-proto` `0.11.16 → 0.11.19`. The `cargo-deny` CI job previously ran only `check licenses`, so no workflow ever checked advisories (despite `AGENTS.md` implying `cargo audit` was a gate); it now runs `check licenses advisories`.

- **Every workflow's `GITHUB_TOKEN` is now least-privilege** — `ci.yml`,
  `release.yml`, `release-npm.yml`, `publish.yml`, `rebuild-docker.yml` and
  `publish-mcp-registry.yml` declare a `contents: read` default instead of
  inheriting whatever the repository default happens to be, clearing the 12
  `actions/missing-workflow-permissions` code-scanning alerts (the first batch
  CodeQL default setup produced). Publishing jobs keep exactly what they need —
  `id-token: write` for crates.io/npm trusted publishing (they also gain the
  `contents: read` their checkout needs), `packages: write` for ghcr and
  `contents: write` for the GitHub Release — and the PR path-filter job adds
  `pull-requests: read` for `dorny/paths-filter`.

- **`scripts/verify-release-channels.py` compares the registry host instead of
  a URL prefix** — `identifier.startswith("ghcr.io/")` was flagged as
  `py/incomplete-url-substring-sanitization`; the MCP listing identifier is now
  split on `/` and its registry segment compared exactly, which also rejects a
  bare `ghcr.io` with no image path.

- **Dependency review gates every pull request**
  (`.github/workflows/dependency-review.yml`) — a PR that *introduces* a
  dependency with a `high`-or-worse advisory now fails before merge, instead of
  being reported after the fact by Dependabot alerts.

- **Dependabot opens scheduled version updates** (`.github/dependabot.yml`) —
  weekly PRs for the Cargo workspace, the three npm surfaces and the GitHub
  Actions used by the workflows; security updates were already enabled.

### Changed

- **Test toolchain moved to `vitest` 5.0.5** — the dashboard lockfile had pinned 4.1.11, so 4.1.12 first closed GHSA-qxg6-89mg-rqv8 (code injection via unauthenticated manual mock registration) and GHSA-xp26-3454-mjgp (Windows backslash traversal bypassing the `@vitest/mocker` redirect allowlist), and Dependabot then took `vitest` to 5.0.5 in `dashboard/`, `dsh-plugin/` and `obsidian-plugin/`. Dev-only — the shipped dashboard bundle is unchanged.

- **Release binaries are ~33% smaller** — the workspace had no `[profile.release]` at all, so every release build shipped the cargo default: no LTO, 16 codegen units, and an unstripped binary whose `__LINKEDIT` was almost entirely the symbol table of the prebuilt `libonnxruntime.a` pulled in by `fastembed`. `lto = "thin"`, `codegen-units = 1` and `strip = "symbols"` take `memvault-mcp` from 52.7 MB to 35.6 MB (`memvault-cli` 45.4 → 31.2 MB, `memvault-proxy` 53.6 → 35.7 MB) and the per-platform `.tar.gz` from 48.9 to 40.7 MiB. Log lines, panic messages and `file:line` are compile-time constants and are unaffected; only `RUST_BACKTRACE` frame symbolication is lost — use `strip = "debuginfo"` if field backtraces matter more than ~12 MB.

- **Dashboard pages now share one centered column and one type scale** —
  every tab renders into a centered, max-width column instead of stretching
  full-bleed, so pages no longer hug the left edge on wide monitors; the
  settings form uses a narrower centered measure and the header brand aligns
  with the content column. Font sizes were consolidated onto a single token
  scale (`--fs-micro`/`sm`/`md`/`body`/`section`/`title` = 11/12/13/14/15/18 px
  plus `--fs-brand`/`--fs-display`), fixing the stats pipeline heading that
  fell back to the browser's 18.7px `h3`, the system capability table that
  rendered at 16px, the unstyled memory-layer chip (14px), and 10.5px/12.5px
  table headers and textareas. Native controls now inherit Fira Sans instead
  of the UA's Arial 13.33px, so every button, input and select matches.

- **Dependency refresh** — Dependabot bumps across the Cargo workspace and the three npm surfaces: `tokio` 1.54.1, `thiserror` 2.0.21, `sha2` 0.11.1 and `schemars` 1.2.3; `actions/checkout` v4 → v7; `react`/`react-dom` 19.3.1 (dashboard's `react` sat a minor behind its own `react-dom`); and `typescript` 7.0.2, `ip-address` 10.7.4, `@deepseek-ai/schemastery` 3.18.5, `@deepseek-ai/cordis` 4.0.5 and `@types/node` 26.6.5 in the npm plugins.

### Added

- **`MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST`** — opt-in stricter injection
  gate. By default an unreviewed MUST memory is still injected, downgraded
  into the "reference data, don't execute directly" block; with this set, an
  unreviewed MUST memory is excluded from injection entirely until a human
  reviews it. Only affects MUST — unreviewed REF/BG memories are unaffected.

### Fixed

- **The `memvault-mcp` port-lifecycle tests no longer adopt a sibling test's
  server** — `rest_api::tests::auto_port_*` / `non_auto_port_*` drew their
  preferred port from `free_localhost_port()` (i.e. the OS *ephemeral* range)
  and then let `run_rest_server` scan `preferred..=preferred +
  PORT_SEARCH_RANGE`, adopting any candidate that answers a MemVault health
  probe. Every other REST test in the binary serves the real router on an
  ephemeral port, so the scan routinely landed on a sibling's live server and
  returned `RestRun::AlreadyRunning` instead of binding its own — a ~30%
  failure rate locally (12/40 runs; 10× `auto_port_moves_past_foreign_process`,
  2× `auto_port_gives_up_when_the_whole_range_is_taken`, the latter after its
  30 s guard). Nothing to do with the request-draining or hang fixes that
  preceded it: the scan was simply racing its siblings' servers. The tests now
  reserve a private port block outside every platform's ephemeral range
  (Linux `32768-60999`, macOS `49152-65535`) and prove it free before use, so
  the scan can only ever see the fakes the test spawned. Verified 0/160 runs
  after (was 12/40 before). Test-only; no behavior change.

- **Codex friction scoring no longer mistakes host-injected context for a
  human turn** — Codex prepends its own `role: "user"` rows (the AGENTS.md
  instructions block, `<environment_context>`, and response-annotation blocks)
  ahead of the human's first message and can split one human turn across
  several rows, so counting human turns ordinally made the *opening prompt*
  the second turn. A task statement that merely contained a correction word
  ("revert …", "no, …", "actually, …") therefore scored as a correction and
  opened the `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` gate on an otherwise
  friction-free session — exactly the false positive the extract/outcome
  precision guard exists to avoid. A Codex correction is now recognized only
  once the agent has produced a turn, so leading injected context and the whole
  opening prompt are never corrections. Claude Code is unchanged: its JSONL
  carries one row per human turn and no host-injected `user` rows.

- **Codex episode `task` and extraction now see past host-injected context** —
  Codex writes its AGENTS.md instructions block and `<environment_context>` as
  `role: "user"` rows ahead of the human's first message, and wraps attachment
  and response-annotation turns so the human's real text sits after a
  `## My request:` marker. `transcript::parse_turns` treated every one of those
  rows as a conversation turn, so `first_user_turn` returned the AGENTS.md
  boilerplate as the episode `task` (and derived a bogus `task_type` from its
  headings), while extraction and the LLM judge context carried the host's own
  scaffolding as if the user had written it. Pure-context rows (AGENTS.md,
  `<environment_context>`, `<app-context>`, client time-context) are now
  recognized as host-injected and dropped; request wrappers (`# Response
  annotations:`, `# Files mentioned by the user:`) are unwrapped to the text
  after `## My request:` so the opening prompt — however it is wrapped or split
  across rows — is the human's own words, and never lost.

- **Friction scoring and outcome inference now read Codex rollouts, not only
  Claude Code transcripts** — `friction::score` recognized just Claude Code's
  top-level `user`/`assistant` rows, so every Codex `rollout.jsonl` row
  (speaker nested in `payload`) was skipped, the score stayed a flat zero, the
  `MEMVAULT_HOOK_EXTRACT_MIN_FRICTION` gate never opened, and the Codex `Stop`
  hooks silently never extracted or recorded an outcome: an empty review inbox
  and `episodes = 0` on an otherwise healthy Codex-only install. Codex
  `function_call` / `function_call_output` rows now score the same three
  signals — retries (two consecutive non-zero `Process exited with code N`
  exits from the same tool), refusals (`exec_command failed: CreateProcess {
  message: "Rejected(…)" }`), and corrections in a later human turn — and
  `outcome --infer` reads the same terminal states, so a Codex session that
  ends on a failed command infers Failure/Partial instead of being
  indistinguishable from a clean one.

- **CLI `extract` (the Stop-hook's auto-save path) now guards by speaker role
  and is idempotent across repeated hook firings** — it previously flattened
  a host transcript into one blob before pattern-matching, so an assistant
  paraphrase or a quoted document line could be extracted with the same
  trust as the user's own words, and it wrote straight to the store,
  bypassing both the dedup/delta-write path (`MemoryWriter`) and any
  watermark, so a replayed or growing transcript could re-extract (and
  re-save) the same turns on every Stop. `extract --save` now runs
  `extract_guarded` per transcript turn with that turn's real `SourceRole`,
  tracks a per-session watermark so already-processed turns are skipped, and
  saves through `MemoryWriter` like every other save path. The rule-based
  extractor also no longer matches preference keywords inside an explicit
  negation or quotation ("这不是我的偏好", "引用:…"), and recognizes a bare
  future-tense imperative ("以后请…") that previously fell through with no
  signal.

- **Dashboard checkbox rows no longer collapse to a stacked layout** — the
  generic `.form-row label` column rule was overriding `.checkbox-label`, and
  the 100%-wide field rule stretched native checkboxes, so the "approve
  immediately" toggle sat centred above its label. Checkboxes/radios are now
  inline again, and the wide agent-registry table scrolls in place on narrow
  screens instead of forcing the whole page sideways.

- **The `memvault-cli` instance-detection test no longer flakes** —
  `server::tests::running_server_detects_existing_instance` answered its health
  probe from a hand-rolled fake server that wrote the response without ever
  reading the request. Closing a socket while the client's request is still
  unread makes the kernel send a RST, so hyper intermittently failed the probe
  with `Canceled(UnexpectedMessage)` even though the request had been answered,
  and the test died with "should detect the instance" (6/3000 through
  `running_server` locally; 4981/10000 when the same tiny server was polled in
  a tight loop). The fake endpoint now drains the request before replying, as a
  real HTTP server does; the identical pattern in `memvault-core`'s
  `local_server::tests::serve_once` and `memvault-mcp`'s
  `rest_api::tests::spawn_foreign_server` fixtures is fixed the same way. The
  test also now
  holds the `MEMVAULT_HOME` env lock across its probes, so the sibling
  `port_and_pid_files_roundtrip_under_home` test can no longer point
  `MEMVAULT_HOME` at its own directory mid-probe and make the recorded port
  unreadable. Test-only; no behavior change.

- **The `memvault-mcp` auto-port tests can no longer wedge CI** —
  `rest_api::tests::auto_port_*` / `non_auto_port_*` call `run_rest_server`,
  which on a *free* preferred port binds it and serves until the process
  receives a shutdown signal, i.e. it never returns. The tests reached that
  path whenever a fake "foreign" server had not bound its port yet (they waited
  a fixed 100 ms) or when the health probe missed it, so the test hung forever
  while holding `PORT_LOCK` — wedging the other three auto-port tests with it
  and leaving the `memvault-mcp` unit-test binary running until the CI job was
  cancelled (the mcp binary printed two `ok` lines and then nothing for the
  half hour the job lived). The tests now wait, bounded, for the fake to
  actually own the port and run every must-return `run_rest_server` call behind
  a wall-clock guard, so a miss fails the test instead of hanging the job.
  Visible only once the `memvault-cli` flake above stopped aborting
  `cargo test` before the mcp binary ran.

## [0.6.3] - 2026-10-22

### Fixed

- **Codex hook troubleshooting now matches the fixed 0.6.2 bundle** — the
  plugin README, `docs/TROUBLESHOOTING.md` §7.4 and
  `integrations/codex/README.md` now agree that a missing `/hooks` entry means
  a plugin-root `plugin.json` slipped into the bundle; the fix is 0.6.2+ with
  `.claude-plugin/plugin.json` + `hooks/hooks.json` and no root manifest.
  `codex exec --dangerously-bypass-hook-trust` is documented as a
  noninteractive loader check, while desktop sessions still approve once in
  `/hooks`. The 0.6.1 changelog entry's correction is now punctuated clearly.

### Changed

- **Obsidian plugin dev dependency bump** — `@vitest/mocker` and `vitest` are
  now 4.1.12 (Dependabot #58), keeping the plugin's test toolchain current.

## [0.6.2] - 2026-10-22

### Fixed

- **Codex hook discovery regressed through the portable plugin manifest** —
  0.6.1's claim that `plugins/memvault/plugin.json` was safe when it omitted
  `hooks` did not hold on `codex-cli 0.163.0-alpha.11.1`: with that root
  manifest present, Codex treated it as authoritative and did not enumerate
  `hooks/hooks.json`, so plugin hooks were never registered even though MCP
  and skills loaded. The bundle again ships only the
  `.claude-plugin/plugin.json` manifest that Claude Code and Codex read
  (restoring the verified 0.4.5 behavior); CI's `agent-plugins` job and
  `plugins/memvault/tests/run-tests.sh` now reject a plugin-root manifest as
  a regression guard. Marked the 0.6.1 verification claim below as invalid on
  alpha.11.1.
- **Imports left memories vectorless until restart** — every bulk-import path
  writes through the raw store, bypassing the router's embed-on-save, so
  imported memories were invisible to semantic search until the next server
  start or a manual `backfill-embeddings`. All of them now kick the shared
  embedding backfill in-process: REST `POST /api/import` (JSON + Markdown),
  `/api/skills/import`, `/api/agents/import/run`; MCP `import_skills` and
  `extract_memories` auto-save (reusing the server's `backfill_guard`, so
  overlapping triggers dedup); and the one-shot CLI `import`, `import-skills`
  and `import-agent` (which backfill synchronously and reuse the embedder
  already built for dedup — no second model load). `run_rest_server` and the
  import endpoints now also share ONE backfill guard: an import landing
  during the startup warm-up backfill previously ran a second concurrent
  pass, re-embedding the same rows (wasted provider calls, avoidable 429s on
  rate-limited providers).

- **`memvault-cli reindex` always printed 0 added / 0 resynced / 0 removed** —
  `MarkdownStore::open` already runs a self-heal reindex before the CLI's
  explicit pass, so the explicit pass had nothing left to report even when
  the open had just repaired a stale cache. New `MarkdownStore::open_with_report`
  surfaces the open-time `ReindexReport` through `OpenedStore.markdown_reindex`,
  and the CLI now prints both passes: `Reindex on open: …` (the real work) and
  `Reindex (verify): …` (the confirmation pass).

- **Dashboard stats counted skills by `skill_meta.is_some()`** — switched to
  the canonical `memory_type == Skill`. Equivalent today (skill_meta is only
  written for real skills and cleared on type changes), but no longer a
  latent divergence.

- **`sync` backups accumulated without bound; hand-written files quoting a
  marker could still be clobbered** — `.memvault-backups/` now keeps only the
  newest 5 backups per protected file (pruning is best-effort and never
  fails a sync), and the generated-file marker check was tightened twice:
  only the first 3 non-blank lines are inspected, and within those the marker
  must *open* the line. A team handbook saying "its sync writes a
  `# Memory Context` header" near the top (found by E2E) is hand-written and
  is now preserved + backed up instead of overwritten.

- **SOP import dropped sub-section triggers/verifications** — `# Release` +
  `## Cleanup` each carrying their own `trigger:`/`verification:` lines kept
  only the first (the parser's documented "absorbed" semantics applied to
  steps but silently discarded later metadata). Multiple trigger/verification
  lines within one skill's span are now joined with `"; "`. The one-skill-per-H1
  absorption semantics themselves are unchanged (documented design).

- **Hand-edited Markdown files lost their creation time on resync** —
  `Exporter::memory_to_markdown` always wrote `created:`/`updated:`
  frontmatter, but `Importer::parse_markdown` never read them back, so every
  reindex resync (and every Markdown import) reset `created_at` to the edit
  moment. Both fields now round-trip (RFC 3339; unparseable values keep the
  lossy-format convention of falling back to defaults).

- **First semantic queries raced the embedding model warm-up** — stdio MCP
  answered `mode="semantic"` with `-32602` (an error shape that reads as a
  caller mistake and vanished on retry) and REST silently degraded to
  keyword for the first ~250 ms of process life, because both sampled
  provider readiness once (`try_get`/`ready`). Both now apply the write
  path's bounded readiness wait (`get_timeout(EMBED_WRITE_WAIT)`, which
  joins the single-flight build). After the wait: a warming provider
  degrades honestly (`search_mode` reports `keyword`); MCP keeps the
  explicit error only for the deliberate keyword-only configuration, where
  it is a permanent fact rather than a race.

- **Vector near-duplicate path bypassed the role-swap guard** (found by
  E2E) — the 0.6.x fix for reordered-argument facts ("Alice owes Bob
  $100" vs "Bob owes Alice $100") only guarded the keyword path
  (`ordered_similarity`). With an embedder active, the vector path had no
  such guard and short role-swapped sentences embed nearly identically
  (measured cosine 0.99 for 张三欠李四100元 / 李四欠张三100元), so the
  reversed fact was silently swallowed as a near-duplicate. Unsegmented
  CJK made it worse: a whole sentence tokenizes to ONE token, keyword
  Jaccard is 0, and the decision falls through to the unguarded vector
  path every time. Vector candidates whose character multiset equals the
  incoming text's (but whose text differs) are now rejected as distinct
  facts. Also fixed the adjacent single-token case in the keyword guard:
  with fewer than two tokens per side there is no bigram signal, so
  identical unsegmented CJK content used to score 0.5 (below the 0.7
  merge threshold) and accumulate as duplicates when no embedder is
  configured; exact single-token duplicates now dedup normally.

## [0.6.1] - 2026-10-21

### Fixed

- **CI `agent-plugins` lint crashed on `os.path.isfile`** — the plugin-manifest
  lint step in `.github/workflows/ci.yml` referenced `os` without importing it,
  so a `NameError` failed the job on every pull request. Added the missing
  `import os`.

- **`memvault-cli sync` ignored `inject_channel`** — `AgentProfile.inject_channel`
  pins an agent to one canonical delivery path (`mcp` / `proxy` / `sync`) so the
  same memory isn't delivered twice (docs/DESIGN.md §17.5), and
  `MemoryRouter::channel_allows` already had a `Sync` variant with test
  coverage for it. But `SyncEngine::sync` never consulted it: pinning
  `claude-code` to `mcp` still regenerated a full `CLAUDE.md` on every
  `sync`, so that agent got the same memories through both the SessionStart
  hook and the synced file. `SyncEngine::with_router` now gates each
  per-host target (`CLAUDE.md`→`claude-code`, `.cursorrules`→`cursor`,
  `.clinerules`→`cline`, `.github/copilot-instructions.md`→`vscode`) against
  its owning agent's `inject_channel` and reports a skip with reason instead
  of writing when that agent is pinned elsewhere; `memvault-cli sync` wires
  the agent registry in. `AGENTS.md`/`MEMORY-INDEX.md` have no single owning
  host and stay ungated. Omitting `with_router` keeps the old unconditional
  behavior.
- **`memvault-cli sync` no longer clobbers a hand-written `CLAUDE.md`/
  `AGENTS.md`/etc.** — `SyncEngine::write_if_changed`'s marker check used to
  test the *freshly generated* content (which always contains the marker),
  never the file already on disk, so it never actually fired and every sync
  silently overwrote whatever a user had written by hand. It now checks the
  existing file: anything without a MemVault marker is left in place and
  gets a timestamped backup under `.memvault-backups/` instead, reported as
  a skip-with-reason. `.cursorrules`/`.clinerules`/
  `.github/copilot-instructions.md` gained the same marker the other
  targets already had, so a *second* `sync` run correctly recognizes and
  updates MemVault's own prior output for those three too (previously they
  had no marker at all, so once the file existed every later sync also
  silently no-op'd on them).
- **A near-duplicate save could leave the original record vectorless
  forever** — when a save landed without a vector (e.g. during the
  embedder's boot warm-up) and the identical content was saved again later
  with the embedder ready, the delta-write dedup path reported the retry as
  `skipped` with `embedded: true` but never touched the *original* row's
  `NULL` embedding — so semantic search kept missing it indefinitely despite
  the "success" response. `MemoryWriter::save`'s skip path now backfills the
  existing row from the retry's freshly computed vector when (and only when)
  the row doesn't already have one, covering CLI/REST/MCP/proxy saves and
  proxy auto-extraction uniformly (they all go through `MemoryWriter`); a
  redundant MCP-specific re-embed-on-skip code path is removed in favor of
  this shared fix.
- **No way to backfill vectors for an existing SQLite store on demand** —
  the background auto-backfill only runs inside a live `memvault-mcp`
  process; a store populated by `memvault-cli import` (JSON/Markdown export
  never carries embeddings — they're derived, not part of the `Memory`
  model) or one that predates enabling an embedder had no way to get vectors
  without starting a server and waiting. New `memvault-cli
  backfill-embeddings` drains every vectorless memory synchronously via the
  new `MemoryRouter::backfill_all_embeddings` and reports the total.
- **`review --reject` could delete an already-approved memory** —
  its own help text says it's for *pending* review, and the MCP
  `review_memory` tool already refused with "already approved; use
  delete_memory instead", but `memvault-cli review --reject`, `DELETE
  /api/inbox/{id}/reject` and the proxy's `review_memory` "reject" action
  all deleted unconditionally regardless of `human_reviewed`. All three now
  carry the same guard MCP had.
- **`confirm-read` reported the requested count, not the actual one** —
  CLI `confirm-read`, `POST /api/confirm-read` and MCP `confirm_read` echoed
  back `ids.len()` as "confirmed", so a batch with even one nonexistent id
  overstated how many access counts actually incremented.
  `MemoryStore::record_access` now returns the real number of rows changed
  (`Result<usize>` instead of `Result<()>`) and all three callers report
  that instead.
- **Editing a plain fact through the Web Dashboard silently stamped it with
  a default `skill_meta`** — the dashboard's `updateMemory` always sends
  `skill_trigger`/`skill_steps`/`skill_verification` as explicit `null` on
  every edit, skill or not (`dashboard/src/api.ts`), and `PUT
  /api/memories/{id}` read "key present" (even as an explicit null) as
  "this request carries skill intent", so it created an empty `skill_meta`
  on every dashboard edit of a non-skill memory — inflating the Stats tab's
  Skills count. The three fields now only count as explicit intent when they
  carry an actual value (`Some(Some(_))`), not merely `Some(None)`.
- **Dashboard header/Memories-tab counts went stale until a manual page
  refresh** — the Review tab's dedicated reject flow and recording a task
  outcome (which can auto-save a failure lesson) only refreshed their own
  tab's data, relying on a 30s background poll to catch up the Memories
  count and Stats badges; both now also refresh immediately, matching the
  pattern the Memories-tab approve/reject/quick-edit actions already used.

### Changed

- **Homebrew now installs from the npm mirror, not GitHub Releases** — the
  `dreamor/tap` formula used to `url` the 48 MB GitHub Release archive, which
  some corporate/throttled networks serve at single-digit KB/s, so `brew
  install memvault` / `brew upgrade` spent tens of minutes on one download and
  looked hung (and could be killed mid-download, leaving a stale lock). The
  Apple Silicon binaries are now published as the npm package
  `@dreamor/memvault-bin` and the formula pulls that tarball from
  `registry.npmmirror.com` (multi-MB/s) with `registry.npmjs.org` as a Homebrew
  `mirror` fallback. A new workflow,
  [`.github/workflows/release-npm.yml`](.github/workflows/release-npm.yml),
  owns the whole chain (build → publish via npm trusted publishing / OIDC →
  wait for the mirror → repoint the tap with the mirror tarball's `sha256`), and
  is now the only writer of the tap: `release.yml` no longer has an
  `update-homebrew` job, so the two can never race. `scripts/update-homebrew-formula.sh`
  generates the new formula, `scripts/bootstrap-npm-bin-package.sh` performs the
  one-time interactive bootstrap npm requires (the first publish plus the
  trusted-publisher configuration), and `scripts/verify-release-channels.py`
  gained an `npm-bin` channel that fails when the package or the formula
  checksum drifts.

- **The npmmirror wait had to change — it mirrors lazily** — a brand-new package
  is not pulled by `registry.npmmirror.com` until something asks for its
  *metadata*. The `/<pkg>/-/<file>.tgz` URL is a blind rewrite to their CDN and
  keeps returning 404 until the package has been mirrored, so probing it does
  nothing. The first `@dreamor/memvault-bin` bootstrap did exactly that, saw ten
  minutes of 404s, and then reported failure on a publish that had in fact
  succeeded (the package appeared under a minute once the metadata was
  requested). `release-npm.yml` and `scripts/update-homebrew-formula.sh` now wait
  on the *version endpoint* while nudging the mirror (metadata request +
  `PUT /-/package/<pkg>/syncs`), and `scripts/bootstrap-npm-bin-package.sh`
  delegates to that same wait instead of duplicating it.

### Fixed

- **A prerelease could never be published** — npm rejects a prerelease
  without an explicit dist-tag (`You must specify a tag using --tag when
  publishing a prerelease version`), so an `0.7.0-rc.1` release would have
  failed in `release-npm.yml`'s publish step *after* a full macOS build.
  The workflow now derives the dist-tag from the prerelease identifier
  (`0.7.0-rc.1` → `rc`), so a candidate publishes under its own tag instead
  of taking `latest` away from the stable line.

- **The registry wait budget was too tight for a real first publish** — on the
  first genuine publish of the 52 MB `@dreamor/memvault-bin` tarball,
  npmjs.org's version endpoint 404'd for ~7 minutes while the package was
  processed (29 of the 40 allowed attempts). The budget is now 15 minutes per
  registry instead of 10, so a slow npm cannot fail an otherwise good release.

- **`--only npm-bin --skip homebrew` could never pass** — `check_npm_bin`
  also asserted that the tap formula pins the version, but `--skip homebrew`
  is exactly the `publish-homebrew=false` repair path, where the tap is left
  on the previous version on purpose. Every npm-only repair therefore ended
  red for a reason that had nothing to do with npm. Skipping `homebrew` now
  skips the tap assertion, and the report says so explicitly.

- **A `release-npm.yml` repair dispatch could quietly revert the tap** — every
  job checked out the tag named by the `version` input, so repairing an
  older release ran *that tag's* `scripts/update-homebrew-formula.sh`:
  dispatching v0.6.0 (tagged before the npm work) regenerated the
  GitHub-Release formula and pushed it, putting the 48 MB slow download back
  on the tap. The `update-homebrew` and `verify` jobs now take their tooling
  from the ref the workflow runs on (the tag itself, on a tag push) and the
  version from the input, and `update-homebrew` refuses to push a formula
  that does not pull `@dreamor/memvault-bin` from the mirror. `publish-npm`
  still checks out the tag, because the binaries must be built from the
  released source.

- **A save right after stdio startup could persist without a vector** — the MCP
  handshake completes before the lazy `native` provider finishes building
  (ONNX session + tokenizer load, or a first-launch HuggingFace download), so a
  `save_memory` arriving in that window sampled the provider with the
  non-blocking `try_get()` and silently wrote a `NULL` embedding even when the
  model was already cached and landed milliseconds later. Worse, the boot-time
  backfill (`on_initialized`) also sampled readiness once, so a memory saved
  during the warm-up was never re-embedded — it stayed invisible to
  semantic/hybrid search for the process lifetime. `save_memory` (MCP + REST)
  now waits for readiness via the new bounded, cancellation-safe
  `LazyEmbedder::get_timeout` (a readiness wait, not the unreliable "sleep
  ~100ms" workaround), the boot backfill waits for the provider to land instead
  of sampling it, and the save response gains `vector_pending` so a deferred
  vector is distinguishable from a deliberate keyword-only save. The wait budget
  now lives in the (renamed) `EMBED_WRITE_WAIT` constant, since it covers more
  than `save_memory`.
- **MCP/REST outcome records wait for the embedding provider too** — the same
  boot race applied to `record_outcome` (and its `--infer` variant), whose
  episode rows and the lessons reflection distils from them could be written
  without a vector. Both paths now use the bounded `get_timeout`, so an outcome
  recorded right after startup keeps its vector; the backfill still covers a
  genuinely cold first-launch download.
- **`memvault-proxy` `save_memory` now matches MCP/REST** — the proxy's save
  tool called `store.save` directly, skipping *both* auto-embedding (so every
  proxy save was vectorless until a background backfill happened to run) and
  the delta-write dedup/merge step. It now embeds via `get_timeout`, writes
  through `MemoryWriter` (skills force-insert), and returns the same JSON shape
  (`status`/`id`/`priority`/`embedded`/`vector_pending`/…). Its tool output
  changed from the `Memory saved: id=…` text line to that JSON object.
- **Stop hook duplicated the session's inferred outcome** — a Claude Code
  `Stop` hook fires once per assistant turn, and outcome inference derives the
  session's `task` from the same opening turn each time, so enabling
  `MEMVAULT_HOOK_OUTCOME=1` appended a byte-identical episode (and, for
  failures, distilled a fresh lesson) on every turn. `episode::record_outcome`
  is now idempotent within a session: when an episode with the same
  `(session_id, task, status, cause)` is already on record it is returned via
  the new `RecordedOutcome.duplicate_of` instead of inserting another row, and
  the CLI/MCP/REST call sites skip reflection/effectiveness for the duplicate.
  Manual, session-less callers (`memvault-cli outcome --task ...`) keep the
  historical append behavior; a genuinely different outcome for the same
  session still records. `EpisodeFilter` gained an optional `session_id` for
  the session-scoped lookup.
- **REST accepted blank memory content** — `POST /api/memories` with
  `content: ""` (or whitespace-only) returned 200 and saved a blank row
  (a *missing* field was already rejected 422 by serde). Blank content is now
  refused with a 400 at every write boundary: `Memory::validate_content` is
  enforced by both storage backends' `save`/`save_with_embedding`/`update`
  (so `PUT /api/memories/{id}` can no longer blank a row either) and by
  `MemoryWriter::save` *before* the delta-write dedup lookup — previously an
  empty body could "merge" into an unrelated existing memory (contributing no
  residue) and look like a success.

- **`memvault-proxy` auto-extracted memories are now embedded and deduped** —
  the extraction pipeline (`ResponseExtractor::save_extracted`) wrote through
  `store.save` with only a keyword `Deduplicator` guard, so every
  auto-extracted memory landed with a `NULL` embedding — invisible to
  semantic/hybrid search — and one arriving during the provider warm-up stayed
  vectorless. It now shares the proxy's `LazyEmbedder`, embeds via the same
  bounded `get_timeout(EMBED_WRITE_WAIT)` wait as MCP/REST `save_memory`,
  writes through `MemoryWriter` (delta-write dedup/merge instead of dropping
  near-duplicates), and reports a `vector_pending` count so a deferred vector
  is distinguishable from a deliberate keyword-only save.

## [0.6.0] - 2026-10-20

### Added

- **Forget feature: permanent, auditable memory erasure** — `delete`/`Review
  --reject` were really soft deletes (a full `memory_history` snapshot
  survives forever and is restorable via `restore`), and decay's archiving
  never actually purged anything. New `MemoryStore::purge`/`record_forgotten`/
  `list_forgotten_log` trait methods (implemented by both the sqlite and
  markdown backends) back a true GDPR-style erasure path:
  - **Auto-forget** — memories archived longer than
    `MEMVAULT_FORGET_RETENTION_DAYS` (default 90 days) are permanently
    purged (live row + all history snapshots + orphaned `compliance_events`
    rows); off by default (`MEMVAULT_FORGET_AUTO_ENABLED=1` to opt in),
    Must-priority exempt, Shared-visibility exempt from this automatic path.
    Runs standalone or at the end of `DecayManager::run_decay` via the new
    `with_forget` hook.
  - **Manual/bulk forget** — erase one memory by id, or a criteria-matched
    set (namespace/namespace-prefix/source agent/tag/type/age), with a
    dry-run preview required before execution: `memvault-cli forget`/`forget-log`,
    the `forget_memory` MCP tool, and `POST /api/forget` / `GET
    /api/forgotten-log`.
  - **Audit trail** — every erasure writes a `forgotten_log` row (new
    sqlite table) recording a SHA-256 content hash, namespace,
    priority/visibility, scope (manual/bulk/auto), reason, requester, and
    timestamp — never the erased content itself.
  - New `Memory.archived_at` field (sqlite migrations 21–23) records when a
    memory first transitioned into `archived:*`, since `updated_at` is
    bumped by `record_access` even while still archived and can't be used
    as a retention signal.
  - The auto-forget sweep is attached to every "run decay" entry point
    (`memvault-cli decay`, `run_decay` MCP tool, `POST /api/decay`) via
    `DecayManager::with_forget`, so it actually fires at the end of a
    normal decay cycle once `MEMVAULT_FORGET_AUTO_ENABLED=1` is set —
    previously the hook existed but nothing wired it in. New `GET
    /api/forget/config` reports current enabled/retention status, and
    `archived_before_days` on `POST /api/forget` lets the same sweep
    criteria be previewed/run on demand regardless of that flag.
  - Dashboard: a distinct "Forget Permanently" action in the memory detail
    panel (separate from "Delete", with an explicit irreversibility warning
    and type-to-confirm), an "Auto-Forget Sweep" panel under Settings
    showing current enabled/retention status with an on-demand
    preview-then-confirm run, a "Bulk Forget by Filter" panel with its own
    preview-then-confirm under Settings, and a read-only forgotten-log audit
    table.
  - `ForgetReport` (and the CLI/MCP/REST/Dashboard surfaces built on it)
    now reports `excluded: Vec<(id, reason)>` — e.g. forgetting a single
    Must-protected memory by id without `allow_must` used to silently
    report "0 purged" with no indication why; it now says
    `excluded: [["mem_...", "must-protected"]]` (REST/MCP JSON),
    `[excluded:must-protected] mem_...` (CLI), or a result line in the
    Dashboard.
- **Canonical `project:*` namespaces** — callers spelled the same project
  differently (`MemVault`, `memvault`, `memvault/`), and because namespace
  filters are exact matches, memories saved under one spelling were silently
  invisible to sessions using another. All write paths and read filters now
  canonicalize project namespaces (lowercase, trailing slashes/whitespace
  stripped) via `canonical_project_namespace()` — CLI/MCP/REST saves,
  `session_start` injection, search/list/vector filters, dedup, sync, proxy
  context and rule imports alike. Pre-existing rows are migrated once on
  open: sqlite runs an idempotent backfill, the markdown backend rewrites
  file frontmatter during `reindex`, so no memory becomes unreachable.
- **Documented that `agent_id` never filters retrieval** — it is provenance
  (`source_agent`) only; store queries filter on namespace/type/priority and
  agent differentiation happens at injection time via `inject_rules`
  (README + DESIGN).
- **Silent background embedding-model download on first launch** — the
  `native` embedding provider used to download its model (HuggingFace, into
  the MemVault home) *before* the REST/MCP server bound its port, so a fresh
  install took minutes to answer and the dashboard was unreachable in the
  meantime. The server now wires a `LazyEmbedder`: the port/dashboard come up
  immediately, a background task kicks the download at boot, requests degrade
  to keyword-only while the model is still downloading (no blocking, no
  error spam), and semantic/hybrid search, save-side auto-embedding, dedup
  and embedding backfill hot-swap on automatically the moment the provider
  lands. The same lazy wiring applies to the MCP stdio/SSE servers and the
  `memvault-proxy`. Explicit `MEMVAULT_EMBEDDING_PROVIDER=none|disabled|off`
  still skips the download entirely (pure keyword). `memvault-cli server
  start` wait now defaults to 15s (was 90s — the first-run model download no
  longer gates it).
- **Dashboard / REST backend auto-start (claude-mem style)** — the Web
  Dashboard is already embedded in the `memvault-mcp` binary on every
  distribution channel; it now also starts itself: `memvault-mcp` gained
  `--auto-port` (adopts an already-running MemVault on the preferred port;
  otherwise moves to the next free port up to +100 and records it in
  `mcp.port` next to `mcp.pid` under the MemVault home) and a MemVault
  identity endpoint `GET /api/health`. `memvault-cli` gained `server start`
  (idempotent probe + detached spawn; prints the active URL, `--quiet` prints
  the URL alone, wait tunable via `MEMVAULT_SERVER_START_TIMEOUT`),
  `server status`, `server stop`, and `dashboard` (start + open the browser).
  The installers start it after install (`install.sh`,
  `install.ps1 -SkipAutoStart` disables it), the Homebrew formula ships a
  `service` block (`brew services start memvault` = LaunchAgent/systemd
  boot persistence), Windows registers a Task Scheduler logon task by default,
  and the Codex plugin's SessionStart hook re-ensures the server (self-heals
  after reboot). The Vite dev proxy reads `mcp.port` instead of hardcoding
  3777. Docs: `docs/INSTALL.md §2.6` auto-start section.
- **Release-channel verification (`scripts/verify-release-channels.py`)** — `release.yml` gained a `verify-release` job that reads every release-locked channel back over the public network once the publish jobs finish and fails the run on any mismatch: the GitHub Release is published (not a draft) and carries the four platform archives plus `SHA256SUMS`, all four crates are on crates.io, the Homebrew tap pins the tag, `ghcr.io/dreamor/memvault:<tag>` and its Docker Hub mirror resolve, and the `io.github.dreamor/memvault` listing is `active` **and** the ghcr tag it advertises still resolves. "Released" now means "verified released" instead of "the push step exited 0".
- **Daily release-drift check (`.github/workflows/verify-release-drift.yml`)** — runs the same assertions against the latest GitHub Release at 02:47 UTC and opens (or comments on) a `Release channel drift detected` issue when a channel stops serving it, so a formula rollback, a deleted image tag or a retired listing does not sit unnoticed the way a `0.3.0` listing advertising a deleted ghcr tag did.
- **`retire_version` mode for the MCP Registry workflow** — `mcp-publisher status --status deleted` is the only supported way to take down a listing whose image tag no longer exists (the registry rejects a repoint to an unlabelled image and has no DELETE endpoint).
- **Web Dashboard shows the active storage backend** — a new "Storage Backend" card on the System tab (backed by `GET /api/system/backend`) shows `sqlite` or `markdown` so a markdown-backend user finds out from the dashboard, not from a `backup`/`checkpoints`/`restore` error message after the fact.
- **One-prompt agent install backed by `docs/AGENT-INSTALL.md`** — the Quick Start in both READMEs (EN + zh-CN) now opens with a copy-paste prompt that points the agent at a new machine-readable guide, [`docs/AGENT-INSTALL.md`](docs/AGENT-INSTALL.md) (raw: `https://raw.githubusercontent.com/dreamor/memvault/master/docs/AGENT-INSTALL.md`). The agent executes the whole setup itself — install the binaries, register `memvault-mcp` in its own host (Tier-1 plugin or the `mcpServers` snippet), verify with a save/search round-trip, then keep saving. The first draft of the prompt only named the goal and pointed at the README's Quick Start anchor, which left the agent to reconstruct the steps from a marketing-heavy rendered page; the new target is ordered to be run top-to-bottom. The manual `curl | bash` / Homebrew / crates.io commands stay directly below under "Or install manually", and the guide is linked from `docs/INSTALL.md` and the README docs index.

### Changed

- **新增 Codex 插件刷新脚本（`scripts/refresh-codex-plugin.sh`）** — 将“改仓库后手动升级 marketplace + 重装插件 + 重新信任 hooks”的运维点收敛成一条幂等命令，并同步说明到 `integrations/codex/README.md` 与 `plugins/memvault/README.md`。
- **README 表格同步 Codex 验证结果** — Codex CLI 行的 Stop hook 从“⚠️ 待验证”更新为“✅ 已信任并验证”（桌面 `/hooks` 信任 + `MEMVAULT_HOOK_EXTRACT=1` / `MEMVAULT_HOOK_OUTCOME=1` 开启），中英文 README 同步。
- **Codex plugin now ships a portable Agent Plugins manifest** — `plugins/memvault/plugin.json` (agent-plugins.org `$schema`) is the ChatGPT/Codex entry alongside the `.claude-plugin/plugin.json` Claude Code reads. Codex discovers `hooks/hooks.json` by default as long as the portable manifest doesn't define `hooks`; `scripts/check-release-versions.py`, CI's `agent-plugins` lint and `plugins/memvault/tests/run-tests.sh` now enforce that (previously they forbade any root plugin manifest, based on an obsolete Codex-0.162 behavior). Registration was claimed on `codex-cli 0.163.0-alpha`; **0.6.2 correction:** that conclusion does not hold on `codex-cli 0.163.0-alpha.11.1` — the portable root manifest prevented hook registration, so 0.6.2 removes it again (see `[0.6.2]`).
- **The MCP Registry listing is published by the release run itself** — `release.yml` now calls `publish-mcp-registry.yml` as a `publish-mcp` job, so a tag release no longer depends on someone remembering to re-publish the listing by hand. The manual workflow is kept for repairs (repoint / retire).
- **Silent channel skips are now explicit failures** — `release.yml`'s Docker Hub mirror and Homebrew steps used to warn and stay green when `DOCKERHUB_*` / `HOMEBREW_TAP_TOKEN` were unset, so a release could quietly ship GHCR-only or a stale formula while the run said "success". Both now fail with an actionable message, and the opt-out is the explicit `publish-dockerhub = false` / `publish-homebrew = false` dispatch input.
- **`verify-release-channels.py` is scoped to the release-locked channels** — GitHub Releases, crates.io, Homebrew, ghcr, Docker Hub and the MCP Registry. The npm dsh plugin and the Obsidian plugin release on their own schedule, so they are deliberately excluded from both the per-release and the daily check.

### Fixed

- **Plugin self-test failed from the installed plugin cache** — `plugins/memvault/tests/run-tests.sh` computed the repository root as three levels above the test directory and read its `.claude-plugin/marketplace.json`, but Claude Code/Codex install caches ship only the plugin body, so the manifest-consistency check died with `FileNotFoundError` there (11/12 in the cache vs 12/12 in a checkout). The script now detects the missing repo-level index and degrades to validating the plugin-local manifests (`plugin.json`, `.claude-plugin/plugin.json`, `hooks/hooks.json` and their hook scripts), reporting the repo-level part as a TAP `# SKIP` instead of a failure; the full cross-check still runs in a checkout.
- **Plugin self-test demanded 3 commands while the bundle ships 4** — `plugins/memvault/tests/run-tests.sh` asserted `("commands", 3)` and failed every run after `memvault-save.md` was added; the count now matches the four shipped command files (doctor/review/save/sync).
- **Agent registry drift between `agents.example.yaml` and the built-in fingerprints** — `cursor-ide` didn't match the `cursor` id every `integrations/mcp-clients/cursor.json` and the Rust fingerprint (`agent_adapt.rs`) actually use, so a customized `cursor-ide` entry was silently never applied; `cline`/`continue`/`jetbrains`/`vscode`/`windsurf`/`zed` had no registry entry at all, so those hosts fell back to the global-only `default` profile. Renamed `cursor-ide` → `cursor` and registered the six missing hosts.
- **CI's `agent-plugins` lint job never ran on the paths it actually checks** — `ci.yml`'s path filters covered `crates/**`/`dashboard/**` etc. but not `plugins/**`, `integrations/**`, `scripts/**`, `commands/**`, `pi-extension/**`, `.claude-plugin/**`, `.agents/**`, or the root plugin manifests, so a PR touching only those never triggered the job that lints them. Filters now cover them, and the lint itself now also validates root `plugin.json` and `commands/*.toml`.
- **`commands/` (Grok, TOML) and `plugins/memvault/commands/` (Claude Code/Codex, Markdown) exposed different command sets** — the former was missing `memvault-sync`, the latter `memvault-save`. Both now ship all four (doctor/review/save/sync).
- **Stale and self-contradictory plugin docs** — `plugins/memvault/README.md` never documented the `session-outcome.sh` Stop hook / `MEMVAULT_HOOK_OUTCOME` or the `/memvault-save` command; root `plugin.json` had no `mcpServers` block even though `integrations/README.md` says Grok reads it; the Qoder and Hermes sections of `integrations/README.md` contradicted their own hook files / README caveats; `integrations/mcp-clients/README.md` didn't warn that Continue's target config is YAML, not JSON; `obsidian-plugin/` had no README at all. All fixed.
- Removed `obsidian-plugin/versions.json` — dead file in this repo; the release-tracked `versions.json` lives in the downstream `dreamor/memvault-obsidian` repo and was never read or written from here.

## [0.5.0] - 2026-10-16

### Added

- **Markdown storage backend (`MEMVAULT_STORAGE_BACKEND=markdown`)** — an alternative to the default SQLite store where a directory of human/git/Obsidian-editable `{id}.md` files is canonical, with an internal SQLite cache (FTS/embeddings/episodes/skills/relations) kept eventually consistent and self-healing via `memvault-cli reindex` (or `reindex --watch` for continuous polling). `memvault-cli migrate-to-markdown --dir <dir>` does a one-time copy from the active store, carrying embeddings over. Checkpoints/restore/backup stay SQLite-only for now and fail with a clear error on the markdown backend. Select the directory with `--markdown-dir` / `MEMVAULT_MARKDOWN_DIR`.
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

[Unreleased]: https://github.com/dreamor/memvault/compare/v0.6.9...HEAD
[0.6.9]: https://github.com/dreamor/memvault/compare/v0.6.8...v0.6.9
[0.6.4]: https://github.com/dreamor/memvault/compare/v0.6.3...v0.6.4
[0.4.4]: https://github.com/dreamor/memvault/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/dreamor/memvault/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/dreamor/memvault/compare/v0.4.0...v0.4.2
[0.4.1]: https://www.npmjs.com/package/@dreamor/dsh-memvault/v/0.4.1
[0.4.0]: https://github.com/dreamor/memvault/releases/tag/v0.4.0
[0.3.0]: https://github.com/dreamor/memvault/releases/tag/v0.3.0
