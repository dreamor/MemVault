# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

> ### ⚠️ Behaviour change — read before upgrading
>
> **`memory://user-profile` no longer serves unreviewed AI-extracted MUST
> rules.** It now applies the same review gate as `session_start`
> (`MEMVAULT_INJECT_REQUIRE_REVIEW`, default on), so an unapproved MUST draft no
> longer reaches a resource the server advertises to MCP clients as "Loaded at
> conversation start". Human-reviewed MUST rules are unaffected, and
> `memvault-cli save` and the Dashboard's direct save both stamp a memory
> reviewed at the source, so a rule you recorded deliberately still appears
> immediately. **If `memory://user-profile` comes back thinner after upgrading,
> that is the fix, not data loss** — what it holds back is in the review inbox
> (`memvault-cli inbox`, Dashboard → Review Inbox), and
> `MEMVAULT_INJECT_REQUIRE_REVIEW=false` restores the previous behaviour.
>
> **`memory://project-context` is unchanged, and deliberately not gated.** It
> serves REFERENCE-level memories, where an unreviewed draft genuinely is
> reference data and the `[未审核]` wrapper says something true; gating it too
> would hand every fresh vault an empty project context, because `Memory::new`
> defaults to `ai_generated = true` and so agent-captured and auto-extracted
> memories are all unreviewed until someone reads them. Its `list_resources`
> description now says this out loud — what the server advertised and what it
> filtered used to disagree. See `docs/DESIGN.md` §13.8 for why the two
> resources are treated differently.
>
> ### ⚠️ More behaviour changes — audit hardening pass
>
> - **MCP `review_memory` (approve/edit/reject) now requires admin
>   credentials** whenever any agent key is configured, matching the REST
>   review endpoints. Scripted callers that approved memories with an
>   unregistered `agent_id` under a keyed deployment were using the bypass this
>   removes; they need the admin key now. Keyless local mode is unchanged.
> - **A human save that merges into a row carrying a never-reviewed
>   instruction (or skill procedure) keeps the result pending** instead of
>   marking the whole merge reviewed — the attestation covers the body you
>   saved, not the old directive riding along. Approve the merged payload in
>   the review inbox.
> - **In `quarantine` mode, a low-trust rewrite can no longer move a reviewed
>   memory's namespace** (`project:X → global` or `→ project:Y`); the reviewed
>   value is pinned and the file rewritten. Migrate through a reviewed edit.
> - **Markdown full-snapshot updates cannot change proposal/supersession
>   relationships.** Use the dedicated lifecycle operations. Updates queued
>   behind an approval must reread when its publication changed the revision;
>   old read/access snapshots can no longer revive the retired target.

### Added

- **Complete-membership paging for mirror clients.** Operational read-only
  `GET /api/sync/memories` returns binary-ID-keyset pages with `records`,
  `next_cursor`, `has_more`, `total_count`, and `membership_digest`; page limits
  are capped at 1,000. Continuation cursors are bounded opaque v1 ASCII tokens,
  not memory IDs. `GET /api/memories/{id}` provides a read-only lookup.
  Obsidian validates traversal completeness before considering orphan removal;
  partial reads and older-server list fallbacks update notes without removing
  orphans. The digest describes membership, not a content-version snapshot.
- **Explicit structured fact current-state intake (REST/MCP).** Optional
  `fact_assertion` (`key`, `value`, `source_namespace`, `source_order`,
  `cardinality`) and `fact_write=preserve|resolve` default to conservative
  preserve. SQLite atomic resolve is restricted to the same actually keyed
  producer's unreviewed, scoped AI Fact/REFERENCE single-valued stream; human,
  MUST, instruction, skill, shared, proposal, imported/unmanaged, suspended, or
  other-producer heads cannot be silently replaced. Force remains literal,
  unmanaged insertion. Read-only assertion/management metadata is available
  in memory JSON, alongside the actual persisted read-only `identity_verified`
  flag; semantic edits suspend the stored ownership guard.
- **Resolved fact auth/recording separation.** Actual registered-key proof is
  mandatory for management independently of `MEMVAULT_IDENTITY_VERIFICATION`.
  Disabling public identity recording leaves new rows unverified in read views
  while keyed owner updates retain active private management; policy toggles
  do not re-attest old rows. The legacy authentication/recording API is unchanged.
- **Cross-transport resolved extraction retries.** Explicit structured resolve
  uses the same canonical `extractor` source type on REST and MCP, so identical
  same-ordinal retries are idempotent in either order. Different payloads still
  conflict; preserve/unstructured extraction keeps legacy source types and IDs.
- **Auditable fact decisions and provider-free refusals.** Structured writes
  report inserted/superseded/idempotent/stale/conflict/ambiguity/review-required/
  unsupported decisions with stored-only IDs and evidence. REST refusals use
  HTTP 409 (unsupported 400) and MCP `is_error=true`; stale/idempotent are
  non-mutating HTTP 200 decisions. Embedding is best-effort only after a
  committed resolve, bound to its actual winner. Markdown explicitly refuses
  atomic resolve before memory-file/provider work. Extraction metadata is
  opt-in and positional; no NLP keys or claims of resolving every free-text
  fact are introduced. See DESIGN §9.1.2.

- **Private durable Markdown approval intents.** An approval records the exact
  reviewed replacement before either canonical file changes, outside the
  lower-trust vault. Recovery reads these intents, never reciprocal Markdown
  links, and publishes the pair in one cache transaction only after both files
  are durable. Failed live writes reconcile or compensate without exposing a
  retired target and an unapproved replacement; stale edits and forget
  tombstones prevent replay. Recovery enumerates journal operations rather than
  truncating the review inbox, and permanent erasure removes retained journal
  payloads as well as memory/history data.
- **Historical keyword recall validates versions before ranking.** Matching
  memory IDs are reconstructed at `as_of`, filtered against the historical
  state, and only their effective snapshot IDs enter the ranked FTS query.
  Invalid snapshots no longer consume a raw-candidate budget or hide a later
  valid result. Discovery and projection share one read transaction; no new
  public search fields or temporal interpretation are introduced.
- **MemoryAgentBench Conflict Resolution harness**
  (`docs/experiments/memory_agent_bench.py`). Seeds a numbered fact row
  verbatim, retrieves with `/api/search`, and scores with the benchmark's own
  `substring_exact_match`; the official `factconsolidation` prompts are embedded
  verbatim and written into the results spec. First run (2026-11-10,
  `factconsolidation_sh_6k`, n=100, 455 facts): **53% search vs 4% control
  (+49 pp)**, retrieval gold coverage 99%, search p50 23.5 ms / p95 27.9 ms.
  Failures are the benchmark's intended conflict trap — in 43/47 wrong answers
  the newer fact was in the top-5 and the model returned its pre-training answer
  anyway — so this is reported as a model-instruction limit, not a retrieval
  failure. The MH 32K row is **not** a memory-system result yet: keyword
  retrieval covers only 18% of gold facts at top-20 (43% at top-100) on 2,310
  facts, giving 5% vs 1% control. Hybrid/semantic retrieval is required before
  multi-hop can be read as a MemVault result. `mab_results*.json` is git-ignored
  like the BEAM artifacts.
- **BEAM adherence is scored end to end, not just plumbing-validated.**
  100K split (20 conversations, 80 probes): control **31.3% → experiment
  88.8%** (+57.5 pp), instruction 27.5% → 77.5%, preference 35% → 100%, judge
  stability 90%, injection p50 4.4 ms / p95 4.9 ms. 500K split (10 of 35
  conversations, 40 probes): 65.0% → 87.5% (+22.5 pp) but with judge stability
  only 60% and a much higher control rate, so it is recorded as the weaker
  sample rather than headlined. Both tables and the 500K caveat are in
  `docs/experiments/README.md`.
- **Both benchmark harnesses now honour the gateway's per-minute request
  limit.** `beam_adherence.py` and `memory_agent_bench.py` throttle calls
  (`VERIFY_MIN_CALL_INTERVAL_S`, default 1.1 s) and retry a 429 with backoff
  instead of aborting a half-finished run. Two concurrent runs had tripped the
  account's 60 requests/minute limiter and thrown away ~40% of a scored run;
  there is no partial-result checkpoint to resume from.
- **The experiments directory now has an explicit status index.** H1–H7 and
  the runtime regression are labelled historical evidence, the BEAM and
  MemoryAgentBench harnesses are labelled active, and TRACE-RECALL-BENCH is
  labelled planned with its actual implementation boundary. Generated
  `beam_results*.json` / `mab_results*.json` files are ignored rather than being
  mistaken for committed evidence.
- **Obsidian mirror notes carry their status.** A synced note's frontmatter
  gains `status` (`active` / `expired` / `archived` / `superseded` /
  `proposal`) plus the fields it derives from — `expires_at`, `archived_at`,
  `occurred_at`, `superseded_by`, `proposes_supersede_of` — emitted only when
  set, so an ordinary active note's properties stay as short as before. A note
  that is not in effect also gets a reading-mode callout above its content,
  because Obsidian collapses frontmatter by default and hides it entirely in
  reading mode; without the callout an expired memory still *looked* live. The
  raw timestamps are written alongside the derived `status` so a Dataview query
  can recompute liveness itself — `status` is a snapshot taken at sync time, and
  a memory that expires later reads `active` until the next sync.
- **Obsidian sync notices a memory leaving `active`.** `updated_at` does not
  move when a memory expires (that is a comparison against the clock) or when it
  is archived or superseded (that is written by some *other* memory's approval),
  so the mirror previously showed a dead memory as live forever. The note's
  recorded `status` is now compared against the derived one and forces a rewrite
  when they differ. Notes written before this change carry no `status`; the
  first sync after upgrading rewrites each of them once, then converges.
- **A wire contract for the REST memory payload**
  (`contracts/memory-json-keys.json`), asserted from both sides by
  `test_memory_json_matches_the_wire_contract` in `rest_api.rs` and
  `obsidian-plugin/src/contract.test.ts`. The manifest also records which
  `Memory` struct fields are deliberately *not* on the wire (`last_read_at`,
  `corroborating_agents`, `source_trace_ids`) and the Rust
  test fails if a struct field lands in neither list, so a new field can no
  longer stay invisible to every client without someone deciding that.
- **`memvault-cli doctor` finds reviewed memories stranded on the archive
  layer.** `live_looking_archived` (Info) reports human-reviewed
  MUST/REFERENCE rows sitting on L0 that carry none of the deliberate-archive
  markers — not superseded, not expired, not `source:trace` raw evidence, not
  cited as a distillation source by any derived memory. Since 0.6.8 a single
  `is_archived` gate excludes L0 rows from every injection path regardless of
  priority or review state, so a store carrying such rows — pre-0.6.7 legacy
  data, or a stray demotion — silently loses them from injection while `list`
  still shows them looking live; until now the only way to find them was
  diffing session-start output across the upgrade. Each finding names the
  one-command restore: `memvault-cli update <id> --priority <same>` re-derives
  the layer (MUST→L3, REFERENCE→L2). Info rather than Warn because promotion
  also demotes the sources it consumes without citing every member id, so a
  hit can still be a row whose content lives on inside a consolidated memory —
  a human makes that call.
- **`ORT_DYLIB_PATH` is documented for `ort-load-dynamic` test runs.**
  Contributors on legacy distros who switch the fastembed feature hit a
  cascade where one failed `dlopen` poisons `ort`'s process-global init mutex
  and 60+ unrelated tests report `Mutex poisoned` — mass breakage that is one
  missing environment variable. `CONTRIBUTING.md`'s toolchain note and
  `docs/TROUBLESHOOTING.md` §9.5 now name the symptom, the cause, and the fix.

### Changed

- **CI runs Hermes's offline standard-library regression suite** alongside
  OpenCode's Node fixtures. The cross-platform storage workflow also invokes the
  Windows default-home configuration test separately from temporary-root storage
  tests; existing Rust jobs, feature flags and coverage thresholds are unchanged.
- **The BEAM adherence harness supports reasoning-model gateways.**
  `beam_adherence.py` now accepts `VERIFY_JUDGE_MAX_TOKENS` and
  `VERIFY_EXTRA_BODY`, retries empty model responses, refuses a headline
  verdict when answers are empty or judge JSON fails to parse, and applies the
  objective markdown-fence check only to rules that actually request code
  blocks. This keeps a reasoning API from being scored on truncated or empty
  output without the run saying so.

- **`docs/DESIGN.md` is renumbered around the shipped architecture.** The old standalone business-model chapter is gone, the multi-agent and three-track chapters now follow the architecture narrative, and the former storage-backend appendix is folded into the data-model chapter at §8.3. References across the READMEs, experiment notes, code comments, release checks and the design doc itself were updated in the same change; appendix lettering and historical text are unchanged.
- **The proxy advertises what its `memory://` resources actually serve.**
  `memvault-proxy` answers `memory://` reads locally rather than forwarding
  them, and `merge_by_key` prefers local over upstream — so its one-line
  descriptions silently overrode the honest ones `memvault-mcp` advertises for
  the same URIs. `project-context` described itself as "Project-specific
  reference memories" while serving ungated content that may include unapproved
  AI-extracted drafts, and `user-profile` said nothing about the review gate it
  applies or the conversation-start loading it is advertised as. Both now match
  `memvault-mcp` wording for wording. No filtering behaviour changed —
  `project-context` staying ungated is the design (DESIGN §13.8), not a gap;
  what changed is that a client connecting through the proxy is now told so,
  the same way one connecting directly already was.
- **The Obsidian plugin declares the whole memory payload.** `RemoteMemory` was
  a hand-kept copy of `memory_to_json` that had drifted to 10 of its 24 fields
  with nothing watching it. Every change up to now had been additive so nothing
  broke, but a rename would have arrived as `undefined` and been written into
  frontmatter as an empty value. It now declares all 24, and the plugin's
  second, narrower `Memory` interface is gone — `main.ts` and `sync.ts` share
  one type instead of keeping two copies in step by hand.
- **`obsidian-plugin/tsconfig.json` typechecks its test files.** They were
  excluded, which meant the contract fixture — the object whose key set *is* the
  TypeScript half of the wire contract — could lose a field with no compiler
  anywhere noticing, since vitest transpiles without checking. Nothing shipped
  changes: `build` still bundles only `src/main.ts`.
- **BREAKING (Rust API): `SearchQuery` no longer has an `agent_id` field.**
  §13.3 is explicit that agent identity never constrains retrieval — every agent
  searches the same shared corpus, and per-agent differentiation happens at
  injection time via `inject_rules`, never in the store. The field contradicted
  that rule and was read by nothing: no store query, the reranker, the hybrid
  merger or the router ever consulted it, and `SearchQuery` is never serialized
  to or from JSON, so there is no wire format to preserve. What made it survive
  is that it sat between `query` and the three real filters, so it read as a
  fourth one that had come loose rather than as something the design forbids —
  and eight call sites kept feeding it the authenticated caller identity, which
  made it look wired. Those copies are gone; the request-level `agent_id` on
  MCP `search_memory` and REST `/api/search` is unchanged and still used for
  authentication and audit, and both now say so in their schema docs. Affects
  external consumers of `memvault-core` who construct `SearchQuery` with an
  explicit `agent_id`. Per-agent retrieval isolation remains the Phase 3 work
  §13.4 lists, and needs §13.3 and the Read row of the sharing matrix rewritten
  before it is built.
- **The non-SQL search filters are one function.** The post-merge vector path
  and the historical `as_of` path each carried their own copy of the
  namespace/type/priority/superseded/expiry/event-window predicates, alongside a
  third copy expressed in SQL. The two Rust copies are now
  `storage::memory_matches_filters`; the SQL one has to stay SQL. Three copies
  of a filter set is three chances to add a predicate to one path only, which is
  how every search fix below happened. The Rust paths also now agree on *order*:
  `as_of` projects first and filters the reconstructed state, so a memory
  superseded since `as_of` still counts as current truth then.
- **Archived (L0) injection candidates are now recorded when dropped, not
  silent.** The 0.6.8 archive-layer exclusion itself is unchanged — L0 rows
  still never inject — but every drop site now leaves a trace: the
  session-start body choke point, the layered overflow sweep, and the
  lessons/skills fetchers record the row in `SessionInjection.skipped` with a
  new `InjectSkipReason::ArchivedLayer` (`"archived_layer"` on the wire,
  `archived-layer` in Display). It was the only exclusion that never accounted
  for itself — its own comment defended the silence ("archived material is not
  a 'skipped candidate'") — and the cost showed up in a real upgrade: reviewed
  preferences stranded on L0 vanished from every session while the debug log
  reported `skipped=0`, findable only by diffing injection output across
  versions. The debug log's `skipped=` count, the REST `/api/session`
  envelope, and the proxy's `LoadReport` pick the entries up with no schema
  change (serde-defaulted field); `memvault-cli doctor`'s
  `live_looking_archived` check finds the same rows store-side. The skills
  fetcher records only trigger-MATCHED archived drops — its candidate search
  is deliberately broad, and recording every L0 row in it would bury the
  report in rows that were never candidates. The deterministic fast path and
  the two `memory://` resources still return bare payloads with no report
  channel to record into.
- **`review_memory` approve/edit responses say which layer the memory landed
  on.** Proposals inherit their target's layer, so approving an edit to an
  archived (L0) memory yields another archived memory — correct, but the old
  `"Memory <id> approved."` read as "live", and a caller only discovered the
  approved text was not injecting when a later session came up empty. Live
  rows now report `Layer: L2.` (or L1/L3); an L0 result carries an explicit
  warning plus the one-command restore (`memvault-cli update <id> --priority
  <same>` re-derives the layer).

### Fixed

- **Semantic and hybrid recall select eligible candidates before `top_k`.**
  Namespace, type, priority, lifecycle and time filters apply before vector
  selection; the 2,000-candidate scan cap remains explicit in diagnostics.
  Historical live-index and empty-query recall reconstruct eligible states
  before pagination. Hybrid fusion preserves historical keyword provenance and
  counts each input channel once, independently of its provenance labels.
- **Fact consolidation preserves role order and distinct numeric values.**
  Cluster members must be pairwise compatible, so a similar intermediate fact
  does not join otherwise distinct claims.
- **Evidence effects follow the source's current state.** Expired, retired,
  archived and pending-proposal evidence no longer contributes to active support,
  contradiction or decay acceleration; retained source traces remain readable.
  Repeated saves also preserve new source-trace IDs and accepted distinct
  verified contributors instead of reporting those metadata changes as a skip.
- **Nullable skill patches clear existing procedure fields.** Explicit `null`
  clears trigger/verification and `null` or `[]` clears steps; omitted fields are
  unchanged. Clear-only edits do not create empty metadata on ordinary facts,
  and only procedure changes increment an existing skill's version.
- **Extraction validates the model response shape.** Missing or incorrectly
  typed `memories` fields follow the existing rule-extraction fallback; an
  explicit `{"memories":[]}` remains a valid empty result.
- **Windows journal defaults use the native `USERPROFILE` home.** Explicit
  `MEMVAULT_HOME` overrides and native-home tilde expansion retain their documented
  behavior. This is path compatibility, not a claim of Windows ACL isolation.
- **SQLite fact checkpoint restores preserve the live record's management
  binding, including an unmanaged binding.** Reused IDs cannot lose or inherit
  ownership from an old snapshot; any retained guard is suspended atomically
  with its derived index and history/FTS writes.
- **Atomic fact resolution uses one post-lock system transition timestamp.**
  Successor creation/update, predecessor retirement, and its history boundary
  agree exactly, preventing overlapping current facts in `as_of` reads while
  preserving event time, expiry, source order, and ordinary save clocks.
- **REST/MCP write orchestration now shares one service.** REST extraction
  auto-save no longer bypasses delta-write/embedding; repeated identical
  extractions dedup and expose additive per-write outcomes. Vector diagnostics
  inspect the persisted outcome row, and pending vectors use shared best-effort
  backfill. Batch candidates are prevalidated before writes. REST direct-save
  provenance follows the authenticated actor rather than a spoofed body id;
  extraction origin labels remain separate and cannot claim verified identity
  from an admin gate alone. Search defaults/envelopes are unchanged, with actual
  keyword fallback and diagnostics available even for zero hits (REST headers,
  MCP structured content). Expired rows are ineligible for dedup lookup, scan
  and vector fallback: auto-save of expired content creates a fresh visible id,
  preserving the old expiry rather than reviving its row. Live candidates keep
  existing TTL merge behavior. No structured fact-resolution policy is introduced.
- **`as_of` search no longer gives up after three rejected candidates.** The
  historical-FTS recall pulled `top_k * 3` BM25 rows and *then* checked which
  of them was the version actually in effect at `as_of`. BM25 ranks every
  historical version independently and a rejected version does not claim its
  memory's dedup slot, so each one burned a candidate outright: with `top_k=1`
  the entire budget was three rows, which one repeatedly-edited memory could
  exhaust on its own. The failure landed hardest on exactly the memories this
  path exists for — a memory needs the historical index only when its text has
  changed since `as_of`, and changing repeatedly is what gives it many
  snapshots to crowd the ranking with. The scan now pages through the tier
  until it fills `top_k`, the tier runs out, or an explicit candidate budget is
  spent (logged when it is, so "gave up early" stays distinguishable from
  "nothing matches"). A tier whose matches were all the wrong version now also
  falls through to the next, looser tier instead of returning empty.
- **Markdown: a crash mid-approval no longer strands a rule with no way
  back.** `approve_supersede` writes two files and the markdown backend has no
  transaction spanning them, so a crash between retiring the target and
  approving its replacement left *neither* served — retrieval excludes pending
  proposals — with no error and nothing in the logs. SQLite was already atomic
  here; markdown relied on a human noticing the rule had gone missing and
  re-running the approval. Opening the vault now scans for the half-landed
  state (target's `superseded_by` pointing back at a still-pending proposal —
  a mutual link only the approval path creates) and completes it, persisting to
  the markdown file rather than just the cache. Recovery completes rather than
  rolls back: a human did approve this, the retirement already records that
  intent, and un-retiring would resurrect a rule someone explicitly replaced.
  Reported as `recovered_approvals` in the open-time reindex summary and in
  `memvault-cli reindex`.
- **Markdown: a `reindex` pass can no longer roll back a completed approval.**
  The pass read each file and its cache row *before* taking `tombstone_gate`
  and wrote that snapshot afterwards. An approval landing in between meant the
  pass cached a target with no `superseded_by`; since `get` reads only the
  cache, the next approval saw "unclaimed", retired the target a second time,
  and left two live replacements of one fact. `approve_gate` could not help —
  it serializes approvals against each other, not against a reindex pass
  carrying an older snapshot. Each file's read-and-decide step is now a
  separate `plan_file` call, made twice: once outside the gate as a cheap
  "already in sync, nothing to write" hint, and once under it as the decision
  that actually writes. The same staleness in the pass's final orphan sweep —
  which used the directory listing from the start of the pass, so a `save`
  landing mid-pass had its cache row deleted — is now re-checked with a fresh
  stat under the gate. Not reachable from the shipped binaries today (only the
  CLI's `reindex` runs a pass, and it holds the exclusive vault lock), but a
  watch loop added to the server would have made it live.
- **Corroboration votes no longer vouch for a directive nobody saw.**
  `merge_memory` withheld corroboration when a merge *adopted* unreviewed
  material, but not when it merely retained some: two identity-verified agents
  saving the same prose with no `instruction` and no residual tripped none of
  the revocation conditions, so both votes landed and — with
  `MEMVAULT_CORROBORATION_GATE` on — cleared the two-identity threshold for a
  retained `instruction` neither agent had ever carried. `is_trusted` gates the
  memory as a whole, and the whole includes the directive text `tagged_line`
  injects verbatim. Votes are now withheld while an unreviewed directive
  payload survives the merge, and the block is self-maintaining: the count
  cannot creep up one identical save at a time. Existing honest votes are kept
  rather than cleared, and body-only corroboration between verified agents is
  unaffected.
- **Promoted summaries inherit the earliest expiry of their sources.** The
  L1→L2, L2→L3, and fact-consolidation builders created new memories with no
  `expires_at`, and the pipeline's already-expired input exclusion could not
  compensate: a temporary fact ("the contract with vendor X runs until June")
  was consolidated into a *permanent* summary that kept injecting long after
  every source had retired — human approval made that durable. The derived
  bound is now the minimum of the source bounds (`None` only when every source
  is permanent); it can shorten or preserve a lifetime, never lengthen one.
  Making temporary knowledge permanent is a human act — review the summary and
  clear the bound deliberately. See DESIGN §9.6.
- **`sync --watch` regenerates the rules files when a rule expires.** Expiry
  writes nothing — no row changes, `updated_at` does not move — so the
  watcher's `COUNT`/`MAX(updated_at)` change hash never moved and
  CLAUDE.md/AGENTS.md/.cursor rules kept carrying retired rules until an
  unrelated write happened. `sync_state_hash` now folds in the live count of
  expired memories: it flips at the expiry instant (the generation already
  excludes them) and is stable between crossings, so polls do not churn the
  files. The markdown backend delegates the hash to its SQLite cache, so the
  one change covers both stores.
- **`as_of` queries re-filter the RECONSTRUCTED state, not the row that
  matched.** The SQL predicates run against the current row; a point-in-time
  query then projects each hit back to the state it held at `as_of`. Passing
  the former says nothing about the latter: a memory moved from `project:A`
  to `project:B` with its text untouched was recalled by a "project:B + old
  instant" query through the live index and served in its project:A state —
  one project's history leaking into another's results. The store-level
  projection (`project_results_as_of`) now re-runs every non-text predicate
  (namespace, type, priority, event-time window, expiry, currency) on the
  reconstructed state, matching what the historical-FTS recall and the
  semantic/hybrid paths already did. Covered in both directions per field:
  the new value returns nothing, the old value returns the projected state.
- **A wrong historical snapshot no longer shadows the right one.** BM25 ranks
  every snapshot of a memory independently, so when several versions matched
  an `as_of` query, the highest-ranked one used to claim the memory's
  deduplication slot *before* the check that it was the version actually in
  effect — a wrong-era snapshot could then be discarded and take the correct
  one down with it. The slot is now claimed only after a snapshot passes the
  version and filter checks.
- **Pending edit proposals no longer absorb normal saves.** The keyword dedup
  path offered pending proposals (`proposes_supersede_of` set) as merge
  candidates even though the vector path had always excluded them in SQL. A
  save of text near-identical to a pending draft was skipped or merged into
  the draft — invisible to normal retrieval while pending, and deleted with
  it if a human later rejected the proposal: the caller's save died quietly
  on someone else's review decision. Candidate eligibility is now one shared
  predicate (`superseded_by IS NULL AND proposes_supersede_of IS NULL`, the
  vector path's rule) used by both the check and the scan; approved
  proposals, whose link is consumed on approval, stay ordinary candidates.
- **Saves that computed an embedding persist `visibility` and
  `superseded_by`.** The `save_with_embedding` INSERT had drifted from
  `save`'s: both columns were missing, so those saves landed on the column
  DEFAULTs. `visibility` was the damaging half — a `shared` save became
  `scoped` whenever the embedder happened to be up, the same business request
  behaved differently run to run, and `list_shared` never saw the row. A
  full-field round-trip test now guards the column list against future
  drift.
- **Approving an edit proposal is now one atomic storage operation.**
  `proposal::approve` wrote the target's retirement and the proposal's
  approval as two ordered-but-separable store calls. The ordering made every
  failure *recoverable*, but two windows remained: a crash between the writes
  left the vault with the old rule retired and the replacement unapproved —
  no live version to inject — until a human noticed and retried; and two
  *concurrent* approvals of two proposals for one target could both pass the
  "still unclaimed" check and both land, leaving two live replacements of the
  same fact. Both writes now happen inside the new
  `MemoryStore::approve_supersede`: a single `BEGIN IMMEDIATE` transaction on
  SQLite (the check runs under the write lock, making it a compare-and-swap),
  a serialized critical section on Markdown; refusals write nothing and leave
  the proposal reusable, and the retry arm for half-landed legacy state is
  preserved. Regression tests race two real approvals on a file-backed store
  (the in-memory pool serializes every call and hid the race) and inject a
  failure into the second write, asserting the target's retirement rolls back
  with it. See DESIGN §9.3.1.
- **A permanent forget now erases the historical full-text index too.**
  `memory_history_fts` (migration 28) indexes the text of every snapshot so
  `as_of` search can recall a memory by the words it had then, which made it a
  fourth place forgotten content could survive: `purge` erased the snapshots but
  left their index entries, so the retired wording stayed `MATCH`-able. Both
  deletes are in the same transaction. §9.5's promise that only a content hash
  survives in `forgotten_log` now holds.
- **`as_of` search honours the same filters as live search.** The historical
  recall path matched on text and applied nothing else, so a search scoped to
  one project returned another project's history — a cross-namespace leak the
  live keyword path does not have, because `append_filter_clauses` puts those
  predicates in SQL. Namespace, type, priority and the event-time window are now
  re-applied to the reconstructed state.
- **The event-time window binds on semantic and hybrid search.**
  `vector_search` takes only an embedding, a `top_k` and a namespace, so
  `occurred_from`/`occurred_to` constrained the keyword branch alone and a
  semantic search still returned memories from outside the window — including
  undated ones, which a window is defined to exclude. Fixed on both the MCP and
  REST composition paths, after the merge and before reranking so `top_k` is
  filled with in-window memories rather than padded with out-of-window ones.
- **`type_filter` and `priority_filter` bind on semantic and hybrid search.**
  The same defect on two more fields, and the reason the post-merge pass is now
  handed the whole query instead of a time-axes-only copy of it. MCP
  `search_memory` and REST `/api/search` also stopped building their
  `SearchQuery` twice, which is how the two copies drifted in the first place.
- **A TTL supplied on a re-save now takes effect.** Saving near-identical
  content with an `expires_at` was dropped twice over: `metadata_would_change`
  did not notice the new bound, so the write came back `skipped` without ever
  reaching the merge, and `merge_memory` did not copy `expires_at` either. A
  permanent memory therefore stayed permanent no matter how often a TTL was
  supplied for it. An explicit bound now wins in both directions — a re-save can
  also bring an expiry *forward* — while an unstated one is left alone, because
  most saves never consider TTL and treating `None` as "make it permanent" would
  silently immortalise an expiring memory. `friction_evidence` was dropped by the
  same merge and is now adopted when absent.
- **Approving an edit proposal validates its target before writing anything.**
  The approval used to be persisted and its link consumed *before* the target
  was superseded, so a deleted or already-claimed target produced an error *and*
  left an injectable memory marked reviewed whose target was never superseded —
  with no link left to retry through. `MemoryStore` has no transaction spanning
  two calls, so the two writes still cannot be made atomic; they are now ordered
  so that every failure leaves something recoverable, and a retry after a
  partial failure completes rather than refusing.
- **Approving a second proposal for the same target is refused, not doubled.**
  Nothing at propose time forbids two proposals for one memory, and approving
  both overwrote the target's `superseded_by` while leaving the first
  replacement active and unretired — two contradictory versions of one fact,
  both injectable. The second approval now names both the target and the winner
  and leaves the loser pending.
- **`MEMVAULT_MARKDOWN_DIR` is tilde-expanded on the compliance fast path.**
  `open_with_backend` expands it; `compliance-enqueue`'s lock-avoiding shortcut
  resolves the directory itself and was passing the value straight to `PathBuf`.
  With the `coding-agent` profile — which writes a literal
  `~/.memvault/markdown` into the environment — that created
  `./~/.memvault/markdown` under whatever cwd the hook ran in, so compliance
  queued into one cache while everything else read another, and changing
  directory looked like changing vaults. `MEMVAULT_HOME` in the native-embedding
  model cache had the same latent gap.
- **`memory_to_json` emitted `proposes_supersede_of` twice**, once per branch of
  a change merged from two directions, each copy carrying its own justification
  comment. `serde_json`'s map collapsed the duplicate, so it was harmless at
  runtime — and stays undetectable there, which is worth knowing: the new
  contract test pins the key *set*, and a doubled literal is a source-level
  defect that only review catches.
- **`memory://user-profile` bypassed the review gate.** §13.8 enumerates every
  injection channel that enforces `MEMVAULT_INJECT_REQUIRE_REVIEW`, and the MCP
  resources were not in that list — the same structural omission that let `sync`
  through in 0.7.0, whose own write-up predicts the failure mode: "this
  paragraph enumerated the channels that enforce the gate, and `sync` was simply
  not in the list — so nothing looked wrong." An unapproved MUST draft therefore
  reached a resource the server advertises as loaded at conversation start,
  rendered under a wrapper telling the reader not to execute instruction-like
  text, which contradicts the only thing MUST priority means. It now calls
  `apply_unreviewed_gate` rather than reimplementing the predicate, for the same
  reason `sync` does, and gates before `trim_to_budget` so the token budget is
  not spent on memories about to be dropped. `memory://project-context` is left
  ungated on purpose — see the banner above.
- **Homebrew upgrades reload the service instead of leaving launchd on a stale
  code-signing registration.** `post_install` now uses `brew services restart`,
  and the generated service uses `keep_alive successful_exit: false`: abnormal
  exits are restarted, while the `--auto-port` "another instance is already
  serving" exit stays stopped instead of looping.

### Security

- **`top_k` is bounded at every process boundary** (CWE-770/CWE-789,
  code-scanning #16). A caller-supplied `top_k` was an unbounded `usize` that
  sized candidate sets, merge buffers and result allocations before anything
  checked it. REST `/api/search`, MCP `search_memory` and the proxy's
  `search_memory` tool now clamp to `MAX_TOP_K` (1000) up front, and
  `vector_search` clamps again as defense in depth — unlike `search`, it
  previously had no bound at all. `top_k=0` still means "return nothing"; only
  the upper end is trimmed, so no working query changes behaviour.
- **MCP `review_memory` now requires admin credentials.** The
  approve/edit/reject entry point gated on plain `authenticate_agent`, while
  its REST twins (`POST /api/memories/:id/approve|reject|edit`) required
  `authenticate_admin`. Once *any* agent has a registered key, plain auth
  still accepts an invented `agent_id` with no key — so any caller with MCP
  transport access could approve proposals (auto-superseding the live rule),
  edit memory content, or reject drafts under a deployed key regime. Review is
  the human's surface; it now fails closed like the rest of the admin routes.
  Unauthenticated local mode (no keys configured at all) is unchanged.
- **`init` writes `.env` privately from the first byte.** The profile-apply
  path wrote a temp file with `std::fs::write` — `0644` under a typical `022`
  umask — containing the *entire* merged body, existing API keys included, and
  only renamed it into place afterwards. `private_fs::write_private_atomic`
  now creates the temp `O_EXCL` with mode `0600`, fsyncs, and renames, so the
  secret is never world-readable at any point, a stale temp from a crashed run
  is discarded rather than trusted, and an `.env` left at `0644` by an earlier
  version is tightened as a side effect of the replace.
- **Quarantine revokes review when the skill procedure changes.** The
  trust-policy change detection compared only `content` and `instruction`, but
  `skill_meta` renders into the injection just the same: a low-trust writer
  could rewrite a reviewed skill's `steps`, `verification`, or `trigger` while
  keeping body and directive byte-identical, and the new procedure inherited
  the old approval. `apply_trust_policy` now compares the full reviewed
  payload through a single `review_payload_changed` predicate.
- **Quarantine pins namespace moves.** The clamp restricted `scoped → shared`
  visibility but not the namespace, so a low-trust writer could move a
  *reviewed* memory from `project:alpha` to `global` — or to `project:beta` —
  with the content untouched and the approval intact, smuggling an authorized
  rule into every session's (or another project's) injection set. The reviewed
  namespace is now pinned in both directions; migration is a human-reviewed
  edit.
- **A replaced directive no longer rides its predecessor's corroboration
  votes.** With `MEMVAULT_CORROBORATION_GATE` on, `is_trusted` accepts a MUST
  memory on `corroborating_agents` alone — and an unreviewed instruction
  replacement revoked `human_reviewed` while keeping the old votes and
  `identity_verified`, so "Require approval" → "Disable approval" could pass
  the gate on votes cast for the old text. Revocation (both the delta-write
  replacement and `merge_memory`'s unreviewed-absorption branch) now clears
  corroboration and verification, re-recording only a verified incoming author
  as the sole vouch for the text they submitted. See DESIGN §13.7.
- **A reviewed save can no longer launder a stale unreviewed directive.**
  `merge_memory` upgrades `human_reviewed` from a reviewed incoming — but
  instruction/skill_meta merge first-non-empty-wins, so a human saving the
  same body without an instruction kept the *old row's* never-reviewed
  directive and marked the whole result reviewed, injecting it under
  "已人工确认". The upgrade now requires the surviving directive payload to be
  covered by a review (adopted from this incoming, or already reviewed on the
  existing row); otherwise the merge stays pending.

## [0.7.0] - 2026-11-07

> ### ⚠️ Behaviour changes — read before upgrading
>
> **`memvault-cli sync` no longer writes unreviewed, superseded, expired or
> pending-proposal memories into the generated instruction files** (`AGENTS.md`,
> `CLAUDE.md`, `.cursorrules`, `.clinerules`,
> `.github/copilot-instructions.md`, `MEMORY-INDEX.md`). Those files are read by
> Codex, Cursor, Aider, Cline and Copilot at session start, which makes `sync` an
> injection channel — and it was the one channel that bypassed
> `MEMVAULT_INJECT_REQUIRE_REVIEW` entirely. **If your generated files get shorter
> after upgrading, that is the fix, not data loss.** `sync` prints
> `Held back N memory(ies) from the generated files:` with a per-reason breakdown,
> and `MEMVAULT_INJECT_REQUIRE_REVIEW=false` restores the old behaviour for the
> review gate (the other three exclusions are unconditional, because writing a
> replaced, lapsed or still-undecided rule into an instruction file is not a trust
> question). See `docs/TROUBLESHOOTING.md` §8.3.
>
> **Injection now shortens a memory before dropping it.** When the token budget
> runs out a memory is degraded to an excerpt or a summary rather than cut, so more
> of the ranked list is delivered. Shortened text always carries a ` […]` marker
> and the level is reported separately from the skipped list, because a degraded
> memory *was* delivered. MUST memories are never shortened. If you diff injected
> context across the upgrade, expect abbreviated entries that used to be absent.
>
> **Schema migrations 24–28** apply automatically on open: `expires_at` and
> `proposes_supersede_of` (both nullable, so existing rows are unaffected), two
> partial indexes, and the `memory_history_fts` virtual table. All additive — no
> existing data is rewritten. The schema fingerprint reported by
> `memvault-cli status` changes accordingly.
>
> **The canonical rule text injected into every agent changed** (`AGENTS.md` /
> `CLAUDE.md` / `.cursorrules` / `.clinerules` / `.kiro/steering` / `.qoder/rules`).
> It now tells agents to propose edits rather than re-save when correcting an
> existing memory, because re-saving merges through delta-write and an unreviewed
> merge revokes the target's review flag. Regenerate your rule files with
> `memvault-cli sync` after upgrading.
>
> No public API was removed. `Memory`, `SearchResult`, `SessionStartOutput`,
> `SessionInjection`, `LoadReport`, `DecayReport`, `PromoteResult` and
> `SyncReport` each **gained** fields — additive for JSON consumers, but a
> source-breaking change for Rust code that constructs those structs with literal
> syntax. That, plus the behaviour changes above, is why this is a minor bump
> rather than a patch.

### Added

- **`scripts/e2e-release-check.sh` — an 81-assertion end-to-end release gate, run
  by CI on every push.** It drives the real built binaries and a real REST server
  across every user-visible behaviour this release added or changed: profiles
  (list, dry-run, write, idempotence, unknown-name refusal, env-held keys left
  alone), expiry (excluded from search, still visible in `list` with a marker,
  never injected from the shared pool), all three time axes (event-time window
  selection, inverted window → 400, malformed stamp → 400, `occurred_at` on the
  direct-save path), `as_of` travel (historical recall tagged `hist-kw#`, snapshot
  vs live provenance, `None` before creation, `list --as-of` including a
  since-deleted memory), the full proposal lifecycle (pending → target still
  injecting the **original** directive → approve → new directive injected; plus
  reject, ambiguity refusal, and the `directive_still_says_old` warning), progressive
  disclosure under a 120-token budget (at least one memory delivered where the old
  behaviour delivered zero, degraded and skipped reported separately, MUST verbatim
  despite the budget), `sync` exclusions asserted against the **bytes of
  AGENTS.md** with a control that a live MUST is still written, decay's
  `expired_archived`, both new doctor checks, the `update` command, and the
  export/import round trip through the directory form.
  It exists because four defects in this release were visible *only* end to end,
  and all four lived in the wiring between layers where every layer's own tests
  passed: an edit that approved cleanly and changed no behaviour, a second
  injection channel that bypassed the review gate, a provenance field dropped by
  the reranker, and a skip message that pointed the user at the wrong place. The
  script's header names all four so the next person knows what it is for.
  No LLM key and no embedding provider needed — everything runs on the
  deterministic paths, which is what makes it safe to gate a release on. It asserts
  on observable behaviour (CLI stdout, REST JSON, generated file bytes) and never on
  internal state. Three runs are green and stable.
  Writing it found two more bugs in the *tests* rather than the product, both worth
  recording because they are the kind that make a suite lie: an assertion that
  checked `PROP_OUT` for a warning that only the content-only proposal emits (so it
  demanded the wrong behaviour), and a target memory saved without `--force`, where
  delta-write could merge it and quietly drop the `instruction` every subsequent
  assertion depended on. The script now asserts the instruction was stored before
  relying on it — a test that passes because the field it checks is absent is worse
  than one that fails.
- **`memvault-cli update <id>` — in-place editing of an existing memory.** The
  gap was narrow but real: `expires_at` could only be set at `save` time, so a
  deadline that moved or a freeze lifted early had no CLI path at all and the
  memory stayed retired and uninjected. The documented workarounds were the
  Dashboard or a hand-built `curl`.
  Only the flags passed are changed; everything else is left alone. The two
  timestamp fields use the same clearable tri-state as REST — a value sets,
  `--clear-expires` / `--clear-occurred` lifts, omitting leaves untouched — and
  `--expires-at`/`--clear-expires` are declared `conflicts_with` each other so the
  ambiguity cannot arise. Clearing one axis never touches the other, which is the
  §9.6 rule that the two are not folded; a test pins it.
  Two details are deliberate. **Changing `--priority` also moves the layer**
  (MUST→L3, REFERENCE→L2, BACKGROUND→L1), because the layer is what the promote
  pipeline and injection ordering read — leaving it behind would let a demoted
  MUST keep its L3 persona slot. And **editing content or instruction invalidates
  the stored vector**: the new text is re-embedded when a provider is available
  and the stale vector cleared otherwise, so semantic search cannot keep matching
  the *old* wording. That mirrors REST `PUT /api/memories/{id}`; two paths that
  disagreed here would leave one of them serving stale vectors. When the vector is
  cleared the command says so and names `backfill-embeddings`.
  Invoking `update` with no fields is a usage error rather than a silent no-op
  with exit 0, which would leave the user believing an edit landed. `--unreview`
  states its consequence inline: with `MEMVAULT_INJECT_REQUIRE_REVIEW` on (the
  default) the memory stops being injected into any session — the supported way to
  retire a rule you no longer trust without deleting its history.
- **Two `doctor` checks.** `orphaned_proposal` (Warn) finds proposals whose target
  was deleted or forgotten while the proposal waited — approving one cannot do
  what the reviewer was shown, since `supersede` would fail on a missing row or
  silently leave the approved text replacing nothing. The review UI already
  renders this, but a proposal can sit in the inbox for weeks and its target can
  disappear *after* it was listed; this is what finds them without opening each
  one. A target that has itself been superseded is reported too, as a re-check
  prompt rather than an error: approving still works, it just may not be what the
  proposer meant. `expired_unarchived` (Info) finds memories past their
  `expires_at` that were never archived. Not a correctness bug — expiry already
  removed them from retrieval — but archiving is what connects expiry to the
  auto-forget sweep, which measures retention against `archived_at` (§9.5), so an
  expired-but-never-archived memory lingers forever: invisible to retrieval and
  unreachable by the purge. The detail names the fix (`memvault-cli decay`).
- **Point-in-time memory reads (`as_of`) — valid-time travel.** Answers "what
  did MemVault believe at T", which the version chain already supported but
  could not express: `write_history` snapshots the *pre-operation* row on every
  update and delete, so snapshot 1 was current during `[created_at, t1)`,
  snapshot 2 during `[t1, t2)`, and the live row during `[tn, now)`. The state at
  `as_of` is the first snapshot whose `changed_at` postdates it, falling back to
  the live row. **No new version schema was needed** — this closes the
  "Temporal / time-travel" row in `docs/DESIGN.md` §2.5 that MemVault was losing
  to Zep and agent-memory.
  Three paths with deliberately different guarantees:
  `get_as_of(id, at)` / `memvault-cli as-of <id> --at <T>` is exact and returns
  *nothing* rather than the current row when there was no state then (not created
  yet, already deleted, or past its expiry);
  `list_as_of` / `memvault-cli list --as-of <T>` is exact and enumerates live ids
  ∪ `memory_history` ids, so a memory deleted *after* `as_of` still appears — at
  that instant it was the current truth;
  `search(as_of=…)` / `search --as-of` / `search_memory` / `POST /api/search`
  filters and reconstructs correctly but still **matches against current index
  text**, so a memory whose wording has since changed may not be recalled. That
  limitation is reported rather than hidden: every result carries
  `as_of_source` (`live` vs `history` + which snapshot and when it was replaced),
  and the CLI prints the caveat up front. Exact historical-text search needs an
  FTS index over snapshots and is tracked in §15.
  Supersession composes correctly by construction — because `supersede` goes
  through `update`, the pre-supersede snapshot has `superseded_by: None`, so
  querying a superseded memory before the supersede returns it as the current
  truth it was then. Reranking uses `as_of` as its reference instant when set, so
  "recent" means recent relative to the moment being asked about. On the markdown
  backend, point-in-time reads only see changes since the last reindex (the cache
  is derived and a snapshot is not a file) — documented on the impl, not buried.
  `as_of_source` is carried through every stage that rebuilds a `SearchResult`
  (the reranker reconstructs one per hit; `HybridMerger` mutates in place) with a
  regression test on both the enabled and disabled rerank paths — dropping it
  there would have made every point-in-time result on the MCP/REST paths
  indistinguishable from a current one, which is the single worst failure mode
  this feature has. The projection is applied once per request: inside
  `MemoryStore::search` for the keyword path, and after the merge for
  semantic/hybrid, whose `vector_search` calls bypass the store's own projection.
- **`expires_at`: a valid-time bound so temporary facts retire themselves.**
  "The deploy freeze runs until Friday" previously decayed slowly and kept being
  re-injected as stale truth. Once past the bound a memory leaves retrieval and
  injection but stays inspectable and restorable — expiry retires a fact, only
  `forget` erases one — matching how superseded memories already behave. Filtered
  in every path that can reach an agent's context (keyword search, vector search,
  and `list_shared`, which injects into *every* session and was the most damaging
  place a lapsed fact could surface), and deliberately **not** filtered from
  `list`, which is the inspection path. Settable via `save_memory` /
  `POST /api/memories` / `memvault-cli save --expires-at <RFC3339>` (or
  `--ttl 30m|12h|7d` as a shorthand). The decay cycle now archives expired
  memories regardless of `decay_score` **and regardless of `must_exempt`**: that
  config protects a MUST from an *inferred* signal (disuse), while `expires_at`
  is an *explicit* declaration, and a MUST whose window has closed is a binding
  instruction that is no longer true. `DecayReport.expired_archived` keeps the
  count visible instead of folding it into `archived`; archiving on expiry is also
  what connects expiry to the auto-forget sweep, which measures retention against
  `archived_at`. Round-trips through the markdown backend via an `expires:`
  frontmatter key.
- **Three time axes, documented as a constraint rather than an accident**
  (`docs/DESIGN.md` §9.6): ingest-time (`created_at`), event-time
  (`occurred_at`, already shipped) and valid-time (`expires_at` + `as_of`). They
  are never folded into one column, because folding them is what makes "what did
  we believe in August" unanswerable without also losing "when did this actually
  happen" — and un-folding them later is a breaking migration. An event-time
  *filter* (`occurred_from`/`occurred_to`) is still open (§15).
- **Schema migrations 24–25.** `memories.expires_at TEXT` (nullable; absent =
  never expires, so every existing row is unaffected) and a partial index
  `idx_memories_expires_at … WHERE expires_at IS NOT NULL` — partial because the
  overwhelming majority of memories never expire, so a full index would be nearly
  all NULLs. `expires_at` is written in a fixed-width RFC3339 form
  (`…T00:00:00.000000Z`) rather than `to_rfc3339()`'s variable precision, which
  is what makes plain lexicographic SQL comparison exactly chronological and
  therefore keeps the index usable instead of forcing a `datetime()` scan.
  `as_of` added no schema at all. The schema fingerprint reported by
  `memvault-cli status` changes accordingly.
- **Scenario profiles: `memvault-cli init --profile <name>` and
  `MEMVAULT_PROFILE`.** Four named bundles — `coding-agent`, `assistant`, `team`,
  `edge-lite` — replacing "read a 12KB `.env.example` and reason about fourteen
  independent switches" with one decision. Every setting carries a rationale that
  `init` prints and writes into the generated file as a comment, so the file
  explains itself six months later. `init --list` shows all four with every
  setting and its reason; `--dry-run` previews without writing.
  A profile is the **lowest**-precedence layer (CLI flag > process environment >
  env file > profile > built-in default) and is strictly additive: it fills only
  keys no higher layer set, so leaving one configured permanently can never
  silently undo hand-tuning. Three properties were deliberate:
  - **No secrets, ever.** A profile names a provider (`native`, `ollama`,
    `openai`) and never writes an `*_API_KEY`, because `init` writes a file users
    routinely commit or share. Asserted by a test that scans every profile.
  - **An unknown name applies nothing.** No fallback to a default profile — a
    typo must not silently apply a different profile's fourteen settings. The
    error names the offending value and lists the valid ones.
  - **No knobs MemVault does not have.** The comparable feature in agent-memory
    (§2.3) configures graph backend, bi-temporal on/off, multimodal normalisers
    and edge/cloud deployment. MemVault has none of those to select, so exposing
    them would be theatre.
  `edge-lite` is the interesting one: it sets `MEMVAULT_EMBEDDING_PROVIDER=off`
  and MUST memories still inject, because §6.1.2 resolves them deterministically
  with zero embedding calls. No embedding provider is therefore a *supported*
  configuration rather than a degraded one. `team` pairs `CORROBORATION_GATE`
  with `IDENTITY_VERIFICATION` on purpose — the gate counts identity-verified
  agents, so enabling it alone would count self-declared ids, which is exactly
  the spoofing §13.7 exists to stop. It also sets
  `MARKDOWN_TRUST_MODE=quarantine` despite selecting the sqlite backend, so that
  switching backends later cannot silently inherit `owned` — which §8.3.4 states
  plainly is session-injection authority for anyone who can write the directory.
  `init` refuses to modify a key that is already set and offers no `--force`, for
  a concrete reason: inside an env file the **first** occurrence of a key wins, so
  appending a "replacement" line would be silently inert — the user would see
  their new value in the file and the old one at runtime. Changing an existing
  key is a hand edit, and the output says so. Writes go through a sibling temp
  file plus rename, because a crash mid-write must not leave a truncated env file
  that silently drops half the configuration. `init` is dispatched before the
  store opens, alongside `server` and `dashboard`: the moment you most need
  `init --profile edge-lite` is when the current configuration will not start,
  and requiring a working store to fix the store's configuration would be
  circular.
- **A distinct `profile` config-provenance source.** `memvault-cli status` now
  reports `env / file / profile / default` per key, and names the active profile
  with how many settings it supplied. Without this, profile-applied keys were
  indistinguishable from real exports — `apply_to_env` writes through `set_var`,
  so all of them would have read as `env`, leaving the user to wonder which
  settings they had chosen. Hiding the lowest layer makes the highest one look
  arbitrary. `MEMVAULT_FORGET_*` and `MEMVAULT_SERVE_WEB` were added to
  `CONFIG_KEYS` at the same time: they were read by `forget.rs` and `decay.rs`
  but never tracked, so a profile setting them produced values `status` could not
  show — and auto-forget is irreversible, which makes it the worst possible place
  for an invisible setting.
- **Review-gated agent edits: `propose_memory_edit` / `propose-edit` /
  `POST /api/memories/{id}/propose-edit`** (`docs/DESIGN.md` §9.3.1,
  `crate::proposal`). An agent can now correct a memory it did not author —
  including a binding MUST — by exact unique-`old_text` replacement in either the
  `content` or the `instruction` field, without being able to change anything by
  itself. The edit lands as a **pending memory** carrying
  `proposes_supersede_of`; the target is left completely untouched and keeps
  injecting. Approving supersedes the target with the edited version; rejecting
  discards the proposal.
  The obvious alternative — apply the edit in place and revoke its trust — looks
  equivalent because §13.8's gate excludes untrusted memories from injection
  anyway. It is not: the target would stop injecting too, so **any agent could
  suppress any MUST merely by proposing an edit to it**, a denial-of-injection
  vector aimed at exactly the threat model §13.7 exists for. Keeping the target
  live until a human decides costs one extra row and closes it. Verified end to
  end against a live server: injection renders the old directive while the
  proposal is pending, and the new one only after approval.
  Other properties, each with a reason. **Trust is never inherited** — a proposal
  resets `human_reviewed`/`ai_generated`/`identity_verified`/
  `corroborating_agents`, so a reviewed, corroborated MUST cannot lend its
  authority to an agent's rewrite of itself — while everything that makes it a
  *replacement* rather than a downgrade is inherited (priority, namespace, type,
  layer, visibility, `occurred_at`, `expires_at`; rewording a temporary fact must
  not quietly make it permanent). **Ambiguity is refused, never resolved**: zero
  matches and 2+ matches both error and name the count, with non-overlapping
  counting so `"aa"` in `"aaa"` is one match. **Pending proposals are excluded
  from retrieval** alongside superseded memories, since returning a memory next to
  its near-identical pending edit leaves the caller unable to tell which is real;
  they stay visible through `list_pending`/the Inbox. **Authorization is split by
  consequence**: proposing authenticates as an agent (requiring admin there would
  make it useless to the only caller that needs it), approving requires admin.
  Approval is routed through one core function so that every approve path
  (REST approve, REST inbox-edit-approve, MCP `review_memory` approve and edit,
  CLI `review --approve`) applies the supersede — previously each flipped
  `human_reviewed` inline, which would have left two live near-identical memories
  and no record that one was meant to replace the other. The review queue now
  labels a proposal as `[proposes edit to <target>]`, because approving one has a
  consequence on another memory that a reviewer must be able to see first.
  Migrations 26–27 add `proposes_supersede_of` as a real column plus a partial
  index. It is a column rather than a tag because approving it triggers a
  destructive action on a *different* row, and an action like that should be keyed
  on a validated field rather than a string prefix in a user-editable list.
  Round-trips through the markdown frontmatter, since in that mode the file is the
  source of truth and losing the key on reindex would orphan the proposal —
  approving it would then flip the review flag and never supersede, silently
  leaving two live near-identical memories.
- **`directive_still_says_old`: detection of an edit that would change nothing.**
  Found by running the real flow end to end, not by the unit tests. Injection
  renders `instruction` as the directive and `content` as the body (§6.2), so an
  agent that corrects only the body leaves the thing every future session actually
  obeys saying the old thing — the proposal approves cleanly, supersedes the
  target, and changes no behaviour, while leaving body and directive contradicting
  each other. Reported as a CLI warning carrying the exact follow-up command, and
  as a `directive_still_says_old` field on the MCP/REST responses. Deliberately
  *reported* rather than silently patched: rewriting the instruction too would be
  a guess about text the caller never asked to change, and that decision belongs
  to the proposer.
- **Progressive disclosure under the injection budget**
  (`crate::disclosure`, `docs/DESIGN.md` §6.5). `trim_to_budget` used to walk
  ranked candidates and **break** at the first one that did not fit, cutting it
  and everything behind it. The cut was reported rather than silent, but
  reported-or-not the agent did not get the memory — with the default 1500-token
  budget and a few long memories, the tail of the ranked list was routinely
  unreachable however relevant it was.
  It now **degrades before dropping**: `Full` → `Excerpt` (leading ~320 chars at
  a sentence, else word, boundary) → `Summary` (the first sentence when usable,
  else ~96 chars), taking the first level that fits the *remaining* budget so a
  memory is shortened only as far as necessary. Only when even the summary does
  not fit does the old break-and-report path apply. Measured on a live server
  with `token_budget: 120` and four distinct long memories: the previous logic
  injected **zero**; this injects one at excerpt level and reports the other
  three as `token-budget-exceeded`. Nothing is silently dropped either way.
  This is not "cram more in" — §3.2 establishes that follow-through *falls* as
  the number of injected memories rises, so the goal is keeping more memories
  reachable at a lower per-memory cost.
  Two invariants. **MUST is never degraded**: "never filtered, never truncated"
  is the L3 claim (§11.1), and a binding instruction whose qualifying clause was
  summarised away is worse than one that overflowed the budget. **A degraded
  memory says so**: the shortened text carries a ` […]` marker and the level is
  reported through `TrimReport::degraded` → `SessionStartOutput.degraded` → REST
  `/api/session`. An abbreviation that looks complete is worse than a drop, since
  the agent would act on a rule with its conditions silently removed. `degraded`
  is kept separate from `skipped` because they are different outcomes — a
  degraded memory *was* delivered — and folding them together would report a
  delivered-but-abbreviated rule as a dropped one.
  The layers are **derived on demand, not stored**, because they are
  deterministic and extractive: no migration, no backfill, no frontmatter key,
  and above all no staleness — a stored summary sitting next to content that was
  later edited is a contradiction the agent would be shown as fact, and keeping
  the two in sync would need a hook on every write path (`update`, `supersede`,
  markdown reindex, proposal approval, `restore`). `L2` is `Memory::content`
  itself, never a third copy. An LLM-generated summary added later should be a
  stored `Option<String>` these functions *prefer* when present, not a
  replacement, since the derived form is what keeps disclosure working with no
  provider configured (`edge-lite`, §7.5).
- **The fusion trap from §2.3 was investigated and does not apply here — with the
  constraint recorded and pinned.** agent-memory must apply MaxP per
  (channel, unit) *before* cross-channel fusion because it indexes L0/L1/L2 as
  separate physical entries of the same channel; otherwise a memory with more
  layers is recalled more often and RRF accumulates each as independent evidence,
  systematically over-weighting long memories **while the metrics look like they
  improved**. MemVault's layers are injection-time derivations that are never
  indexed: FTS covers `content`/`instruction`/`tags`, one row yields one
  candidate per channel, and `HybridMerger::merge` already keys its accumulator
  by `memory.id`. Adding l0/l1 to the FTS index would reintroduce that bug and
  also distort BM25 by indexing a prefix of text already indexed.
  `disclosure::tests::test_disclosure_layers_do_not_change_hybrid_ranking` pins
  the invariant, asserting each fused score equals exactly the RRF sum for its
  own rank with one contribution per channel — so the next person to consider
  indexing the layers gets a failing test and an explanation rather than a quiet
  regression in ranking quality.
- **The Web Dashboard can now review edit proposals, see expiry, and see which
  memories were shortened.** Four fields the backend had been returning since
  earlier in this release were consumed by no UI, which left the proposal feature
  unusable through the surface §13 designates as the primary review interface.
  `MemoryView` gains `expires_at`, `archived_at`, `proposes_supersede_of` and an
  optional `proposal` block.
  The Review tab renders a proposal as what it is: a badge saying "proposed edit —
  not a new memory", a side-by-side diff of the target's current text against the
  proposal, the target id, and the consequence of each action (approve supersedes
  the target; reject deletes only the proposal). Without that a proposal is
  indistinguishable from a near-duplicate draft, and the natural action is to
  reject it as a duplicate — silently discarding a correction. A warning is shown
  when the target has since been forgotten or superseded, since approving then
  does something other than what the reviewer expects.
  `REST /api/inbox` was extended to attach the target's current text and
  instruction, mirroring the MCP `list_inbox` tool, so the diff needs one call
  rather than a lookup per proposal. The Session preview lists degraded memories
  **separately** from skipped ones, because they are different outcomes — a
  degraded memory was delivered, just shortened (§6.5) — and filing it under
  "dropped" would tell the admin a rule never reached the agent when it did.
  The memory detail panel shows `expires_at` with its lapsed state (which is what
  answers "why is this not being injected") and `archived_at` (which answers "why
  is this about to be permanently purged", since auto-forget measures retention
  against it). The create/edit form gains an `expires_at` field that sends an
  explicit `null` when left empty rather than omitting the key — the REST field is
  clearable, so an absent key would leave an existing bound untouched and the form
  could never remove an expiry. It is a plain RFC3339 text input rather than a
  `datetime-local` widget on purpose: the stored form is UTC, and round-tripping
  through a local-time control would silently shift the bound by the browser's
  offset, which for "the freeze ends Friday midnight" is the entire meaning.
  Bilingual labels added for every new string; embedded web assets re-synced via
  `scripts/sync-dashboard-assets.sh`, which CI fails on drift.
- **The Dashboard can now *query* by time, not just display it.** The search tab
  gains a collapsed "Time filters" panel with `as of` (valid-time) and
  `occurred from` / `occurred to` (event-time), and results render their
  `as_of_source` provenance so a reconstructed state is never read as the current
  one. Displaying the fields without a way to query by them would have left time
  travel reachable only from the CLI, MCP and REST — i.e. not from the surface
  §13 designates as the primary one.
  Three choices worth recording. The panel is **collapsed by default** and shows
  an "(active)" marker on the toggle when a filter is set, because a
  collapsed-but-active filter would otherwise silently change every subsequent
  search with no visible indication. The inputs are `datetime-local`, which is
  correct *here* and deliberately different from the `expires_at` form field: a
  query instant means "9am my time" and converting to UTC is exact, whereas a
  stored bound converted through a local-time widget would be silently shifted by
  the browser's offset — for "the freeze ends Friday midnight" that shift is the
  entire meaning. And unset filters are **omitted from the request rather than
  sent as null**: unlike the form there is nothing to clear, and an empty string
  would be a 400 on the backend. The hint text states both axes' meanings and that
  an event-time window never matches undated memories, because that is the result
  most likely to be misread as "nothing happened".
- **Event-time filtering: `occurred_from` / `occurred_to`.** "What happened in
  March" was not directly queryable even though "what did we believe in March"
  was, because `occurred_at` was stored but not filterable. Added to
  `SearchQuery`, `search_memory`, `POST /api/search` and
  `memvault-cli search`.
  Two separate parameters rather than one `since`/`until` range, per the rule
  §2.3 took from agent-memory that the axes are never folded: a memory can have
  *happened* in March and still have been *believed* in August, and a single
  shared range would make one of those two questions unanswerable — with which
  one it loses depending on the caller's guess.
  `test_event_time_and_valid_time_axes_stay_independent` pins exactly that case:
  the same memory, the same month, opposite answers from the two axes.
  **Undated memories never match a window**, deliberately — an undated memory
  cannot be said to have happened inside a range, and admitting it would return
  the same rows for every window, which is noise dressed as a result. Because
  most memories are undated, the CLI states the rule whenever the flag is used
  rather than only when the result comes back empty. An inverted window is a 400
  and not an empty result, because an empty result would be *correct* and
  completely unhelpful — it sends the caller to debug their data instead of their
  typo.
  Compared through SQL `datetime()` rather than as raw text, unlike `expires_at`:
  that field is written in a fixed-width form precisely so lexicographic
  comparison stays chronological and its partial index stays usable, whereas
  `occurred_at` uses `to_rfc3339()` with variable fractional precision and has no
  index, so there is nothing to lose by normalising both sides.
- **`occurred_at` is now settable on the direct-save path** — `POST
  /api/memories`, `PUT /api/memories/{id}` (clearable tri-state), MCP
  `save_memory`, and `memvault-cli save --occurred-at`. It was previously
  reachable only through the *extraction* endpoints, which was coherent in
  principle (event-time describes the source conversation an extraction came
  from) but left a dated transcript ingested by direct save with no way to carry
  its dates — making the event-time window above unusable for exactly the
  material that has dates. Found while building the BEAM harness, which resolves
  a session date for every probe and had nowhere to put it.
  Parsing is shared through `models::parse_optional_stamp` so the CLI, MCP and
  REST surfaces cannot drift into different tolerance for the same flag, and a
  malformed stamp is an error rather than a silent `None` — a mistyped date that
  quietly became "undated" would lose the axis with no signal, and nothing
  downstream could tell it had ever been supplied. Blank and whitespace-only
  input means "not supplied", not "parse error", because a caller that
  round-trips every field sends `""` for the ones it has no value for.
- **Historical-text search: `as_of` now recalls by the text a memory had then.**
  This closes the limitation recorded in §9.6 when `as_of` shipped — matching ran
  against the current index only, so "what did we believe about X in August"
  missed exactly the memories that had changed since, which are the interesting
  ones. Migration 28 adds `memory_history_fts`, and `search_history_at` unions a
  historical pass with the projected current-index pass.
  Three details are load-bearing. Snapshots are indexed through the **same
  `fts_columns` helper** as the live index, so the two can never tokenize one
  memory differently — indexing from the raw JSON with `json_extract` would have
  been shorter and subtly inconsistent. A historical match is kept **only when
  that snapshot is the one actually in effect at `as_of`**: a memory edited three
  times has three historical versions and only one was current then, so without
  this check the query would return text the memory did not hold at the instant
  being asked about. And such hits carry a distinct `hist-kw#N` provenance
  (`HitSource::HistoryKeyword`), because "matched today" and "matched then"
  answer different questions.
  That last one required a fix elsewhere: the single-path provenance stamping at
  the end of `search` overwrote `hit_sources` unconditionally, which would have
  relabelled every historical hit as a current-text match and erased the one
  signal distinguishing the two. It now fills only empty `hit_sources`.
  The index is backfilled on open by count comparison, the same self-healing
  shape as `rebuild_fts_index`, so an upgraded vault's existing snapshots become
  searchable with no repair command; an unparseable snapshot is logged and
  skipped rather than preventing the store from opening.
  **Residual, documented rather than hidden:** this is the keyword path only. The
  vector path still embeds current text, so a reworded memory is reachable by its
  old *words* but not by its old *meaning*. Covering it would mean embedding
  every snapshot and keeping those vectors in step with the history table — a
  much larger change for a narrower gain, since the failure being fixed was "the
  wording changed", which is lexical.

### Changed

- **`docs/INSTALL.md` §1.6.1 and the `docs/RUNBOOK.md` REST reference now cover
  what this release added.** The install guide is where a new user decides how to
  configure MemVault, and it did not mention scenario profiles at all — which
  defeats the point of adding them, since the alternative it left was reading a
  12KB `.env.example`. §1.6.1 gives the four commands, a one-line-per-profile
  table, and the two properties that make profiles safe to leave set (lowest
  precedence; refuses to touch an already-set key and says why per key), plus the
  `MEMVAULT_PROFILE` runtime equivalent and the fact that an unrecognised name
  applies nothing rather than falling back.
  The RUNBOOK's endpoint table was missing `POST /api/memories/{id}/propose-edit`
  entirely, and its `POST /api/search` row did not mention that the endpoint now
  takes two independent time axes. Updated: search (both axes, the keyword-only
  caveat on historical recall, and that an inverted window is a 400 rather than an
  empty result), save and update (`expires_at` / `occurred_at`, with the clearable
  tri-state spelled out), session (`skipped` vs `degraded` are different outcomes
  and are reported separately), decay (`expired_archived` broken out), and all
  three inbox rows — including that approving a proposal supersedes its target and
  names it in `superseded_target`, which an operator reading only "approve" would
  not expect.
- **Agent-facing docs now cover the temporal and proposal features.** The four
  skills said nothing about any of them, which is a discoverability gap rather
  than a documentation nit: an agent that follows `memvault-save` would never
  learn `expires_at` exists and would keep saving temporary facts as permanent
  ones, and one that follows `memvault-recall` would never learn it can ask what
  was true last month. `memvault-save` gains guidance on `expires_at`/`--ttl` for
  time-bounded facts, on `occurred_at` as the separate "when did it happen" axis,
  and on using `propose_memory_edit` rather than overwrite-or-delete-and-resave
  when correcting a memory it did not author (including that editing
  `instruction` changes the directive while `content` changes only the body, so
  proposing one when both say the same thing changes no behaviour).
  `memvault-recall` gains the two-axes distinction with the failure mode named:
  an empty `occurred_from`/`occurred_to` result usually means "undated", not
  "nothing happened". Edited in the canonical `plugins/memvault/skills` and
  propagated with `scripts/gen-rule-copies.sh` rather than by copying files, which
  is what the parity check expects.
- **`docs/TROUBLESHOOTING.md` §8.3: "a rule disappeared from AGENTS.md after
  upgrading".** The `sync` exclusion change is correct but visible, and a user who
  upgrades to find their instruction files shorter needs the answer in the doc
  whose whole format is symptom → cause → fix. Covers all four held-back classes
  with a way to check each, the `Held back N memory(ies)` breakdown `sync` already
  prints, the fix per cause, and the `MEMVAULT_INJECT_REQUIRE_REVIEW=false` escape
  hatch — with the consequence stated, since that switch also turns the gate off
  for session injection and the two channels deliberately share one trust rule.
  Also distinguishes this from the lookalike case where `sync` reports
  `Synced 0 memories` because it is pointed at a different database (§5.3).
  Every command in the entry was run against the built binary first; an earlier
  draft cited a `MEMVAULT_REST` variable that does not exist.
- **MCP `search_memory`'s tool description names the two time axes.** The
  parameters had doc comments, which become JSON-schema descriptions, but the
  top-level description still listed only the pre-existing options — and that
  description is what an agent reads when deciding what the tool can do. It now
  states that `as_of` is valid-time and recalls from the historical index, that
  `occurred_from`/`occurred_to` is event-time and never matches undated memories,
  and that the two are not interchangeable. `docs/DESIGN.md` §8.3's parameter list
  gains the two new fields for the same reason.
- **README's "Why MemVault" now compares against real 2026 competitors instead of
  strawmen.** The old table measured MemVault against "Plain CLAUDE.md" and
  "Vector DB + RAG" — options nobody evaluating a memory layer is actually
  choosing between, and which MemVault beats on every row by construction. A table
  that cannot be lost is not evidence. It is replaced with three sections drawn
  from the §2.5 survey: what only MemVault does (six capabilities, each naming who
  else has it, which is mostly "nobody" with the nearest attempt identified); what
  MemVault does differently against four alternatives a reader would really
  consider (Claude Code Auto Memory, Codex Memories, Mem0, memSearch); and — the
  part that makes the other two credible — **where MemVault is behind**, naming the
  leader for each: multimodal input, graph depth, cloud/team tier, vector-path
  time travel, and published benchmark scores. Every claim links to §2 so it is
  falsifiable rather than asserted.
  The framing sentence changed too. "Looking for a self-hosted alternative to Mem0
  or Zep?" was written when auto-injection was MemVault's differentiator; §2.2
  established that the Code Agents now do that natively, so the opening names the
  part that is still uncontested — one shared corpus across every agent, what
  entitles a memory to bind, and whether it was obeyed.
  The latency row carries an explicit caveat that the comparison is **not**
  like-for-like: Mem0's published p50 covers its whole retrieval pipeline while
  ours is one `/api/session` call from a 20-probe plumbing run on a local debug
  build with no embedding provider. The defensible claim is narrower and is stated
  as such — MUST resolution never touches an embedding model, so its latency does
  not depend on one being installed, warm or reachable. Publishing the number
  without that caveat would be exactly the incomparable self-report §2.6 warns
  against, in the same release that wrote the warning.
  `README.zh-CN.md` was updated in the same change; syncing it afterwards is how
  the drift corrected earlier in this release happened in the first place.
- **`README.zh-CN.md` brought back in sync with the English README.** It had
  drifted to the point of covering none of this release's five features — CI only
  checks the version tables (`scripts/check-release-versions.py`), never prose, so
  the drift is silent and stays that way unless someone looks. Added: time travel
  (`as_of`), self-expiring facts (`expires_at`), review-gated edit proposals, the
  degrade-before-dropping budget behaviour, the disposable-index property (both
  backends, including the markdown `memory_history` asymmetry), the `init` /
  `as-of` / `propose-edit` / `list --as-of` command rows, the
  `propose_memory_edit` tool row, `MEMVAULT_PROFILE` plus the corrected precedence
  chain, the `sync` exclusion-and-reporting behaviour, and the experiments index
  entry distinguishing hypothesis validation from the external benchmark harness.
  A duplicated `propose_memory_edit` row introduced while editing was removed.
- **`scripts/check-rule-parity.sh` now also gates the four command variants.**
  They are not copies — each host gets its own wording — so byte parity cannot
  apply, and a rewrite that dropped a safety-critical instruction would have
  passed CI silently. They are checked by content instead: each must still carry
  the `proposes_supersede_of` note that tells a reviewer a pending proposal is
  not a near-duplicate draft (§9.3.1). Verified with a negative test (removing
  the note from one variant makes the script exit 1 and name the file), since a
  gate that cannot fail is not a gate. The skill copies were already covered —
  `plugins/memvault/skills` is canonical and `skills/` plus `.openclaw/skills`
  must be byte-identical — and an earlier entry in this release incorrectly
  claimed otherwise; that claim is corrected here and in §9.3.1.
- **README now states the disposable-index property explicitly**, and the
  Documentation index distinguishes the two kinds of evidence in
  `docs/experiments/` (MemVault's own H1–H7 hypothesis validation vs. the
  third-party BEAM harness). The index claim was stale — it still described the
  directory as only H1–H7 after the BEAM harness landed. The new README bullet is
  deliberately scoped to what is actually true per backend: in markdown mode the
  `.md` files are the memory and `.memvault-cache.db` is a shadow index you can
  delete; in sqlite mode `data.db` is the source of truth and only the parts
  inside it self-heal (FTS rebuilds on open when its row count disagrees with
  `memories`; the embedding column is droppable and backfillable). It also states
  the one asymmetry rather than hiding it — `memory_history` lives in the cache,
  so a rebuilt markdown cache loses the version chain and `as_of` falls back to
  the live row (§9.6).
- **`docs/DESIGN.md` §2 market analysis refreshed against the 2026 competitive
  landscape, and §11/§12 recalibrated to match.** The previous reference set
  (MemPalace, TiMEM, Memoria, Khoj, Smart Connections, Claudian) predates the
  current market: none of those six appear in any 2026 survey, and MemPalace —
  previously treated as the closest peer and as a pluggable storage backend —
  has left the market entirely. §2 now covers fifteen shipping products across
  four tiers (framework-agnostic memory layers; **platform-native memory**;
  the in-flight peer openJiuwen `agent-memory`, tracked separately because it
  is ~198K LOC of real code aimed at the same neutral-substrate position; and
  the benchmark landscape), with the legacy tables retained in §2.7 for
  continuity with older CHANGELOG entries.
  The load-bearing correction is that **auto-injection is no longer an empty
  space**. Claude Code Auto Memory (v2.1.32), Codex Memories and Cursor
  Memories all auto-write and auto-load with zero infrastructure, which lands
  directly on the L2 moat rating. §11.1 therefore demotes **L2 from 🔴 core
  moat to 🟡 differentiator**, narrowed to the part a per-tool vendor
  structurally cannot do — one corpus shared across every agent — and adds
  **L6 "trusted memory" as a new 🔴 core moat**, naming parts that were
  already shipped but never positioned as a single claim: provenance-gated
  MUST (§6.2), identity verification and the multi-agent corroboration gate
  (§13.7), exclusion of unreviewed memories from injection (§13.8), the
  `owned`/`quarantine` trust boundary (§8.3.4) and the SHA-256 erasure
  tombstone (§9.5). L3 (follow-through) stays 🔴 — nothing in the survey
  measures whether an injected memory was obeyed.
  §2.5 splits the comparison into two tables, one on the market's own six
  dimensions (where MemVault loses rows on modality, time, graph and cloud
  form, and now says so) and one on dimensions no surveyed product is measured
  on (where MemVault is the only player). §2.6 records the counter-intuitive
  benchmark finding that LoCoMo/LongMemEval no longer discriminate a memory
  system from a 1M-token context window, and retargets evaluation at BEAM's
  instruction-adherence axis and MemoryAgentBench's conflict/forgetting axis,
  published on AMB with a full harness specification and p50/p95 latency
  reported next to accuracy.
  §12 gains two risks (platform-native memory eroding the injection moat;
  evaluation effort spent on non-discriminating benchmarks), **raises
  big-vendor copying from 🟢 to 🟡** because three vendors shipping native
  memory within a year is now observed behaviour rather than a hypothesis, and
  retires two rows whose mitigations have since landed (MCP message
  interception, via the Phase 2/3 proxy; compliance visibility, via §13.9).
  §4.1/§4.2/§5/§7.1 are corrected to stop describing MemPalace/Mem0 adapters
  that were never built — the pluggability that actually shipped is the
  sqlite↔markdown backend switch of §8.3. New work items land in §15:
  `as_of` valid-time queries plus `expires_at` (with an explicit constraint
  that valid-time and event-time stay separate axes), per-memory L0/L1
  disclosure layers (with the Fuser MaxP caveat that must ship alongside them),
  agent self-editing of Markdown memories gated through the Inbox, scenario
  profiles, the public benchmark run, and multimodal input.
  Two of those were added because building the rest exposed them: historical-text
  search (the residual half of `as_of`), and event-time being unsettable on the
  direct-save path. A third — `edit_md_file` — was recovered from §2.3, where it
  had been recorded only as a description of what agent-memory ships. A
  comparison table is not a work list, and an item that lives only in one is an
  item that gets lost.

- **BEAM instruction/preference-adherence harness**
  (`docs/experiments/beam_adherence.py`). The first third-party public benchmark
  MemVault can be scored on, and the axis chosen deliberately: §2.6 records that
  LoCoMo/LongMemEval no longer separate a memory system from a 1M-token context
  window, so a good score there would prove nothing, while BEAM's
  `instruction_following` and `preference_following` categories are the only
  public metric that measures whether an injected rule was **obeyed** — the L3/L6
  claim (§11.1) that no surveyed product even attempts. Pulls real
  `Mohammadta/BEAM` rows over the HF datasets-server HTTP API (no pyarrow or
  pandas needed), extracts the adherence probes with their `source_chat_ids`
  provenance, seeds each rule as a human-authored MUST, and runs control
  (question only) against experiment (question plus MemVault's own
  `/api/session` injection block) — the same real-subprocess harness shape as
  `verify_h6.py`/`verify_h7.py`.
  Three things about it are load-bearing rather than cosmetic. **The harness
  asserts its own mechanism**: every probe records `injection_format` and
  `instruction_layer_used`, and a run where any seeded MUST arrived as ordinary
  context reports `INVALID (harness invariant violated)` instead of emitting a
  verdict — because a MUST that silently degraded to REFERENCE would still put
  the rule text in the block while measuring a different claim. **Scoring is
  honest about being judge-dependent**: BEAM's rubric items are semantic
  descriptions, not literal strings, so unlike H5's proper-noun stems they cannot
  be substring-matched; the judge is primary, `--judge-stability N` measures
  self-agreement under a reworded prompt and prints a do-not-cite caveat below
  80%, and the one reliable objective check (markdown-fence presence for
  `format_instruction` probes) is reported separately and never merged in.
  **The full harness spec is emitted with the numbers** — models, base URLs,
  MemVault commit, prompt templates verbatim, scoring rules, split, n, and
  selection order — because most memory benchmarks have no official leaderboard
  and vendor self-reports use incompatible judges, so an unaccompanied number is
  not comparable; the script says exactly that on stdout when `--out` is omitted.
  Probes are taken in dataset order and never sampled. Embeddings default to off,
  which makes the run double as a measurement of §6.1.2's deterministic-MUST
  claim. `--dry-run` validates loading, provenance and time-anchor resolution
  without spending a token; `--dry-run --with-server` additionally boots
  MemVault and exercises seeding plus injection with no LLM calls.
  Plumbing validation on 20 probes across 5 conversations of the 100K split:
  20/20 provenance resolved, 20/20 event-time anchors resolved, 20/20 seeded,
  20/20 rule present in the injection block, 20/20 delivered via the `MustRef`
  instruction layer, injection **p50 ≈1.2 ms / p95 ≈1.6 ms** measured around
  `/api/session` alone (repeats within ±0.1 ms) — against mem0's published frame
  of p50 ≤ 1.1 s. That is
  a plumbing validation, **not** a benchmark score: no adherence number has been
  produced yet, and none should be cited without an LLM run plus its
  judge-stability measurement.
- **Two BEAM loader quirks handled explicitly**, both of which silently produce
  empty results if assumed away: `probing_questions` is a *Python literal* string
  (single-quoted, written straight from a Python pipeline) so `json.loads` fails
  and `ast.literal_eval` is required; and `time_anchor` is stamped on the first
  turn of each session only, so a 188-turn conversation with 60/56/72-turn
  sessions carries exactly three anchors — the loader walks back unbounded to the
  session start and records `time_anchor_origin` as `direct`/`inherited:<id>`/
  `none`, since an inherited anchor is a sound reading of the data but still a
  reading.

### Fixed

- **A latent test bug that broke on every `.0` release.**
  `older_server_is_replaced_and_the_rest_are_adopted` derived the "previous
  version" with `cli.2.saturating_sub(1)` on the patch component, which saturates
  to 0 when the patch is already 0 — so on 0.7.0 the derived "older" version came
  out *equal* to the current one, `classify` returned `Adopt` instead of
  `Replace`, and the test failed. That is not a corner case: it is every minor and
  major bump, i.e. exactly the releases where a leftover server from the previous
  version matters most. The 0.7.0 bump is what caught it; 0.6.12 and every earlier
  patch version masked it. Replaced with a `previous_release` helper that borrows
  from the minor and then the major, plus its own test over the interesting
  triples, and the caller now asserts the derived version really is older before
  asserting on it — a test whose premise silently became false is worse than one
  that fails loudly.
- **`import --format json` could not read the directory `export --format json`
  wrote to.** Export accepts a directory and writes `export.json` *inside* it, so
  backing up and restoring with the same path is the obvious thing to do — and it
  failed with a bare `Is a directory (os error 21)`, because JSON import read the
  path directly while markdown import already handled both forms. Import now
  resolves a directory to `export.json` inside it, and a directory without one
  produces an actionable error naming both the missing file and the export command
  that would have created it, rather than an OS errno. Verified by round-tripping
  a memory carrying `expires_at` through a directory path and confirming the bound
  survived — the asymmetry was found while checking whether the new temporal
  fields survive export/import at all (they do: both go through `Memory`'s serde).
- **`memvault-cli list` showed expired, superseded and pending-proposal memories
  with no indication that they are excluded from retrieval.** `list` is the
  inspection path and deliberately shows what retrieval filters out (§9.6) — but
  that is only useful if the reader can *tell*. An expired memory looked identical
  to a live one, so the natural conclusion on seeing "my rule is in `list` but the
  agent never gets it" was that search is broken, when the actual answer was that
  the memory had retired. Each state now carries a marker naming the consequence
  rather than just the state: `[expired <when>, not injected]`,
  `[superseded by <id>, not injected]`, `[proposes edit to <id>, awaiting review]`,
  and `[expires <when>]` for a bound that has not passed yet (worth showing, not a
  problem). States compose — a memory that is both expired and a proposal shows
  both, because it is not less of either.
- **The injected rule text was steering agents toward an action that silently
  de-reviews human-approved memories.** `plugins/memvault/rules/memvault.md` —
  the canonical block copied into `AGENTS.md`, `CLAUDE.md`, `.cursorrules`,
  `.clinerules`, `.kiro/steering` and `.qoder/rules`, i.e. text every agent reads
  at session start — told agents to write back "a correction of your behavior"
  with `save_memory`. But a save whose content is similar to an existing memory
  goes through delta-write, and `writer::merge_memory` sets
  `human_reviewed = false` / `ai_generated = true` when the incoming write is
  unreviewed (`test_merge_revokes_review_when_unreviewed_residual_added`). So an
  agent following that instruction to record a correction could merge into a
  human-approved MUST and revoke its review flag — after which §13.8's gate stops
  injecting it as a directive. The rule text was routing agents into the exact
  failure `propose_memory_edit` exists to prevent.
  The canonical block now separates the two cases: new durable learning →
  `save_memory`; correcting a memory that already exists → `propose_memory_edit`,
  with the reason stated so an agent that has never read §9.3.1 still knows why
  the slower-looking path is the correct one. It also gained a line telling agents
  to set `expires_at` on temporary facts, since a fact saved without a bound
  becomes a permanent one. Regenerated through `scripts/gen-rule-copies.sh` so all
  five copies stay byte-identical, as `check-rule-parity.sh` requires.
- **`run_promote` consolidated expired facts and pending edit proposals into
  higher-layer memories.** All four phases built their candidate sets from
  `store.list()` and filtered only on layer/type/superseded, so two classes of
  non-current memory were eligible. The proposal case is the serious one: a
  proposal inherits its target's priority and therefore its **layer**, so one
  aimed at a REFERENCE memory arrives at L2 with type `preference` — which
  satisfies the L3 `stable` filter (`access_count >= 2 || type == Preference`)
  with no access history at all. An unreviewed agent rewrite could therefore be
  consolidated into a persona rule that a human then approves on the strength of a
  summary that looks authoritative, and only *then* starts injecting. Expired
  memories were the same shape of problem: expiry does not change `layer`, so a
  lapsed L1 fact stayed eligible for consolidation.
  This was **not** a live injection leak — promotion writes
  `human_reviewed = false`, so §13.8's gate holds the output out of sessions — but
  it corrupted what the reviewer was being asked to approve, which is the one thing
  the gate depends on. `promote::hold_back` now excludes both at all four phases
  and reports the counts in `PromoteResult::excluded`, surfaced through the CLI,
  REST and MCP responses.
  Superseded memories are deliberately **not** added to the check. `supersede`
  demotes the old row to L0, so the layer filters already skip it, and a test
  pins that. Counting them would inflate the report with memories no phase ever
  considered — which is how a number gets ignored.
- **`memvault-cli sync` wrote unreviewed, superseded, expired and
  pending-proposal memories into `AGENTS.md` / `CLAUDE.md` / `.cursor/rules/` —
  bypassing the review gate entirely.** Pre-existing for the first two, extended
  to the latter two by this release's additions. `SyncEngine::select_memories`
  built from `store.list()`, which filters *nothing* on purpose because it is the
  inspection path, and scored candidates only on project relevance. Since those
  generated files are read by Codex, Cursor and Aider at session start, `sync` is
  an injection channel — and an unfiltered one is strictly weaker than the one
  §13.8 hardened, because the text reaches the model with no prompt-level wrapper
  at all. An auto-extracted draft that the gate was specifically built to keep out
  of every session was being written to disk where every agent would read it.
  The reason this survived is recorded in §13.8 rather than glossed over: that
  section enumerates the channels enforcing the gate, and `sync` was simply not
  in the list, so nothing looked wrong.
  Fixed by applying the same exclusions in `select_memories_reporting`. The review
  gate is applied by **calling `router::format::apply_unreviewed_gate`** rather
  than reimplementing the predicate, so the file channel and the session channel
  cannot drift apart. Superseded (§14 Track C), pending proposals (§9.3.1) and
  expired (§9.6) are excluded unconditionally — writing a replaced or lapsed rule
  into an instruction file is not a trust question, so
  `MEMVAULT_INJECT_REQUIRE_REVIEW=false` restores the old behaviour for the gate
  only. `SyncReport.memories_excluded` reports each reason separately and
  `memvault-cli sync` prints them, so "why isn't this rule in my AGENTS.md" has an
  answer without reading the source; silently dropping memories from a generated
  file would be the same class of failure as the leak itself.
  Seven existing sync tests encoded the old contract and now mark their fixtures
  reviewed. They were updated rather than reverted, and a `reviewed()` helper was
  introduced for them: a test about archived namespaces or low decay that used an
  *unreviewed* fixture would pass for the wrong reason, and would keep passing if
  the behaviour it claimed to check were deleted.
- **`list_inbox` did not expose edit proposals, making them unreviewable.** The
  MCP tool builds its own JSON shape rather than serialising `Memory`, and it
  omitted `proposes_supersede_of`. A pending proposal therefore looked *exactly*
  like a near-duplicate draft — same namespace, near-identical content, both
  unreviewed — so a reviewer following the documented flow would reject it as a
  duplicate (silently discarding a correction) or approve it without knowing that
  doing so supersedes a different memory. The CLI already labelled proposals
  (`[proposes edit to <target>]`) and REST returns full `Memory` objects, so the
  gap was specific to the tool an agent actually uses. `list_inbox` now returns
  `proposes_supersede_of` plus a `proposal` block carrying the target's current
  `content`/`instruction`, whether the target is still live, and what each action
  would do — so the diff can be made in one call instead of a lookup per
  proposal, and the consequence is stated in the payload rather than left to the
  reviewer knowing the mechanism. `memory_to_json` gains
  `proposes_supersede_of` for the REST/Dashboard side. The review skill and all
  four of its command variants (`commands/`, `plugins/memvault/commands/`,
  `integrations/codex/prompts/`, `integrations/opencode/command/`) now instruct
  the agent to check the field first. The three byte-identical `SKILL.md` copies
  were re-synced — `scripts/check-rule-parity.sh` does enforce that, with
  `plugins/memvault/skills` as canonical, so editing only the `skills/` copy would
  have failed CI. The four *command* variants are genuinely uncovered: they are
  independently worded rather than copies, so no byte-parity check can apply, and
  a rewrite that dropped the safety note would have passed. They now carry a
  content check for the `proposes_supersede_of` instruction instead.
- **`memvault-proxy`'s `review_memory` approved edit proposals without
  superseding their target.** The proxy carries its own copy of that tool — it is
  not a pass-through to `memvault-mcp` — and both of its approve branches
  (`action=approve` and `action=edit`) still flipped `human_reviewed` inline.
  Because the proxy, the MCP server and REST all share one store, a proposal
  created through any of them could be approved through the proxy, which would
  mark it reviewed and leave the target live beside it: two near-identical
  memories, both injectable, and no record that one was meant to replace the
  other. Both branches now route through `proposal::approve` /
  `proposal::approve_memory` like the five paths already routed there, and the
  tool response names the superseded target. That makes seven call sites across
  four surfaces (REST ×2, MCP server ×2, CLI ×1, proxy ×2) all reaching the same
  core function — which is the point of having one: this bug existed because the
  copies were found by reading call sites rather than by grepping for a helper. `propose_memory_edit` is deliberately
  *not* added to the proxy: it exposes 14 of the MCP server's 21 tools and has
  never mirrored the full set. Regression test added, since the bug was precisely
  that one of several copies of the same logic had been missed.
- **Stale MCP tool counts in four more places.** Adding `propose_memory_edit`
  took the surface from 20 to 21 tools; `README.md`, `README.zh-CN.md` and
  `DESIGN.md` were updated at the time but `docs/INSTALL.md`,
  `plugins/memvault/README.md`, `integrations/codex/README.md` and
  `integrations/opencode/README.md` were not. `docs/INSTALL.md` is the worst of
  the four because it is an installation *verification* step — a user following
  it would count 21 tools, see that the number did not match the guide, and
  suspect a broken install.
- **`archived_at` was invisible in the REST memory view.** `memory_to_json`
  emitted `occurred_at` and `superseded_by` but not `archived_at`, so the
  Dashboard could show that a memory had been archived by decay without being
  able to say *when* — and `archived_at` is precisely the field
  `ForgetManager::auto_forget` measures its retention window against, which made
  "why is this about to be permanently purged" unanswerable from the UI.
  Surfaced alongside the new `expires_at`.
- **An unreviewed caller can no longer swap a reviewed memory's
  instruction.** Re-saving an identical body with a different
  `instruction` goes through the near-exact duplicate branch's metadata
  refresh, which force-replaces the instruction — the very text injection
  renders. The merge-level trust revocation only covered first-time
  adoption (old instruction empty), so an unreviewed MCP `save_memory`
  replaced a human-approved MUST's directive ("Require approval" →
  "Disable approval") while `human_reviewed`/`ai_generated` stayed
  untouched, and the session injected it under the
  `[MEMORY CONTEXT - 指令（已人工确认）]` header. Replacing an existing
  directive with unreviewed text now revokes trust exactly like every
  other unreviewed absorption (round-trip regression: the reloaded row
  fails `is_trusted`); a reviewed incoming replacement keeps it.
- **Signed number updates are no longer deduped away.** The tokenizer
  dropped `+`/`-`, so "balance 5" → "balance -5" scored 1.00 and was
  Skipped as a pure duplicate — the sign flip (a value update) was lost.
  A sign that starts a token directly followed by a digit now stays
  attached (`-5` ≠ `5`); word-internal hyphens keep their separator
  behavior ("well-known", "model-3" tokenize as before). Fact
  consolidation's word splitter now delegates to the same tokenizer so
  the two can never drift apart again (it previously merged "+5" and
  "-5" facts at similarity 1.0).

## [0.6.12] - 2026-11-04

### Added

- **Markdown trust modes: `owned` (default) and `quarantine`.** MemVault is a
  local-first single-user store, so a hand-edited `{id}.md` is now trusted
  byte-for-byte by default: `human_reviewed`, `ai_generated`,
  `identity_verified`, priority and visibility in frontmatter are authoritative,
  exactly as if the owner had used the API. The trade-off is explicit — write
  access to the vault is session-injection authority. Set
  `MEMVAULT_MARKDOWN_TRUST_MODE=quarantine` for a shared/synced vault or a
  low-trust writer: unseen files start unreviewed + `ai_generated`, priority is
  capped at `REFERENCE`, `visibility: shared` is downgraded to `scoped`, and an
  existing row cannot be escalated. Unknown non-empty values fail closed to
  `quarantine`. The policy is shared by `reindex`, CLI Markdown import, and
  REST `/api/import`; JSON import and `migrate-to-markdown` remain trusted
  lossless paths.
- **Durable Markdown erase tombstone (`<markdown-dir>/.memvault-forgotten.json`).**
  A permanent forget now leaves a content-hash-only tombstone in the vault
  itself, not just in the rebuildable cache. `reindex` removes a reappearing
  stale file for a forgotten id, refuses to import it, and reports
  `skipped_forgotten`; `save`/`update` reject the retired id. The file contains
  no erased body and must be kept with the vault/backups.
- **Durable background compliance queue for `SessionEnd`.** Codex hard-caps
  `SessionEnd` hooks at three seconds, which made synchronous LLM compliance
  judging unsafe once a provider was configured. The hook now runs the
  LLM-free `memvault-cli compliance-enqueue`, persisting the host session id
  and transcript path in `compliance_finalize_jobs`; the long-lived
  `memvault-mcp --transport http` process consumes the queue and judges
  outside the hook deadline. Jobs are idempotent per host session and survive
  server restarts.

### Changed

- **Markdown trust enforcement is consistent across every file-origin entry
  point.** `Importer` now applies the same `owned`/`quarantine` policy as
  `reindex`, so importing a file through the CLI or REST cannot bypass the
  vault's configured trust model. A quarantined new file also cannot
  self-declare `priority: MUST` and claim the token/count exemptions reserved
  for trusted rules.
- **Quarantine now pins the whole ranking surface.** New files are forced to
  `layer: L1`; authority tags (`decision`/`procedure`/`gotcha`), confidence
  ceilings, and server-maintained decay/access/read/recency bookkeeping can no
  longer be forged to buy retrieval authority. Symlinked `.md` entries are
  skipped instead of followed, and forget removes the link, never its target.
- **Cold-start agent imports no longer bypass the review gate.**
  `import-agent` (CLI and REST) treats adapter output as second-hand bytes:
  unapproved candidates are forced back to `ai_generated=true`, so
  `is_trusted()` does not accept them merely because the source file claimed
  human authorship. Only the CLI's explicit `--approve` vouches for them.
- **Compliance fallbacks are explicit and bounded.** A configured
  `MEMVAULT_COMPLIANCE_WORKER_TIMEOUT_SECS` (default 90) bounds each job;
  missing/empty transcripts, no LLM provider, invalid judge output, provider
  errors, and deadline expiry all close the remaining rows as `unknown` with
  a reason. Stale `running` jobs are requeued after a worker crash. The
  `SessionEnd` hook timeout is now 3 seconds and its status text says it is
  queueing rather than finalizing.

### Fixed

- **Merge trust revocation now actually reaches the database.** Follow-up
  audit caught the persistence half of the review-inheritance fix: the
  sqlite `UPDATE memories` column list omitted `ai_generated`, so the flip
  `merge_memory` performs when absorbing unreviewed content (and the
  markdown quarantine's identical flip) existed only in memory — the
  reloaded row kept the `!ai_generated` human-authorship shortcut in
  `is_trusted()` and sessions were still injected `[MUST] 已人工确认`
  smuggled text. The column is now written by `UPDATE` (both storage
  backends share it — markdown's cache IS the sqlite store) and is pinned
  by a save→update→get round-trip test plus a full delta-write merge
  reproduction asserting the reloaded row fails the trust gate.
- **Numeric value updates survive the merge path end to end.** Keeping
  single-char tokens turned the silent `Skipped` into an honestly reported
  `Merged`, but the residual filter applied the same ≥-threshold rule at
  sentence level: "timeout is 5 seconds" → "timeout is 9 seconds" scores
  ~0.8 and was dropped as "known", so the updated value still never reached
  the stored content. Near-duplicate segments now stay residual when they
  carry a digit-bearing token the existing content has never seen, and
  near-exact (>0.95) whole-content matches with such a token route to
  `Merge` instead of `Skip` in both `check_duplicate` and `scan`. Pure word
  swaps ("in all editors" → "in the editors") remain covered paraphrases
  and are still dropped — the value-update exception does not reopen
  paraphrase bloat.
- **Merging unreviewed content no longer inherits human-review authority.**
  `writer::merge_memory` kept `human_reviewed = true` on a reviewed memory
  while appending unreviewed residual text (or adopting an unreviewed
  instruction / skill procedure), so content that never passed the review
  inbox was injected as `[MUST] 已人工确认`. A merge that pulls in unreviewed
  material now revokes `human_reviewed` and resets `ai_generated` — the same
  rule as the Markdown quarantine fix — routing the merged memory back
  through review. Merges of reviewed incoming content, and metadata-only
  merges with no new text, keep the review status as before.
- **Global MUST rules no longer silently vanish from project sessions.** The
  store matches `namespace` exactly, so a project-scoped session could only
  reach `global` MUST memories through the cross-namespace fallback — which
  fires only while the project has few non-MUST results. Adding project REF
  memories therefore dropped rules like "never deploy without approval" with
  no skip record. The MUST-completeness union now sweeps the project
  namespace plus `global`, matching what `deterministic_injection` and the
  layered overflow sweep already did.
- **The promotion pipeline no longer merges across namespaces.** L1→L2
  grouped by tag alone over an all-namespace scan, consolidating different
  projects' preferences into one global L2 (conflicting values lost to
  `consolidate_l1`'s 200-char join); fact consolidation and entity
  normalization had the same leak. All three passes are now namespace-scoped,
  and the promoted L2 keeps its source namespace.
- **Dedup no longer swallows single-character value changes.** `tokenize`
  dropped one-char tokens, scoring "timeout 5" vs "timeout 9" at 1.00 and
  skipping the update as a duplicate. Single-char tokens are kept now — the
  rule `consolidation_words` in promote already followed.
- **The review gate runs before injection budget cuts.** `trim_to_budget`
  and the count cap exempt MUST, so an unreviewed MUST used to survive the
  cuts, crowd out reviewed REF memories, then get gated away itself —
  injecting neither. Both review gates now run right after the archive
  exclusion, before quotas/budget/caps, so untrusted candidates never
  consume a slot that trusted content could use.
- **Embedding backfill terminates against empty-data providers.** A
  provider answering 200 with an empty `data` array made `do_backfill`
  report the batch as done without persisting anything, spinning
  `backfill_all_embeddings` forever. The batch now reports actual persisted
  progress (0 ends the drain) and logs a provider-configuration warning.
- **Merges keep the union of evidence traces.** `merge_memory` dropped the
  incoming memory's `source_trace_ids`, leaving the merged memory pointing
  at a strict subset of its own evidence chain.
- **The dsh plugin keeps per-session bookkeeping.** Turn numbers, pending
  turn text, and the outcome-evidence tracker were plugin-global: concurrent
  sessions overwrote each other's pending extraction, and one session's
  disposal uploaded another session's conversation as `infer_outcome`
  evidence/judge text. Everything is now keyed by `session.id` and dropped
  at disposal.
- **dsh proxy spawn failures no longer crash the host or fake readiness.**
  A missing binary emitted an unhandled `error` event that killed the whole
  dsh process (exit 1); a port already owned by another service passed the
  `/health` probe while our child died from the bind failure. The spawn
  error is now caught, logged, and fails `ready`; the readiness probe fails
  fast when the child died and re-checks after a short settle window.
- **The pi extension retries recall after transient failures and
  authenticates when keyed.** `injectedFor` was set BEFORE the recall
  round-trip, so one 503 disabled recall for the rest of the session; the
  session is now marked only on success. Requests also carry
  `x-memvault-api-key` from `MEMVAULT_API_KEY` (deployments with registered
  keys answered 401, silently swallowed), and a 401 is surfaced on stderr.
- **OpenCode idle extraction is watermarked and authenticates when keyed.**
  `session.idle` re-sent the whole transcript on every firing, filling the
  review inbox with duplicate drafts; the plugin now tracks extracted
  message counts per session, sends only the new tail, and advances the
  watermark only on a successful POST (failed extractions retry). Requests
  carry `x-memvault-api-key` from `MEMVAULT_API_KEY`, and 401s are surfaced
  instead of swallowed.
- **Hermes extraction actually persists and reads the real contract.**
  `extract_session` sent neither `auto_save` nor `source_id` — the endpoint
  defaults to extract-only, so drafts were returned and dropped, never
  reaching the review inbox — and then read a non-existent `extracted`
  response key, always reporting 0. It now sends `auto_save: true` +
  `source_id`, unwraps `{ok, data}` and counts `saved_ids`, and carries
  `x-memvault-api-key` when `MEMVAULT_API_KEY` is set. Session marking also
  moved after a successful recall (same transient-failure class as pi).
- **Bulk/auto forget executes the confirmed set, not a re-evaluated one.**
  The confirmation dialog checked the preview COUNT while the server
  re-evaluated the criteria at execute time — the matched set could drift in
  between and erase memories nobody confirmed. `POST /api/forget` now
  accepts a pinned `ids` list, and the dashboard executes with exactly the
  previewed `matched` ids. The auto-forget sweep also refuses to run before
  its config has loaded instead of silently guessing a 90-day retention.

- **Obsidian plugin lint toolchain no longer resolves the vulnerable
  `minimatch`.** An npm override pins every transitive copy to `^11.0.1`
  (resolved 11.0.4), clearing GHSA-g3hv-75wg-m249 / CVE-2026-118764 in the
  dev-only ESLint chain. Lint, typecheck, build, and 39 plugin tests pass with
  the override.
- **Quarantine revokes an approval when the reviewed bytes change.** Keeping
  the old frontmatter while replacing the Markdown body used to inherit
  `human_reviewed`, `identity_verified`, and corroboration, so the replacement
  injected as a trusted MUST instruction. In `quarantine` mode any body or
  `instruction` change now clears all three and resets `ai_generated` to
  `true`; `owned` mode still treats the file as the owner's authoritative
  edit.
- **Legacy cache-only erasure records are promoted to the durable tombstone.**
  Upgrading used to merge pre-tombstone `forgotten_log` rows only in memory,
  so deleting `.memvault-cache.db` made the erased file importable again.
  `reindex`/store open now writes those rows into
  `.memvault-forgotten.json` before building the active id set.
- **Compliance job completion and lease ownership are race-safe.** Each claim
  issues a `lease_token`; heartbeat/complete/fail are fenced by it, so a
  paused worker whose job was reclaimed can no longer mark the replacement
  worker's job done. The claim also snapshots the batch watermark, and
  completion atomically returns the job to `queued` when a newer pending
  batch exists, instead of losing that batch after reporting `done`.
- **A worker that lost its compliance lease can no longer write verdicts.**
  Job-state fencing alone still let the old worker finish judging and update
  `compliance_events` after a takeover. Verdict and unknown-fallback writes
  now check the lease token in the same SQL statement, and a failed heartbeat
  cancels the in-flight judge future. Batch watermarks also use a persisted
  monotonic `batch_seq` instead of SQLite `rowid`; deleting the newest event
  no longer lets a fresh event reuse its id and fall inside an older claim.
- **The compliance watermark migration is restartable and runs only once.**
  The old path skipped the legacy `batch_max_rowid` copy whenever
  `batch_max_seq` already existed, so a crash between `ALTER TABLE` and the
  copy left a zero watermark and let new events reuse low sequence numbers.
  The migration now runs in one immediate transaction and records a completion
  marker after the copy, so an interrupted upgrade still recovers while a
  legitimate empty-batch watermark of `0` is no longer re-inflated from the
  dead legacy column on every restart.
- **A compliance heartbeat can no longer starve the work it is guarding.**
  The heartbeat used to await the store mutex inside the selected `select!`
  branch, so work queued for that same mutex was no longer polled and the pair
  could deadlock. Work and heartbeat now stay concurrent futures for the whole
  lease.
- **Omitting `ai_generated` in Markdown now keeps the documented safe
  default.** The parser treated an absent field as human-authored
  (`false`), while `DESIGN.md` promised an unreviewed draft (`true`). A
  hand-written file that should inject as a trusted instruction must say
  `ai_generated: false` explicitly (or be approved).
- **Permanent erasure now covers soft-deleted memories on both backends.**
  `forget` used to treat "no live row" as "this id never existed": the
  Markdown backend returned `NotFound` and left the restorable
  `memory_history` snapshot in `.memvault-cache.db`, while SQLite purged the
  history through a shortcut that skipped the MUST/Shared protection checks,
  the `forgotten_log` audit entry, and the compliance orphan cleanup. The
  erase pipeline is now unified (resolve live row or history snapshot →
  protection → audit → purge → compliance cleanup), `MarkdownStore::purge`
  no longer requires a live row, `NotFound` is distinguished from storage
  failures, and a failed cleanup is reported in `failed` instead of being
  silent (H6).
- **Permanent erase survives stale Markdown copies.** With the Markdown
  backend, a `git checkout`, sync client, editor undo, or backup copy could
  previously re-add an erased file on the next `reindex`; the audit row did not
  block re-import. The durable tombstone above closes that resurrection path
  and is merged with the legacy cache-only `forgotten_log`, so upgrades keep
  blocking ids forgotten before the tombstone existed.
- **Forget covers a stale Markdown file even without cache or history.**
  If the disposable cache was rebuilt and an id survives only as `{id}.md`,
  forget now reads that persisted snapshot, applies the normal protection /
  audit / purge / compliance pipeline, and writes the durable tombstone. The
  in-process tombstone set avoids re-parsing the whole JSON file on every save,
  and a final write-gated check prevents `reindex` from racing a forget and
  restoring the stale row.
- **`reindex` no longer ignores metadata-only Markdown edits.** The
  no-change check whitelist was too narrow, so hand-revoking
  `human_reviewed`/`identity_verified` (or editing `archived:`) without a
  body change left the cache pinned to the stale values. All persisted
  semantic frontmatter fields now participate in change detection. The trust
  policy is mode-specific: `owned` (default) accepts the owner's edits, while
  `quarantine` clamps `human_reviewed`/`identity_verified`/`ai_generated`
  upgrades so a file cannot mint a "human-authored" instruction out of an
  AI-generated memory (H1).
- **Corroboration counts verified agents, not a memory-level flag.** A
  verified contributor merging into an unverified author's memory used to
  flip `identity_verified` and let later merges add the unverified author to
  `corroborating_agents` — one agent could appear as two identities and trip
  the MUST corroboration gate. Verification is now bound to concrete
  `source_agent.id`s already recorded in `corroborating_agents` (or to a
  direct verified save with an empty corroboration list), so repeated
  confirmations from one agent never increase the count (M2).
- **`SessionEnd` can enqueue while a Markdown-backed server is running.**
  The producer used to open a full `MarkdownStore` and fail on the vault's
  `.memvault.lock`, which the long-lived server holds for its whole
  lifetime. `compliance-enqueue` now writes directly to the compliance side
  store in `<markdown-dir>/.memvault-cache.db`, matching the SQLite fast
  path without weakening the vault's file-concurrency lock (#5).
- **A resumed session reopens its completed compliance job.** Jobs are
  deduplicated per host session, but a `done` job with a newly accumulated
  pending injection batch is now returned to `queued` with a fresh attempt
  count and the newer transcript; duplicate SessionEnd deliveries with no
  new rows remain idempotently `done` (#6).
- **Stale `running` compliance jobs are recovered without another restart.**
  Reaping now runs periodically in the worker loop instead of only at
  startup, so a consumer restarted before the ten-minute threshold still
  recovers the job when it later expires. Active jobs publish a heartbeat
  every 30 seconds, so a legitimately slow LLM batch keeps its lease and is
  not judged twice (#7).
- **OpenCode idle extraction now persists to the review inbox.** The plugin
  sent neither `auto_save=true` nor the `source_id` the endpoint uses for
  provenance, so extracted candidates were returned and dropped. It now
  requests persistence, records the agent as the source, and warns when a
  response contains candidates but no saved ids (L2).

- **Obsidian plugin 0.3.7 stops the panel flashing on every auto-refresh
  tick.** The sidebar now fetches first and swaps the panel in one step, and
  skips the rebuild entirely when the payload is unchanged, so the default 10s
  refresh stops clearing the DOM and replaying the card entrance animation.
  Scroll position survives a real update, and changing the interval (including
  `0` to disable it) takes effect without reopening the view; the legacy
  settings path no longer silently turns `0` into `10`.

## [0.6.11] - 2026-10-30

### Added

- **`scripts/fetch-ort-cache.sh` pre-seeds the ONNX Runtime build cache.**
  `ort-sys` downloads its prebuilt static library with a 30-minute global
  timeout and no retry, so a stalled connection makes `cargo build` look hung.
  The script resolves the pinned `ort-sys` version from `Cargo.lock`, downloads
  the matching archive with `curl` retries, verifies its SHA-256, and populates
  the cache directory `ort-sys` checks first; see `docs/TROUBLESHOOTING.md`
  §9.4.

### Changed

- **MCP tool descriptions now route between sibling tools.** The five
  lowest-scoring TDQS entries (`get_compliance_report`, `extract_memories`,
  `record_outcome`, `run_promote`, `get_memory_evidence`) now name the sibling
  to use instead, state `run_dedup`/`run_decay` ordering, and describe their
  return payloads, so agents stop guessing between the
  compliance/outcome/evidence pairs. `extract_memories` also documents that
  `auto_save=true` writes go through delta-write rather than duplicating
  near-identical memories, and `get_compliance_report` /
  `get_effectiveness_report` document their `limit` parameters in the input
  schema.

- **Documentation references resynced with the code.** The generated command
  table in `CONTRIBUTING.md` now includes the Obsidian plugin's
  `npm run lint` gate and documents `cargo fmt --all -- --check` exactly as CI
  runs it; the environment-variable table now lists `MEMVAULT_SERVE_WEB`.

### Fixed

- **Compliance reports no longer fabricate 100%.** `ComplianceReport.compliance_rate`,
  `AggregateSummary.overall_rate` and `AggregateSummary.must_rate` are now
  `Option<f64>`: they are `None` (rendered `N/A` in the dashboard and MCP/REST
  payloads) until at least one MUST row has a real followed/violated verdict.
  Previously every pending row produced a hard-coded `1.0`, so a session the
  agent may have ignored looked perfectly compliant.

- **The injection → compliance loop is actually closed.** `SessionStart` now
  stamps the host-native `session_id` on each compliance row
  (`compliance_events.host_session_id`, migrated in place), and a new
  `SessionEnd` hook (`MEMVAULT_HOOK_COMPLIANCE=1`, on by default) runs
  `memvault-cli compliance-finalize --hook-input`: with an LLM extractor
  configured each injected memory is judged followed/violated/unknown against
  the transcript, without one the rows are closed as explicitly `unknown`.
  `--stale-before` closes historical backlogs left pending by older releases,
  and every `SessionStart` sweeps rows older than 24 h that never got a
  report (MCP/REST callers, hosts without a session-end event), so the
  backlog cannot rebuild silently. The hook exits 0 on every failure path, so
  a memory outage never breaks the host session.

- **Unreviewed memories no longer reach new sessions.** New
  `MEMVAULT_INJECT_REQUIRE_REVIEW` gate, **on by default**, removes every
  AI-extracted memory that a human has not reviewed from injection — all
  priorities, not just MUST — across the CLI/MCP body, the MCP
  `[MORE - 摘要]` overflow summaries, REST `/api/session`, and the proxy
  deterministic fast path, recording an `unreviewed-gated` skip reason.
  `MEMVAULT_INJECT_REQUIRE_REVIEW_FOR_MUST` remains as the legacy MUST-only
  filter and defense-in-depth when the broad gate is disabled. `memvault-cli
  save` is treated as an explicit human-initiated write and is therefore not
  gated; Stop-hook extraction, `extract_memories` and outcome lessons still
  land in the review inbox first.

- **Proxy and MCP compliance tools authenticate like every other tool.**
  `memvault-proxy`'s `report_compliance`, `get_compliance_report` and
  `notify_response` (plus the other proxy tools that skipped auth) and
  `memvault-mcp`'s `report_compliance`, `get_compliance_report` and
  `get_effectiveness_report` now run `authenticate_agent` before acting:
  anonymous callers can no longer tamper with compliance records, read
  compliance summaries under a victim agent id, or trigger extraction
  writes once any agent key is registered. Proxy `save_memory` now stamps
  `identity_verified` from `authenticate_agent_verified`, matching MCP/REST.

- **Markdown reindex treats trust fields monotonically.** An external edit
  to a vault file can keep or *downgrade* `human_reviewed`,
  `identity_verified`, `corroborating_agents` and `priority`, but cannot
  upgrade them — a filesystem-writable process can no longer mint a "human
  reviewed" / "identity verified" MUST memory by rewriting frontmatter.
  Blocked upgrades are rewritten back into the file so reindex converges
  instead of resyncing forever.

- **`POST /api/forget` fails closed on an invalid `type`.** A misspelled or
  invented `memory_type` value now returns `400` instead of silently
  degrading to a no-filter criteria that used to purge *every* type in the
  namespace — a single typo can no longer widen a destructive query.

- **Export skips ids that could escape the output directory.** Memory ids
  containing `..` or path separators no longer become `export_to_dir`
  write targets, closing an arbitrary-file-write primitive reachable via
  store/import-injected ids.

- **Forget now purges history of already-soft-deleted memories.** Purging
  an id whose live row is gone still erases its `memory_history` snapshots,
  so `restore_checkpoint` cannot resurrect "permanently erased" content;
  `purge` is idempotent for previously-deleted ids.

- **Failed duplicate markdown saves restore the original file.** When a
  second save with the same id is rejected by the cache, the directory now
  keeps the canonical row's file instead of deleting it (which previously
  made the next reindex drop the row entirely — data loss on a failed
  write).

- **REST agent routes accept header credentials.** Agent-facing endpoints
  (`/api/session`, `/api/memories`, `/api/outcome`, `/api/search`) now honor
  `X-MemVault-Agent-Id` / `X-MemVault-Api-Key` headers in addition to the
  body contract, so the server no longer ignores header keys that admin
  routes require.

- **`archived_at` survives the markdown round-trip.** Exported frontmatter
  now carries `archived:` and imports/reindexes parse it back, so an
  archived memory no longer silently loses its archive time and escapes the
  auto-forget retention sweep.

- **`server stop` verifies the PID before signalling it.** A stale pid file
  can no longer make the CLI kill an unrelated process: the command line is
  inspected first (Linux `/proc`, POSIX `ps`, Windows CIM) and the stop is
  refused unless the executable is `memvault-mcp`. Uninspectable PIDs also
  fail closed with instructions to remove the stale file.

- **Namespace-only markdown edits now reindex.** The unchanged comparison
  includes `namespace`, so hand-editing that field no longer leaves the file
  and cache silently divergent forever.

- **`/api/extract` warns when candidates were not saved.** With the default
  `auto_save=false`, a non-empty extraction now returns an explicit
  `warning` alongside the empty `saved_ids` instead of looking like a
  successful ingest.

- **The pi extension matches pi's current ExtensionAPI.** `before_agent_start`
  now returns a `CustomMessage` wrapper (`customType`/`content`/`display`)
  and reads the session id from `ctx.sessionManager`; `memvault-review` uses
  the `(name, { description, handler })` command shape. A root `package.json`
  `pi` manifest makes `pi install git:github.com/dreamor/memvault` load the
  extension, and a Node contract test locks both shapes down.

- **The dsh plugin no longer overwrites `~/.memvault/proxy.yaml`.** Spawned
  proxies receive `--transport`, `--port`, and `--db` on the command line,
  preserving existing `upstreams` and `markdown_dir` entries instead of
  replacing the user's config with an empty topology.

- **`agents.yaml` no longer fails startup for optional metadata.** Missing
  `description` or `inject_rules` now defaults to an empty description and
  the built-in injection rules instead of rejecting the entire registry.

- **The Codex plugin refresh script no longer trips Codex's 30-second Git
  marketplace clone timeout on this repository.** Git registrations now use a
  sparse checkout for `plugins/memvault` and `.agents/plugins`; legacy
  non-sparse registrations are recreated with sparse paths when an in-place
  marketplace upgrade hits a timeout or truncated clone.

## [0.6.10] - 2026-10-29

### Fixed

- **`search_memory` now honors `top_k=0` as "return no memories" in every
  mode.** The SQLite keyword path clamped `top_k` up to 1
  (`clamp(1, 1000)`), so a caller passing `top_k: 0` got one result back in
  keyword mode — and in hybrid mode whenever it degraded to keyword — while
  semantic/hybrid returned `[]`. Zero is now short-circuited to an empty
  result at the MCP tool and REST `/api/search` layers (before any embedder
  warm-up wait or embedding call) and in the storage layer (`search` and
  `vector_search`), with regression tests across all three modes and the
  degraded hybrid→keyword path. `top_k=1`, ordinary values and the 1000 cap
  are unchanged.

- **Linux x86_64 release binaries can no longer drift past the documented
  glibc floor.** The v0.6.9 `x86_64-unknown-linux-gnu` archive was built on
  `ubuntu-latest`, whose glibc had advanced to 2.43; the shipped binaries
  required `remainderf@GLIBC_2.43` & friends and failed to even print
  `--version` on Ubuntu 24.04 (glibc 2.39) — a P1 upgrade blocker inside the
  documented support range. The GNU build jobs are now pinned to
  `ubuntu-24.04` / `ubuntu-24.04-arm`, and every release runs a post-build
  gate on the pinned runner (which is itself the minimum supported
  environment): `objdump -T` symbol check (no `GLIBC_* > 2.39`, no
  `GLIBCXX_* > 3.4.33`) plus a functional smoke test — all three binaries
  `--version` (`memvault-proxy` gained the flag), CLI save/search on a
  throwaway database, and MCP & Proxy stdio `initialize`. The floor is now
  uniformly documented as glibc ≥ 2.39 (README ×2, AGENT-INSTALL,
  CONTRIBUTING, RELEASING).

### Changed

- **README and MCP Registry logos now load on networks that block
  `raw.githubusercontent.com`.** Both READMEs and
  `integrations/mcp-registry/server.json` reference the mascot through jsDelivr
  (`cdn.jsdelivr.net/gh/dreamor/memvault@master/...`) instead of GitHub's raw
  host, which is DNS-poisoned on some networks; GitHub's camo proxy serves the
  jsDelivr URL normally, so the rendered logo no longer depends on raw ingress.

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
  same memory isn't delivered twice (docs/DESIGN.md §6.1.3), and
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

[Unreleased]: https://github.com/dreamor/memvault/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/dreamor/memvault/compare/v0.6.12...v0.7.0
[0.6.12]: https://github.com/dreamor/memvault/compare/v0.6.11...v0.6.12
[0.6.11]: https://github.com/dreamor/memvault/compare/v0.6.10...v0.6.11
[0.6.10]: https://github.com/dreamor/memvault/compare/v0.6.9...v0.6.10
[0.6.9]: https://github.com/dreamor/memvault/compare/v0.6.8...v0.6.9
[0.6.4]: https://github.com/dreamor/memvault/compare/v0.6.3...v0.6.4
[0.4.4]: https://github.com/dreamor/memvault/compare/v0.4.3...v0.4.4
[0.4.3]: https://github.com/dreamor/memvault/compare/v0.4.2...v0.4.3
[0.4.2]: https://github.com/dreamor/memvault/compare/v0.4.0...v0.4.2
[0.4.1]: https://www.npmjs.com/package/@dreamor/dsh-memvault/v/0.4.1
[0.4.0]: https://github.com/dreamor/memvault/releases/tag/v0.4.0
[0.3.0]: https://github.com/dreamor/memvault/releases/tag/v0.3.0
